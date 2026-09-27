//! Focus groups through `Ui` (`group.rs`).

use accesskit::Role as AkRole;

use crate::a11y::aid;
use crate::claims::Claim;
use crate::events::{Event, Key, KeyInput, Mods, UiEvent, activate_source, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{
    Command, NodeKind, Role, Transaction, group_flag as g, interaction_flag as f, press, reported,
    trap_flag,
};
use crate::states::state_bit::{CHECKED, DISABLED, SELECTED};
use crate::ui::Ui;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 400.0,
};
const LISTEN: u32 = mask::FOCUS | mask::ACTIVATE;
const RADIOS: u8 = g::VERTICAL | g::LOOP | g::SELECT_ON_FOCUS;
const BOTH: u8 = g::HORIZONTAL | g::VERTICAL;

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'_>)) {
    let mut t = Transaction::new(2);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
}

fn view(t: &mut Transaction, parent: u32, id: u32) {
    t.create(id, NodeKind::View).append(parent, id);
}

/// A focusable pressable (a facade Pressable) of `role`.
fn item(t: &mut Transaction, parent: u32, id: u32, role: Role) {
    view(t, parent, id);
    t.interaction_press(id, LISTEN, true, press::PRESSABLE)
        .role(id, role);
}

/// A plain focusable button.
fn button(t: &mut Transaction, parent: u32, id: u32) {
    item(t, parent, id, Role::Button);
}

/// Root 0: button 1, group 10 (`role`, `flags`) holding `items` of
/// `item_role` (ids 11, 12, …), button 2.
fn app(role: Role, flags: u8, item_role: Role, items: usize) -> Ui {
    let mut ui = Ui::new(1.0);
    apply(&mut ui, |t| {
        view(t, NIL, 0);
        button(t, 0, 1);
        view(t, 0, 10);
        t.role(10, role).group(10, flags);
        for i in 0..items as u32 {
            item(t, 10, 11 + i, item_role);
        }
        button(t, 0, 2);
    });
    ui.take_events();
    ui
}

/// The example: Small 11, Medium 12, Large 13 in a vertical radiogroup
/// with `selectOnFocus`.
fn radios() -> Ui {
    app(Role::RadioGroup, RADIOS, Role::RadioButton, 3)
}

fn key(ui: &mut Ui, key: Key) -> Option<u32> {
    key_with(ui, key, Mods::default())
}

fn key_with(ui: &mut Ui, key: Key, mods: Mods) -> Option<u32> {
    ui.dispatch(&Event::KeyDown(KeyInput {
        key,
        mods,
        ..KeyInput::default()
    }));
    focused(ui)
}

fn tab(ui: &mut Ui, shift: bool) -> Option<u32> {
    let mods = Mods {
        shift,
        ..Mods::default()
    };
    key_with(ui, Key::Tab, mods)
}

fn focused(ui: &Ui) -> Option<u32> {
    ui.focused().map(|n| n.0)
}

fn focus(ui: &mut Ui, id: u32) {
    apply(ui, |t| {
        t.command(id, Command::Focus);
    });
}

/// The `ACTIVATE` events queued since the last call, as (node, source).
fn activations(ui: &mut Ui) -> Vec<(u32, u32)> {
    ui.take_events()
        .iter()
        .filter(|e: &&UiEvent| e.kind == out_kind::ACTIVATE)
        .map(|e| (e.node, (e.key >> 4) & 3))
        .collect()
}

const KEY: u32 = activate_source::KEY;

/// The group is one Tab stop, the checked member: Tab from before it
/// lands on Medium, and the next Tab leaves it, both ways.
#[test]
fn tab_stops_on_the_selected_member() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        t.states(12, CHECKED);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(12));
    assert_eq!(tab(&mut ui, false), Some(2));
    assert_eq!(tab(&mut ui, true), Some(12));
    assert_eq!(tab(&mut ui, true), Some(1));
    // `selected` counts as well (a tab), the first in tree order.
    apply(&mut ui, |t| {
        t.states(12, 0).states(13, SELECTED).states(11, SELECTED);
    });
    assert_eq!(tab(&mut ui, false), Some(11));
    // Tab into a group activates nothing.
    assert_eq!(activations(&mut ui), []);
}

/// No selected member: the first, then the last focused.
#[test]
fn tab_stops_on_the_last_focused_else_the_first() {
    let mut ui = app(Role::None, BOTH | g::LOOP, Role::Button, 3);
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
    assert_eq!(key(&mut ui, Key::Down), Some(12));
    assert_eq!(tab(&mut ui, false), Some(2));
    assert_eq!(tab(&mut ui, true), Some(12));
    // A click (a focus) on a member makes it the last focused too.
    focus(&mut ui, 13);
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(13));
}

/// A member holding the focus is the stop while it does: Small, reached
/// by a click while Medium is checked, Tabs out of the group (the web
/// would go to Medium).
#[test]
fn the_focused_member_is_the_stop() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        t.states(12, CHECKED);
    });
    focus(&mut ui, 11);
    assert_eq!(tab(&mut ui, false), Some(2));
    assert_eq!(tab(&mut ui, true), Some(12));
    focus(&mut ui, 11);
    assert_eq!(tab(&mut ui, true), Some(1));
}

/// Arrows follow the orientation: vertical takes ↑↓, horizontal ←→,
/// both all four.
#[test]
fn arrows_follow_the_orientation() {
    for (flags, moves) in [
        (g::VERTICAL, [false, true, false, true]),
        (g::HORIZONTAL, [true, false, true, false]),
        (BOTH, [true, true, true, true]),
    ] {
        let mut ui = app(Role::None, flags | g::LOOP, Role::Button, 3);
        for (k, (key_, moved)) in [Key::Right, Key::Down, Key::Left, Key::Up]
            .into_iter()
            .zip(moves)
            .enumerate()
        {
            focus(&mut ui, 12);
            let to = key(&mut ui, key_);
            let want = match (moved, k % 2 == 0 && k < 2 || k == 1) {
                (false, _) => 12,
                (true, true) => 13,
                (true, false) => 11,
            };
            assert_eq!(to, Some(want), "{flags:b} {key_:?}");
        }
    }
}

/// Arrows skip disabled (either flag), inert and hidden members, and
/// Tab never lands on them.
#[test]
fn arrows_skip_disabled_inert_and_hidden_members() {
    let mut ui = app(Role::None, BOTH | g::LOOP, Role::Button, 6);
    apply(&mut ui, |t| {
        t.interaction_press(12, LISTEN, true, press::PRESSABLE | press::DISABLED)
            .states(13, DISABLED)
            .interaction_bits(14, LISTEN, f::FOCUSABLE | f::INERT);
        let mut hidden = crate::host::default_style().to_taffy();
        hidden.display = taffy::Display::None;
        t.layout(15, &hidden);
    });
    focus(&mut ui, 11);
    assert_eq!(key(&mut ui, Key::Down), Some(16));
    assert_eq!(key(&mut ui, Key::Down), Some(11));
    assert_eq!(key(&mut ui, Key::Up), Some(16));
    // The first member disabled, as the facade sends it (no longer
    // focusable): the stop is the first enabled one.
    apply(&mut ui, |t| {
        t.interaction_press(11, LISTEN, false, press::PRESSABLE | press::DISABLED)
            .states(11, DISABLED);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(16));
}

/// `loop` wraps at both ends; without it the arrows stop there (and
/// activate nothing).
#[test]
fn loop_wraps_or_stops() {
    let mut ui = radios();
    focus(&mut ui, 13);
    activations(&mut ui);
    assert_eq!(key(&mut ui, Key::Down), Some(11));
    assert_eq!(key(&mut ui, Key::Up), Some(13));
    assert_eq!(activations(&mut ui), [(11, KEY), (13, KEY)]);

    apply(&mut ui, |t| {
        t.group(10, g::VERTICAL | g::SELECT_ON_FOCUS);
    });
    assert_eq!(key(&mut ui, Key::Down), Some(13));
    focus(&mut ui, 11);
    activations(&mut ui);
    assert_eq!(key(&mut ui, Key::Up), Some(11));
    assert_eq!(activations(&mut ui), []);
}

/// Home and End go to the ends, whatever the orientation.
#[test]
fn home_and_end_go_to_the_ends() {
    let mut ui = app(Role::None, g::HORIZONTAL, Role::Button, 4);
    focus(&mut ui, 12);
    assert_eq!(key(&mut ui, Key::End), Some(14));
    assert_eq!(key(&mut ui, Key::Home), Some(11));
    // With Ctrl, Alt or Meta the group leaves the key alone.
    let ctrl = Mods {
        ctrl: true,
        ..Mods::default()
    };
    assert_eq!(key_with(&mut ui, Key::End, ctrl), Some(11));
    assert_eq!(key_with(&mut ui, Key::Right, ctrl), Some(11));
    // Shift does not stop it.
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    assert_eq!(key_with(&mut ui, Key::Right, shift), Some(12));
}

/// `selectOnFocus` activates the member reached, once per move, from
/// the keyboard; a key that moves nothing activates nothing.
#[test]
fn select_on_focus_activates_once_per_move() {
    let mut ui = radios();
    focus(&mut ui, 11);
    activations(&mut ui);
    key(&mut ui, Key::Down);
    assert_eq!(activations(&mut ui), [(12, KEY)]);
    key(&mut ui, Key::End);
    assert_eq!(activations(&mut ui), [(13, KEY)]);
    key(&mut ui, Key::End);
    assert_eq!(activations(&mut ui), []);
    // Without it, moves only.
    apply(&mut ui, |t| {
        t.group(10, g::VERTICAL | g::LOOP);
    });
    activations(&mut ui);
    key(&mut ui, Key::Home);
    assert_eq!(focused(&ui), Some(11));
    assert_eq!(activations(&mut ui), []);
}

/// A key claim on the chord wins: JS keeps the arrows (highlight mode,
/// typeahead) and the focus stays.
#[test]
fn a_claim_wins_over_the_group() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        t.claims(10, 3, &[Claim::named(0, Key::Down)]);
    });
    focus(&mut ui, 11);
    ui.take_events();
    assert_eq!(key(&mut ui, Key::Down), Some(11));
    let e = ui.take_events();
    assert!(e.iter().any(|e| e.kind == out_kind::CLAIM && e.node == 10));
    assert!(!e.iter().any(|e| e.kind == out_kind::ACTIVATE));
    // Unclaimed Up still moves.
    assert_eq!(key(&mut ui, Key::Up), Some(13));
}

/// A group inside a trap is one stop of the trap's Tab cycle.
#[test]
fn a_group_in_a_trap_follows_its_scope() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        view(t, 0, 20);
        t.trap(20, trap_flag::ACTIVE);
        button(t, 20, 21);
        view(t, 20, 30);
        t.role(30, Role::RadioGroup).group(30, RADIOS);
        for id in [31, 32, 33] {
            item(t, 30, id, Role::RadioButton);
        }
        t.states(32, CHECKED);
        button(t, 20, 22);
    });
    focus(&mut ui, 21);
    let ring: Vec<_> = (0..4).map(|_| tab(&mut ui, false).unwrap()).collect();
    assert_eq!(ring, [32, 22, 21, 32]);
    assert_eq!(key(&mut ui, Key::Down), Some(33));
    assert_eq!(tab(&mut ui, true), Some(21));
}

/// A trap in a group scopes Tab and arrows to itself. Group 40 holds
/// button 41, trap 42 (buttons 43, 44) and button 45.
#[test]
fn a_trap_in_a_group_keeps_its_keys() {
    let mut ui = Ui::new(1.0);
    apply(&mut ui, |t| {
        view(t, NIL, 0);
        view(t, 0, 40);
        t.group(40, g::VERTICAL | g::LOOP);
        button(t, 40, 41);
        view(t, 40, 42);
        t.trap(42, trap_flag::ACTIVE);
        button(t, 42, 43);
        button(t, 42, 44);
        button(t, 40, 45);
    });
    focus(&mut ui, 43);
    assert_eq!(key(&mut ui, Key::Down), Some(43));
    assert_eq!(key(&mut ui, Key::End), Some(43));
    assert_eq!(tab(&mut ui, false), Some(44));
    assert_eq!(tab(&mut ui, false), Some(43));
}

/// A group nested in another is one member of the outer group, entered
/// at its own stop. Outer 40 (vertical) holds toolbars 41 (horizontal:
/// 43, 44, 45) and 42 (horizontal: 46, 47), and button 48.
#[test]
fn a_nested_group_is_one_member() {
    let mut ui = Ui::new(1.0);
    apply(&mut ui, |t| {
        view(t, NIL, 0);
        button(t, 0, 1);
        view(t, 0, 40);
        t.group(40, g::VERTICAL | g::LOOP);
        for (bar, items) in [(41, &[43, 44, 45][..]), (42, &[46, 47][..])] {
            view(t, 40, bar);
            t.group(bar, g::HORIZONTAL);
            for &id in items {
                button(t, bar, id);
            }
        }
        button(t, 40, 48);
        button(t, 0, 2);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(43));
    assert_eq!(tab(&mut ui, false), Some(2));
    assert_eq!(tab(&mut ui, true), Some(43));
    // ←→ in the toolbar, ↑↓ between the outer members.
    assert_eq!(key(&mut ui, Key::Right), Some(44));
    assert_eq!(key(&mut ui, Key::Down), Some(46));
    assert_eq!(key(&mut ui, Key::Right), Some(47));
    assert_eq!(key(&mut ui, Key::Down), Some(48));
    // Back into the first toolbar at its last focused, and out again at
    // the second's.
    assert_eq!(key(&mut ui, Key::Down), Some(44));
    assert_eq!(key(&mut ui, Key::Up), Some(48));
    assert_eq!(key(&mut ui, Key::Up), Some(47));
    // Home and End are the innermost group's.
    assert_eq!(key(&mut ui, Key::Home), Some(46));
    // The outer stop follows the focus: Tab leaves, and returns to it.
    assert_eq!(tab(&mut ui, false), Some(2));
    assert_eq!(tab(&mut ui, true), Some(46));
    // An empty toolbar is no member.
    apply(&mut ui, |t| {
        t.remove(46).remove(47);
    });
    focus(&mut ui, 44);
    assert_eq!(key(&mut ui, Key::Down), Some(48));
}

/// The selected member removed: the stop falls back to the last
/// focused, else the first.
#[test]
fn the_selected_member_removed() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        t.states(13, CHECKED);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(13));
    apply(&mut ui, |t| {
        t.remove(13);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
    focus(&mut ui, 12);
    apply(&mut ui, |t| {
        t.states(11, CHECKED);
    });
    focus(&mut ui, 2);
    assert_eq!(tab(&mut ui, true), Some(11));
    apply(&mut ui, |t| {
        t.remove(11);
    });
    focus(&mut ui, 2);
    assert_eq!(tab(&mut ui, true), Some(12), "the last focused");
}

/// The last focused removed: the first; an id reused by a new node is
/// not the last focused (its generation differs).
#[test]
fn the_last_focused_removed() {
    let mut ui = app(Role::None, BOTH, Role::Button, 3);
    focus(&mut ui, 13);
    apply(&mut ui, |t| {
        t.remove(13);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
    focus(&mut ui, 12);
    apply(&mut ui, |t| {
        t.remove(12);
        item(t, 10, 12, Role::Button);
    });
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
}

/// A composite role takes only its item role: in a radiogroup, a plain
/// button is a Tab stop of its own that arrows skip. A text input in a
/// group keeps its arrows.
#[test]
fn a_composite_role_takes_its_items_only() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        button(t, 10, 14);
        t.create(15, NodeKind::Input).append(10, 15);
    });
    focus(&mut ui, 1);
    let ring: Vec<_> = (0..4).map(|_| tab(&mut ui, false).unwrap()).collect();
    assert_eq!(ring, [11, 14, 15, 2]);
    focus(&mut ui, 13);
    assert_eq!(key(&mut ui, Key::Down), Some(11));
    focus(&mut ui, 14);
    assert_eq!(key(&mut ui, Key::Down), Some(14));

    // Role-less, the input is a member, but arrows in it stay its own.
    let mut ui = app(Role::None, BOTH | g::LOOP, Role::Button, 1);
    apply(&mut ui, |t| {
        t.create(15, NodeKind::Input).append(10, 15);
    });
    focus(&mut ui, 15);
    assert_eq!(key(&mut ui, Key::Right), Some(15));
    focus(&mut ui, 11);
    assert_eq!(key(&mut ui, Key::Right), Some(15));
}

/// Clearing the flags unmakes the group: every member is a stop again.
#[test]
fn no_flags_unmake_the_group() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        t.group(10, 0);
    });
    focus(&mut ui, 1);
    let ring: Vec<_> = (0..4).map(|_| tab(&mut ui, false).unwrap()).collect();
    assert_eq!(ring, [11, 12, 13, 2]);
    focus(&mut ui, 12);
    assert_eq!(key(&mut ui, Key::Down), Some(12));
}

/// Tabs and tab lists reach assistive technology with their roles, and
/// a tab reports `selected`.
#[test]
fn tabs_have_their_roles() {
    let mut ui = app(Role::TabList, g::HORIZONTAL | g::LOOP, Role::Tab, 2);
    apply(&mut ui, |t| {
        t.role_reporting(11, Role::Tab, reported::SELECTED)
            .states(11, SELECTED);
    });
    let tree = ui.a11y_tree(VIEW);
    let node = |id: u32| {
        tree.nodes
            .iter()
            .find(|(n, _)| *n == aid(NodeId(id)))
            .map(|(_, n)| n.clone())
            .unwrap()
    };
    assert_eq!(node(10).role(), AkRole::TabList);
    assert_eq!(node(11).role(), AkRole::Tab);
    assert_eq!(node(11).is_selected(), Some(true));
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
    assert_eq!(key(&mut ui, Key::Right), Some(12));
}

/// The key goes to the node it leaves: `KEY_DOWN` on the old focus,
/// then the focus change, then (`selectOnFocus`) the activation, and
/// Tab alike.
#[test]
fn the_key_goes_to_the_node_it_leaves() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        for id in [1, 11, 12] {
            t.interaction_press(id, LISTEN | mask::KEY, true, press::PRESSABLE);
        }
    });
    focus(&mut ui, 11);
    ui.take_events();
    let events = |ui: &mut Ui| -> Vec<(u8, u32)> {
        let e = ui.take_events();
        e.iter().map(|e| (e.kind, e.node)).collect()
    };
    key(&mut ui, Key::Down);
    assert_eq!(
        events(&mut ui),
        [
            (out_kind::KEY_DOWN, 11),
            (out_kind::BLUR, 11),
            (out_kind::FOCUS, 12),
            (out_kind::ACTIVATE, 12),
        ]
    );
    focus(&mut ui, 1);
    ui.take_events();
    tab(&mut ui, false);
    assert_eq!(
        events(&mut ui),
        [
            (out_kind::KEY_DOWN, 1),
            (out_kind::BLUR, 1),
            (out_kind::FOCUS, 12)
        ]
    );
}

/// `selectOnFocus` activates with no modifiers, as the web's `click()`:
/// Shift+↓ is no Shift+click.
#[test]
fn select_on_focus_activates_without_modifiers() {
    let mut ui = radios();
    focus(&mut ui, 11);
    ui.take_events();
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    assert_eq!(key_with(&mut ui, Key::Down, shift), Some(12));
    let acts: Vec<u32> = ui
        .take_events()
        .iter()
        .filter(|e| e.kind == out_kind::ACTIVATE)
        .map(|e| e.key)
        .collect();
    assert_eq!(acts, [KEY << 4]);
}

/// A disabled member still focusable (a `View focusable` with state
/// `DISABLED`, or `focus()` on it) is the stop while it holds focus:
/// Tab goes on in tree order, and arrows move to the enabled members
/// around it.
#[test]
fn a_focused_disabled_member_is_the_stop() {
    let mut ui = app(Role::None, g::VERTICAL, Role::Button, 4);
    apply(&mut ui, |t| {
        t.states(12, DISABLED);
    });
    focus(&mut ui, 12);
    assert_eq!(tab(&mut ui, false), Some(2));
    focus(&mut ui, 12);
    assert_eq!(tab(&mut ui, true), Some(1));
    focus(&mut ui, 12);
    assert_eq!(key(&mut ui, Key::Down), Some(13));
    // Gone from it, it is skipped again.
    assert_eq!(key(&mut ui, Key::Up), Some(11));
    focus(&mut ui, 12);
    assert_eq!(key(&mut ui, Key::Up), Some(11));
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(11));
}

/// Focus on a non-member in the group (a "More…" button in a
/// radiogroup) leaves the last focused member as it was.
#[test]
fn a_non_member_is_never_the_last_focused() {
    let mut ui = app(Role::RadioGroup, g::VERTICAL, Role::RadioButton, 3);
    apply(&mut ui, |t| {
        button(t, 10, 14);
    });
    focus(&mut ui, 12);
    focus(&mut ui, 14);
    focus(&mut ui, 1);
    assert_eq!(tab(&mut ui, false), Some(12));
    assert_eq!(tab(&mut ui, false), Some(14));
}

/// Removing a group drops its state; its id reused by a plain View is
/// no group.
#[test]
fn a_removed_group_is_forgotten() {
    let mut ui = radios();
    apply(&mut ui, |t| {
        for id in [11, 12, 13, 10] {
            t.remove(id);
        }
    });
    assert!(ui.groups.is_empty());
    apply(&mut ui, |t| {
        view(t, 0, 10);
        for id in [11, 12] {
            button(t, 10, id);
        }
    });
    focus(&mut ui, 1);
    let ring: Vec<_> = (0..3).map(|_| tab(&mut ui, false).unwrap()).collect();
    // Appended after 2; every item a stop.
    assert_eq!(ring, [2, 11, 12]);
}

/// The group's arrow axis reaches AccessKit: horizontal or vertical,
/// none for both.
#[test]
fn the_orientation_is_in_the_accessibility_tree() {
    use accesskit::Orientation;
    let mut ui = radios();
    let orientation = |ui: &mut Ui| {
        let tree = ui.a11y_tree(VIEW);
        let (_, n) = tree
            .nodes
            .iter()
            .find(|(n, _)| *n == aid(NodeId(10)))
            .unwrap();
        n.orientation()
    };
    assert_eq!(orientation(&mut ui), Some(Orientation::Vertical));
    apply(&mut ui, |t| {
        t.group(10, g::HORIZONTAL);
    });
    assert_eq!(orientation(&mut ui), Some(Orientation::Horizontal));
    apply(&mut ui, |t| {
        t.group(10, BOTH);
    });
    assert_eq!(orientation(&mut ui), None);
}
