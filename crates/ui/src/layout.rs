//! Layout over Craie-owned storage, through the owned flex engine
//! (`craie-layout`, ARCHITECTURE.md §4).
//!
//! The engine owns no tree here. It reads children and rows straight out
//! of the `Host` through `LayoutTree` and writes results into `Layouts`, a
//! parallel per-node store. Dirty propagation is explicit: a node's cache
//! key covers only its own inputs, not its children, so a mutation clears
//! the cache on the node and every ancestor up to the root.
//!
//! Units: everything in this module is logical (DPI-independent). The
//! display scale enters only at emit time, when text origins and raster
//! sizes convert to physical pixels.
//!
//! The value types (`Size`, `LayoutInput`, `Layout`, the cache) are
//! Taffy's; its features are restricted to `flexbox`, and block, grid,
//! float, and calc are compiled out.

use craie_layout::{
    FlexScratch, LayoutTree, Node, compute_cached, compute_flex, compute_hidden, compute_leaf,
    compute_root,
};
use taffy::util::{MaybeResolve, ResolveOrZero};
use taffy::{
    AvailableSpace, Cache, Layout, LayoutInput, LayoutOutput, Line, Point, RequestedAxis, RunMode,
    Size as TSize, SizingMode,
};

use crate::geom::{Rect, Size};
use crate::host::{Host, NodeFlags, NodeId, Paragraph};
use crate::input::Inputs;
use crate::mutation::NodeKind;
use crate::text::TextEngine;
use crate::text::paragraph::{Paragraph as TextParagraph, SpanStyle, TextSpec, TextStyle};
use craie_layout::LayoutRow;

/// Computed border box for a node, relative to its parent's content box,
/// in logical units. Written by `round_layout`, read by paint and
/// hit-testing (which accumulate offsets during traversal).
///
/// `content` is the node's content-box origin relative to its border box:
/// border.left + padding.left, border.top + padding.top.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutData {
    pub rect: Rect,
    /// Content-box origin relative to the border box: border+padding
    /// left/top.
    pub content: [f32; 2],
    /// Total border+padding insets (horizontal, vertical): the content
    /// box size is `rect.size - insets`.
    pub insets: [f32; 2],
    /// Padding box relative to the border-box origin — the rect that
    /// `overflow: clip|hidden|scroll` clips descendants to.
    pub clip_box: Rect,
    /// Maximum scroll offsets (x, y) in logical points, from Taffy's
    /// scrollable overflow rect. Zero when content fits.
    pub scroll_extent: [f32; 2],
}

/// A text leaf's retained paragraph, plus the wrap width its lines were
/// made for (`u32::MAX` bits = unwrapped). Shaped again only when the
/// paragraph's text or metrics change; a new wrap width only rewraps
/// (no shaping). Emitting it costs no re-rasterization.
pub struct MeasuredText {
    pub layout: TextParagraph,
    pub wrap_bits: u32,
}

/// Per-node layout results. `cache`, `unrounded` and `rects` are indexed
/// by node id and grow with the host arena. Layout inputs are not here:
/// each node owns its row in `Host::layout`.
pub struct Layouts {
    /// Style for ids outside the host arena.
    default: LayoutRow,
    cache: Vec<Cache>,
    unrounded: Vec<Layout>,
    rects: Vec<LayoutData>,
    /// Slots created since their last layout: their `rects` row is the
    /// previous occupant's.
    unlaid: Vec<bool>,
    /// A probe pass (`Ui::probe_targets`) is running: lists commit
    /// nothing.
    pub(crate) probing: bool,
    /// Layout passes run (cost counter).
    pub passes: u64,
    /// Taffy cache behavior, for experiments: hits/misses since `new`.
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Nodes whose border-box origin changed in the last passes: their
    /// subtree's placements move. Drained by the scene sync.
    pub moved: craie_core::dirty::DirtyQueue,
    /// Nodes whose size, insets, or clip box changed (origin unchanged):
    /// their own chunk and clip update.
    pub resized: craie_core::dirty::DirtyQueue,
    /// Nodes whose scroll extent changed: their offset is re-clamped.
    pub extents: craie_core::dirty::DirtyQueue,
    /// The flex engine's reused buffers.
    scratch: FlexScratch,
}

fn row_mut<T: Default + Clone>(rows: &mut Vec<T>, i: usize) -> &mut T {
    if i >= rows.len() {
        rows.resize(i + 1, T::default());
    }
    &mut rows[i]
}

impl Layouts {
    pub fn new() -> Layouts {
        Layouts {
            default: crate::host::default_style(),
            passes: 0,
            cache: Vec::new(),
            unrounded: Vec::new(),
            rects: Vec::new(),
            unlaid: Vec::new(),
            probing: false,
            cache_hits: 0,
            moved: Default::default(),
            resized: Default::default(),
            extents: Default::default(),
            cache_misses: 0,
            scratch: FlexScratch::default(),
        }
    }

    /// Final relative rect for a node, or ZERO if never laid out.
    pub fn rect(&self, id: NodeId) -> Rect {
        self.rects
            .get(id.0 as usize)
            .map(|d| d.rect)
            .unwrap_or(Rect::ZERO)
    }

    /// A new occupant of `id`'s slot: its layout is unknown until the
    /// next pass lays it out.
    pub fn forget(&mut self, id: NodeId) {
        if self.unlaid.len() <= id.index() {
            self.unlaid.resize(id.index() + 1, false);
        }
        self.unlaid[id.index()] = true;
    }

    /// Whether `id`'s current occupant has been laid out.
    pub fn is_laid_out(&self, id: NodeId) -> bool {
        id.index() < self.rects.len() && !self.unlaid.get(id.index()).copied().unwrap_or(false)
    }

    /// Full paint-relevant data for a node, or defaults.
    pub fn data(&self, id: NodeId) -> LayoutData {
        self.rects.get(id.0 as usize).copied().unwrap_or_default()
    }

    /// Drops every per-node layout cache (resize: every wrap width may
    /// change).
    pub fn clear_all_caches(&mut self, slots: usize) {
        if self.cache.len() < slots {
            self.cache.resize(slots, Cache::default());
        }
        for c in &mut self.cache {
            *c = Cache::default();
        }
    }
}

impl Default for Layouts {
    fn default() -> Layouts {
        Layouts::new()
    }
}

/// Recomputes layout for the tree rooted at `root` (a real node; wrap
/// multiple roots in a View), then copies results into `Layouts`. Call
/// `invalidate` first. TEXT leaves are measured through `text` and
/// retained in `texts` (indexed by node id) for paint. Returns
/// (root-layout ms, finalize ms).
pub fn compute_timed(
    host: &mut Host,
    store: &mut Layouts,
    text: &mut TextEngine,
    texts: &mut Vec<Option<MeasuredText>>,
    inputs: &mut Inputs,
    root: NodeId,
    available: Size,
) -> (f64, f64) {
    let mut view = TreeView {
        host,
        store,
        text,
        texts,
        inputs,
    };
    let space = TSize {
        width: AvailableSpace::Definite(available.width),
        height: AvailableSpace::Definite(available.height),
    };
    let t = std::time::Instant::now();
    compute_root(&mut view, root.0, space);
    let compute_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    view.finalize(root);
    let round_ms = t.elapsed().as_secs_f64() * 1000.0;
    (compute_ms, round_ms)
}

/// Clears the layout cache for every node in the host's dirty queue and all
/// of its ancestors, then clears the flags. Child changes invalidate the
/// path to the root because a node's cache key does not include its
/// children.
pub fn invalidate(host: &mut Host, store: &mut Layouts) {
    for id in host.dirty.layout.take().into_iter().map(NodeId) {
        let mut cur = id;
        loop {
            // Detached/subtree-orphaned nodes carry DETACHED as their
            // parent — walking into the sentinel would index the arena
            // out of bounds, so the bounds check comes first.
            let idx = cur.0 as usize;
            if idx >= host.slot_count() || cur == NodeId::DETACHED {
                break;
            }
            *row_mut(&mut store.cache, idx) = Cache::default();
            if let Some(n) = host.node_mut(cur) {
                n.flags.clear(NodeFlags::LAYOUT);
            }
            let p = host.parent(cur);
            if p.is_nil() || p == NodeId::DETACHED {
                break;
            }
            cur = p;
        }
    }
}

/// The engine's view of the world: host tree + layout rows + per-node
/// state + the text engine for leaf measurement.
struct TreeView<'a> {
    host: &'a mut Host,
    store: &'a mut Layouts,
    text: &'a mut TextEngine,
    texts: &'a mut Vec<Option<MeasuredText>>,
    inputs: &'a mut Inputs,
}

impl TreeView<'_> {
    fn style_of(&self, id: NodeId) -> &LayoutRow {
        self.host
            .layout
            .get(id.index())
            .unwrap_or(&self.store.default)
    }

    /// Copies the computed (unrounded) layout into `rects` recursively.
    /// Coordinates stay logical/fractional here — snapping happens on the
    /// physical grid at paint time, so a non-integer display scale never
    /// accumulates rounding error.
    fn finalize(&mut self, id: NodeId) {
        let layout = self.get_unrounded_layout(id);
        self.set_final_layout(id, &layout);
        let count = self.host.child_count(id);
        for i in 0..count {
            self.finalize(self.host.child_at(id, i));
        }
    }

    /// Leaf measurement. TEXT nodes shape through Parley at the content
    /// width Taffy offers; other leaves are zero-sized.
    ///
    /// `available` carries Taffy's sizing mode: a definite width is the
    /// content box to wrap at, `MaxContent` measures unwrapped, and
    /// `MinContent` wraps at every break opportunity (wrap width zero).
    /// `known` dimensions are already final — nothing to measure.
    /// `parent` is the containing block's size (percentages).
    fn measure(
        &mut self,
        id: NodeId,
        known: TSize<Option<f32>>,
        available: TSize<AvailableSpace>,
        parent: TSize<Option<f32>>,
    ) -> TSize<f32> {
        if let (Some(w), Some(h)) = (known.width, known.height) {
            return TSize {
                width: w,
                height: h,
            };
        }
        let Some(node) = self.host.node(id) else {
            return TSize::ZERO;
        };
        if node.kind == NodeKind::Input {
            // Inputs measure like text: the editor wraps at the definite
            // content width; otherwise it takes its natural width.
            let w = match available.width {
                AvailableSpace::Definite(w) => w,
                _ => 160.0, // unconstrained default editing width
            };
            let size = self.inputs.measure(self.text, id.0, w);
            return TSize {
                width: size.width.max(w.min(160.0)),
                height: size.height,
            };
        }
        if node.kind == NodeKind::Vector {
            // The view box is the intrinsic content size (aspect kept). A
            // sized axis (known, or set in the style) is its content box:
            // Taffy's `known` is the border box, `available` the content
            // box of a sized axis.
            let Some(a) = self.host.vectors.get(&id.0).and_then(|v| v.asset.as_ref()) else {
                return TSize::ZERO;
            };
            let style = self.style_of(id);
            let size = style.size();
            let content = |known: Option<f32>, set: bool, avail: AvailableSpace| {
                (known.is_some() || set)
                    .then(|| avail.into_option())
                    .flatten()
            };
            // Min and max sizes resolve against the containing block
            // (Taffy's rules: padding and border percentages against its
            // width); under border-box sizing they lose padding and
            // border to become content sizes.
            let padding = style.padding().resolve_or_zero(parent.width, no_calc);
            let border = style.border().resolve_or_zero(parent.width, no_calc);
            let inset = [
                padding.left + padding.right + border.left + border.right,
                padding.top + padding.bottom + border.top + border.bottom,
            ];
            let content_box = style.box_sizing() == taffy::BoxSizing::ContentBox;
            let limit = |d: taffy::LengthPercentageAuto, of: Option<f32>, axis: usize| {
                d.maybe_resolve(of, no_calc).map(|v| {
                    if content_box {
                        v
                    } else {
                        (v - inset[axis]).max(0.0)
                    }
                })
            };
            let [width, height] = crate::vector::constrained(
                a.view_box,
                [
                    content(known.width, !size.width.is_auto(), available.width),
                    content(known.height, !size.height.is_auto(), available.height),
                ],
                [
                    limit(style.min_size().width, parent.width, 0),
                    limit(style.min_size().height, parent.height, 1),
                ],
                [
                    limit(style.max_size().width, parent.width, 0),
                    limit(style.max_size().height, parent.height, 1),
                ],
            );
            return TSize { width, height };
        }
        if node.kind != NodeKind::Text {
            return TSize::ZERO;
        }
        let text_dirty = node.flags.contains(NodeFlags::TEXT);

        // available.width is the content box: wrap text there.
        let wrap = match available.width {
            AvailableSpace::Definite(w) => Some(w.max(0.0)),
            AvailableSpace::MinContent => Some(0.0),
            AvailableSpace::MaxContent => None,
        };
        let wrap_bits = wrap.map(f32::to_bits).unwrap_or(u32::MAX);

        let slot = id.0 as usize;
        if slot >= self.texts.len() {
            self.texts.resize_with(slot + 1, || None);
        }
        if !text_dirty && let Some(m) = &mut self.texts[slot] {
            if m.wrap_bits != wrap_bits {
                // Another width: rewrap the shaped paragraph.
                self.text.rewrap(&mut m.layout, wrap);
                m.wrap_bits = wrap_bits;
                self.host.dirty.content.push(id.0);
            }
            let m = self.texts[slot].as_ref().unwrap();
            return TSize {
                width: m.layout.width,
                height: m.layout.height,
            };
        }

        let Some(p) = self.host.paragraph(id) else {
            return TSize::ZERO;
        };
        let layout = shape_paragraph(self.text, p, wrap);
        let size = TSize {
            width: layout.width,
            height: layout.height,
        };
        self.texts[slot] = Some(MeasuredText { layout, wrap_bits });
        // The shaped paragraph changed: its chunk must be re-emitted.
        self.host.dirty.content.push(id.0);
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::TEXT);
        }
        size
    }
}

/// Resolves nothing through `calc` (the feature is compiled out).
fn no_calc(_: *const (), _: f32) -> f32 {
    0.0
}

/// Virtualized lists (§7): a list is a leaf to its parent whose content
/// is its rendered rows, each laid out at the list's content width and
/// placed at its item's offset. Only rendered rows are visited.
impl TreeView<'_> {
    fn list_layout(&mut self, id: NodeId, inputs: LayoutInput, style: &LayoutRow) -> LayoutOutput {
        // A probe (the animation driver's) lays rows out without
        // recording measurements or estimates.
        let commit = inputs.run_mode == RunMode::PerformLayout;
        let record = commit && !self.store.probing;
        let parent_w = inputs.parent_size.width;
        let inset = style.padding().resolve_or_zero(parent_w, no_calc)
            + style.border().resolve_or_zero(parent_w, no_calc);
        // The definite content height, as flex resolves a percentage row
        // gap against it: the known height, else the style's (clamped
        // by min/max), minus the insets of a border box.
        // Min/max heights as content-box bounds (the used size of an auto
        // height is the content clamped by them).
        let parent_h = inputs.parent_size.height;
        let inset_v = inset.top + inset.bottom;
        let to_inner = |h: f32| {
            if style.box_sizing() == taffy::BoxSizing::ContentBox {
                h
            } else {
                (h - inset_v).max(0.0)
            }
        };
        let (min_inner, max_inner) = match inputs.sizing_mode {
            SizingMode::InherentSize => (
                style
                    .min_size()
                    .height
                    .maybe_resolve(parent_h, no_calc)
                    .map(to_inner),
                style
                    .max_size()
                    .height
                    .maybe_resolve(parent_h, no_calc)
                    .map(to_inner),
            ),
            SizingMode::ContentSize => (None, None),
        };
        let clamp = |h: f32| {
            let h = max_inner.map_or(h, |m| h.min(m));
            min_inner.map_or(h, |m| h.max(m))
        };
        let inner_h = match inputs.known_dimensions.height {
            Some(h) => Some((h - inset_v).max(0.0)),
            None => match inputs.sizing_mode {
                SizingMode::InherentSize => style
                    .size()
                    .height
                    .maybe_resolve(parent_h, no_calc)
                    .map(|h| clamp(to_inner(h))),
                SizingMode::ContentSize => None,
            },
        };
        let gap = style.gap().height;
        let mut out = compute_leaf(inputs, style, |known, available| {
            // The content-box width: the leaf resolves padding, border,
            // min/max, and percentages into the available width. (A
            // known width in a size probe is the border box.)
            let width = match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => known.width.map(|w| (w - inset.left - inset.right).max(0.0)),
            };
            match width {
                Some(w) => TSize {
                    width: w,
                    height: self.list_rows(
                        id,
                        w,
                        gap,
                        inner_h,
                        &clamp,
                        commit,
                        record,
                        [inset.left, inset.top],
                    ),
                },
                // An intrinsic-size probe: no width to wrap rows at.
                None => TSize {
                    width: 0.0,
                    height: self.host.lists.get(id.0).map_or(0.0, |l| l.total()),
                },
            }
        });
        // Rows placed with a percentage gap resolved after sizing can
        // extend past the list: they still scroll into view.
        if commit && let Some(l) = self.host.lists.get(id.0) {
            let o = &mut out.scrollable_overflow_rect;
            o.bottom = o.bottom.max(inset.top + l.total());
        }
        out
    }

    /// Lays out list `id`'s rendered rows at content width `w` and
    /// returns the list's content height. With `commit` (final layout)
    /// each row is laid out and placed at `origin` + its item offset,
    /// and with `record` too its height becomes a measurement (a probe
    /// lays out without recording: offsets are the recorded ones);
    /// otherwise nothing is recorded.
    /// `gap` resolves against `inner_h` (a definite content height). A
    /// percentage without one sizes the list without gaps, then places
    /// rows with the gap resolved against that size as min/max clamp it
    /// (`clamp`), as flex does.
    #[allow(clippy::too_many_arguments)]
    fn list_rows(
        &mut self,
        id: NodeId,
        w: f32,
        gap: taffy::LengthPercentage,
        inner_h: Option<f32>,
        clamp: &dyn Fn(f32) -> f32,
        commit: bool,
        record: bool,
        origin: [f32; 2],
    ) -> f32 {
        let (size_gap, deferred) = match gap.maybe_resolve(inner_h, no_calc) {
            Some(g) => (g, false),
            None => (0.0, true),
        };
        let count = self.host.lists.get(id.0).map_or(0, |l| l.len());
        if record {
            self.host.lists.estimate(self.text, id.0, w);
        }
        // (row, index, extent, margin, overflow); a row with no index,
        // one past the end, or a duplicate index is hidden.
        type Row = (NodeId, u32, f32, taffy::Rect<f32>, taffy::Rect<f32>);
        let mut rows: Vec<Row> = Vec::new();
        let mut hidden: Vec<NodeId> = Vec::new();
        let children: Vec<NodeId> = self.host.children(id).to_vec();
        for row in children {
            let index = self.host.list_index[row.index()];
            let hidden_style = self.style_of(row).display() == taffy::Display::None;
            if index >= count || hidden_style || rows.iter().any(|r| r.1 == index) {
                hidden.push(row);
                continue;
            }
            let margin = self
                .style_of(row)
                .margin()
                .resolve_or_zero(Some(w), no_calc);
            let rw = (w - margin.left - margin.right).max(0.0);
            // A row at the row width, its height from its content.
            let out = self.compute_child(
                row.0,
                LayoutInput {
                    run_mode: if commit {
                        RunMode::PerformLayout
                    } else {
                        RunMode::ComputeSize
                    },
                    sizing_mode: SizingMode::InherentSize,
                    axis: if commit {
                        RequestedAxis::Both
                    } else {
                        RequestedAxis::Vertical
                    },
                    known_dimensions: TSize {
                        width: Some(rw),
                        height: None,
                    },
                    known_dimensions_are_definite: TSize {
                        width: true,
                        height: true,
                    },
                    parent_size: TSize {
                        width: Some(w),
                        height: None,
                    },
                    available_space: TSize {
                        width: AvailableSpace::Definite(rw),
                        height: AvailableSpace::MaxContent,
                    },
                    vertical_margins_are_collapsible: Line::FALSE,
                },
            );
            let (height, overflow) = (out.size.height, out.scrollable_overflow_rect);
            rows.push((
                row,
                index,
                height + margin.top + margin.bottom,
                margin,
                overflow,
            ));
        }
        if !commit {
            let measured: Vec<(u32, f32)> = rows.iter().map(|r| (r.1, r.2)).collect();
            return self
                .host
                .lists
                .total_at(self.text, id.0, w, size_gap, &measured);
        }
        let size = if record {
            let Some(list) = self.host.lists.map.get_mut(&id.0) else {
                return 0.0;
            };
            for r in &rows {
                list.extents.measure(r.1 as usize, r.2);
            }
            let size = list.extents.total_gap(size_gap);
            list.gap = if deferred {
                gap.maybe_resolve(Some(clamp(size)), no_calc).unwrap_or(0.0)
            } else {
                size_gap
            };
            size
        } else {
            let measured: Vec<(u32, f32)> = rows.iter().map(|r| (r.1, r.2)).collect();
            self.host
                .lists
                .total_at(self.text, id.0, w, size_gap, &measured)
        };
        let Some(list) = self.host.lists.map.get(&id.0) else {
            return 0.0;
        };
        let offsets: Vec<f32> = rows.iter().map(|r| list.offset(r.1 as usize)).collect();
        for (k, (r, y)) in rows.iter().zip(offsets).enumerate() {
            let (row, _, extent, margin, overflow) = *r;
            let st = self.style_of(row);
            let padding = st.padding().resolve_or_zero(Some(w), no_calc);
            let border = st.border().resolve_or_zero(Some(w), no_calc);
            let size = TSize {
                width: (w - margin.left - margin.right).max(0.0),
                height: extent - margin.top - margin.bottom,
            };
            self.set_layout(
                row.0,
                &Layout {
                    order: k as u32,
                    location: Point {
                        x: origin[0] + margin.left,
                        y: origin[1] + y + margin.top,
                    },
                    size,
                    scrollable_overflow_rect: overflow,
                    scrollbar_size: TSize::ZERO,
                    border,
                    padding,
                    margin,
                },
            );
        }
        for row in hidden {
            compute_hidden(self, row.0);
        }
        size
    }
}

impl LayoutTree for TreeView<'_> {
    fn row(&self, node: Node) -> &LayoutRow {
        self.style_of(NodeId(node))
    }

    fn child_count(&self, node: Node) -> usize {
        self.host.child_count(NodeId(node))
    }

    fn child(&self, node: Node, index: usize) -> Node {
        self.host.child_at(NodeId(node), index).0
    }

    fn compute_child(&mut self, node: Node, inputs: LayoutInput) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden(self, node);
        }
        compute_cached(self, node, inputs, |tree, node, inputs| {
            let id = NodeId(node);
            let hidden = tree.host.node(id).is_none();
            // Copy the row: the leaf arm borrows `tree` mutably for the
            // measure callback.
            let style = *tree.style_of(id);
            if hidden || style.display() == taffy::Display::None {
                return compute_hidden(tree, node);
            }
            if tree.host.kind(id) == Some(NodeKind::List) {
                return tree.list_layout(id, inputs, &style);
            }
            if tree.host.child_count(id) > 0 {
                // Every container is flex for now.
                compute_flex(tree, node, inputs)
            } else {
                let parent = inputs.parent_size;
                compute_leaf(inputs, &style, |known, available| {
                    tree.measure(id, known, available, parent)
                })
            }
        })
    }

    fn set_layout(&mut self, node: Node, layout: &Layout) {
        *row_mut(&mut self.store.unrounded, node as usize) = *layout;
    }

    fn cache_get(&mut self, node: Node, inputs: &LayoutInput) -> Option<LayoutOutput> {
        let hit = row_mut(&mut self.store.cache, node as usize).get(inputs);
        if hit.is_some() {
            self.store.cache_hits += 1;
        } else {
            self.store.cache_misses += 1;
        }
        hit
    }

    fn cache_store(&mut self, node: Node, inputs: &LayoutInput, output: LayoutOutput) {
        row_mut(&mut self.store.cache, node as usize).store(inputs, output)
    }

    fn cache_clear(&mut self, node: Node) {
        *row_mut(&mut self.store.cache, node as usize) = Cache::default();
    }

    fn scratch(&mut self) -> &mut FlexScratch {
        &mut self.store.scratch
    }
}

impl TreeView<'_> {
    fn get_unrounded_layout(&self, id: NodeId) -> Layout {
        self.store
            .unrounded
            .get(id.0 as usize)
            .copied()
            .unwrap_or_default()
    }

    fn set_final_layout(&mut self, id: NodeId, layout: &Layout) {
        if let Some(u) = self.store.unlaid.get_mut(id.0 as usize) {
            *u = false;
        }
        let data = row_mut(&mut self.store.rects, id.0 as usize);
        let before = *data;
        data.rect = Rect::new(
            layout.location.x,
            layout.location.y,
            layout.size.width,
            layout.size.height,
        );
        data.content = [
            layout.border.left + layout.padding.left,
            layout.border.top + layout.padding.top,
        ];
        data.insets = [
            layout.border.left + layout.border.right + layout.padding.left + layout.padding.right,
            layout.border.top + layout.border.bottom + layout.padding.top + layout.padding.bottom,
        ];
        // Padding box (border box minus border widths) — the rect that
        // clips descendants under overflow clip/hidden/scroll.
        data.clip_box = Rect::new(
            layout.border.left,
            layout.border.top,
            (layout.size.width - layout.border.left - layout.border.right).max(0.0),
            (layout.size.height - layout.border.top - layout.border.bottom).max(0.0),
        );
        // Taffy's scrollable overflow rect is measured from the padding-
        // box origin; the reachable scroll extent is how far the content
        // overflows it (clamped at zero — negative overflow is
        // unreachable in LTR top-down).
        let so = &layout.scrollable_overflow_rect;
        data.scroll_extent = [
            (so.right - data.clip_box.size.width).max(0.0),
            (so.bottom - data.clip_box.size.height).max(0.0),
        ];
        if data.scroll_extent != before.scroll_extent {
            self.store.extents.push(id.0);
        }
        let reach = data.rect != before.rect || data.clip_box != before.clip_box;
        if data.rect.origin != before.rect.origin {
            self.store.moved.push(id.0);
        } else if data.rect.size != before.rect.size
            || data.insets != before.insets
            || data.content != before.content
            || data.clip_box != before.clip_box
        {
            self.store.resized.push(id.0);
        }
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::LAYOUT);
        }
        if reach {
            self.host.touch(id);
        }
    }
}

/// Shapes a text node's paragraph at wrap width `wrap`: its spans become
/// span styles (the font each span resolved to when applied, size,
/// weight, italic, letter spacing, and span zero's line height); a span's
/// index is its paint slot, so a color change patches a paint record and
/// never reshapes.
pub fn shape_paragraph(text: &mut TextEngine, p: &Paragraph, wrap: Option<f32>) -> TextParagraph {
    let spans: Vec<SpanStyle> = p
        .spans
        .iter()
        .enumerate()
        .map(|(i, s)| SpanStyle {
            start: s.start,
            style: TextStyle {
                size: s.font_size,
                weight: s.weight,
                italic: s.italic,
                font: p.fonts.get(i).copied().flatten(),
                letter_spacing: s.letter_spacing,
                line_height: s.line_height,
            },
        })
        .collect();
    text.layout_text(
        &TextSpec {
            text: &p.text,
            spans: &spans,
        },
        wrap,
    )
}
