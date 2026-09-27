//! Focus traps, `modal` and `inert` (ARCHITECTURE-update topic 3).
//!
//! A trap is a node (`Mutation::Trap`) that keeps Tab inside its scope
//! while active: its subtree plus the layers it owns, and theirs. A
//! modal trap also makes everything outside that scope inert. `inert`
//! (`interaction_flag::INERT`) takes a node and its subtree out of hit
//! testing, focus and accessibility; the layers it owns escape it, as a
//! portal escapes an inert DOM parent.
//!
//! Scope follows owners: a root-level layer's scope parent is its owner
//! (`order.rs`), any other node's is its tree parent. So in
//!
//! ```text
//! root: [app, dialog layer z 70, menu layer z 50 (owner: More)]
//! app ─ Delete…
//! dialog layer ─ trap (modal) ─ view ─ Cancel, Delete, More
//! menu layer ─ menu ─ Rename, Archive
//! ```
//!
//! the menu is inside the trap: Tab cycles Cancel, Delete, More, Rename,
//! Archive (a layer's focusables right after its owner's subtree), and
//! the pointer reaches the dialog and the menu but not the app.
//!
//! The executor records trap flags and focus requests as they arrive;
//! `settle_traps`, at the end of the transaction, activates and
//! deactivates against them, picks where focus ends (restore, request,
//! auto-focus, a lost focus) and moves it once, then refreshes the gate
//! the hit test and the accessibility tree read. A trap that is hidden
//! (`display: none`, as Suspense hides) or inert is inactive.

use std::collections::HashMap;

use crate::events::{mask, out_kind};
use crate::group::GroupWalk;
use crate::host::{NodeFlags, NodeId};
use crate::mutation::{NIL, NodeKind, trap_flag};
use crate::ui::Ui;

/// An active trap.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Active {
    pub id: NodeId,
    pub generation: u16,
    pub flags: u8,
    /// The focus when it activated, as (id, generation).
    pub restore: Option<(NodeId, u16)>,
}

/// The top modal's reach: the nodes whose subtrees it leaves
/// interactive (the modal and the layers in its scope), and their tree
/// ancestors, which the hit test passes through without hitting. Both
/// sorted by id for binary search.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Gate {
    pub modal: Option<NodeId>,
    roots: Vec<u32>,
    path: Vec<u32>,
}

/// Where a node stands against the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Its subtree is interactive.
    Root,
    /// It holds a root below: pass through, testing children.
    Path,
    /// Inert.
    Out,
}

impl Gate {
    pub fn class(&self, id: NodeId) -> Class {
        if self.roots.binary_search(&id.0).is_ok() {
            Class::Root
        } else if self.path.binary_search(&id.0).is_ok() {
            Class::Path
        } else {
            Class::Out
        }
    }
}

#[derive(Default)]
pub(crate) struct Traps {
    /// Declared trap flags, id-keyed: few nodes are traps.
    pub declared: HashMap<u32, u8>,
    /// Active traps, each after the active traps whose scope holds it,
    /// else in activation order: the last modal is the top one.
    pub stack: Vec<Active>,
    pub gate: Gate,
    /// The previous gate's buffers, reused.
    spare: Gate,
    /// Nodes flagged `INERT`.
    pub inert: usize,
    /// A trap flag or an inert flag changed in this transaction.
    pub dirty: bool,
    /// The focused node was removed in this transaction.
    pub focus_removed: bool,
    /// The active traps holding the focus when the transaction began,
    /// innermost first.
    chain: Vec<NodeId>,
    /// The focus when the transaction began: what traps activating in
    /// it restore.
    before: Option<(NodeId, u16)>,
    /// The node the transaction's last Focus command asked for (not
    /// blurred since).
    pub request: Option<NodeId>,
    /// Nodes whose `AUTO_FOCUS` flag this transaction set.
    pub auto_focused: Vec<NodeId>,
}

impl Traps {
    fn is_active(&self, id: NodeId) -> bool {
        self.stack.iter().any(|a| a.id == id)
    }

    /// A trap declared active, on the stack or not (hidden).
    fn any_declared(&self) -> bool {
        self.declared.values().any(|&f| f & trap_flag::ACTIVE != 0)
    }

    /// The last modal on the stack: the innermost, or the most recently
    /// activated of unrelated ones.
    pub fn top_modal(&self) -> Option<NodeId> {
        self.stack
            .iter()
            .rev()
            .find(|a| a.flags & trap_flag::MODAL != 0)
            .map(|a| a.id)
    }

    /// An active modal trap (the accessibility tree's modal flag).
    pub fn is_modal(&self, id: NodeId) -> bool {
        self.stack
            .iter()
            .any(|a| a.id == id && a.flags & trap_flag::MODAL != 0)
    }
}

impl Ui {
    /// `n`, then its scope parents: a root-level layer's live owner, or
    /// the tree parent. Owner cycles end after one jump per owner.
    pub(crate) fn scope_chain(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut jumps = self.host.owners.len();
        std::iter::successors(Some(n).filter(|n| self.host.is_live(*n)), move |&c| {
            let p = self.host.parent(c);
            if p.is_nil()
                && jumps > 0
                && let Some(o) = self.layer_owner(c)
            {
                jumps -= 1;
                return Some(o);
            }
            Some(p).filter(|p| p.is_node())
        })
    }

    /// The live owner of layer `id`, wherever the layer sits.
    fn layer_owner(&self, id: NodeId) -> Option<NodeId> {
        let node = self.host.node(id)?;
        if !node.flags.contains(NodeFlags::LAYER) {
            return None;
        }
        let o = NodeId(*self.host.owners.get(&id.0)?);
        (o.0 != NIL && self.host.is_live(o)).then_some(o)
    }

    /// Whether `t` is on `n`'s scope chain (`n` itself included).
    pub(crate) fn in_scope(&self, n: NodeId, t: NodeId) -> bool {
        self.scope_chain(n).any(|c| c == t)
    }

    /// The owner of root-level layer `id` when its chain reaches the
    /// root: a cycle of owners, or an owner out of the tree, owns
    /// nothing, and the layer takes its place after the app.
    fn effective_owner(&self, id: NodeId) -> Option<NodeId> {
        let o = self.layer_owner(id)?;
        let last = self.scope_chain(o).last()?;
        // A chain that ran out of jumps ends on a layer that still has
        // an owner.
        (self.host.parent(last).is_nil() && self.layer_owner(last).is_none()).then_some(o)
    }

    /// Root-level layers with an effective owner, as (owner, layer),
    /// sorted by owner and then in root order.
    fn owned_layers(&self) -> Vec<(u32, NodeId)> {
        let mut owned = Vec::new();
        if self.host.owners.is_empty() {
            return owned;
        }
        for &c in self.host.children(crate::host::ROOT) {
            if let Some(o) = self.effective_owner(c) {
                owned.push((o.0, c));
            }
        }
        owned.sort_by_key(|&(o, _)| o);
        owned
    }

    fn takes_focus(&self, id: NodeId) -> bool {
        self.host.kind(id) == Some(NodeKind::Input) || self.host.interaction(id).focusable
    }

    /// Hidden by a tree ancestor or itself: `display: none` or `INERT`.
    /// Out of the tree counts as hidden.
    fn hidden(&self, id: NodeId) -> bool {
        let mut cur = id;
        loop {
            let Some(node) = self.host.node(cur) else {
                return true;
            };
            if node.flags.contains(NodeFlags::INERT) || self.host.display_none(cur) {
                return true;
            }
            let p = node.parent();
            if p.is_nil() {
                return false;
            }
            if !p.is_node() {
                return true;
            }
            cur = p;
        }
    }

    /// Inert, by its own flag or an ancestor's, or outside the top
    /// modal's scope.
    pub(crate) fn blocked(&self, id: NodeId) -> bool {
        self.ancestors(id).any(|n| {
            self.host
                .node(n)
                .is_some_and(|h| h.flags.contains(NodeFlags::INERT))
        }) || self
            .traps
            .top_modal()
            .is_some_and(|m| !self.in_scope(id, m))
    }

    /// Whether focus may go to `id`: live, in the tree, shown, focusable
    /// and not blocked.
    fn can_focus(&self, id: NodeId) -> bool {
        self.takes_focus(id) && !self.hidden(id) && !self.blocked(id)
    }

    /// Tab order within `scope` (ROOT: the window): tree order, each
    /// owned layer right after its owner's subtree, unowned layers
    /// after the app. `display: none` and `inert` hide a subtree but
    /// not the layers its nodes own. A focus group is one stop
    /// (`group.rs`): its other members are skipped.
    pub(crate) fn tab_order(&self, scope: NodeId) -> Vec<NodeId> {
        let owned = self.owned_layers();
        let mut out = Vec::new();
        // What the groups walked take away (`group_skips`), filtered out
        // once at the end: all of it lies inside those groups.
        let mut skip: Vec<u32> = Vec::new();
        let mut walk = GroupWalk::default();
        let mut stack: Vec<(NodeId, bool)> = Vec::new();
        if scope.is_nil() {
            for &c in self.host.children(scope).iter().rev() {
                if !owned.iter().any(|&(_, l)| l == c) {
                    stack.push((c, false));
                }
            }
        } else {
            let p = self.host.parent(scope);
            stack.push((scope, !p.is_nil() && self.hidden(p)));
        }
        while let Some((id, above)) = stack.pop() {
            let Some(node) = self.host.node(id) else {
                continue;
            };
            let hidden =
                above || node.flags.contains(NodeFlags::INERT) || self.host.display_none(id);
            let i = self.host.interaction(id);
            if !hidden && i.group {
                self.group_skips(id, &mut skip, &mut walk);
            }
            if !hidden && (node.kind == NodeKind::Input || i.focusable) {
                out.push(id);
            }
            if !owned.is_empty() {
                let from = owned.partition_point(|&(o, _)| o < id.0);
                let to = owned.partition_point(|&(o, _)| o <= id.0);
                for &(_, l) in owned[from..to].iter().rev() {
                    stack.push((l, false));
                }
            }
            // A hidden subtree is walked only down to the owners of
            // layers.
            if hidden
                && !owned
                    .iter()
                    .any(|&(o, _)| self.ancestors(NodeId(o)).skip(1).any(|a| a == id))
            {
                continue;
            }
            for &c in self.host.children(id).iter().rev() {
                stack.push((c, hidden));
            }
        }
        if !skip.is_empty() {
            skip.sort_unstable();
            out.retain(|n| skip.binary_search(&n.0).is_err());
        }
        out
    }

    /// The scope Tab cycles in: the innermost active trap holding the
    /// focus, unless the top modal does not hold it; the top modal
    /// without one; else the window.
    pub(crate) fn tab_scope(&self) -> NodeId {
        if self.traps.stack.is_empty() {
            return crate::host::ROOT;
        }
        let top = self.traps.top_modal();
        let inner = self.focus.and_then(|f| self.traps_holding(f).next());
        match (inner, top) {
            (Some(t), Some(m)) if !self.in_scope(t, m) => m,
            (Some(t), _) => t,
            (None, Some(m)) => m,
            (None, None) => crate::host::ROOT,
        }
    }

    /// The active traps on `n`'s scope chain, innermost first.
    fn traps_holding(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.scope_chain(n).filter(|&c| self.traps.is_active(c))
    }

    /// Where trap `t` puts focus: its first `AUTO_FOCUS` node in Tab
    /// order, else its first focusable; nothing when the top modal does
    /// not hold it.
    pub(crate) fn trap_target(&self, t: NodeId) -> Option<NodeId> {
        if self.traps.top_modal().is_some_and(|m| !self.in_scope(t, m)) {
            return None;
        }
        let order = self.tab_order(t);
        order
            .iter()
            .copied()
            .find(|&n| self.host.interaction(n).auto_focus)
            .or_else(|| order.first().copied())
    }

    /// At the start of a transaction: the focus, and which traps hold
    /// it, for a focus the transaction removes.
    pub(crate) fn begin_traps(&mut self) {
        self.traps.focus_removed = false;
        self.traps.before = self
            .focus
            .and_then(|f| Some((f, self.host.node(f)?.generation)));
        let mut chain = std::mem::take(&mut self.traps.chain);
        chain.clear();
        if let Some(f) = self.focus
            && !self.traps.stack.is_empty()
        {
            chain.extend(self.traps_holding(f));
        }
        self.traps.chain = chain;
    }

    /// `node` leaves: a focus on it blurs now, while the event can still
    /// carry its generation.
    pub(crate) fn focus_leaves(&mut self, node: NodeId) {
        if self.focus != Some(node) {
            return;
        }
        if self.host.interaction(node).listeners & mask::FOCUS != 0 {
            self.pending_events.push(self.event(out_kind::BLUR, node));
        }
        self.focus = None;
        self.traps.focus_removed = true;
        self.a11y_stale = true;
        self.force_paint = true;
    }

    /// At the end of a transaction: deactivates and activates traps,
    /// then moves focus once, to the first of these that applies, each
    /// overriding the one before:
    ///
    /// 1. the outermost valid restore target of a trap deactivating;
    /// 2. the transaction's Focus command, if not blocked or hidden (one
    ///    the traps allowed already took effect);
    /// 3. a trap activating with `AUTO_FOCUS`, deepest first, when the
    ///    focus so far is outside it: its target;
    /// 4. a node mounting with `AUTO_FOCUS` into an active trap that the
    ///    focus so far is outside of;
    /// 5. O1: a focus removed, blocked or hidden goes to the innermost
    ///    active trap that held it and has a target, else nowhere.
    ///
    /// Then refreshes the gate.
    pub(crate) fn settle_traps(&mut self) {
        let request = self.traps.request.take();
        let mounted = std::mem::take(&mut self.traps.auto_focused);
        let focus_inert = self.traps.inert > 0 && self.focus.is_some_and(|f| self.blocked(f));
        if !self.traps.dirty
            && !self.traps.any_declared()
            && !self.traps.focus_removed
            && !focus_inert
            && request.is_none()
        {
            return;
        }
        self.traps.dirty = false;

        // Deactivations: a trap turned off, hidden, or its node gone.
        let declared = std::mem::take(&mut self.traps.declared);
        let mut stack = std::mem::take(&mut self.traps.stack);
        let mut gone = Vec::new();
        stack.retain_mut(|a| {
            let flags = declared.get(&a.id.0).copied().unwrap_or(0);
            let live = self
                .host
                .node(a.id)
                .is_some_and(|n| n.generation == a.generation);
            if live && flags & trap_flag::ACTIVE != 0 && !self.hidden(a.id) {
                a.flags = flags;
                true
            } else {
                gone.push(*a);
                false
            }
        });
        self.traps.stack = stack;
        // Outermost first: what the traps closing hand back.
        let restores: Vec<NodeId> = gone
            .iter()
            .filter(|a| a.flags & trap_flag::RESTORE_FOCUS != 0)
            .filter_map(|a| a.restore)
            .filter(|&(n, g)| self.host.node(n).is_some_and(|h| h.generation == g))
            .map(|(n, _)| n)
            .collect();

        // Activations, outer traps first. A trap activating in this
        // transaction restores what the closing ones hand back, else the
        // focus before it, whatever the transaction focused meanwhile.
        let mut fresh: Vec<(usize, NodeId)> = declared
            .iter()
            .filter(|&(&id, &f)| {
                let id = NodeId(id);
                f & trap_flag::ACTIVE != 0
                    && self.host.is_live(id)
                    && !self.traps.is_active(id)
                    && !self.hidden(id)
            })
            .map(|(&id, _)| (self.scope_chain(NodeId(id)).count(), NodeId(id)))
            .collect();
        fresh.sort();
        let before = restores
            .first()
            .and_then(|&n| Some((n, self.host.node(n)?.generation)))
            .or(self.traps.before);
        for &(_, id) in &fresh {
            // A trap activating inside another one leaves the restore to
            // it: deactivating the inner one alone keeps focus inside.
            let nested = fresh.iter().any(|&(_, o)| o != id && self.in_scope(id, o));
            // Below the active traps it holds, so an inner modal stays
            // on top when its outer one re-activates.
            let at = self
                .traps
                .stack
                .iter()
                .position(|a| self.in_scope(a.id, id))
                .unwrap_or(self.traps.stack.len());
            let active = Active {
                id,
                generation: self.host.node(id).map_or(0, |n| n.generation),
                flags: declared[&id.0],
                restore: if nested { None } else { before },
            };
            self.traps.stack.insert(at, active);
        }
        self.traps.declared = declared;

        // Where focus ends, per the list above.
        let mut target = self.focus;
        if let Some(&n) = restores.iter().find(|&&n| self.can_focus(n)) {
            target = Some(n);
        }
        if let Some(r) = request.filter(|&r| !self.blocked(r) && !self.hidden(r)) {
            target = Some(r);
        }
        // Deepest first: an outer trap then finds the focus inside.
        for &(_, id) in fresh.iter().rev() {
            if self.traps.declared[&id.0] & trap_flag::AUTO_FOCUS != 0
                && !target.is_some_and(|f| self.in_scope(f, id))
                && let Some(t) = self.trap_target(id)
            {
                target = Some(t);
            }
        }
        for n in mounted {
            if self.can_focus(n)
                && let Some(t) = self.traps_holding(n).next()
                && !target.is_some_and(|f| self.in_scope(f, t))
            {
                target = Some(n);
            }
        }
        let lost = target.is_none() && self.traps.focus_removed;
        if lost || target.is_some_and(|f| self.blocked(f) || self.hidden(f)) {
            let chain: Vec<NodeId> = match target {
                Some(f) => self.traps_holding(f).collect(),
                None => self.traps.chain.clone(),
            };
            target = chain
                .into_iter()
                .filter(|&t| self.traps.is_active(t))
                .find_map(|t| self.trap_target(t));
        }
        self.set_focus(target);

        self.refresh_gate();
    }

    /// Cancels presses on nodes that became inert or fell outside the
    /// top modal, as their removal would: the pointer press with its
    /// `PRESS` cancel, and the Space press.
    pub(crate) fn cancel_blocked_presses(&mut self) {
        if self.traps.inert == 0 && self.traps.stack.is_empty() {
            return;
        }
        let pressed = [self.pressed, self.press.map(|p| p.node), self.key_press];
        for n in pressed.into_iter().flatten() {
            if self.host.is_live(n) && self.blocked(n) {
                self.unpress(n);
            }
        }
    }

    /// Rebuilds the gate from the top modal; a change re-hovers.
    fn refresh_gate(&mut self) {
        let mut gate = std::mem::take(&mut self.traps.spare);
        gate.modal = self.traps.top_modal();
        gate.roots.clear();
        gate.path.clear();
        if let Some(m) = gate.modal {
            gate.roots.push(m.0);
            for (&l, &o) in &self.host.owners {
                let l = NodeId(l);
                if o != NIL && self.host.parent(l).is_nil() && self.in_scope(l, m) {
                    gate.roots.push(l.0);
                }
            }
            for &r in &gate.roots {
                let mut p = self.host.parent(NodeId(r));
                while p.is_node() {
                    gate.path.push(p.0);
                    p = self.host.parent(p);
                }
            }
            gate.roots.sort_unstable();
            gate.path.sort_unstable();
            gate.path.dedup();
        }
        if gate != self.traps.gate {
            std::mem::swap(&mut gate, &mut self.traps.gate);
            self.hover_stale = true;
            self.a11y_stale = true;
        }
        self.traps.spare = gate;
    }

    /// Records a node's `INERT` flag; returns whether it changed.
    pub(crate) fn set_inert(&mut self, id: NodeId, inert: bool) -> bool {
        let Some(node) = self.host.node_mut(id) else {
            return false;
        };
        if node.flags.contains(NodeFlags::INERT) == inert {
            return false;
        }
        if inert {
            node.flags.set(NodeFlags::INERT);
            self.traps.inert += 1;
        } else {
            node.flags.clear(NodeFlags::INERT);
            self.traps.inert -= 1;
        }
        self.traps.dirty = true;
        // The node under the pointer may be another now.
        self.hover_stale = true;
        true
    }
}
