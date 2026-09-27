//! Focus traps, `modal` and `inert` through `Ui` (`trap.rs`).

use crate::events::{Button, Event, Key, KeyInput, Mods, UiEvent, mask, out_kind, press_phase};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{Command, NodeKind, Transaction, interaction_flag as f, press, trap_flag};
use crate::ui::Ui;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 400.0,
};
const FOCUS: u32 = mask::FOCUS | mask::POINTER_ENTER_LEAVE;
/// `<FocusTrap modal>` with its defaults.
const MODAL: u8 =
    trap_flag::ACTIVE | trap_flag::MODAL | trap_flag::AUTO_FOCUS | trap_flag::RESTORE_FOCUS;
const TRAP: u8 = MODAL & !trap_flag::MODAL;

fn boxed(x: f32, y: f32, w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: taffy::LengthPercentageAuto::length(x),
            top: taffy::LengthPercentageAuto::length(y),
            ..taffy::Rect::auto()
        },
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

fn fill() -> taffy::Style {
    boxed(0.0, 0.0, 400.0, 400.0)
}

fn view(t: &mut Transaction, parent: u32, id: u32, style: &taffy::Style) {
    t.create(id, NodeKind::View)
        .layout(id, style)
        .append(parent, id);
}

/// A focusable button with focus and hover listeners.
fn button(t: &mut Transaction, parent: u32, id: u32, style: &taffy::Style) {
    view(t, parent, id, style);
    t.interaction(id, FOCUS, true);
}

fn layer(t: &mut Transaction, id: u32, owner: u32, z: i32) {
    t.create(id, NodeKind::View)
        .layout(id, &fill())
        .layer(id, owner)
        .z(id, z)
        .append(NIL, id);
}

fn apply(ui: &mut Ui, t: &Transaction) {
    ui.apply_txn(t).unwrap();
    ui.render(VIEW);
}

/// The app: root 0 holding "Delete…" 1 at (0, 0) and button 2 at
/// (100, 0), 100 × 100 each.
fn app() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    view(&mut t, NIL, 0, &fill());
    button(&mut t, 0, 1, &boxed(0.0, 0.0, 100.0, 100.0));
    button(&mut t, 0, 2, &boxed(100.0, 0.0, 100.0, 100.0));
    apply(&mut ui, &t);
    ui
}

/// Opens the dialog: layer 10 (z 70) holding trap 11 at (0, 200), with
/// Cancel 12, Delete 13 (`autoFocus` when `auto`) and More 14 in a row
/// of 50 × 50 boxes; and the menu More opened: layer 20 (z 50, owned by
/// More) holding 21 and 22 at (300, 300) and (350, 300).
fn open(ui: &mut Ui, seq: u64, flags: u8, auto: bool) {
    let mut t = Transaction::new(seq);
    layer(&mut t, 10, NIL, 70);
    view(&mut t, 10, 11, &boxed(0.0, 200.0, 300.0, 100.0));
    t.trap(11, flags);
    for (i, id) in [12, 13, 14].into_iter().enumerate() {
        button(&mut t, 11, id, &boxed(50.0 * i as f32, 0.0, 50.0, 50.0));
    }
    if auto {
        t.interaction_bits(13, FOCUS, f::FOCUSABLE | f::AUTO_FOCUS);
    }
    layer(&mut t, 20, 14, 50);
    button(&mut t, 20, 21, &boxed(300.0, 300.0, 50.0, 50.0));
    button(&mut t, 20, 22, &boxed(350.0, 300.0, 50.0, 50.0));
    apply(ui, &t);
}

/// Removes the dialog and its menu, one op per node as the bridge does.
fn close(ui: &mut Ui, seq: u64) {
    let mut t = Transaction::new(seq);
    for id in [20, 21, 22, 10, 11, 12, 13, 14] {
        t.remove(id);
    }
    apply(ui, &t);
}

fn focus(ui: &mut Ui, seq: u64, id: u32) {
    let mut t = Transaction::new(seq);
    t.command(id, Command::Focus);
    apply(ui, &t);
}

fn focused(ui: &Ui) -> Option<u32> {
    ui.focused().map(|n| n.0)
}

fn tab(ui: &mut Ui, shift: bool) -> Option<u32> {
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Tab,
        mods: Mods {
            shift,
            ..Mods::default()
        },
        ..KeyInput::default()
    }));
    focused(ui)
}

fn ring(ui: &mut Ui, shift: bool, n: usize) -> Vec<u32> {
    (0..n).map(|_| tab(ui, shift).unwrap()).collect()
}

fn hit(ui: &Ui, x: f32, y: f32) -> Option<u32> {
    ui.hit_test(x, y).map(|n| n.0)
}

/// The focus and blur events queued since the last call, as (kind,
/// node, generation).
fn focus_events(ui: &mut Ui) -> Vec<(u8, u32, u16)> {
    ui.take_events()
        .iter()
        .filter(|e: &&UiEvent| e.kind == out_kind::FOCUS || e.kind == out_kind::BLUR)
        .map(|e| (e.kind, e.node, e.generation))
        .collect()
}

const BLUR: u8 = out_kind::BLUR;
const FOCUSED: u8 = out_kind::FOCUS;

/// Opening focuses the `autoFocus` node; Tab and Shift+Tab cycle the
/// trap and the menu it owns (right after More, its owner).
#[test]
fn opening_auto_focuses_and_tab_wraps_both_ways() {
    let mut ui = app();
    focus(&mut ui, 2, 1);
    open(&mut ui, 3, MODAL, true);
    assert_eq!(focused(&ui), Some(13));
    assert_eq!(ring(&mut ui, false, 5), [14, 21, 22, 12, 13]);
    assert_eq!(ring(&mut ui, true, 5), [12, 22, 21, 14, 13]);
}

/// Without an `autoFocus` node, the first focusable; with focus already
/// inside, no move.
#[test]
fn auto_focus_falls_back_and_keeps_a_focus_inside() {
    let mut ui = app();
    open(&mut ui, 2, MODAL, false);
    assert_eq!(focused(&ui), Some(12));

    let mut ui = app();
    open(&mut ui, 2, 0, true);
    assert_eq!(focused(&ui), None, "inactive: nothing moves");
    focus(&mut ui, 3, 14);
    let mut t = Transaction::new(4);
    t.trap(11, MODAL);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(14));
    // A focus in a layer the trap owns is inside too.
    let mut t = Transaction::new(5);
    t.trap(11, 0);
    apply(&mut ui, &t);
    focus(&mut ui, 6, 21);
    let mut t = Transaction::new(7);
    t.trap(11, MODAL);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(21));
}

/// Focus returns to "Delete…" when the trap turns off and when it
/// unmounts; not when `restoreFocus` is off.
#[test]
fn restores_on_deactivation_and_unmount() {
    let mut ui = app();
    focus(&mut ui, 2, 1);
    open(&mut ui, 3, MODAL, true);
    assert_eq!(focused(&ui), Some(13));
    let mut t = Transaction::new(4);
    t.trap(11, MODAL & !trap_flag::ACTIVE);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(1));

    let mut t = Transaction::new(5);
    t.trap(11, MODAL);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(13));
    close(&mut ui, 6);
    assert_eq!(focused(&ui), Some(1));

    open(&mut ui, 7, MODAL & !trap_flag::RESTORE_FOCUS, true);
    assert_eq!(focused(&ui), Some(13));
    close(&mut ui, 8);
    assert_eq!(focused(&ui), None);
}

/// The restore target is (id, generation): "Delete…" removed and its id
/// reused while the dialog was open gets nothing, and the focus, gone
/// with the dialog, blurs.
#[test]
fn restore_skips_a_reused_id() {
    let mut ui = app();
    focus(&mut ui, 2, 1);
    open(&mut ui, 3, MODAL, true);
    let old = ui.host.node(NodeId(13)).unwrap().generation;
    let mut t = Transaction::new(4);
    t.remove(1);
    button(&mut t, 0, 1, &boxed(0.0, 0.0, 100.0, 100.0));
    apply(&mut ui, &t);
    focus_events(&mut ui);
    close(&mut ui, 5);
    assert_eq!(focused(&ui), None);
    assert_eq!(focus_events(&mut ui), [(BLUR, 13, old)]);
}

/// A modal: the pointer reaches the dialog and its menu, not the app or
/// a toast above; Tab stays inside; a press outside keeps the focus.
/// Non-modal, the app is live.
#[test]
fn modal_gates_hit_and_tab() {
    let mut ui = app();
    let mut t = Transaction::new(2);
    layer(&mut t, 30, NIL, 80);
    button(&mut t, 30, 31, &boxed(200.0, 0.0, 100.0, 100.0));
    apply(&mut ui, &t);
    open(&mut ui, 3, MODAL, true);
    assert_eq!(hit(&ui, 50.0, 50.0), None, "the app");
    assert_eq!(hit(&ui, 250.0, 50.0), None, "the toast");
    assert_eq!(hit(&ui, 75.0, 225.0), Some(13));
    assert_eq!(hit(&ui, 310.0, 310.0), Some(21), "the owned menu");
    assert_eq!(hit(&ui, 250.0, 250.0), Some(11), "the trap's own box");
    assert_eq!(ui.hit_test_walk(50.0, 50.0), None::<NodeId>);
    ui.dispatch(&Event::PointerDown {
        x: 50.0,
        y: 50.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert_eq!(focused(&ui), Some(13));
    let mut t = Transaction::new(4);
    t.command(1, Command::Focus);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(13), "no focus command out of the modal");
    let mut seen = ring(&mut ui, false, 10);
    assert_eq!(seen.last(), Some(&13));
    seen.sort();
    seen.dedup();
    assert_eq!(seen, [12, 13, 14, 21, 22]);

    let mut t = Transaction::new(5);
    t.trap(11, TRAP);
    apply(&mut ui, &t);
    assert_eq!(hit(&ui, 50.0, 50.0), Some(1));
    assert_eq!(hit(&ui, 250.0, 50.0), Some(31));
    // Non-modal: Tab still cycles the trap holding the focus.
    assert_eq!(ring(&mut ui, false, 5), [14, 21, 22, 12, 13]);
}

/// Traps nest: the innermost one holding the focus takes Tab; a nested
/// modal gates the outer one's content too, until it closes.
#[test]
fn nested_traps_and_modals() {
    let mut ui = app();
    open(&mut ui, 2, MODAL, true);
    assert_eq!(focused(&ui), Some(13));
    // The menu becomes a trap of its own: 23 in layer 20, over 21, 22.
    let mut t = Transaction::new(3);
    view(&mut t, 20, 23, &boxed(300.0, 300.0, 100.0, 50.0));
    t.detach(21).detach(22).append(23, 21).append(23, 22);
    t.layout(21, &boxed(0.0, 0.0, 50.0, 50.0))
        .layout(22, &boxed(50.0, 0.0, 50.0, 50.0));
    t.trap(23, TRAP);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(21));
    assert_eq!(ring(&mut ui, false, 3), [22, 21, 22]);
    assert_eq!(hit(&ui, 75.0, 225.0), Some(13), "the outer modal's content");

    let mut t = Transaction::new(4);
    t.trap(23, MODAL);
    apply(&mut ui, &t);
    assert_eq!(
        hit(&ui, 75.0, 225.0),
        None,
        "a nested modal gates the dialog"
    );
    assert_eq!(hit(&ui, 360.0, 310.0), Some(22));
    assert_eq!(hit(&ui, 50.0, 50.0), None);

    // The menu closes: focus goes back to Delete, where it was.
    let mut t = Transaction::new(5);
    t.trap(23, MODAL & !trap_flag::ACTIVE);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(13));
    assert_eq!(hit(&ui, 75.0, 225.0), Some(13));
    assert_eq!(hit(&ui, 50.0, 50.0), None);
}

/// Traps activating together: the outer one owns the restore, so the
/// inner one deactivating alone keeps focus inside the outer one.
#[test]
fn nested_traps_opened_together_restore_once() {
    let mut ui = app();
    focus(&mut ui, 2, 1);
    let mut t = Transaction::new(3);
    layer(&mut t, 10, NIL, 70);
    view(&mut t, 10, 11, &boxed(0.0, 200.0, 300.0, 100.0));
    button(&mut t, 11, 12, &boxed(0.0, 0.0, 50.0, 50.0));
    view(&mut t, 11, 15, &boxed(0.0, 50.0, 300.0, 50.0));
    button(&mut t, 15, 16, &boxed(0.0, 0.0, 50.0, 50.0));
    t.trap(11, TRAP).trap(15, TRAP);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(16), "the inner trap's target");
    assert_eq!(ring(&mut ui, false, 2), [16, 16], "Tab stays in 15");

    let mut t = Transaction::new(4);
    t.trap(15, TRAP & !trap_flag::ACTIVE);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(16), "no restore to 1, outside 11");
    assert_eq!(ring(&mut ui, false, 3), [12, 16, 12]);

    let mut t = Transaction::new(5);
    for id in [10, 11, 12, 15, 16] {
        t.remove(id);
    }
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(1), "the outer trap restores");
}

/// `inert`: no hit (the point falls through to what is below), no Tab,
/// no focus command, no accessibility node, for the node and its
/// subtree; a layer it owns escapes.
#[test]
fn inert_blocks_hit_focus_and_accessibility() {
    let mut ui = app();
    let mut t = Transaction::new(2);
    view(&mut t, 0, 3, &boxed(0.0, 100.0, 100.0, 100.0));
    button(&mut t, 3, 4, &boxed(0.0, 0.0, 50.0, 50.0));
    t.interaction_bits(3, 0, f::INERT);
    layer(&mut t, 5, 4, 10);
    button(&mut t, 5, 6, &boxed(300.0, 300.0, 50.0, 50.0));
    apply(&mut ui, &t);
    assert_eq!(hit(&ui, 25.0, 125.0), Some(0));
    assert_eq!(ui.hit_test_walk(25.0, 125.0), Some(NodeId(0)));
    assert_eq!(hit(&ui, 325.0, 325.0), Some(6));
    assert_eq!(ring(&mut ui, false, 4), [1, 2, 6, 1]);
    focus(&mut ui, 3, 4);
    assert_eq!(focused(&ui), Some(1));

    let tree = ui.a11y_tree(VIEW);
    let has = |id: u32| {
        tree.nodes
            .iter()
            .any(|(a, _)| *a == crate::a11y::aid(NodeId(id)))
    };
    assert!(!has(3) && !has(4));
    assert!(has(1) && has(6));

    // A node can be inert itself: the flag on the button.
    let mut t = Transaction::new(4);
    t.interaction_bits(1, FOCUS, f::FOCUSABLE | f::INERT);
    apply(&mut ui, &t);
    assert_eq!(hit(&ui, 50.0, 50.0), Some(0));
    assert_eq!(focused(&ui), None);
}

/// O1 in a trap: a focus that becomes inert goes to the trap's
/// `autoFocus` node; one removed, with the `autoFocus` node, to its
/// first focusable. Each blurs.
#[test]
fn lost_focus_in_a_trap_goes_to_its_target() {
    let mut ui = app();
    open(&mut ui, 2, MODAL, true);
    tab(&mut ui, false);
    assert_eq!(focused(&ui), Some(14));
    focus_events(&mut ui);
    let mut t = Transaction::new(3);
    t.interaction_bits(14, FOCUS, f::FOCUSABLE | f::INERT);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(13));
    let g13 = ui.host.node(NodeId(13)).unwrap().generation;
    let g14 = ui.host.node(NodeId(14)).unwrap().generation;
    assert_eq!(focus_events(&mut ui), [(BLUR, 14, g14), (FOCUSED, 13, g13)]);

    let mut t = Transaction::new(4);
    t.remove(13);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), Some(12));
    let g12 = ui.host.node(NodeId(12)).unwrap().generation;
    assert_eq!(focus_events(&mut ui), [(BLUR, 13, g13), (FOCUSED, 12, g12)]);
}

/// O1 outside traps: the focus goes nowhere, with a blur, whether the
/// node became inert or was removed.
#[test]
fn lost_focus_outside_traps_blurs() {
    let mut ui = app();
    focus(&mut ui, 2, 1);
    focus_events(&mut ui);
    let mut t = Transaction::new(3);
    t.interaction_bits(1, FOCUS, f::FOCUSABLE | f::INERT);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), None);
    let g1 = ui.host.node(NodeId(1)).unwrap().generation;
    assert_eq!(focus_events(&mut ui), [(BLUR, 1, g1)]);

    focus(&mut ui, 4, 2);
    focus_events(&mut ui);
    let g2 = ui.host.node(NodeId(2)).unwrap().generation;
    let mut t = Transaction::new(5);
    t.remove(2);
    apply(&mut ui, &t);
    assert_eq!(focused(&ui), None);
    assert_eq!(focus_events(&mut ui), [(BLUR, 2, g2)], "the old generation");
}

/// A hovered node that becomes inert, or a modal opening over it, gets
/// a leave on the next frame.
#[test]
fn hover_leaves_an_inert_node() {
    let mut ui = app();
    ui.dispatch(&Event::PointerMove { x: 50.0, y: 50.0 });
    ui.take_events();
    let mut t = Transaction::new(2);
    t.interaction_bits(1, FOCUS, f::FOCUSABLE | f::INERT);
    apply(&mut ui, &t);
    let leaves: Vec<u32> = ui
        .take_events()
        .iter()
        .filter(|e| e.kind == out_kind::POINTER_LEAVE)
        .map(|e| e.node)
        .collect();
    assert_eq!(leaves, [1]);

    ui.dispatch(&Event::PointerMove { x: 150.0, y: 50.0 });
    ui.take_events();
    open(&mut ui, 3, MODAL, true);
    let leaves: Vec<u32> = ui
        .take_events()
        .iter()
        .filter(|e| e.kind == out_kind::POINTER_LEAVE)
        .map(|e| e.node)
        .collect();
    assert_eq!(leaves, [2]);
}

/// Makes 1 and 2 pressables that listen to presses and activations.
fn pressables(ui: &mut Ui) {
    let mut t = Transaction::new(2);
    for id in [1, 2] {
        t.interaction_press(id, FOCUS | PRESSES, true, press::PRESSABLE);
    }
    apply(ui, &t);
    ui.take_events();
}

const PRESSES: u32 = mask::PRESS | mask::ACTIVATE;

/// The press events queued since the last call, as (kind, node, phase
/// or source).
fn press_events(ui: &mut Ui) -> Vec<(u8, u32, u32)> {
    ui.take_events()
        .iter()
        .filter(|e| e.kind == out_kind::PRESS || e.kind == out_kind::ACTIVATE)
        .map(|e| (e.kind, e.node, (e.key >> 4) & 3))
        .collect()
}

/// A press on a node that becomes inert, or falls outside a new modal,
/// cancels as its removal would: `PRESS` cancel, and no activation on
/// release. Space's press is dropped too.
#[test]
fn a_blocked_node_loses_its_press() {
    let (press_ev, cancel) = (out_kind::PRESS, press_phase::CANCEL);
    let mut ui = app();
    pressables(&mut ui);
    let down = |ui: &mut Ui, x: f32| {
        ui.dispatch(&Event::PointerDown {
            x,
            y: 50.0,
            button: Button::Primary,
            mods: Mods::default(),
        });
    };
    let up = |ui: &mut Ui, x: f32| {
        ui.dispatch(&Event::PointerUp {
            x,
            y: 50.0,
            button: Button::Primary,
        });
    };

    down(&mut ui, 50.0);
    assert_eq!(press_events(&mut ui), [(press_ev, 1, press_phase::IN)]);
    let mut t = Transaction::new(3);
    let pressable = press::PRESSABLE << f::PRESS_SHIFT;
    t.interaction_bits(1, FOCUS | PRESSES, f::FOCUSABLE | f::INERT | pressable);
    apply(&mut ui, &t);
    assert_eq!(press_events(&mut ui), [(press_ev, 1, cancel)]);
    up(&mut ui, 50.0);
    assert_eq!(press_events(&mut ui), []);

    // Outside a modal that opens mid-press.
    down(&mut ui, 150.0);
    assert_eq!(press_events(&mut ui), [(press_ev, 2, press_phase::IN)]);
    open(&mut ui, 4, MODAL, true);
    assert_eq!(press_events(&mut ui), [(press_ev, 2, cancel)]);
    up(&mut ui, 150.0);
    assert_eq!(press_events(&mut ui), []);
    close(&mut ui, 5);

    // Space: down on the focused 2, 2 inert then back, focused again,
    // Space up activates nothing.
    focus(&mut ui, 6, 2);
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Space,
        ..KeyInput::default()
    }));
    for (seq, flags) in [(7, f::FOCUSABLE | f::INERT), (8, f::FOCUSABLE)] {
        let mut t = Transaction::new(seq);
        t.interaction_bits(2, FOCUS | PRESSES, flags | pressable);
        apply(&mut ui, &t);
    }
    focus(&mut ui, 9, 2);
    assert_eq!(focused(&ui), Some(2));
    ui.take_events();
    ui.dispatch(&Event::KeyUp(KeyInput {
        key: Key::Space,
        ..KeyInput::default()
    }));
    assert_eq!(press_events(&mut ui), []);
}

/// Under a modal, the accessibility tree holds the dialog (flagged
/// modal) and its menu; the app and its paths are gone, the layer
/// holding the dialog stays as a bare container.
#[test]
fn modal_accessibility_tree() {
    let mut ui = app();
    open(&mut ui, 2, MODAL, true);
    let tree = ui.a11y_tree(VIEW);
    let node = |id: u32| {
        tree.nodes
            .iter()
            .find(|(a, _)| *a == crate::a11y::aid(NodeId(id)))
            .map(|(_, n)| n)
    };
    assert!(node(0).is_none() && node(1).is_none());
    assert!(node(11).unwrap().is_modal());
    assert!(node(13).is_some() && node(21).is_some());
    assert_eq!(node(10).unwrap().children(), [crate::a11y::aid(NodeId(11))]);
    assert_eq!(tree.focus, crate::a11y::aid(NodeId(13)));
}

/// Without traps, Tab walks the app, then each layer: an owned one
/// right after its owner's subtree, unowned ones after the app in root
/// order; an owner cycle owns nothing.
#[test]
fn tab_order_follows_owners() {
    let mut ui = app();
    open(&mut ui, 2, 0, false);
    let order = |ui: &Ui| -> Vec<u32> {
        ui.tab_order(crate::host::ROOT)
            .iter()
            .map(|n| n.0)
            .collect()
    };
    assert_eq!(order(&ui), [1, 2, 12, 13, 14, 21, 22]);
    assert_eq!(ring(&mut ui, false, 7), [1, 2, 12, 13, 14, 21, 22]);
    // The dialog layer now owned by 1: it follows "Delete…".
    let mut t = Transaction::new(3);
    t.layer(10, 1);
    apply(&mut ui, &t);
    assert_eq!(order(&ui), [1, 12, 13, 14, 21, 22, 2]);
    // A cycle: 10 owned by 21 (in 20), 20 owned by 14 (in 10).
    let mut t = Transaction::new(4);
    t.layer(10, 21);
    apply(&mut ui, &t);
    assert_eq!(order(&ui), [1, 2, 12, 13, 14, 21, 22]);
}
