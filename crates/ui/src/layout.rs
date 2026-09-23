//! Taffy low-level integration over Craie-owned storage.
//!
//! Taffy owns no tree here. It reads children and styles straight out of
//! the `Host` through its `TraversePartialTree`/`LayoutPartialTree` traits
//! and writes results into `Layouts`, a parallel per-node store. Per the
//! gpui-react precedent, dirty propagation is explicit: a node's Taffy
//! cache key covers only its own inputs, not its children, so a mutation
//! clears the cache on the node and every ancestor up to the root.
//!
//! Units: everything in this module is logical (DPI-independent). The
//! display scale enters only at emit time, when text origins and raster
//! sizes convert to physical pixels.
//!
//! Taffy features are restricted to `flexbox`; block/grid/float/calc are
//! compiled out until Craie needs them.

use taffy::util::ResolveOrZero;
use taffy::{
    AvailableSpace, Cache, CacheTree, Layout, LayoutFlexboxContainer, LayoutInput, LayoutOutput,
    LayoutPartialTree, Line, NodeId as TaffyId, Point, RequestedAxis, RoundTree, RunMode,
    Size as TSize, SizingMode, Style, TraversePartialTree, TraverseTree, compute_cached_layout,
    compute_flexbox_layout, compute_hidden_layout, compute_leaf_layout, compute_root_layout,
};

use crate::geom::{Rect, Size};
use crate::host::{Host, NodeFlags, NodeId, Paragraph};
use crate::input::Inputs;
use crate::mutation::NodeKind;
use crate::scene::PaintSlot;
use crate::text::parley::Layout as TextLayout;
use crate::text::parley::style::{FontStyle, FontWeight, StyleProperty};
use crate::text::{ParagraphSpec, TextEngine, TextSpan as SpecSpan};

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

/// A text leaf's retained Parley layout, plus the wrap width it was
/// produced for (`u32::MAX` bits = unwrapped). Rebuilt only when the
/// paragraph's metrics change or the wrap width changes; emitting a
/// retained layout costs no reshaping and no re-rasterization.
pub struct MeasuredText {
    pub layout: TextLayout<PaintSlot>,
    pub wrap_bits: u32,
}

/// Per-node layout results. `cache`, `unrounded` and `rects` are indexed
/// by node id and grow with the host arena. Layout inputs are not here:
/// each node owns its row in `Host::layout`.
pub struct Layouts {
    /// Style for ids outside the host arena.
    default: Style,
    cache: Vec<Cache>,
    unrounded: Vec<Layout>,
    rects: Vec<LayoutData>,
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
            cache_hits: 0,
            moved: Default::default(),
            resized: Default::default(),
            extents: Default::default(),
            cache_misses: 0,
        }
    }

    /// Final relative rect for a node, or ZERO if never laid out.
    pub fn rect(&self, id: NodeId) -> Rect {
        self.rects
            .get(id.0 as usize)
            .map(|d| d.rect)
            .unwrap_or(Rect::ZERO)
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

fn to_taffy(id: NodeId) -> TaffyId {
    TaffyId::new(id.0 as u64)
}

fn from_taffy(id: TaffyId) -> NodeId {
    NodeId(u64::from(id) as u32)
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
    compute_root_layout(&mut view, to_taffy(root), space);
    let compute_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    view.finalize(to_taffy(root));
    let round_ms = t.elapsed().as_secs_f64() * 1000.0;
    (compute_ms, round_ms)
}

/// Clears the Taffy cache for every node in the host's dirty queue and all
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

/// Taffy's view of the world: host tree + style table + per-node state +
/// the text engine for leaf measurement.
struct TreeView<'a> {
    host: &'a mut Host,
    store: &'a mut Layouts,
    text: &'a mut TextEngine,
    texts: &'a mut Vec<Option<MeasuredText>>,
    inputs: &'a mut Inputs,
}

impl TreeView<'_> {
    fn style_of(&self, id: NodeId) -> &Style {
        self.host
            .layout
            .get(id.index())
            .unwrap_or(&self.store.default)
    }

    /// Copies the computed (unrounded) layout into `rects` recursively.
    /// Coordinates stay logical/fractional here — snapping happens on the
    /// physical grid at paint time, so a non-integer display scale never
    /// accumulates rounding error.
    fn finalize(&mut self, node_id: TaffyId) {
        let layout = self.get_unrounded_layout(node_id);
        self.set_final_layout(node_id, &layout);
        let count = self.child_count(node_id);
        for i in 0..count {
            self.finalize(self.get_child_id(node_id, i));
        }
    }

    /// Leaf measurement. TEXT nodes shape through Parley at the content
    /// width Taffy offers; other leaves are zero-sized.
    ///
    /// `available` carries Taffy's sizing mode: a definite width is the
    /// content box to wrap at, `MaxContent` measures unwrapped, and
    /// `MinContent` wraps at every break opportunity (wrap width zero).
    /// `known` dimensions are already final — nothing to measure.
    fn measure(
        &mut self,
        id: NodeId,
        known: TSize<Option<f32>>,
        available: TSize<AvailableSpace>,
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
        if !text_dirty
            && let Some(m) = &self.texts[slot]
            && m.wrap_bits == wrap_bits
        {
            return TSize {
                width: m.layout.width(),
                height: m.layout.height(),
            };
        }

        let Some(p) = self.host.paragraph(id) else {
            return TSize::ZERO;
        };
        let layout = shape_paragraph(self.text, p, wrap);
        let size = TSize {
            width: layout.width(),
            height: layout.height(),
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
    fn list_layout(
        &mut self,
        node_id: TaffyId,
        inputs: LayoutInput,
        style: &Style,
    ) -> LayoutOutput {
        let id = from_taffy(node_id);
        let commit = inputs.run_mode == RunMode::PerformLayout;
        let parent_w = inputs.parent_size.width;
        let inset = style.padding.resolve_or_zero(parent_w, no_calc)
            + style.border.resolve_or_zero(parent_w, no_calc);
        compute_leaf_layout(inputs, style, no_calc, |known, available| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            match width {
                Some(w) => TSize {
                    width: w,
                    height: self.list_rows(id, w, commit, [inset.left, inset.top]),
                },
                // An intrinsic-size probe: no width to wrap rows at.
                None => TSize {
                    width: 0.0,
                    height: self.host.lists.get(id.0).map_or(0.0, |l| l.extents.total()),
                },
            }
        })
    }

    /// Lays out list `id`'s rendered rows at content width `w` and
    /// returns the list's content height. With `commit` (final layout)
    /// the rows' heights become measurements and each row is placed at
    /// `origin` + its item offset; otherwise nothing is recorded.
    fn list_rows(&mut self, id: NodeId, w: f32, commit: bool, origin: [f32; 2]) -> f32 {
        let count = self.host.lists.get(id.0).map_or(0, |l| l.len());
        if commit {
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
            let hidden_style = self.style_of(row).display == taffy::Display::None;
            if index >= count || hidden_style || rows.iter().any(|r| r.1 == index) {
                hidden.push(row);
                continue;
            }
            let margin = self.style_of(row).margin.resolve_or_zero(Some(w), no_calc);
            let rw = (w - margin.left - margin.right).max(0.0);
            // A row at the row width, its height from its content.
            let out = self.compute_child_layout(
                to_taffy(row),
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
            return self.host.lists.total_at(self.text, id.0, w, &measured);
        }
        let Some(list) = self.host.lists.map.get_mut(&id.0) else {
            return 0.0;
        };
        for r in &rows {
            list.extents.measure(r.1 as usize, r.2);
        }
        let offsets: Vec<f32> = rows
            .iter()
            .map(|r| list.extents.offset(r.1 as usize))
            .collect();
        let total = list.extents.total();
        for (k, (r, y)) in rows.iter().zip(offsets).enumerate() {
            let (row, _, extent, margin, overflow) = *r;
            let st = self.style_of(row);
            let padding = st.padding.resolve_or_zero(Some(w), no_calc);
            let border = st.border.resolve_or_zero(Some(w), no_calc);
            let size = TSize {
                width: (w - margin.left - margin.right).max(0.0),
                height: extent - margin.top - margin.bottom,
            };
            self.set_unrounded_layout(
                to_taffy(row),
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
            compute_hidden_layout(self, to_taffy(row));
        }
        total
    }
}

impl TraversePartialTree for TreeView<'_> {
    type ChildIter<'a>
        = std::iter::Map<std::iter::Copied<std::slice::Iter<'a, NodeId>>, fn(NodeId) -> TaffyId>
    where
        Self: 'a;

    fn child_ids(&self, parent: TaffyId) -> Self::ChildIter<'_> {
        self.host
            .children(from_taffy(parent))
            .iter()
            .copied()
            .map(to_taffy)
    }

    fn child_count(&self, parent: TaffyId) -> usize {
        self.host.child_count(from_taffy(parent))
    }

    fn get_child_id(&self, parent: TaffyId, index: usize) -> TaffyId {
        let id = self.host.child_at(from_taffy(parent), index);
        if id.is_nil() {
            TaffyId::new(u64::MAX)
        } else {
            to_taffy(id)
        }
    }
}

impl TraverseTree for TreeView<'_> {}

impl LayoutPartialTree for TreeView<'_> {
    type CoreContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type CustomIdent = String;

    fn get_core_container_style(&self, node_id: TaffyId) -> Self::CoreContainerStyle<'_> {
        self.style_of(from_taffy(node_id))
    }

    fn set_unrounded_layout(&mut self, node_id: TaffyId, layout: &Layout) {
        *row_mut(&mut self.store.unrounded, from_taffy(node_id).0 as usize) = *layout;
    }

    fn compute_child_layout(&mut self, node_id: TaffyId, inputs: LayoutInput) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node_id);
        }
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            let id = from_taffy(node_id);
            let hidden = tree.host.node(id).is_none();
            // Clone the style: the leaf arm borrows `tree` mutably for the
            // measure callback while Taffy still holds the style ref.
            let style = tree.style_of(id).clone();
            if hidden || style.display == taffy::Display::None {
                return compute_hidden_layout(tree, node_id);
            }
            if tree.host.kind(id) == Some(NodeKind::List) {
                return tree.list_layout(node_id, inputs, &style);
            }
            if tree.host.child_count(id) > 0 {
                // Every container is flex for now.
                compute_flexbox_layout(tree, node_id, inputs)
            } else {
                compute_leaf_layout(
                    inputs,
                    &style,
                    |_, _| 0.0,
                    |known, available| tree.measure(id, known, available),
                )
            }
        })
    }
}

impl LayoutFlexboxContainer for TreeView<'_> {
    type FlexboxContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node_id: TaffyId) -> Self::FlexboxContainerStyle<'_> {
        self.style_of(from_taffy(node_id))
    }

    fn get_flexbox_child_style(&self, child_node_id: TaffyId) -> Self::FlexboxItemStyle<'_> {
        self.style_of(from_taffy(child_node_id))
    }
}

impl CacheTree for TreeView<'_> {
    fn cache_get(&mut self, node_id: TaffyId, input: &LayoutInput) -> Option<LayoutOutput> {
        let hit = row_mut(&mut self.store.cache, from_taffy(node_id).0 as usize).get(input);
        if hit.is_some() {
            self.store.cache_hits += 1;
        } else {
            self.store.cache_misses += 1;
        }
        hit
    }

    fn cache_store(&mut self, node_id: TaffyId, input: &LayoutInput, output: LayoutOutput) {
        row_mut(&mut self.store.cache, from_taffy(node_id).0 as usize).store(input, output)
    }

    fn cache_clear(&mut self, node_id: TaffyId) {
        *row_mut(&mut self.store.cache, from_taffy(node_id).0 as usize) = Cache::default();
    }
}

impl RoundTree for TreeView<'_> {
    fn get_unrounded_layout(&self, node_id: TaffyId) -> Layout {
        self.store
            .unrounded
            .get(from_taffy(node_id).0 as usize)
            .copied()
            .unwrap_or_default()
    }

    fn set_final_layout(&mut self, node_id: TaffyId, layout: &Layout) {
        let id = from_taffy(node_id);
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
    }
}

/// Shapes a paragraph: span zero is the base style, later spans override
/// over their byte ranges. The brush is the span index, which is also the
/// span's paint slot in the text chunk, so a color change patches a
/// paint record and never reshapes.
pub fn shape_paragraph(
    text: &mut TextEngine,
    p: &Paragraph,
    wrap: Option<f32>,
) -> TextLayout<PaintSlot> {
    let base = p.spans.first().copied().unwrap_or_default();
    let defaults = [
        StyleProperty::FontSize(base.font_size),
        StyleProperty::FontWeight(FontWeight::new(base.weight as f32)),
        StyleProperty::FontStyle(if base.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }),
        StyleProperty::Brush(PaintSlot(0)),
    ];
    let mut spans: Vec<SpecSpan> = Vec::new();
    for (i, s) in p.spans.iter().enumerate().skip(1) {
        let slot = PaintSlot(i as u32);
        let end = p
            .spans
            .get(i + 1)
            .map_or(p.text.len(), |n| n.start as usize);
        let range = s.start as usize..end;
        spans.push(SpecSpan {
            range: range.clone(),
            style: StyleProperty::FontSize(s.font_size),
        });
        spans.push(SpecSpan {
            range: range.clone(),
            style: StyleProperty::FontWeight(FontWeight::new(s.weight as f32)),
        });
        spans.push(SpecSpan {
            range: range.clone(),
            style: StyleProperty::FontStyle(if s.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }),
        });
        spans.push(SpecSpan {
            range,
            style: StyleProperty::Brush(slot),
        });
    }
    text.layout_paragraph(
        &ParagraphSpec {
            text: &p.text,
            defaults: &defaults,
            spans: &spans,
        },
        wrap,
    )
}
