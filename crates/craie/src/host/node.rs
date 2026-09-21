/// Stable node identity, assigned by the React side of the bridge and
/// recycled there once a removal is acknowledged. Natively a `NodeId` is a
/// direct index into the host arena — there is no native free list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Nil / "root list" sentinel. Real ids stay below `DETACHED`.
    pub const NIL: NodeId = NodeId(u32::MAX);
    /// Marker for a record that was created but never attached.
    pub const DETACHED: NodeId = NodeId(u32::MAX - 1);

    pub fn is_nil(self) -> bool {
        self == NodeId::NIL
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Root sentinel: children of ROOT are top-level nodes.
pub const ROOT: NodeId = NodeId::NIL;

const EMPTY_KIND: u16 = u16::MAX;
const HIDDEN: u16 = 1 << 15;

/// Node kinds are indices into the host's kind table (one row table per
/// component kind), not an enum — the bridge registers `View`, `Text`, and
/// later native escape-hatch kinds. The top bit marks a hidden node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct NodeKind(pub u16);

impl NodeKind {
    pub const EMPTY: NodeKind = NodeKind(EMPTY_KIND);
    /// A layout/paint container.
    pub const VIEW: NodeKind = NodeKind(0);
    /// A text leaf: owns a row in the host's text side table.
    pub const TEXT: NodeKind = NodeKind(1);
}

/// Content of a TEXT-kind node. Lives in `Host::texts`, addressed by the
/// header's `aux` slot. `String` keeps the value owned; the wire layer
/// hands us `&str` borrowed from the transaction buffer.
#[derive(Clone, Debug, Default)]
pub struct TextRow {
    pub text: String,
    /// Logical point size; scaled at measure/paint time.
    pub font_size: f32,
    /// 0xRRGGBBAA.
    pub color: u32,
}

const DEFAULT_FONT_SIZE: f32 = 14.0;
const DEFAULT_COLOR: u32 = 0xFFFF_FFFF;

/// Content of a VIEW-kind node: paint data only. Layout inputs live in the
/// interned style; the flex result lives in `Layouts`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ViewRow {
    /// Background fill, 0xRRGGBBAA. Alpha 0 paints nothing.
    pub color: u32,
}

/// Per-node dirty bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct NodeFlags(pub u8);

impl NodeFlags {
    pub const NONE: NodeFlags = NodeFlags(0);
    pub const LAYOUT: NodeFlags = NodeFlags(1 << 0);
    /// Text content or metrics changed — the retained Parley layout is stale.
    pub const TEXT: NodeFlags = NodeFlags(1 << 1);

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

impl std::ops::BitOr for NodeFlags {
    type Output = NodeFlags;
    fn bitor(self, rhs: NodeFlags) -> NodeFlags {
        NodeFlags(self.0 | rhs.0)
    }
}

/// Fixed-size record for every retained node: 20 bytes.
///
/// Topology lives outside the header: `Host::children` holds each node's
/// ordered children (indexed access, no links) and `parent` is the only
/// upward edge. `aux` is the node's row index in its kind's side table
/// (text content, scroll state, ...). `generation` is bumped on slot
/// reuse so a stale (id, generation) reference — e.g. an event listener
/// captured before removal — can never reach the new node.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeHeader {
    pub parent: u32,
    /// Row in this kind's side table. `u32::MAX` when the kind stores no row.
    pub aux: u32,
    /// Interned style handle; `u32::MAX` when unstyled.
    pub style: u32,
    /// Kind index with the `HIDDEN` top bit; `EMPTY_KIND` marks a free slot.
    kind: u16,
    /// Incremented each time this slot is reused.
    pub generation: u16,
    pub flags: NodeFlags,
}

impl NodeHeader {
    const EMPTY: NodeHeader = NodeHeader {
        parent: NodeId::DETACHED.0,
        aux: u32::MAX,
        style: u32::MAX,
        kind: EMPTY_KIND,
        generation: 0,
        flags: NodeFlags::NONE,
    };

    pub fn kind(&self) -> NodeKind {
        NodeKind(self.kind & !HIDDEN)
    }

    pub fn hidden(&self) -> bool {
        self.kind & HIDDEN != 0
    }

    pub fn is_empty(&self) -> bool {
        self.kind == EMPTY_KIND
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        if hidden {
            self.kind |= HIDDEN;
        } else {
            self.kind &= !HIDDEN;
        }
    }
}

/// Dense arena indexed directly by bridge-assigned ids.
///
/// The React side allocates and recycles ids; natively `NodeId` indexes the
/// vector, empty slots are marked `EMPTY_KIND`, and `generation` makes stale
/// ids safe to touch. Children live in `children`, a side table of ordered
/// `Vec<NodeId>` parallel to the arena (empty vecs allocate nothing), with
/// `roots` holding the NIL-parented top level.
pub struct Host {
    nodes: Vec<NodeHeader>,
    children: Vec<Vec<NodeId>>,
    roots: Vec<NodeId>,
    live: usize,
    /// Side table for TEXT-kind nodes, indexed by `header.aux`. Rows are
    /// recycled through `free_texts` when nodes are removed.
    texts: Vec<TextRow>,
    free_texts: Vec<u32>,
    /// Side table for VIEW-kind nodes, indexed by `header.aux`.
    views: Vec<ViewRow>,
    free_views: Vec<u32>,
    /// Nodes needing layout invalidation, in mark order. Pushed by
    /// `mark_dirty` when a node's LAYOUT flag goes 0->1, drained by the
    /// layout pass — no whole-arena dirty scan anywhere.
    layout_dirty: Vec<NodeId>,
    /// Coarse repaint signal: set by any mutation that can change pixels.
    /// Paint clears it. Fine-grained damage regions come later.
    paint_dirty: bool,
}

impl Host {
    pub fn new() -> Host {
        Host {
            nodes: Vec::new(),
            children: Vec::new(),
            roots: Vec::new(),
            live: 0,
            texts: Vec::new(),
            free_texts: Vec::new(),
            views: Vec::new(),
            free_views: Vec::new(),
            layout_dirty: Vec::new(),
            paint_dirty: false,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeHeader> {
        self.nodes.get(id.index()).filter(|n| !n.is_empty())
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut NodeHeader> {
        self.nodes.get_mut(id.index()).filter(|n| !n.is_empty())
    }

    /// Creates a node at the bridge-assigned id. Fails if the slot is live.
    pub fn create(&mut self, id: NodeId, kind: NodeKind) -> Option<&mut NodeHeader> {
        let index = id.index();
        if index >= self.nodes.len() {
            self.nodes.resize(index + 1, NodeHeader::EMPTY);
            self.children.resize_with(index + 1, Vec::new);
        }
        let node = &mut self.nodes[index];
        if !node.is_empty() {
            return None;
        }
        let generation = node.generation;
        *node = NodeHeader {
            kind: kind.0,
            generation,
            ..NodeHeader::EMPTY
        };
        if kind == NodeKind::TEXT {
            let row = self.free_texts.pop().unwrap_or_else(|| {
                self.texts.push(TextRow {
                    text: String::new(),
                    font_size: DEFAULT_FONT_SIZE,
                    color: DEFAULT_COLOR,
                });
                (self.texts.len() - 1) as u32
            });
            node.aux = row;
        } else if kind == NodeKind::VIEW {
            let row = self.free_views.pop().unwrap_or_else(|| {
                self.views.push(ViewRow::default());
                (self.views.len() - 1) as u32
            });
            node.aux = row;
        }
        self.live += 1;
        Some(node)
    }

    /// Ordered children of `parent`; ROOT yields the top-level list.
    pub fn children(&self, parent: NodeId) -> &[NodeId] {
        if parent.is_nil() {
            return &self.roots;
        }
        self.children
            .get(parent.index())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn child_count(&self, parent: NodeId) -> usize {
        self.children(parent).len()
    }

    /// The `index`-th child of `parent`, O(1). NIL when out of range.
    pub fn child_at(&self, parent: NodeId, index: usize) -> NodeId {
        self.children(parent)
            .get(index)
            .copied()
            .unwrap_or(NodeId::NIL)
    }

    fn children_mut(&mut self, parent: NodeId) -> &mut Vec<NodeId> {
        if parent.is_nil() {
            return &mut self.roots;
        }
        &mut self.children[parent.index()]
    }

    /// Appends `child` at the end of `parent`'s child list. `parent` of NIL
    /// appends to the root list.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, NodeId::NIL);
    }

    /// Inserts `child` before `before`; NIL `before` appends.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.detach(child);
        let list = self.children_mut(parent);
        let pos = if before.is_nil() {
            list.len()
        } else {
            list.iter().position(|&c| c == before).unwrap_or(list.len())
        };
        list.insert(pos, child);
        self.nodes[child.index()].parent = parent.0;
        self.mark_dirty(child, NodeFlags::LAYOUT);
    }

    /// Detaches `id` and marks its slot empty. The bridge sends explicit
    /// removes per node, so a removed node's subtree is released by its own
    /// remove ops; here we only free this node's side-table row.
    pub fn remove(&mut self, id: NodeId) {
        self.detach(id);
        let index = id.index();
        self.children[index] = Vec::new();
        let node = &mut self.nodes[index];
        if node.aux != u32::MAX {
            match node.kind() {
                NodeKind::TEXT => self.free_texts.push(node.aux),
                NodeKind::VIEW => self.free_views.push(node.aux),
                _ => {}
            }
        }
        let generation = node.generation.wrapping_add(1);
        *node = NodeHeader {
            generation,
            ..NodeHeader::EMPTY
        };
        self.live -= 1;
    }

    /// Unlinks `id` from its parent without freeing the slot.
    pub fn detach(&mut self, id: NodeId) {
        let parent = self.nodes[id.index()].parent;
        if parent == NodeId::DETACHED.0 {
            return;
        }
        let list = self.children_mut(NodeId(parent));
        if let Some(pos) = list.iter().position(|&c| c == id) {
            list.remove(pos);
        }
        let n = &mut self.nodes[id.index()];
        n.parent = NodeId::DETACHED.0;
        // The parent's layout output depended on this child.
        self.mark_dirty(NodeId(parent), NodeFlags::LAYOUT);
    }

    /// Number of slots ever allocated, including empty ones. For sweeps.
    pub fn slot_count(&self) -> usize {
        self.nodes.len()
    }

    /// Parent of `id`, or NIL/DETACHED sentinels.
    pub fn parent(&self, id: NodeId) -> NodeId {
        self.node(id)
            .map(|n| NodeId(n.parent))
            .unwrap_or(NodeId::NIL)
    }

    /// Text row of a TEXT-kind node.
    pub fn text(&self, id: NodeId) -> Option<&TextRow> {
        let node = self.node(id).filter(|n| n.kind() == NodeKind::TEXT)?;
        self.texts.get(node.aux as usize)
    }

    /// Sets a text node's content. Marks layout + paint dirty.
    pub fn set_text(&mut self, id: NodeId, text: &str) {
        let Some(node) = self.node(id) else { return };
        if node.kind() != NodeKind::TEXT {
            return;
        }
        let aux = node.aux;
        if aux == u32::MAX {
            return;
        }
        self.texts[aux as usize].text.clear();
        self.texts[aux as usize].text.push_str(text);
        self.mark_dirty(id, NodeFlags::LAYOUT | NodeFlags::TEXT);
    }

    /// Sets a text node's font size (logical points) and color (0xRRGGBBAA).
    /// Font size invalidates layout; a color-only change repaints without
    /// touching layout or the retained Parley layout.
    pub fn set_text_props(&mut self, id: NodeId, font_size: f32, color: u32) {
        let Some(node) = self.node(id) else { return };
        if node.kind() != NodeKind::TEXT {
            return;
        }
        let aux = node.aux;
        if aux == u32::MAX {
            return;
        }
        let row = &mut self.texts[aux as usize];
        let size_changed = row.font_size != font_size;
        row.font_size = font_size;
        row.color = color;
        if size_changed {
            self.mark_dirty(id, NodeFlags::LAYOUT | NodeFlags::TEXT);
        } else {
            // Color lives in the emitted instances, not the layout.
            self.paint_dirty = true;
        }
    }

    /// View row of a VIEW-kind node.
    pub fn view(&self, id: NodeId) -> Option<&ViewRow> {
        let node = self.node(id).filter(|n| n.kind() == NodeKind::VIEW)?;
        self.views.get(node.aux as usize)
    }

    /// Sets a view's background color (0xRRGGBBAA).
    pub fn set_view_paint(&mut self, id: NodeId, color: u32) {
        let Some(node) = self.node(id) else { return };
        if node.kind() != NodeKind::VIEW {
            return;
        }
        let aux = node.aux;
        if aux == u32::MAX {
            return;
        }
        self.views[aux as usize].color = color;
        self.paint_dirty = true;
    }

    /// Sets the wire style id on a node (`u32::MAX` clears it).
    pub fn set_style(&mut self, id: NodeId, style: u32) {
        if let Some(node) = self.node_mut(id) {
            node.style = style;
        }
        self.mark_dirty(id, NodeFlags::LAYOUT);
    }

    /// Sets or clears the hidden bit.
    pub fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        if let Some(node) = self.node_mut(id) {
            node.set_hidden(hidden);
        }
        self.mark_dirty(id, NodeFlags::LAYOUT);
    }

    /// Marks a node dirty and notes the repaint. LAYOUT transitions enqueue
    /// the node once — the layout pass drains `layout_dirty` instead of
    /// scanning the arena.
    pub fn mark_dirty(&mut self, id: NodeId, flags: NodeFlags) {
        if let Some(node) = self
            .nodes
            .get_mut(id.index())
            .filter(|n| !n.is_empty())
        {
            if flags.contains(NodeFlags::LAYOUT) && !node.flags.contains(NodeFlags::LAYOUT) {
                self.layout_dirty.push(id);
            }
            node.flags.set(flags);
        }
        self.paint_dirty = true;
    }

    /// Drains the layout-dirty queue. Called once per layout pass.
    pub fn take_layout_dirty(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.layout_dirty)
    }

    /// True when any mutation since the last `clear_paint_dirty` can change
    /// pixels.
    pub fn paint_dirty(&self) -> bool {
        self.paint_dirty
    }

    /// Forces the next paint (e.g. after a resize cleared layout caches).
    pub fn force_paint(&mut self) {
        self.paint_dirty = true;
    }

    pub fn clear_paint_dirty(&mut self) {
        self.paint_dirty = false;
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
}

impl Default for Host {
    fn default() -> Host {
        Host::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: NodeKind = NodeKind(0);
    const TEXT: NodeKind = NodeKind(1);

    #[test]
    fn create_and_index() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        host.create(NodeId(5), TEXT).unwrap();
        assert_eq!(host.len(), 2);
        assert_eq!(host.node(NodeId(5)).unwrap().kind(), TEXT);
        // Unoccupied slots read as absent.
        assert!(host.node(NodeId(3)).is_none());
        // Creating over a live slot fails.
        assert!(host.create(NodeId(0), TEXT).is_none());
    }

    #[test]
    fn append_maintains_order_and_counts() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        for i in 1..=3u32 {
            host.create(NodeId(i), TEXT).unwrap();
            host.append_child(NodeId(0), NodeId(i));
        }
        assert_eq!(
            host.children(NodeId(0)),
            vec![NodeId(1), NodeId(2), NodeId(3)]
        );
        assert_eq!(host.child_count(NodeId(0)), 3);
        assert_eq!(host.node(NodeId(2)).unwrap().parent, 0);
    }

    #[test]
    fn insert_before_splices() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        for i in 1..=3u32 {
            host.create(NodeId(i), TEXT).unwrap();
            host.append_child(NodeId(0), NodeId(i));
        }
        host.create(NodeId(9), TEXT).unwrap();
        host.insert_before(NodeId(0), NodeId(9), NodeId(2));
        assert_eq!(
            host.children(NodeId(0)),
            vec![NodeId(1), NodeId(9), NodeId(2), NodeId(3)]
        );
    }

    #[test]
    fn reappend_moves_node() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        for i in 1..=3u32 {
            host.create(NodeId(i), TEXT).unwrap();
            host.append_child(NodeId(0), NodeId(i));
        }
        host.append_child(NodeId(0), NodeId(1));
        assert_eq!(
            host.children(NodeId(0)),
            vec![NodeId(2), NodeId(3), NodeId(1)]
        );
        assert_eq!(host.child_count(NodeId(0)), 3);
    }

    #[test]
    fn remove_recycles_slot_and_bumps_generation() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        host.create(NodeId(1), TEXT).unwrap();
        host.append_child(NodeId(0), NodeId(1));
        let gen0 = host.node(NodeId(1)).unwrap().generation;
        host.remove(NodeId(1));
        assert_eq!(host.len(), 1);
        assert!(host.node(NodeId(1)).is_none());
        assert_eq!(host.children(NodeId(0)), vec![]);
        // Reuse of the same id carries the bumped generation, so a stale
        // (id, generation) pair can never reach the new node.
        let node = host.create(NodeId(1), TEXT).unwrap();
        assert_eq!(node.generation, gen0.wrapping_add(1));
    }

    #[test]
    fn roots_are_children_of_nil() {
        let mut host = Host::new();
        host.create(NodeId(0), VIEW).unwrap();
        host.create(NodeId(1), VIEW).unwrap();
        host.append_child(ROOT, NodeId(0));
        host.append_child(ROOT, NodeId(1));
        assert_eq!(host.children(ROOT), vec![NodeId(0), NodeId(1)]);
        host.remove(NodeId(0));
        assert_eq!(host.children(ROOT), vec![NodeId(1)]);
    }

    #[test]
    fn hidden_bit_does_not_change_kind() {
        let mut host = Host::new();
        let node = host.create(NodeId(0), VIEW).unwrap();
        node.kind = VIEW.0 | HIDDEN;
        let node = host.node(NodeId(0)).unwrap();
        assert!(node.hidden());
        assert_eq!(node.kind(), VIEW);
    }

    #[test]
    fn header_stays_compact() {
        assert_eq!(std::mem::size_of::<NodeHeader>(), 20);
    }
}
