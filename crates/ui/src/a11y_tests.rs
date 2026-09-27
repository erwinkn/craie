//! What assistive technology reads from app states: check roles report
//! `checked` as toggled, `expanded` and `selected` appear where the prop
//! was given, and a state change reaches the next tree update.

use accesskit::{Action, Node, Role, Toggled, TreeUpdate};

use crate::a11y::aid;
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{NIL, NodeKind, Role as UiRole, Transaction, reported};
use crate::states::state_bit::{CHECKED, EXPANDED, SELECTED};
use crate::ui::Ui;
use crate::wire;

const VIEW: Size = Size {
    width: 200.0,
    height: 200.0,
};

/// A root holding one scope per `(role, reported, bits)`: nodes 1, 2, …
fn ui_with(nodes: &[(UiRole, u8, u64)]) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).place(NIL, 0, NIL);
    for (i, &(role, rep, bits)) in nodes.iter().enumerate() {
        let id = i as u32 + 1;
        t.create(id, NodeKind::View)
            .place(0, id, NIL)
            .role_reporting(id, role, rep)
            .states(id, bits);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui
}

fn set_bits(ui: &mut Ui, id: u32, bits: u64) {
    let mut t = Transaction::new(2);
    t.states(id, bits);
    ui.apply_txn(&t).unwrap();
}

fn node(tree: &TreeUpdate, id: u32) -> Node {
    tree.nodes
        .iter()
        .find(|(n, _)| *n == aid(NodeId(id)))
        .map(|(_, n)| n.clone())
        .unwrap()
}

/// `<Pressable accessibilityRole="checkbox" checked={on}>`: a checkbox
/// that clicks, and whose toggled state follows `checked` through the
/// update a toggle publishes.
#[test]
fn checkbox_toggled_follows_checked() {
    let mut ui = ui_with(&[(UiRole::CheckBox, 0, 0)]);
    assert!(ui.take_a11y_stale());
    let n = node(&ui.a11y_tree(VIEW), 1);
    assert_eq!(n.role(), Role::CheckBox);
    assert!(n.supports_action(Action::Click));
    assert_eq!(n.toggled(), Some(Toggled::False));

    let before = ui.host.revs.semantic;
    set_bits(&mut ui, 1, CHECKED);
    assert!(ui.host.revs.semantic > before);
    assert!(ui.host.dirty.semantic.contains(1));
    assert!(ui.take_a11y_stale(), "a toggle publishes an update");
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).toggled(), Some(Toggled::True));

    set_bits(&mut ui, 1, 0);
    assert!(ui.take_a11y_stale());
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).toggled(), Some(Toggled::False));
}

/// Switch and radio are check roles too.
#[test]
fn switch_and_radio_toggle_and_click() {
    let ui = ui_with(&[(UiRole::Switch, 0, 0), (UiRole::RadioButton, 0, CHECKED)]);
    let tree = ui.a11y_tree(VIEW);
    let (switch, radio) = (node(&tree, 1), node(&tree, 2));
    assert_eq!(switch.role(), Role::Switch);
    assert_eq!(switch.toggled(), Some(Toggled::False));
    assert!(switch.supports_action(Action::Click));
    assert_eq!(radio.role(), Role::RadioButton);
    assert_eq!(radio.toggled(), Some(Toggled::True));
    assert!(radio.supports_action(Action::Click));
}

/// A menu trigger given `expanded` reads collapsed, then expanded; a
/// list row given `selected` reads its selection.
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

    ui.take_a11y_stale();
    set_bits(&mut ui, 1, EXPANDED);
    assert!(ui.take_a11y_stale());
    assert_eq!(node(&ui.a11y_tree(VIEW), 1).is_expanded(), Some(true));
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

/// The role op carries its reported states; unknown bits and roles past
/// the last are rejected.
#[test]
fn role_op_round_trips() {
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .role_reporting(0, UiRole::RadioButton, reported::ALL);
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);

    let mut bad = buf.clone();
    *bad.last_mut().unwrap() = 1 << 2;
    assert!(wire::decode(&bad).is_err(), "unknown reported bit");
    let mut bad = buf;
    let at = bad.len() - 2;
    bad[at] = UiRole::RadioButton as u8 + 1;
    assert!(wire::decode(&bad).is_err(), "unknown role");
}
