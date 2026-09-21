//! Accessibility: an AccessKit semantic tree projected from the retained
//! UI, plus the action channel back in.
//!
//! The tree is rebuilt wholesale when structure, content, focus, or
//! scroll state changes — trees are small and `TreeUpdate` semantics
//! (each node overwrites) make a full snapshot a legal update. Retained
//! node `n` maps to `NodeId(n + 1)`; `NodeId(0)` is the window root.
//!
//! The platform adapter's callbacks can arrive on non-UI threads, so
//! `A11yShared` is the handoff: the UI thread publishes the latest tree
//! (for assistive tech that attaches late) and drains requested actions.

use std::sync::{Arc, Mutex};

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node,
    NodeId as A11yId, Rect as A11yRect, Role, TreeId, TreeInfo, TreeUpdate,
};

use crate::events::mask;
use crate::geom::Size;
use crate::host::{NodeId, NodeKind, ROOT, StyleId};
use crate::ui::Ui;

/// The window root's accessibility id.
pub const ROOT_AID: A11yId = A11yId(0);

/// Retained node `id`'s accessibility id (`0` is the window root).
pub fn aid(id: NodeId) -> A11yId {
    A11yId::from(id.0 as u64 + 1)
}

/// Inverse of `aid` — `None` for the window root itself.
pub fn nid(id: A11yId) -> Option<NodeId> {
    let raw: u64 = id.0;
    raw.checked_sub(1).map(|v| NodeId(v as u32))
}

/// Shared state between the UI thread and the platform adapter.
pub struct A11yShared {
    /// The most recently built tree, served to assistive technology that
    /// attaches after the first update was already delivered.
    pub latest: Mutex<Option<TreeUpdate>>,
    /// Actions requested by assistive tech; drained on the UI thread.
    pub actions: Mutex<Vec<ActionRequest>>,
}

impl A11yShared {
    pub fn new() -> Arc<A11yShared> {
        Arc::new(A11yShared {
            latest: Mutex::new(None),
            actions: Mutex::new(Vec::new()),
        })
    }
}

impl Default for A11yShared {
    fn default() -> A11yShared {
        A11yShared {
            latest: Mutex::new(None),
            actions: Mutex::new(Vec::new()),
        }
    }
}

/// Serves the last published tree when assistive technology attaches.
pub struct Activation {
    pub shared: Arc<A11yShared>,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.shared.latest.lock().unwrap().clone()
    }
}

/// Queues action requests for the UI thread and pokes the event loop.
pub struct ActionSink {
    pub shared: Arc<A11yShared>,
    pub wake: crate::platform::Wake,
}

impl ActionHandler for ActionSink {
    fn do_action(&mut self, request: ActionRequest) {
        self.shared.actions.lock().unwrap().push(request);
        self.wake.wake();
    }
}

/// Accessibility detached; nothing to release — the adapter owns state.
pub struct Deactivation;

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {}
}

impl Ui {
    /// Builds a full `TreeUpdate` snapshot in logical coordinates.
    /// `viewport` is the window's logical size (the root's bounds).
    pub fn a11y_tree(&self, viewport: Size) -> TreeUpdate {
        let mut nodes: Vec<(A11yId, Node)> = Vec::new();
        let mut root = Node::new(Role::Window);
        root.set_bounds(A11yRect {
            x0: 0.0,
            y0: 0.0,
            x1: viewport.width as f64,
            y1: viewport.height as f64,
        });
        let mut kids: Vec<A11yId> = Vec::new();
        for &child in self.host.children(ROOT) {
            if let Some(n) = self.a11y_node(child, &mut nodes) {
                kids.push(n);
            }
        }
        root.set_children(kids);
        nodes.push((ROOT_AID, root));
        let focus = self.focused().map(aid).unwrap_or(ROOT_AID);
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT_AID)),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    /// One retained node -> one semantic node (plus recursed children).
    /// Returns the node's a11y id, or `None` when the subtree is hidden.
    fn a11y_node(&self, id: NodeId, out: &mut Vec<(A11yId, Node)>) -> Option<A11yId> {
        let node = self.host.node(id)?;
        if node.hidden() {
            return None;
        }
        let style = self.layouts.style(StyleId(node.style));
        if style.display == taffy::Display::None {
            return None;
        }
        let props = self.host.props(id);
        let kind = node.kind();

        let scroll_x = style.overflow.x == taffy::Overflow::Scroll;
        let scroll_y = style.overflow.y == taffy::Overflow::Scroll;
        let clickable = props.listeners & (mask::POINTER_DOWN | mask::POINTER_UP) != 0;

        let mut an = match kind {
            NodeKind::TEXT => {
                let mut n = Node::new(Role::Label);
                if let Some(t) = self.host.text(id) {
                    n.set_value(t.text.clone());
                }
                n
            }
            NodeKind::INPUT => {
                let state = self.inputs.get(id.0);
                let multiline = state.map(|s| s.multiline).unwrap_or(false);
                let mut n = Node::new(if multiline {
                    Role::MultilineTextInput
                } else {
                    Role::TextInput
                });
                n.set_value(self.inputs.text(id.0));
                if let Some(s) = state
                    && !s.placeholder.is_empty()
                {
                    n.set_placeholder(s.placeholder.clone());
                }
                n.add_action(Action::Focus);
                n.add_action(Action::Blur);
                n.add_action(Action::ReplaceSelectedText);
                n
            }
            _ => {
                let role = if scroll_x || scroll_y {
                    Role::ScrollView
                } else if clickable {
                    Role::Button
                } else {
                    Role::GenericContainer
                };
                let mut n = Node::new(role);
                if clickable {
                    n.add_action(Action::Click);
                }
                if scroll_x {
                    n.add_action(Action::ScrollLeft);
                    n.add_action(Action::ScrollRight);
                }
                if scroll_y {
                    n.add_action(Action::ScrollUp);
                    n.add_action(Action::ScrollDown);
                }
                n
            }
        };

        if props.focusable && kind != NodeKind::INPUT {
            an.add_action(Action::Focus);
        }
        if let Some(label) = self.a11y_label(id) {
            an.set_label(label.to_string());
        }
        let r = self.abs_rect(id);
        an.set_bounds(A11yRect {
            x0: r.origin.x as f64,
            y0: r.origin.y as f64,
            x1: (r.origin.x + r.size.width) as f64,
            y1: (r.origin.y + r.size.height) as f64,
        });
        if style.overflow.x != taffy::Overflow::Visible
            || style.overflow.y != taffy::Overflow::Visible
        {
            an.set_clips_children();
        }
        let mut kids: Vec<A11yId> = Vec::new();
        for &child in self.host.children(id) {
            if let Some(c) = self.a11y_node(child, out) {
                kids.push(c);
            }
        }
        if !kids.is_empty() {
            an.set_children(kids);
        }
        let a = aid(id);
        out.push((a, an));
        Some(a)
    }
}
