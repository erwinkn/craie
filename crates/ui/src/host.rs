//! The retained host tree.
//!
//! React is authoritative for composition; the host stores only what
//! native execution needs. Every node is a 16-byte header in a dense
//! arena indexed by the bridge-assigned id. Per-usage stores sit beside
//! the arena, indexed by the same id:
//!
//! ```text
//! nodes[]        header: parent, child span, kind, flags, generation
//! layout[]       the node's own layout inputs (no shared records)
//! spatial[]      local transform, opacity, scroll offset
//! paint[]        fill, border, radius
//! paragraphs[]   UTF-8 text + style span list (text nodes)
//! interaction[]  listener mask, focusable, role
//! list_index[]   item index of a list row
//! ```
//!
//! Children are a `Span` into one capacity-classed pool, so a leaf pays
//! nothing for children. Truly sparse facts (labels, surface payloads,
//! list states) live in id-keyed maps.
//!
//! Every mutation advances the revisions it can invalidate and queues the
//! dirty work it creates. Queues schedule work; revisions prove validity.

use std::collections::HashMap;

use craie_core::dirty::DirtyQueue;
use craie_core::geom::Affine;
use craie_core::rev::Rev;
use craie_core::span::{Span, SpanPool};
use taffy::Style;

use crate::mutation::{NIL, NodeKind, Role, TextSpan};

/// Stable node identity, assigned by the React side of the bridge and
/// recycled there immediately after removal. Natively a `NodeId` is a
/// direct index into the arena; the header's `generation` tells a new
/// occupant from an old one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Nil / "root list" sentinel. Real ids stay below `DETACHED`.
    pub const NIL: NodeId = NodeId(u32::MAX);
    /// Parent link of a node that exists but is not attached.
    pub const DETACHED: NodeId = NodeId(u32::MAX - 1);

    pub fn is_nil(self) -> bool {
        self == NodeId::NIL
    }

    /// Neither NIL nor DETACHED.
    pub fn is_node(self) -> bool {
        self.0 < NodeId::DETACHED.0
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Root sentinel: children of ROOT are top-level nodes.
pub const ROOT: NodeId = NodeId::NIL;

/// Layout inputs of a node that sends none, and the base every style
/// decodes over: React Native defaults (flex, column, stretch, no
/// shrink), so a cross-platform kit needs no normalization
/// (ARCHITECTURE.md §4).
pub fn default_style() -> Style {
    Style {
        flex_direction: taffy::FlexDirection::Column,
        flex_shrink: 0.0,
        ..Style::default()
    }
}

/// Node ids index dense stores, so they are bounded: 2^24 slots. The
/// bridge allocates ids densely from zero and recycles them.
pub const MAX_NODES: u32 = 1 << 24;

/// Per-node flag bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct NodeFlags(pub u8);

impl NodeFlags {
    pub const NONE: NodeFlags = NodeFlags(0);
    /// The slot holds a live node.
    pub const LIVE: NodeFlags = NodeFlags(1 << 0);
    /// Layout inputs changed; the layout cache up to the root is stale.
    pub const LAYOUT: NodeFlags = NodeFlags(1 << 1);
    /// Text content or metrics changed; the shaped paragraph is stale.
    pub const TEXT: NodeFlags = NodeFlags(1 << 2);

    pub fn contains(self, other: NodeFlags) -> bool {
        self.0 & other.0 != 0
    }

    pub fn set(&mut self, other: NodeFlags) {
        self.0 |= other.0;
    }

    pub fn clear(&mut self, other: NodeFlags) {
        self.0 &= !other.0;
    }
}

/// Fixed-size record for every node slot: 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeHeader {
    /// Parent id, `NIL` at the root level, `DETACHED` when unattached.
    pub parent: u32,
    /// Ordered children in the host's child pool.
    pub children: Span,
    pub kind: NodeKind,
    pub flags: NodeFlags,
    /// Incremented each time the slot is freed, so a stale
    /// (id, generation) pair never reaches a later occupant.
    pub generation: u16,
}

impl NodeHeader {
    const EMPTY: NodeHeader = NodeHeader {
        parent: NodeId::DETACHED.0,
        children: Span::EMPTY,
        kind: NodeKind::View,
        flags: NodeFlags::NONE,
        generation: 0,
    };

    pub fn is_live(&self) -> bool {
        self.flags.contains(NodeFlags::LIVE)
    }

    pub fn parent(&self) -> NodeId {
        NodeId(self.parent)
    }
}

/// Spatial state: applied after layout, never an input to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spatial {
    /// Local transform about the border-box center.
    pub transform: Affine,
    pub opacity: f32,
    /// Content offset of a scroll container (logical points).
    pub scroll: [f32; 2],
}

impl Default for Spatial {
    fn default() -> Spatial {
        Spatial {
            transform: Affine::IDENTITY,
            opacity: 1.0,
            scroll: [0.0; 2],
        }
    }
}

impl Spatial {
    /// Owns a transform record in the scene (drawn in its own space).
    pub fn transformed(&self) -> bool {
        self.transform != Affine::IDENTITY
    }

    /// Composites as an isolated group.
    pub fn layered(&self) -> bool {
        self.opacity < 1.0
    }
}

/// Box paint of a view, input, or surface.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxPaint {
    /// Background fill, 0xRRGGBBAA. Alpha 0 paints nothing.
    pub fill: u32,
    /// Border ring color, 0xRRGGBBAA.
    pub border_color: u32,
    /// Border ring width, logical points.
    pub border_width: f32,
    /// Corner radius, logical points.
    pub radius: f32,
}

/// A text node's paragraph: UTF-8 plus style spans over byte ranges.
/// Span zero starts at byte 0 and carries the base style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paragraph {
    pub text: String,
    /// Span zero is the base style. `family` indexes `Host::families`.
    pub spans: Vec<TextSpan>,
    /// Each span's primary font, resolved from its family and style once,
    /// when the span was applied.
    pub fonts: Vec<Option<crate::text::fonts::FontInstanceId>>,
}

/// Which events a node subscribes to, whether it takes focus, and what
/// it is for assistive technology.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    /// Event subscription mask (see `events::mask`).
    pub listeners: u32,
    /// Participates in Tab traversal and accepts click focus.
    pub focusable: bool,
    pub role: Role,
}

/// A surface node's retained data: kind, parameters, and payload bytes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceData {
    pub kind: u32,
    pub params: [u32; 4],
    pub payload: Vec<u8>,
}

/// Revisions. A derived value records the revisions it was built from
/// and is valid while they are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revs {
    /// Tree shape, visibility (`display`), and draw topology (transform
    /// records, opacity layers).
    pub structure: Rev,
    pub layout_input: Rev,
    pub text_content: Rev,
    pub text_metrics: Rev,
    pub paint: Rev,
    pub transform: Rev,
    /// Clip topology (`overflow`).
    pub clip: Rev,
    pub semantic: Rev,
    /// Surface payloads and other retained resources.
    pub resource: Rev,
}

/// Dirty work queues, drained by the frame pipeline.
#[derive(Default)]
pub struct DirtyQueues {
    /// Nodes whose layout cache (and every ancestor's) is stale.
    pub layout: DirtyQueue,
    /// Nodes whose scene chunk must be regenerated.
    pub content: DirtyQueue,
    /// Nodes whose chunk paint records need patching (color only).
    pub paint: DirtyQueue,
    /// Nodes whose transform, opacity, or scroll offset changed.
    pub spatial: DirtyQueue,
    /// Nodes whose accessibility projection changed.
    pub semantic: DirtyQueue,
}

pub struct Host {
    nodes: Vec<NodeHeader>,
    child_pool: SpanPool<NodeId>,
    roots: Span,
    live: usize,
    pub layout: Vec<Style>,
    pub spatial: Vec<Spatial>,
    pub paint: Vec<BoxPaint>,
    pub paragraphs: Vec<Paragraph>,
    /// Font family names spans refer to (interned, grow-only).
    pub families: Vec<String>,
    pub interaction: Vec<Interaction>,
    pub labels: HashMap<u32, Box<str>>,
    pub surfaces: HashMap<u32, SurfaceData>,
    /// Item index of a list row (a child of a List node); NIL otherwise.
    pub list_index: Vec<u32>,
    /// List states and scroll anchors (§7).
    pub lists: crate::list::Lists,
    pub revs: Revs,
    pub dirty: DirtyQueues,
    /// Bytes copied from transactions into host stores: paragraph text
    /// and span lists, labels, surface payloads (cost counter).
    pub copied_bytes: u64,
}

impl Default for Host {
    fn default() -> Host {
        Host::new()
    }
}

impl Host {
    /// The host index of family `name` (interned).
    pub fn family(&mut self, name: &str) -> u32 {
        match self.families.iter().position(|f| f == name) {
            Some(i) => i as u32,
            None => {
                self.families.push(name.to_string());
                self.families.len() as u32 - 1
            }
        }
    }

    /// The family name of a span (`""`: the default family).
    pub fn family_name(&self, family: u32) -> &str {
        self.families
            .get(family as usize)
            .map_or("", String::as_str)
    }

    pub fn new() -> Host {
        Host {
            nodes: Vec::new(),
            child_pool: SpanPool::new(NodeId::NIL),
            roots: Span::EMPTY,
            live: 0,
            layout: Vec::new(),
            spatial: Vec::new(),
            paint: Vec::new(),
            paragraphs: Vec::new(),
            families: Vec::new(),
            interaction: Vec::new(),
            labels: HashMap::new(),
            surfaces: HashMap::new(),
            list_index: Vec::new(),
            lists: crate::list::Lists::default(),
            revs: Revs::default(),
            dirty: DirtyQueues::default(),
            copied_bytes: 0,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeHeader> {
        self.nodes.get(id.index()).filter(|n| n.is_live())
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut NodeHeader> {
        self.nodes.get_mut(id.index()).filter(|n| n.is_live())
    }

    pub fn kind(&self, id: NodeId) -> Option<NodeKind> {
        self.node(id).map(|n| n.kind)
    }

    pub fn is_live(&self, id: NodeId) -> bool {
        self.node(id).is_some()
    }

    /// Number of slots ever allocated, including free ones.
    pub fn slot_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Parent link of `id`: a live parent, `DETACHED`, or `NIL` (root
    /// level or absent). Ancestor walks stop on both sentinels.
    pub fn parent(&self, id: NodeId) -> NodeId {
        self.node(id).map_or(NodeId::NIL, |n| n.parent())
    }

    /// Ordered children of `parent`; ROOT yields the top-level list.
    pub fn children(&self, parent: NodeId) -> &[NodeId] {
        let span = if parent.is_nil() {
            self.roots
        } else {
            match self.nodes.get(parent.index()) {
                Some(n) => n.children,
                None => return &[],
            }
        };
        self.child_pool.get(span)
    }

    pub fn child_count(&self, parent: NodeId) -> usize {
        self.children(parent).len()
    }

    /// The `index`-th child of `parent`, NIL when out of range.
    pub fn child_at(&self, parent: NodeId, index: usize) -> NodeId {
        self.children(parent)
            .get(index)
            .copied()
            .unwrap_or(NodeId::NIL)
    }

    /// Child-pool allocation behavior (E10).
    pub fn child_pool_stats(&self) -> craie_core::span::SpanStats {
        self.child_pool.stats
    }

    /// Bytes held by the child pool's backing store.
    pub fn child_pool_bytes(&self) -> usize {
        self.child_pool.backing_bytes()
    }

    fn ensure(&mut self, id: NodeId) {
        let n = id.index() + 1;
        if self.nodes.len() < n {
            self.nodes.resize(n, NodeHeader::EMPTY);
            self.layout.resize_with(n, default_style);
            self.spatial.resize(n, Spatial::default());
            self.paint.resize(n, BoxPaint::default());
            self.paragraphs.resize_with(n, Paragraph::default);
            self.interaction.resize(n, Interaction::default());
            self.list_index.resize(n, NIL);
        }
    }

    /// Creates a node at the bridge-assigned id with default stores.
    /// Returns false when the slot is live.
    pub fn create(&mut self, id: NodeId, kind: NodeKind) -> bool {
        self.ensure(id);
        let i = id.index();
        if self.nodes[i].is_live() {
            return false;
        }
        let generation = self.nodes[i].generation;
        self.nodes[i] = NodeHeader {
            kind,
            flags: NodeFlags::LIVE,
            generation,
            ..NodeHeader::EMPTY
        };
        // A recycled slot starts from defaults: nothing of the previous
        // occupant may leak into the new node.
        self.layout[i] = default_style();
        self.spatial[i] = Spatial::default();
        self.paint[i] = BoxPaint::default();
        let p = &mut self.paragraphs[i];
        p.text.clear();
        p.spans.clear();
        if kind == NodeKind::Text {
            p.spans.push(TextSpan::default());
        }
        self.interaction[i] = Interaction::default();
        self.labels.remove(&id.0);
        self.surfaces.remove(&id.0);
        self.list_index[i] = NIL;
        self.lists.forget(id.0);
        if kind == NodeKind::Surface {
            self.surfaces.insert(id.0, SurfaceData::default());
        }
        self.live += 1;
        self.revs.structure.bump();
        self.dirty.content.push(id.0);
        self.dirty.semantic.push(id.0);
        true
    }

    /// Inserts `child` before `before` under `parent` (NIL `before`
    /// appends, NIL `parent` is the root level). Moves the child if it is
    /// attached elsewhere.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.detach(child);
        let mut span = if parent.is_nil() {
            self.roots
        } else {
            self.nodes[parent.index()].children
        };
        let list = self.child_pool.get(span);
        let pos = if before.is_nil() {
            list.len()
        } else {
            list.iter().position(|&c| c == before).unwrap_or(list.len())
        };
        self.child_pool.insert(&mut span, pos, child);
        if parent.is_nil() {
            self.roots = span;
        } else {
            self.nodes[parent.index()].children = span;
        }
        self.nodes[child.index()].parent = parent.0;
        self.revs.structure.bump();
        self.mark_layout(child);
        self.dirty.semantic.push(child.0);
        if parent.is_node() {
            self.dirty.semantic.push(parent.0);
        }
    }

    /// Unlinks `id` from its parent without freeing the slot.
    pub fn detach(&mut self, id: NodeId) {
        let Some(node) = self.node(id) else { return };
        let parent = node.parent();
        if parent == NodeId::DETACHED {
            return;
        }
        let mut span = if parent.is_nil() {
            self.roots
        } else {
            self.nodes[parent.index()].children
        };
        if let Some(pos) = self.child_pool.get(span).iter().position(|&c| c == id) {
            self.child_pool.remove(&mut span, pos);
        }
        if parent.is_nil() {
            self.roots = span;
        } else {
            self.nodes[parent.index()].children = span;
        }
        self.nodes[id.index()].parent = NodeId::DETACHED.0;
        self.revs.structure.bump();
        if parent.is_node() {
            // The parent's layout depended on this child.
            self.mark_layout(parent);
            self.dirty.semantic.push(parent.0);
        }
    }

    /// Detaches `id`, orphans its children, and frees the slot. The
    /// bridge removes every node of a deleted subtree with its own op;
    /// children removed later find themselves already detached.
    pub fn remove(&mut self, id: NodeId) {
        if !self.is_live(id) {
            return;
        }
        self.detach(id);
        let i = id.index();
        let mut kids = self.nodes[i].children;
        for k in 0..kids.len() {
            let c = self.child_pool.get(kids)[k];
            if let Some(n) = self.nodes.get_mut(c.index()) {
                n.parent = NodeId::DETACHED.0;
            }
        }
        self.child_pool.free(&mut kids);
        let p = &mut self.paragraphs[i];
        p.text = String::new();
        p.spans = Vec::new();
        self.labels.remove(&id.0);
        self.surfaces.remove(&id.0);
        self.list_index[i] = NIL;
        self.lists.forget(id.0);
        let generation = self.nodes[i].generation.wrapping_add(1);
        self.nodes[i] = NodeHeader {
            generation,
            ..NodeHeader::EMPTY
        };
        self.live -= 1;
        self.revs.structure.bump();
        // The frame pipeline frees the node's chunk.
        self.dirty.content.push(id.0);
    }

    /// Flags `id` layout-dirty and queues it once for cache invalidation.
    pub fn mark_layout(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(id.index()).filter(|n| n.is_live()) {
            n.flags.set(NodeFlags::LAYOUT);
            self.dirty.layout.push(id.0);
        }
    }

    /// Flags a text node's shaped paragraph stale and queues relayout
    /// and chunk regeneration.
    pub fn mark_text(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.get_mut(id.index()).filter(|n| n.is_live()) {
            n.flags.set(NodeFlags::TEXT);
        }
        self.mark_layout(id);
        self.dirty.content.push(id.0);
    }

    /// Paragraph of a text node.
    pub fn paragraph(&self, id: NodeId) -> Option<&Paragraph> {
        self.node(id)
            .filter(|n| n.kind == NodeKind::Text)
            .map(|_| &self.paragraphs[id.index()])
    }

    pub fn style(&self, id: NodeId) -> &Style {
        &self.layout[id.index()]
    }

    /// `display: none` on the node itself.
    pub fn display_none(&self, id: NodeId) -> bool {
        self.layout
            .get(id.index())
            .is_some_and(|s| s.display == taffy::Display::None)
    }

    pub fn label(&self, id: NodeId) -> Option<&str> {
        self.labels.get(&id.0).map(|s| &**s)
    }

    pub fn interaction(&self, id: NodeId) -> Interaction {
        self.interaction
            .get(id.index())
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: NodeKind = NodeKind::View;
    const TEXT: NodeKind = NodeKind::Text;

    #[test]
    fn create_and_index() {
        let mut host = Host::new();
        assert!(host.create(NodeId(0), VIEW));
        assert!(host.create(NodeId(5), TEXT));
        assert_eq!(host.len(), 2);
        assert_eq!(host.kind(NodeId(5)), Some(TEXT));
        assert!(host.node(NodeId(3)).is_none());
        assert!(!host.create(NodeId(0), TEXT));
    }

    #[test]
    fn insert_append_and_move() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        for i in 1..=3u32 {
            host.create(NodeId(i), TEXT);
            host.insert_before(NodeId(0), NodeId(i), NodeId::NIL);
        }
        assert_eq!(host.children(NodeId(0)), [NodeId(1), NodeId(2), NodeId(3)]);
        host.create(NodeId(9), TEXT);
        host.insert_before(NodeId(0), NodeId(9), NodeId(2));
        assert_eq!(
            host.children(NodeId(0)),
            [NodeId(1), NodeId(9), NodeId(2), NodeId(3)]
        );
        // Re-inserting moves.
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        assert_eq!(
            host.children(NodeId(0)),
            [NodeId(9), NodeId(2), NodeId(3), NodeId(1)]
        );
        assert_eq!(host.parent(NodeId(2)), NodeId(0));
    }

    #[test]
    fn remove_recycles_slot_and_bumps_generation() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        host.create(NodeId(1), TEXT);
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        host.paragraphs[1].text.push_str("old");
        let gen0 = host.node(NodeId(1)).unwrap().generation;
        host.remove(NodeId(1));
        assert_eq!(host.len(), 1);
        assert!(host.node(NodeId(1)).is_none());
        assert!(host.children(NodeId(0)).is_empty());
        host.create(NodeId(1), TEXT);
        assert_eq!(
            host.node(NodeId(1)).unwrap().generation,
            gen0.wrapping_add(1)
        );
        assert_eq!(host.paragraphs[1].text, "");
    }

    #[test]
    fn remove_orphans_children() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW);
        host.create(NodeId(1), VIEW);
        host.insert_before(ROOT, NodeId(0), NodeId::NIL);
        host.insert_before(NodeId(0), NodeId(1), NodeId::NIL);
        host.remove(NodeId(0));
        assert_eq!(host.parent(NodeId(1)), NodeId::DETACHED);
        assert!(host.children(ROOT).is_empty());
        // The slot is reused; the orphan's later remove must not touch it.
        host.create(NodeId(0), VIEW);
        host.remove(NodeId(1));
        assert!(host.is_live(NodeId(0)));
    }

    #[test]
    fn header_stays_compact() {
        assert_eq!(std::mem::size_of::<NodeHeader>(), 16);
    }
}
