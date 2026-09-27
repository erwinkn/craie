//! Paint order: a parent's children back to front. Tree order, stably
//! sorted by `z`, as React Native's `zIndex` orders siblings: there are
//! no stacking contexts, and ties keep tree order. Painting walks this
//! order and hit testing walks it in reverse; layout, Tab order,
//! selection and accessibility keep tree order.
//!
//! Layers (`Mutation::Layer`) add one rule: a layer never sorts below
//! the sibling that holds its owner (the owner or its ancestor under the
//! same parent). A layer's sort key is its z and its tree position,
//! each raised to at least its owner sibling's, then one step above it:
//!
//! ```text
//! root: [app z 0, dialog z 70, menu z 50 (owner in dialog), toast z 80]
//! keys:  (0, 0, 0)  (70, 1, 0)  (70, 2, 1)                 (80, 3, 0)
//! paint: app, dialog, menu, toast
//! ```
//!
//! An owner that no sibling holds is silently no owner: one outside the
//! parent's subtree, one inside the layer itself, and a removed one
//! (`Host::remove` resets it to NIL, so a recycled id adopts nothing).
//!
//! Only parents that need an order hold one (`NodeFlags::SORTED`, the
//! order in `Host::orders`): a parent whose children all have z = 0 and
//! no owner paints in tree order and never sorts. A tree edit under a
//! sorted parent, a child arriving with a z or an owner, and a z change
//! queue the parent (`NodeFlags::ORDER`); `refresh_orders` re-sorts the
//! queue before a frame's paint and before each dispatch, and a reader
//! in between sorts a queued parent on the spot (`paint_order`). A z
//! change moves nothing, so it costs no layout and leaves reaches as
//! they are: it bumps `revs.structure`, which rebuilds the draw order.

use std::borrow::Cow;

use crate::host::{Host, NodeFlags, NodeId};
use crate::mutation::NIL;

/// A child's sort key: effective z, effective tree position, and how
/// many owners up the chain it sits (1 above its owner sibling).
type Key = (i32, u32, u32);

impl Host {
    /// `SORTED` and `ORDER` of `parent` (ROOT: the root level).
    pub(crate) fn order_flags(&self, parent: NodeId) -> NodeFlags {
        if parent.is_nil() {
            self.root_flags
        } else {
            self.node(parent).map_or(NodeFlags::NONE, |n| n.flags)
        }
    }

    fn order_flags_mut(&mut self, parent: NodeId) -> Option<&mut NodeFlags> {
        if parent.is_nil() {
            Some(&mut self.root_flags)
        } else {
            self.node_mut(parent).map(|n| &mut n.flags)
        }
    }

    /// Queues `parent`'s paint order for re-sorting (once).
    pub(crate) fn queue_order(&mut self, parent: NodeId) {
        if let Some(f) = self.order_flags_mut(parent)
            && !f.contains(NodeFlags::ORDER)
        {
            f.set(NodeFlags::ORDER);
            self.order_queue.push(parent.0);
        }
    }

    /// Sets `id`'s z; returns whether it changed. The draw order
    /// rebuilds; nothing relayouts.
    pub fn set_z(&mut self, id: NodeId, z: i32) -> bool {
        if !self.is_live(id) || self.spatial[id.index()].z == z {
            return false;
        }
        self.spatial[id.index()].z = z;
        let parent = self.parent(id);
        if parent != NodeId::DETACHED {
            self.queue_order(parent);
        }
        self.revs.structure.bump();
        true
    }

    /// Makes `id` a layer container owned by `owner` (NIL: none).
    pub fn set_layer(&mut self, id: NodeId, owner: u32) {
        let Some(n) = self.node_mut(id) else { return };
        n.flags.set(NodeFlags::LAYER);
        if self.owners.insert(id.0, owner) == Some(owner) {
            return;
        }
        let parent = self.parent(id);
        if parent != NodeId::DETACHED {
            self.queue_order(parent);
        }
        self.revs.structure.bump();
    }

    /// A layer container: hit testing passes through its own box.
    pub fn is_layer(&self, id: NodeId) -> bool {
        self.node(id)
            .is_some_and(|n| n.flags.contains(NodeFlags::LAYER))
    }

    /// `parent`'s children in paint order, back to front (ROOT: the
    /// root level). Borrowed unless the order may be stale, which only
    /// a reader between a change and `refresh_orders` sees: queued, or
    /// holding a layer whose owner may have moved.
    pub fn paint_order(&self, parent: NodeId) -> Cow<'_, [NodeId]> {
        let flags = self.order_flags(parent);
        if flags.contains(NodeFlags::ORDER) || self.owners_moved(parent) {
            let mut out = Vec::new();
            let mut keys = Vec::new();
            if self.sort_children(parent, &mut out, &mut keys) {
                return Cow::Owned(out);
            }
        } else if flags.contains(NodeFlags::SORTED)
            && let Some(order) = self.orders.get(&parent.0)
        {
            return Cow::Borrowed(order);
        }
        Cow::Borrowed(self.children(parent))
    }

    /// Whether `parent` holds a layer and the tree changed since the
    /// last refresh (which re-sorts it).
    fn owners_moved(&self, parent: NodeId) -> bool {
        !self.owners.is_empty()
            && self.order_rev != self.revs.structure
            && self
                .owners
                .keys()
                .any(|&l| self.parent(NodeId(l)) == parent)
    }

    /// Re-sorts the queued parents. With owners about, a structure change
    /// anywhere can move an owner to another sibling, so it re-sorts
    /// the owned layers' parents too (a handful of nodes).
    pub fn refresh_orders(&mut self) {
        if !self.owners.is_empty() && self.order_rev != self.revs.structure {
            let parents: Vec<NodeId> = self
                .owners
                .keys()
                .map(|&id| self.parent(NodeId(id)))
                .collect();
            for p in parents {
                if p != NodeId::DETACHED {
                    self.queue_order(p);
                }
            }
        }
        self.order_rev = self.revs.structure;
        if self.order_queue.is_empty() {
            return;
        }
        let queue = std::mem::take(&mut self.order_queue);
        let mut keys = Vec::new();
        for &p in &queue {
            let parent = NodeId(p);
            let Some(f) = self.order_flags_mut(parent) else {
                continue;
            };
            if !f.contains(NodeFlags::ORDER) {
                continue;
            }
            f.clear(NodeFlags::ORDER);
            let mut order = self.orders.remove(&p).unwrap_or_default();
            let sorted = self.sort_children(parent, &mut order, &mut keys);
            let f = self.order_flags_mut(parent).expect("checked above");
            if sorted {
                f.set(NodeFlags::SORTED);
                self.orders.insert(p, order);
            } else {
                f.clear(NodeFlags::SORTED);
            }
        }
        self.order_queue = queue;
        self.order_queue.clear();
    }

    /// Writes `parent`'s paint order into `out`; false (and `out`
    /// untouched) when that is tree order.
    fn sort_children(
        &self,
        parent: NodeId,
        out: &mut Vec<NodeId>,
        keys: &mut Vec<(Key, NodeId)>,
    ) -> bool {
        let kids = self.children(parent);
        let owned = kids.iter().any(|&c| self.is_layer(c));
        if !owned {
            if kids.iter().all(|&c| self.spatial[c.index()].z == 0) {
                return false;
            }
            // Stable and adaptive: one z among thousands of zeros costs
            // about a linear pass.
            out.clear();
            out.extend_from_slice(kids);
            out.sort_by_key(|&c| self.spatial[c.index()].z);
            return true;
        }
        // Each layer's owner sibling, by index.
        let holder: Vec<Option<usize>> = kids
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                let owner = *self.owners.get(&c.0)?;
                let h = self.holder(parent, owner, kids)?;
                (h != i).then_some(h)
            })
            .collect();
        if holder.iter().all(Option::is_none)
            && kids.iter().all(|&c| self.spatial[c.index()].z == 0)
        {
            return false;
        }
        let mut memo: Vec<Option<Key>> = vec![None; kids.len()];
        for i in 0..kids.len() {
            self.key(i, kids, &holder, &mut memo);
        }
        keys.clear();
        keys.extend(memo.iter().zip(kids).map(|(k, &c)| (k.expect("keyed"), c)));
        keys.sort_by_key(|&(k, _)| k);
        out.clear();
        out.extend(keys.iter().map(|&(_, c)| c));
        true
    }

    /// Child `i`'s key, raised above its owner sibling's. An ownership
    /// cycle (layers each owned from inside the other) is cut where it
    /// closes: the child it returns to counts as unowned there.
    fn key(
        &self,
        i: usize,
        kids: &[NodeId],
        holder: &[Option<usize>],
        memo: &mut [Option<Key>],
    ) -> Key {
        if let Some(k) = memo[i] {
            return k;
        }
        let z = self.spatial[kids[i].index()].z;
        let mut k = (z, i as u32, 0);
        if let Some(h) = holder[i] {
            memo[i] = Some(k);
            let (hz, hi, hd) = self.key(h, kids, holder, memo);
            k = (z.max(hz), (i as u32).max(hi), hd + 1);
        }
        memo[i] = Some(k);
        k
    }

    /// The index in `kids` (children of `parent`) of the child that is
    /// `owner` or holds it.
    fn holder(&self, parent: NodeId, owner: u32, kids: &[NodeId]) -> Option<usize> {
        if owner == NIL {
            return None;
        }
        let mut cur = NodeId(owner);
        loop {
            let p = self.node(cur)?.parent();
            if p == parent {
                return kids.iter().position(|&c| c == cur);
            }
            if !p.is_node() {
                return None;
            }
            cur = p;
        }
    }
}
