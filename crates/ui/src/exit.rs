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
//! An exit ends early and frees at once when its root is removed
//! (`removed`, JS cutting it short) or an ancestor leaves the tree,
//! detached or removed (`parent gone`, at the end of that transaction).
//! One that cannot run, its root out of the tree, hidden (`display:
//! none` on it or above) or a list's row, ends at the end of its
//! transaction (`skipped`), and so does one hidden while it runs, on the
//! next frame. An exiting node never comes
//! back: validation rejects placing it, or anything under it. While an
//! exit runs, events of its subtree's nodes are dropped.

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
        self.unhover(node);
        self.unpress(node);
        let Some(anims) = self.host.exits.remove(&node.0) else {
            self.host.detach(node);
            return;
        };
        let parent = self.host.parent(node);
        if !self.drawn(node) || self.host.kind(parent) == Some(NodeKind::List) {
            self.host.detach(node);
            self.skipped_exits.push(node);
            return;
        }
        self.host.exiting.push(node);
        self.set_inert(node, true);
        self.start_exit_keyframes(node, anims);
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

    /// Applies a `REMOVE` of an exit's root: the exit ends now.
    pub(crate) fn cut_exit(&mut self, root: NodeId) {
        self.host.exiting.retain(|&n| n != root);
        self.free_exit(root, end_reason::REMOVED);
    }

    /// At the end of a transaction: exits that could not run end, and so
    /// do those whose parent left the tree.
    pub(crate) fn settle_exits(&mut self) {
        while let Some(root) = self.skipped_exits.pop() {
            self.free_exit(root, end_reason::SKIPPED);
        }
        self.end_exits_where(end_reason::PARENT_GONE, |ui, root| !ui.attached(root));
    }

    /// After a frame's keyframes: exits under a node hidden since
    /// (`display: none`) end, and so do those whose animations are over.
    pub(crate) fn finish_exits(&mut self) {
        if self.host.exiting.is_empty() {
            return;
        }
        self.end_exits_where(end_reason::SKIPPED, |ui, root| !ui.drawn(root));
        self.end_exits_where(end_reason::FINISHED, |ui, root| !ui.exit_running(root));
    }

    fn end_exits_where(&mut self, reason: u32, over: impl Fn(&Ui, NodeId) -> bool) {
        // Freeing one may end others (inside it): look again from the
        // start. Exits are few.
        while let Some(i) = (0..self.host.exiting.len()).find(|&i| over(self, self.host.exiting[i]))
        {
            let root = self.host.exiting.remove(i);
            self.free_exit(root, reason);
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
        // Exits inside go with this one.
        let mut ends = std::mem::take(&mut self.exit_scratch);
        ends.clear();
        ends.push((root, reason));
        if !self.host.exiting.is_empty() {
            self.host.exiting.retain(|&n| {
                let inside = nodes.contains(&n);
                if inside {
                    ends.push((n, end_reason::PARENT_GONE));
                }
                !inside
            });
        }
        for &n in &nodes {
            self.remove_node(n);
        }
        // Events of the freed occupants (their slots moved one generation
        // on) are dropped.
        nodes.sort_unstable_by_key(|n| n.0);
        let host = &self.host;
        let freed = |n: NodeId| host.slot_generation(n).wrapping_sub(1);
        self.pending_events.retain(|e| {
            let n = NodeId(e.node);
            nodes.binary_search_by_key(&n.0, |m| m.0).is_err() || e.generation != freed(n)
        });
        for &(n, reason) in &ends {
            let mut e = UiEvent::new(out_kind::EXIT_END, n.0);
            e.generation = freed(n);
            e.key = reason;
            self.pending_events.push(e);
        }
        self.node_scratch = nodes;
        self.exit_scratch = ends;
    }

    /// Drops queued events of nodes inside a running exit.
    pub(crate) fn drop_exiting_events(&mut self) {
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
