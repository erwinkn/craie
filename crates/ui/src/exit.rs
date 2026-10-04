//! Exits (topic 7, work item 6): a removed node animates out, then goes.
//!
//! An exit is declared ahead (`op::ANIMATION`, trigger exit) and starts
//! when its node is detached. For the app the node is gone; natively it
//! stays in its parent's child list, laid out and drawn, but inert: no
//! hit testing, focus or accessibility, a focus inside moves on as for a
//! removal (O1), and traps and groups inside stop counting. Only the
//! detached root's exit runs: its descendants are not detached. When its
//! animations' active phases are over, native frees the subtree and
//! sends one `EXIT_END` (node = the root); JS reuses the subtree's ids
//! from then on.
//!
//! ```text
//! [A, R, B]  detach R (exit: opacity 0 and height 0 over 200 ms)
//! frames     R fades and collapses; B moves up as R's height falls
//! 200 ms     R and its subtree freed; EXIT_END (R, finished)
//! ```
//!
//! An exit ends early and frees at once when JS cuts it short
//! (`removed`): a `REMOVE` of its root, or an `END_EXIT`, which does
//! nothing once the exit has ended (JS may send it before it sees the
//! `EXIT_END`). It also ends when an ancestor leaves the tree, detached
//! or removed (`parent gone`, at the end of that transaction). One that
//! cannot run, its root out of the tree or a list's row, ends at the end
//! of its transaction (`skipped`); so does one hidden (`display: none`
//! on it or above), at the detach or by a later transaction, frames or
//! not (a minimized window draws none). A variant that hides it between
//! transactions (hover, focus) ends it on the next frame. Structure
//! alone decides at the detach, so validation can tell whether the node
//! keeps its place until the transaction ends. An exiting node never
//! comes back: validation rejects placing it, or anything under it.
//! While an exit runs, events of its subtree's nodes are dropped.

use crate::animation::end_reason;
use crate::events::{UiEvent, out_kind};
use crate::host::NodeId;
use crate::mutation::NodeKind;
use crate::ui::Ui;

impl Ui {
    /// Applies a `DETACH`: a node with a declared exit starts it (or
    /// ends it at the end of the transaction, when it cannot run); any
    /// other unlinks.
    pub(crate) fn detach_node(&mut self, node: NodeId) {
        let exit = self.host.exits.remove(&node.0);
        // An exit starts from what shows, before the hover leaves.
        let from = exit.is_some().then(|| self.row_sample(node));
        self.unhover(node);
        self.unpress(node);
        let (Some(anims), Some(from)) = (exit, from) else {
            self.host.detach(node);
            return;
        };
        // Structure alone decides, as validation models it: a hidden
        // exit starts, and ends with the transaction (`end_hidden_exits`).
        let parent = self.host.parent(node);
        if !self.attached(node) || self.host.kind(parent) == Some(NodeKind::List) {
            self.host.detach(node);
            self.skipped_exits.insert(node);
            return;
        }
        self.host.exiting.insert(node);
        self.set_inert(node, true);
        self.start_exit_keyframes(node, anims, from);
    }

    /// Whether `node` is live and in the tree: its ancestors reach the
    /// root level, not a detached node.
    fn attached(&self, node: NodeId) -> bool {
        if !self.host.is_live(node) {
            return false;
        }
        let mut cur = node;
        loop {
            let p = self.host.parent(cur);
            if p.is_nil() {
                return true;
            }
            if !p.is_node() {
                return false;
            }
            cur = p;
        }
    }

    /// Whether `node` is an exit's root.
    pub(crate) fn exiting(&self, node: NodeId) -> bool {
        self.host.exiting.contains(&node)
    }

    /// Applies a `REMOVE` or `END_EXIT` of an exit's root, running or
    /// about to be skipped: the exit ends now (`removed`). False when
    /// `root` roots no exit.
    pub(crate) fn cut_exit(&mut self, root: NodeId) -> bool {
        #[cfg(test)]
        EXIT_WORK.with(|w| w.set(w.get() + 1));
        let running = self.host.exiting.remove(&root);
        if !(running || self.skipped_exits.remove(&root)) {
            return false;
        }
        self.free_exit(root, end_reason::REMOVED);
        true
    }

    /// At the end of a transaction: exits that could not run end, and so
    /// do those whose parent left the tree.
    pub(crate) fn settle_exits(&mut self) {
        while let Some(root) = self.skipped_exits.pop_first() {
            self.free_exit(root, end_reason::SKIPPED);
        }
        self.end_exits_where(end_reason::PARENT_GONE, |ui, root| !ui.attached(root));
    }

    /// After a transaction's styles, and each frame's: exits under a
    /// node hidden (`display: none`) end, skipped. Hidden, one would
    /// park with its ids held, so it must not wait for a frame: a
    /// minimized window draws none.
    pub(crate) fn end_hidden_exits(&mut self) {
        if !self.host.exiting.is_empty() {
            self.end_exits_where(end_reason::SKIPPED, |ui, root| !ui.drawn(root));
        }
    }

    /// After a frame's keyframes: exits under a node a variant hid since
    /// end, and so do those whose animations are over.
    pub(crate) fn finish_exits(&mut self) {
        self.end_hidden_exits();
        self.end_exits_where(end_reason::FINISHED, |ui, root| !ui.exit_running(root));
    }

    fn end_exits_where(&mut self, reason: u32, over: impl Fn(&Ui, NodeId) -> bool) {
        if self.host.exiting.is_empty() {
            return;
        }
        let ending: Vec<NodeId> = (self.host.exiting.iter())
            .copied()
            .filter(|&root| over(self, root))
            .collect();
        for root in ending {
            // Not when it went with an exit around it, freed before it.
            if self.host.exiting.remove(&root) {
                self.free_exit(root, reason);
            }
        }
    }

    /// Frees an exit's subtree and reports its end with `reason`; exits
    /// inside end with it (`parent gone`). Nothing else of the subtree is
    /// reported: its tweens' and animations' ends, a blur, are dropped.
    fn free_exit(&mut self, root: NodeId, reason: u32) {
        let mut nodes = std::mem::take(&mut self.node_scratch);
        nodes.clear();
        nodes.push(root);
        let mut k = 0;
        while k < nodes.len() {
            let n = nodes[k];
            nodes.extend_from_slice(self.host.children(n));
            k += 1;
        }
        #[cfg(test)]
        EXIT_WORK.with(|w| w.set(w.get() + nodes.len()));
        // Exits inside go with this one.
        let mut ends = std::mem::take(&mut self.exit_scratch);
        ends.clear();
        ends.push((root, reason));
        if !self.host.exiting.is_empty() {
            for &n in &nodes[1..] {
                if self.host.exiting.remove(&n) {
                    ends.push((n, end_reason::PARENT_GONE));
                }
            }
        }
        for &n in &nodes {
            let generation = self.host.node(n).map_or(0, |h| h.generation);
            self.freed.push((n.0, generation));
            self.remove_node(n);
        }
        // Their queued events go when events are taken
        // (`drop_exiting_events`): one pass for a batch of cuts.
        for &(n, reason) in &ends {
            let mut e = UiEvent::new(out_kind::EXIT_END, n.0);
            e.generation = self.host.slot_generation(n).wrapping_sub(1);
            e.key = reason;
            self.pending_events.push(e);
        }
        self.node_scratch = nodes;
        self.exit_scratch = ends;
    }

    /// Drops queued events of nodes exits freed (raised before, by the
    /// occupant that went; not their `EXIT_END`s) and of nodes inside a
    /// running exit.
    pub(crate) fn drop_exiting_events(&mut self) {
        if !self.freed.is_empty() {
            let mut freed = std::mem::take(&mut self.freed);
            freed.sort_unstable();
            #[cfg(test)]
            EXIT_WORK.with(|w| w.set(w.get() + self.pending_events.len()));
            self.pending_events.retain(|e| {
                e.kind == out_kind::EXIT_END
                    || freed.binary_search(&(e.node, e.generation)).is_err()
            });
            freed.clear();
            self.freed = freed;
        }
        if self.host.exiting.is_empty() {
            return;
        }
        let mut events = std::mem::take(&mut self.pending_events);
        events.retain(|e| {
            let n = NodeId(e.node);
            !(self
                .host
                .node(n)
                .is_some_and(|h| h.generation == e.generation)
                && self.ancestors(n).any(|a| self.exiting(a)))
        });
        self.pending_events = events;
    }
}

#[cfg(test)]
thread_local! {
    /// Work done ending exits, per element visited: cuts, freed nodes,
    /// queued events filtered (the bulk test's budget).
    pub(crate) static EXIT_WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
