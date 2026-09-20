//! Taffy low-level integration over Craie-owned storage.
//!
//! Taffy owns no tree here. It reads children and styles straight out of
//! the `Host` through its `TraversePartialTree`/`LayoutPartialTree` traits
//! and writes results into `Layouts`, a parallel per-node store. Per the
//! gpui-react precedent, dirty propagation is explicit: a node's Taffy
//! cache key covers only its own inputs, not its children, so a mutation
//! clears the cache on the node and every ancestor up to the root.
//!
//! Units: everything in this module is physical pixels. Style values
//! written by the bridge/demo are already scaled.
//!
//! Taffy features are restricted to `flexbox`; block/grid/float/calc are
//! compiled out until Craie needs them.

use taffy::{
    AvailableSpace, Cache, CacheTree, Layout, LayoutFlexboxContainer, LayoutInput, LayoutOutput,
    LayoutPartialTree, NodeId as TaffyId, RoundTree, RunMode, Size as TSize, Style,
    TraversePartialTree, TraverseTree, compute_cached_layout, compute_flexbox_layout,
    compute_hidden_layout, compute_leaf_layout, compute_root_layout, round_layout,
};

use crate::geom::{Rect, Size};
use crate::host::{Host, NodeFlags, NodeId, NodeKind, Siblings, StyleId};
use crate::scene::Color;
use crate::text::parley::Layout as TextLayout;
use crate::text::parley::style::StyleProperty;
use crate::text::{ParagraphSpec, TextEngine};

/// Computed border box for a node, relative to its parent's content box,
/// in physical pixels. Written by `round_layout`, read by paint and
/// hit-testing (which accumulate offsets during traversal).
///
/// `content` is the node's content-box origin relative to its border box:
/// border.left + padding.left, border.top + padding.top.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutData {
    pub rect: Rect,
    pub content: [f32; 2],
}

/// A text leaf's retained Parley layout, plus the wrap width it was
/// produced for (`u32::MAX` bits = unwrapped). Rebuilt only when the text
/// row is dirty or the wrap width changes; `emit` against a retained
/// layout costs no reshaping and no re-rasterization.
pub struct MeasuredText {
    pub layout: TextLayout<Color>,
    wrap_bits: u32,
}

/// Per-node layout state plus the interned style table.
///
/// `cache`, `unrounded` and `rects` are indexed by node id and grow with
/// the host arena. `styles` is a deduplicated table: a `StyleId` on a node
/// header is an index into it, and slot 0 is always `Style::DEFAULT`.
pub struct Layouts {
    styles: Vec<Style>,
    /// Wire style id -> index into `styles`. Wire ids are JS-assigned and
    /// dense; slots hold `u32::MAX` until defined.
    wire_styles: Vec<u32>,
    cache: Vec<Cache>,
    unrounded: Vec<Layout>,
    rects: Vec<LayoutData>,
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
            styles: vec![Style::DEFAULT],
            wire_styles: Vec::new(),
            cache: Vec::new(),
            unrounded: Vec::new(),
            rects: Vec::new(),
        }
    }

    /// Interns a style and returns its id. `Style::DEFAULT` maps to id 0,
    /// so unstyled nodes cost nothing.
    pub fn intern(&mut self, style: Style) -> StyleId {
        if let Some(i) = self.styles.iter().position(|s| *s == style) {
            return StyleId(i as u32);
        }
        self.styles.push(style);
        StyleId((self.styles.len() - 1) as u32)
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

    pub fn style(&self, id: StyleId) -> &Style {
        let i = if id == StyleId::NIL { 0 } else { id.0 as usize };
        self.styles.get(i).unwrap_or(&self.styles[0])
    }

    pub fn style_count(&self) -> usize {
        self.styles.len()
    }

    /// Defines or redefines a wire style id.
    pub fn define_style(&mut self, wire_id: u32, style: Style) {
        let i = self.intern(style);
        let slot = wire_id as usize;
        if slot >= self.wire_styles.len() {
            self.wire_styles.resize(slot + 1, u32::MAX);
        }
        self.wire_styles[slot] = i.0;
    }

    pub fn wire_style_defined(&self, wire_id: u32) -> bool {
        self.wire_styles
            .get(wire_id as usize)
            .is_some_and(|&s| s != u32::MAX)
    }

    /// Interned style for a wire style id; NIL when undefined.
    pub fn style_id_for_wire(&self, wire_id: u32) -> StyleId {
        match self.wire_styles.get(wire_id as usize) {
            Some(&s) if s != u32::MAX => StyleId(s),
            _ => StyleId::NIL,
        }
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
/// multiple roots in a View). Clears dirty caches first, runs Taffy, then
/// rounds to the pixel grid. TEXT leaves are measured through `text` and
/// retained in `texts` (indexed by node id) for paint.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    host: &mut Host,
    store: &mut Layouts,
    text: &mut TextEngine,
    texts: &mut Vec<Option<MeasuredText>>,
    scale: f32,
    root: NodeId,
    available: Size,
) {
    invalidate(host, store);
    let mut view = TreeView {
        host,
        store,
        text,
        texts,
        scale,
    };
    let space = TSize {
        width: AvailableSpace::Definite(available.width),
        height: AvailableSpace::Definite(available.height),
    };
    compute_root_layout(&mut view, to_taffy(root), space);
    round_layout(&mut view, to_taffy(root));
}

/// Clears the Taffy cache for every node flagged LAYOUT and all of its
/// ancestors, then clears the flags. Child changes invalidate the path to
/// the root because a node's cache key does not include its children.
fn invalidate(host: &mut Host, store: &mut Layouts) {
    for i in 0..host.slot_count() {
        let id = NodeId(i as u32);
        let dirty = host
            .node(id)
            .map(|n| n.flags.contains(NodeFlags::LAYOUT))
            .unwrap_or(false);
        if !dirty {
            continue;
        }
        let mut cur = id;
        loop {
            *row_mut(&mut store.cache, cur.0 as usize) = Cache::default();
            if let Some(n) = host.node_mut(cur) {
                n.flags.clear(NodeFlags::LAYOUT);
            }
            let p = host.parent(cur);
            if p.is_nil() {
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
    scale: f32,
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

    /// Leaf measurement. TEXT nodes shape through Parley at the content
    /// width Taffy offers; other leaves are zero-sized.
    fn measure(
        &mut self,
        id: NodeId,
        _known: TSize<Option<f32>>,
        available: TSize<AvailableSpace>,
    ) -> TSize<f32> {
        let Some(node) = self.host.node(id) else {
            return TSize::ZERO;
        };
        if node.kind() != NodeKind::TEXT {
            return TSize::ZERO;
        }
        let text_dirty = node.flags.contains(NodeFlags::TEXT);

        // available.width is the content box: wrap text there.
        let wrap = match available.width {
            AvailableSpace::Definite(w) => Some(w.max(0.0)),
            _ => None,
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
        let layout = self.text.layout_paragraph(
            &ParagraphSpec {
                text: &row.text,
                defaults: vec![
                    StyleProperty::Brush(Color(row.color)),
                    StyleProperty::FontSize(row.font_size),
                ],
                spans: vec![],
            },
            self.scale,
            wrap,
        );
        let size = TSize {
            width: layout.width(),
            height: layout.height(),
        };
        self.texts[slot] = Some(MeasuredText { layout, wrap_bits });
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::TEXT);
        }
        size
    }
}

impl TraversePartialTree for TreeView<'_> {
    type ChildIter<'a>
        = std::iter::Map<Siblings<'a>, fn(NodeId) -> TaffyId>
    where
        Self: 'a;

    fn child_ids(&self, parent: TaffyId) -> Self::ChildIter<'_> {
        let first = self.host.first_child(from_taffy(parent));
        self.host.siblings(first).map(to_taffy)
    }

    fn child_count(&self, parent: TaffyId) -> usize {
        self.host
            .node(from_taffy(parent))
            .map(|n| n.child_count as usize)
            .unwrap_or(0)
    }

    fn get_child_id(&self, parent: TaffyId, index: usize) -> TaffyId {
        self.child_ids(parent)
            .nth(index)
            .unwrap_or_else(|| TaffyId::new(u64::MAX))
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
            if hidden {
                return compute_hidden_layout(tree, node_id);
            }
            // Clone the style: the leaf arm borrows `tree` mutably for the
            // measure callback while Taffy still holds the style ref.
            let style = tree.style_of(id).clone();
            if tree
                .host
                .node(id)
                .map(|n| n.child_count > 0)
                .unwrap_or(false)
            {
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
        row_mut(&mut self.store.cache, from_taffy(node_id).0 as usize).get(input)
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
        if let Some(n) = self.host.node_mut(id) {
            n.flags.clear(NodeFlags::LAYOUT);
        }
    }
}
