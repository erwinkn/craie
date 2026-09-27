//! What assistive technology reads from app states: check roles report
//! `checked` as toggled, `expanded` appears where the prop was given,
//! `selected` where it was given on a selectable role, and the tree
//! built after a state change carries it.
//!
//! Every transaction marks the tree stale (`Ui::apply_txn`), so the
//! tree after a state change is what the host publishes next. The
//! semantic revision a state bit bumps is bookkeeping, kept alike for
//! labels, roles and every reported state.

use accesskit::{Action, Node, Role, Toggled, TreeUpdate};

use crate::a11y::aid;
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{NIL, NodeKind, Role as UiRole, Transaction, press, reported};
use crate::states::state_bit::{CHECKED, DISABLED, EXPANDED, SELECTED};
use crate::ui::Ui;
use crate::wire;

const VIEW: Size = Size {
    width: 200.0,
    height: 200.0,
};

/// A root holding one pressable scope per `(role, reported, bits)`:
/// nodes 1, 2, …
fn ui_with(nodes: &[(UiRole, u8, u64)]) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).place(NIL, 0, NIL);
    for (i, &(role, rep, bits)) in nodes.iter().enumerate() {
        let id = i as u32 + 1;
        t.create(id, NodeKind::View)
            .place(0, id, NIL)
            .role_reporting(id, role, rep)
            .interaction_press(id, 0, true, press::PRESSABLE)
            .states(id, bits);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'_>)) {
    let mut t = Transaction::new(2);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

/// Sets scope `id`'s bits; true when the semantic revision moved.
fn set_bits(ui: &mut Ui, id: u32, bits: u64) -> bool {
    let before = ui.host.revs.semantic;
    apply(ui, |t| {
        t.states(id, bits);
    });
    ui.host.revs.semantic > before
}

fn node(tree: &TreeUpdate, id: u32) -> Node {
    tree.nodes
        .iter()
        .find(|(n, _)| *n == aid(NodeId(id)))
        .map(|(_, n)| n.clone())
        .unwrap()
}

/// `<Pressable accessibilityRole="checkbox" checked={on}>`: a checkbox
/// that clicks, and whose toggled state follows `checked` through a
/// toggle and back.
#[test]
fn checkbox_toggled_follows_checked() {
    let mut ui = ui_with(&[(UiRole::CheckBox, 0, 0)]);
    let n = node(&ui.a11y_tree(VIEW), 1);
    assert_eq!(n.role(), Role::CheckBox);
    assert!(n.supports_action(Action::Click));
    assert_eq!(n.toggled(), Some(Toggled::False));

    ui.take_a11y_stale();
    assert!(set_bits(&mut ui, 1, CHECKED));
    assert!(ui.take_a11y_stale());
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).toggled(), Some(Toggled::True));

    assert!(set_bits(&mut ui, 1, 0));
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).toggled(), Some(Toggled::False));

    assert!(set_bits(&mut ui, 1, DISABLED));
    assert!(node(&ui.a11y_tree(VIEW), 1).is_disabled());
}

/// Switch and radio are check roles too; a radio group is a container.
#[test]
fn switch_and_radio_toggle_and_click() {
    let ui = ui_with(&[
        (UiRole::Switch, 0, 0),
        (UiRole::RadioButton, 0, CHECKED),
        (UiRole::RadioGroup, 0, 0),
    ]);
    let tree = ui.a11y_tree(VIEW);
    let (switch, radio, group) = (node(&tree, 1), node(&tree, 2), node(&tree, 3));
    assert_eq!(switch.role(), Role::Switch);
    assert_eq!(switch.toggled(), Some(Toggled::False));
    assert!(switch.supports_action(Action::Click));
    assert_eq!(radio.role(), Role::RadioButton);
    assert_eq!(radio.toggled(), Some(Toggled::True));
    assert!(radio.supports_action(Action::Click));
    assert_eq!(group.role(), Role::RadioGroup);
    assert_eq!(group.toggled(), None);
}

/// A menu trigger given `expanded` is collapsed, then expanded; a list
/// row given `selected` has its selection.
#[test]
fn expanded_and_selected_where_given() {
    let mut ui = ui_with(&[
        (UiRole::Button, reported::EXPANDED, 0),
        (UiRole::ListItem, reported::SELECTED, SELECTED),
        (UiRole::ListItem, reported::SELECTED, 0),
    ]);
    let tree = ui.a11y_tree(VIEW);
    assert_eq!(node(&tree, 1).is_expanded(), Some(false));
    assert_eq!(node(&tree, 1).is_selected(), None);
    assert_eq!(node(&tree, 2).is_selected(), Some(true));
    assert_eq!(node(&tree, 3).is_selected(), Some(false));

    assert!(set_bits(&mut ui, 1, EXPANDED));
    assert!(set_bits(&mut ui, 3, SELECTED));
    let tree = ui.a11y_tree(VIEW);
    assert_eq!(node(&tree, 1).is_expanded(), Some(true));
    assert_eq!(node(&tree, 3).is_selected(), Some(true));
}

/// The kit styles check roles with `selected`: a selected checkbox or
/// radio is checked or not, never "selected".
#[test]
fn selected_check_roles_report_checked_only() {
    let ui = ui_with(&[
        (UiRole::CheckBox, reported::SELECTED, SELECTED | CHECKED),
        (UiRole::RadioButton, reported::SELECTED, SELECTED),
    ]);
    let tree = ui.a11y_tree(VIEW);
    assert_eq!(node(&tree, 1).is_selected(), None);
    assert_eq!(node(&tree, 1).toggled(), Some(Toggled::True));
    assert_eq!(node(&tree, 2).is_selected(), None);
    assert_eq!(node(&tree, 2).toggled(), Some(Toggled::False));
}

/// A plain button is neither collapsed, unselected, nor unchecked; a
/// button's `checked` stays a styling state.
#[test]
fn plain_button_reports_no_states() {
    let ui = ui_with(&[(UiRole::Button, 0, 0), (UiRole::Button, 0, CHECKED)]);
    let tree = ui.a11y_tree(VIEW);
    for id in [1, 2] {
        let n = node(&tree, id);
        assert_eq!(n.role(), Role::Button);
        assert_eq!(n.is_expanded(), None);
        assert_eq!(n.is_selected(), None);
        assert_eq!(n.toggled(), None);
    }
}

/// Click is offered only where it does something, on an enabled
/// pressable: not on a disabled one, nor on a `View
/// accessibilityRole="checkbox"` that only listens for pointers.
#[test]
fn click_only_on_enabled_pressables() {
    let mut ui = ui_with(&[(UiRole::Button, 0, 0), (UiRole::CheckBox, 0, 0)]);
    apply(&mut ui, |t| {
        t.interaction_press(1, 0, false, press::PRESSABLE | press::DISABLED);
        t.interaction(2, crate::events::mask::POINTER_UP, true);
    });
    let tree = ui.a11y_tree(VIEW);
    let (button, check) = (node(&tree, 1), node(&tree, 2));
    assert!(!button.supports_action(Action::Click));
    assert!(!check.supports_action(Action::Click));
    assert_eq!(check.toggled(), Some(Toggled::False), "still a checkbox");
}

/// A ROLE op that changes only the reported states applies: a button
/// that gains `expanded` becomes collapsed, and loses it again.
#[test]
fn reported_alone_updates() {
    let mut ui = ui_with(&[(UiRole::Button, 0, 0)]);
    apply(&mut ui, |t| {
        t.role_reporting(1, UiRole::Button, reported::EXPANDED);
    });
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).is_expanded(), Some(false));
    apply(&mut ui, |t| {
        t.role_reporting(1, UiRole::Button, 0);
    });
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).is_expanded(), None);
}

/// The role op carries its reported states; unknown bits and roles past
/// the last are rejected.
#[test]
fn role_op_round_trips() {
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .role_reporting(0, UiRole::RadioGroup, reported::ALL);
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);

    let mut bad = buf.clone();
    *bad.last_mut().unwrap() = 1 << 2;
    assert!(wire::decode(&bad).is_err(), "unknown reported bit");
    let mut bad = buf;
    let at = bad.len() - 2;
    bad[at] = UiRole::AlertDialog as u8 + 1;
    assert!(wire::decode(&bad).is_err(), "unknown role");
}
