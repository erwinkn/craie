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
    pub const TEXT: NodeFlags = NodeFlags(1 << 1);
    pub const PAINT: NodeFlags = NodeFlags(1 << 2);

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

/// Fixed-size record for every retained node: 40 bytes.
///
/// Children are a doubly-linked sibling list so insertion and removal are
/// O(1) with no per-node allocation. `aux` is the node's row index in its
/// kind's side table (text content, scroll state, ...). `generation` is
/// bumped on slot reuse so a stale (id, generation) reference — e.g. an
/// event listener captured before removal — can never reach the new node.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeHeader {
    pub parent: u32,
    pub first_child: u32,
    pub last_child: u32,
    pub next_sibling: u32,
    pub prev_sibling: u32,
    pub child_count: u32,
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
        first_child: NodeId::NIL.0,
        last_child: NodeId::NIL.0,
        next_sibling: NodeId::NIL.0,
        prev_sibling: NodeId::NIL.0,
        child_count: 0,
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
/// ids safe to touch. Root siblings are linked as children of `ROOT` on a
/// dedicated head/tail pair owned by the host.
pub struct Host {
    nodes: Vec<NodeHeader>,
    live: usize,
    first_root: u32,
    last_root: u32,
    /// Side table for TEXT-kind nodes, indexed by `header.aux`. Rows are
    /// recycled through `free_texts` when nodes are removed.
    texts: Vec<TextRow>,
    free_texts: Vec<u32>,
    /// Side table for VIEW-kind nodes, indexed by `header.aux`.
    views: Vec<ViewRow>,
    free_views: Vec<u32>,
    /// Coarse repaint signal: set by any mutation that can change pixels.
    /// Paint clears it. Fine-grained damage regions come later.
    paint_dirty: bool,
}

impl Host {
    pub fn new() -> Host {
        Host {
            nodes: Vec::new(),
            live: 0,
            first_root: NodeId::NIL.0,
            last_root: NodeId::NIL.0,
            texts: Vec::new(),
            free_texts: Vec::new(),
            views: Vec::new(),
            free_views: Vec::new(),
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

    /// Appends `child` at the end of `parent`'s child list. `parent` of NIL
    /// appends to the root list.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, NodeId::NIL);
    }

    /// Inserts `child` before `before`; NIL `before` appends.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: NodeId) {
        self.detach(child);
        let prev = if before.is_nil() {
            self.tail_of(parent)
        } else {
            NodeId(self.nodes[before.index()].prev_sibling)
        };
        {
            let c = &mut self.nodes[child.index()];
            c.parent = parent.0;
            c.prev_sibling = prev.0;
            c.next_sibling = before.0;
            c.flags.set(NodeFlags::LAYOUT | NodeFlags::PAINT);
        }
        if !prev.is_nil() {
            self.nodes[prev.index()].next_sibling = child.0;
        } else {
            self.set_head(parent, child);
        }
        if !before.is_nil() {
            self.nodes[before.index()].prev_sibling = child.0;
        } else {
            self.set_tail(parent, child);
        }
        self.bump_child_count(parent, 1);
        self.paint_dirty = true;
    }

    /// Detaches `id` and marks its slot empty. The bridge sends explicit
    /// removes per node, so a removed node's subtree is released by its own
    /// remove ops; here we only free this node's side-table row.
    pub fn remove(&mut self, id: NodeId) {
        self.detach(id);
        let node = &mut self.nodes[id.index()];
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
        let (parent, prev, next) = {
            let n = &self.nodes[id.index()];
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        if parent == NodeId::DETACHED.0 {
            return;
        }
        if prev != NodeId::NIL.0 {
            self.nodes[prev as usize].next_sibling = next;
        } else {
            self.set_head(NodeId(parent), NodeId(next));
        }
        if next != NodeId::NIL.0 {
            self.nodes[next as usize].prev_sibling = prev;
        } else {
            self.set_tail(NodeId(parent), NodeId(prev));
        }
        self.bump_child_count(NodeId(parent), -1);
        let n = &mut self.nodes[id.index()];
        n.parent = NodeId::DETACHED.0;
        n.prev_sibling = NodeId::NIL.0;
        n.next_sibling = NodeId::NIL.0;
        self.paint_dirty = true;
    }

    fn head(&self, parent: NodeId) -> u32 {
        if parent.is_nil() {
            self.first_root
        } else {
            self.nodes[parent.index()].first_child
        }
    }

    fn tail_of(&self, parent: NodeId) -> NodeId {
        NodeId(if parent.is_nil() {
            self.last_root
        } else {
            self.nodes[parent.index()].last_child
        })
    }

    fn set_head(&mut self, parent: NodeId, head: NodeId) {
        if parent.is_nil() {
            self.first_root = head.0;
        } else {
            self.nodes[parent.index()].first_child = head.0;
        }
    }

    fn set_tail(&mut self, parent: NodeId, tail: NodeId) {
        if parent.is_nil() {
            self.last_root = tail.0;
        } else {
            self.nodes[parent.index()].last_child = tail.0;
        }
    }

    fn bump_child_count(&mut self, parent: NodeId, delta: i32) {
        if parent.is_nil() {
            return;
        }
        let n = &mut self.nodes[parent.index()];
        n.child_count = n.child_count.wrapping_add_signed(delta);
    }

    /// Iterates the sibling list starting at `first` (inclusive).
    pub fn siblings(&self, first: NodeId) -> Siblings<'_> {
        Siblings {
            host: self,
            next: first.0,
        }
    }

    /// First child of `parent` (or first root for NIL).
    pub fn first_child(&self, parent: NodeId) -> NodeId {
        NodeId(self.head(parent))
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
        self.mark_dirty(id, NodeFlags::LAYOUT | NodeFlags::PAINT | NodeFlags::TEXT);
    }

    /// Sets a text node's font size (logical points) and color (0xRRGGBBAA).
    pub fn set_text_props(&mut self, id: NodeId, font_size: f32, color: u32) {
        let Some(node) = self.node(id) else { return };
        if node.kind() != NodeKind::TEXT {
            return;
        }
        let aux = node.aux;
        if aux == u32::MAX {
            return;
        }
        self.texts[aux as usize].font_size = font_size;
        self.texts[aux as usize].color = color;
        self.mark_dirty(id, NodeFlags::LAYOUT | NodeFlags::PAINT | NodeFlags::TEXT);
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
        self.mark_dirty(id, NodeFlags::PAINT);
    }

    /// Sets the interned style on a node (`u32::MAX` clears it).
    pub fn set_style(&mut self, id: NodeId, style: u32) {
        if let Some(node) = self.node_mut(id) {
            node.style = style;
            node.flags.set(NodeFlags::LAYOUT | NodeFlags::PAINT);
            self.paint_dirty = true;
        }
    }

    /// Sets or clears the hidden bit.
    pub fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        if let Some(node) = self.node_mut(id) {
            node.set_hidden(hidden);
            node.flags.set(NodeFlags::LAYOUT | NodeFlags::PAINT);
            self.paint_dirty = true;
        }
    }

    /// Marks a node dirty and notes the repaint.
    pub fn mark_dirty(&mut self, id: NodeId, flags: NodeFlags) {
        if let Some(node) = self.node_mut(id) {
            node.flags.set(flags);
        }
        self.paint_dirty = true;
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
        for n in self.nodes.iter_mut() {
            n.flags.clear(NodeFlags::PAINT);
        }
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

/// Sibling-list cursor. Returned by `Host::siblings`.
pub struct Siblings<'a> {
    host: &'a Host,
    next: u32,
}

impl Iterator for Siblings<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.next == NodeId::NIL.0 {
            return None;
        }
        let id = NodeId(self.next);
        self.next = self.host.nodes[id.index()].next_sibling;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: NodeKind = NodeKind(0);
    const TEXT: NodeKind = NodeKind(1);

    fn children(host: &Host, parent: NodeId) -> Vec<NodeId> {
        host.siblings(host.first_child(parent)).collect()
    }

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
            children(&host, NodeId(0)),
            vec![NodeId(1), NodeId(2), NodeId(3)]
        );
        assert_eq!(host.node(NodeId(0)).unwrap().child_count, 3);
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
            children(&host, NodeId(0)),
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
            children(&host, NodeId(0)),
            vec![NodeId(2), NodeId(3), NodeId(1)]
        );
        assert_eq!(host.node(NodeId(0)).unwrap().child_count, 3);
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
        assert_eq!(children(&host, NodeId(0)), vec![]);
        assert_eq!(host.node(NodeId(0)).unwrap().child_count, 0);
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
        assert_eq!(children(&host, ROOT), vec![NodeId(0), NodeId(1)]);
        host.remove(NodeId(0));
        assert_eq!(children(&host, ROOT), vec![NodeId(1)]);
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
        assert_eq!(std::mem::size_of::<NodeHeader>(), 40);
    }
}
