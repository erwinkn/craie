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
    Action, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId as A11yId,
    Rect as A11yRect, Role, Toggled, TreeId, TreeInfo, TreeUpdate,
};

use crate::geom::Size;
use crate::host::{NodeFlags, NodeId, ROOT};
use crate::mutation::{NIL, NodeKind, Role as UiRole, reported};
use crate::states::state_bit;
use crate::trap::Class;
use crate::ui::Ui;

/// The AccessKit role for a Craie role.
fn ak_role(role: UiRole) -> Role {
    match role {
        UiRole::None => Role::GenericContainer,
        UiRole::Button => Role::Button,
        UiRole::Label => Role::Label,
        UiRole::TextInput => Role::TextInput,
        UiRole::MultilineTextInput => Role::MultilineTextInput,
        UiRole::ScrollView => Role::ScrollView,
        UiRole::Image => Role::Image,
        UiRole::Heading => Role::Heading,
        UiRole::Link => Role::Link,
        UiRole::CheckBox => Role::CheckBox,
        UiRole::Slider => Role::Slider,
        UiRole::List => Role::List,
        UiRole::ListItem => Role::ListItem,
        UiRole::Group => Role::Group,
        UiRole::Switch => Role::Switch,
        UiRole::RadioButton => Role::RadioButton,
        UiRole::RadioGroup => Role::RadioGroup,
        UiRole::Dialog => Role::Dialog,
        UiRole::AlertDialog => Role::AlertDialog,
    }
}

/// Whether assistive technology reads `selected` on `role`: of Marbre
/// web's selectable roles (tab, option, row, gridcell, treeitem, and
/// the headers), Craie has the list row, which stands in for option and
/// row. Add the others as they become roles.
fn selectable(role: UiRole) -> bool {
    role == UiRole::ListItem
}

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
        let gated = self.traps.gate.modal.is_some();
        for &child in self.host.children(ROOT) {
            if let Some(n) = self.a11y_child(child, &mut nodes, gated) {
                kids.push(n);
            }
        }
        root.set_children(kids);
        nodes.push((ROOT_AID, root));
        for m in self
            .traps
            .stack
            .iter()
            .filter(|a| self.traps.is_modal(a.id))
        {
            let m = aid(self.modal_node(m.id));
            if let Some((_, n)) = nodes.iter_mut().find(|(a, _)| *a == m) {
                n.set_modal();
            }
        }
        let focus = self.focused().map(aid).unwrap_or(ROOT_AID);
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT_AID)),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    /// The node announced as modal for modal trap `t`: the first
    /// `dialog` or `alertdialog` in its subtree (ARIA pairs `aria-modal`
    /// with the dialog), else the trap.
    fn modal_node(&self, t: NodeId) -> NodeId {
        let mut stack = vec![t];
        while let Some(n) = stack.pop() {
            if matches!(
                self.host.interaction(n).role,
                UiRole::Dialog | UiRole::AlertDialog
            ) {
                return n;
            }
            stack.extend(self.host.children(n).iter().rev());
        }
        t
    }

    /// `a11y_node` under a gated parent (`trap.rs`): outside the top
    /// modal a node leaves the tree, and one holding the modal (or a
    /// layer it owns) stays as a bare container.
    fn a11y_child(&self, id: NodeId, out: &mut Vec<(A11yId, Node)>, gated: bool) -> Option<A11yId> {
        if !gated {
            return self.a11y_node(id, out);
        }
        match self.traps.gate.class(id) {
            Class::Root => self.a11y_node(id, out),
            Class::Out => None,
            // Never hidden or inert: a trap under such a node is
            // inactive.
            Class::Path => {
                let mut an = Node::new(Role::GenericContainer);
                let kids: Vec<A11yId> = self
                    .host
                    .children(id)
                    .iter()
                    .filter_map(|&c| self.a11y_child(c, out, true))
                    .collect();
                if kids.is_empty() {
                    return None;
                }
                an.set_children(kids);
                let a = aid(id);
                out.push((a, an));
                Some(a)
            }
        }
    }

    /// One retained node -> one semantic node (plus recursed children).
    /// Returns the node's a11y id, or `None` when the subtree is hidden
    /// or inert.
    ///
    /// The role comes from the node's role field only; the facade sets
    /// defaults (Pressable, TextInput, ScrollView, Text). Content (text,
    /// input value) and behavior (scroll actions) come from the node;
    /// states (checked, expanded, selected, disabled) from its scope bits.
    fn a11y_node(&self, id: NodeId, out: &mut Vec<(A11yId, Node)>) -> Option<A11yId> {
        let node = self.host.node(id)?;
        let style = self.host.style(id);
        if style.display() == taffy::Display::None || node.flags.contains(NodeFlags::INERT) {
            return None;
        }
        let props = self.host.interaction(id);
        let kind = node.kind;

        let overflow = style.overflow();
        let scroll_x = overflow.x == taffy::Overflow::Scroll;
        let scroll_y = overflow.y == taffy::Overflow::Scroll;

        let mut an = Node::new(ak_role(props.role));
        let bits = self.state_bits(id);
        // A check role is always checked or not: a clear bit (or no
        // `checked` prop) reads unchecked.
        if matches!(
            props.role,
            UiRole::CheckBox | UiRole::Switch | UiRole::RadioButton
        ) {
            an.set_toggled(Toggled::from(bits & state_bit::CHECKED != 0));
        }
        // Click only where it does something: on an enabled pressable,
        // whatever its role (a row, a tab). A button-role View with no
        // onPress offers none.
        if self.enabled_pressable(id) {
            an.add_action(Action::Click);
        }
        // Expanded and selected only where the prop was given: a plain
        // button is neither collapsed nor unselected. Selected only on a
        // selectable role too (Marbre web's `aria-selected` rule): the
        // kit styles checkboxes and radios with `selected`, which must
        // not read "checked, selected".
        if props.reported & reported::EXPANDED != 0 {
            an.set_expanded(bits & state_bit::EXPANDED != 0);
        }
        if props.reported & reported::SELECTED != 0 && selectable(props.role) {
            an.set_selected(bits & state_bit::SELECTED != 0);
        }
        if let Some(p) = self.host.paragraph(id) {
            an.set_value(p.text.clone());
        }
        if kind == NodeKind::Input {
            let state = self.inputs.get(id.0);
            an.set_value(self.inputs.text(id.0));
            if let Some(s) = state
                && !s.placeholder.is_empty()
            {
                an.set_placeholder(s.placeholder.clone());
            }
            an.add_action(Action::Focus);
            an.add_action(Action::Blur);
            an.add_action(Action::ReplaceSelectedText);
        }
        if scroll_x {
            an.add_action(Action::ScrollLeft);
            an.add_action(Action::ScrollRight);
        }
        if scroll_y {
            an.add_action(Action::ScrollUp);
            an.add_action(Action::ScrollDown);
        }

        if props.focusable && kind != NodeKind::Input {
            an.add_action(Action::Focus);
        }
        if let Some(label) = self.host.label(id) {
            an.set_label(label.to_string());
        }
        if bits & state_bit::DISABLED != 0 {
            an.set_disabled();
        }
        let r = self.abs_rect(id);
        an.set_bounds(A11yRect {
            x0: r.origin.x as f64,
            y0: r.origin.y as f64,
            x1: (r.origin.x + r.size.width) as f64,
            y1: (r.origin.y + r.size.height) as f64,
        });
        if overflow.x != taffy::Overflow::Visible || overflow.y != taffy::Overflow::Visible {
            an.set_clips_children();
        }
        // A list row reports its place among all items, rendered or not.
        let parent = self.host.parent(id);
        let index = self.host.list_index[id.index()];
        if index != NIL
            && let Some(l) = self.host.lists.get(parent.0)
            && index < l.len()
        {
            an.set_position_in_set(index as usize + 1);
            an.set_size_of_set(l.len() as usize);
        }
        let mut kids: Vec<A11yId> = Vec::new();
        let mut children: Vec<NodeId> = self.host.children(id).to_vec();
        if kind == NodeKind::List {
            // Only the rows layout shows (an index in range, not a
            // duplicate), in item order whatever order they were placed in.
            children.retain(|&c| self.host.list_row_shown(id, c));
            children.sort_by_key(|c| self.host.list_index[c.index()]);
        }
        for child in children {
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
