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

use taffy::{
    AvailableSpace, Cache, CacheTree, Layout, LayoutFlexboxContainer, LayoutInput, LayoutOutput,
    LayoutPartialTree, NodeId as TaffyId, RoundTree, RunMode, Size as TSize, Style,
    TraversePartialTree, TraverseTree, compute_cached_layout, compute_flexbox_layout,
    compute_hidden_layout, compute_leaf_layout, compute_root_layout,
};

use crate::geom::{Rect, Size};
use crate::host::{Host, NodeFlags, NodeId, NodeKind, StyleId};
use crate::input::Inputs;
use crate::scene::{Color, Instance};
use crate::text::parley::Layout as TextLayout;
use crate::text::parley::style::StyleProperty;
use crate::text::{ParagraphSpec, TextEngine};

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

/// Paint-ready instances for a text node, keyed on the inputs that change
/// them: display scale, absolute content origin, and color. A hit replays
/// with zero shaping, rasterization, or glyph-cache lookups.
pub struct EmittedText {
    pub scale_bits: u32,
    /// Logical content-box origin the batch was emitted for.
    pub origin: [f32; 2],
    /// Color baked into `instances` (alpha glyphs only).
    pub color: u32,
    /// Physical-pixel instances, ready to append to the scene.
    pub instances: Vec<Instance>,
}

/// A text leaf's retained Parley layout, plus the wrap width it was
/// produced for (`u32::MAX` bits = unwrapped). Rebuilt only when the text
/// row is dirty or the wrap width changes; `emit` against a retained
/// layout costs no reshaping and no re-rasterization.
pub struct MeasuredText {
    pub layout: TextLayout<Color>,
    pub wrap_bits: u32,
    /// Cached emit output; `None` until first paint after a (re)layout.
    pub emitted: Option<EmittedText>,
}

/// Per-node layout state plus the style table.
///
/// `cache`, `unrounded` and `rects` are indexed by node id and grow with
/// the host arena. `styles` holds wire styles verbatim: the JS side already
/// deduplicates styles and assigns dense ids, so `StyleId` on a node header
/// IS the wire id — no second interning pass here.
pub struct Layouts {
    styles: Vec<Option<Style>>,
    default: Style,
    cache: Vec<Cache>,
    unrounded: Vec<Layout>,
    rects: Vec<LayoutData>,
    /// Taffy cache behavior, for experiments: hits/misses since `new`.
    pub cache_hits: u64,
    pub cache_misses: u64,
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
            styles: Vec::new(),
            default: Style::default(),
            cache: Vec::new(),
            unrounded: Vec::new(),
            rects: Vec::new(),
            cache_hits: 0,
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

    /// The style behind a `StyleId` (a wire id); undefined ids get the
    /// default style.
    pub fn style(&self, id: StyleId) -> &Style {
        self.styles
            .get(id.0 as usize)
            .and_then(Option::as_ref)
            .unwrap_or(&self.default)
    }

    /// Defines or redefines a wire style id.
    pub fn define_style(&mut self, wire_id: u32, style: Style) {
        *row_mut(&mut self.styles, wire_id as usize) = Some(style);
    }

    pub fn wire_style_defined(&self, wire_id: u32) -> bool {
        self.styles
            .get(wire_id as usize)
            .is_some_and(Option::is_some)
    }

    /// Drops every per-node layout cache (used when a redefined style
    /// can't be traced to its dependents).
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

/// Recomputes layout for the tree rooted at `root` (a real node — wrap
/// multiple roots in a View). Drains the dirty queue first, runs Taffy,
/// then rounds to the pixel grid. TEXT leaves are measured through `text`
/// and retained in `texts` (indexed by node id) for paint.
pub fn compute(
    host: &mut Host,
    store: &mut Layouts,
    text: &mut TextEngine,
    texts: &mut Vec<Option<MeasuredText>>,
    inputs: &mut Inputs,
    root: NodeId,
    available: Size,
) {
    invalidate(host, store);
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
    compute_root_layout(&mut view, to_taffy(root), space);
    view.finalize(to_taffy(root));
}

/// `compute` instrumented: returns (root-layout ms, finalize ms) so
/// experiments can price the result-copy pass separately.
pub fn compute_timed(
    host: &mut Host,
    store: &mut Layouts,
    text: &mut TextEngine,
    texts: &mut Vec<Option<MeasuredText>>,
    inputs: &mut Inputs,
    root: NodeId,
    available: Size,
) -> (f64, f64) {
    invalidate(host, store);
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
fn invalidate(host: &mut Host, store: &mut Layouts) {
    for id in host.take_layout_dirty() {
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
        let sid = self
            .host
            .node(id)
            .map(|n| StyleId(n.style))
            .unwrap_or(StyleId::NIL);
        self.store.style(sid)
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
        if node.kind() == NodeKind::INPUT {
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
        if node.kind() != NodeKind::TEXT {
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

        let Some(row) = self.host.text(id) else {
            return TSize::ZERO;
        };
        // No brush default: the row color is applied at emit time, so a
        // color-only change never reshapes or re-lays-out.
        let defaults = [StyleProperty::FontSize(row.font_size)];
        let layout = self.text.layout_paragraph(
            &ParagraphSpec {
                text: &row.text,
                defaults: &defaults,
                spans: &[],
            },
            wrap,
        );
        let size = TSize {
            width: layout.width(),
            height: layout.height(),
        };
        self.texts[slot] = Some(MeasuredText {
            layout,
            wrap_bits,
            emitted: None,
        });
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::TEXT);
        }
        size
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
            let hidden = tree.host.node(id).map(|n| n.hidden()).unwrap_or(true);
            // Clone the style: the leaf arm borrows `tree` mutably for the
            // measure callback while Taffy still holds the style ref.
            let style = tree.style_of(id).clone();
            if hidden || style.display == taffy::Display::None {
                return compute_hidden_layout(tree, node_id);
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
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::LAYOUT);
        }
    }
}
