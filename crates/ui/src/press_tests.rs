//! Presses and activation through `Ui::dispatch` and `a11y_action`:
//! which pressable a press reaches, when it activates, and what focus
//! and focus-visible do meanwhile.

use accesskit::{Action, ActionRequest, TreeId};

use crate::claims::Claim;
use crate::events::{
    Button, Event, Key, KeyInput, Mods, UiEvent, activate_source, mask, out_kind, press_phase,
};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{Command, NodeKind, TextSpan, Transaction, press};
use crate::states::state_bit::{FOCUS_VISIBLE, PRESSED};
use crate::ui::Ui;
use crate::wire::{self, WireError};

const NIL: u32 = u32::MAX;
const LISTEN: u32 = mask::PRESS | mask::ACTIVATE | mask::POINTER_UP;

fn at(x: f32, y: f32, w: f32, h: f32) -> taffy::Style {
    let len = taffy::LengthPercentageAuto::length;
    taffy::Style {
        position: taffy::Position::Absolute,
        inset: taffy::Rect {
            left: len(x),
            top: len(y),
            right: taffy::LengthPercentageAuto::auto(),
            bottom: taffy::LengthPercentageAuto::auto(),
        },
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

/// The task's example, in a 400 × 300 root 0:
///
/// ```text
/// row 1 (pressable, focusable)     0,0    400 × 40
///   text 2 "Deploy failed"         0,0    200 × 40
///   archive 3 (pressable, focus)   300,0  100 × 40
/// composer 4 (input)               0,100  400 × 40
/// mention 5 (keep focus)           0,200  100 × 40
/// hidden 6 (pressable)             0,250  400 × 50, under
/// cover 7 (listens to pointers)    0,250  400 × 50
/// ```
///
/// Row, archive and hidden are scopes (their state bits read).
fn app() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &at(0.0, 0.0, 400.0, 300.0))
        .place(NIL, 0, NIL);
    let pressable = press::PRESSABLE;
    t.create(1, NodeKind::View)
        .layout(1, &at(0.0, 0.0, 400.0, 40.0))
        .interaction_press(1, LISTEN | mask::POINTER_ENTER_LEAVE, true, pressable)
        .states(1, 0)
        .place(0, 1, NIL);
    t.create(2, NodeKind::Text)
        .layout(2, &at(0.0, 0.0, 200.0, 40.0))
        .text(2, "Deploy failed", 16.0, 0xFFFF_FFFF)
        .place(1, 2, NIL);
    t.create(3, NodeKind::View)
        .layout(3, &at(300.0, 0.0, 100.0, 40.0))
        .interaction_press(3, LISTEN | mask::POINTER_ENTER_LEAVE, true, pressable)
        .states(3, 0)
        .place(1, 3, NIL);
    t.create(4, NodeKind::Input)
        .layout(4, &at(0.0, 100.0, 400.0, 40.0))
        .input_config(4, 16.0, "", false)
        .interaction(4, mask::INPUT | mask::FOCUS, true)
        .states(4, 0)
        .place(0, 4, NIL);
    t.create(5, NodeKind::View)
        .layout(5, &at(0.0, 200.0, 100.0, 40.0))
        .interaction_press(5, LISTEN, false, pressable | press::KEEP_FOCUS)
        .place(0, 5, NIL);
    t.create(6, NodeKind::View)
        .layout(6, &at(0.0, 250.0, 400.0, 50.0))
        .interaction_press(6, LISTEN, true, pressable)
        .place(0, 6, NIL);
    t.create(7, NodeKind::View)
        .layout(7, &at(0.0, 250.0, 400.0, 50.0))
        .interaction(7, mask::POINTER_DOWN | mask::POINTER_UP, false)
        .place(0, 7, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 300.0));
    ui.take_events();
    ui
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'_>)) {
    let mut t = Transaction::new(2);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

fn down_with(ui: &mut Ui, x: f32, y: f32, button: Button) {
    ui.dispatch(&Event::PointerDown {
        x,
        y,
        button,
        mods: Mods::default(),
    });
}

fn down(ui: &mut Ui, x: f32, y: f32) {
    down_with(ui, x, y, Button::Primary);
}

fn up(ui: &mut Ui, x: f32, y: f32) {
    ui.dispatch(&Event::PointerUp {
        x,
        y,
        button: Button::Primary,
    });
}

fn click(ui: &mut Ui, x: f32, y: f32) -> Vec<UiEvent> {
    down(ui, x, y);
    up(ui, x, y);
    ui.take_events()
}

fn named(key: Key, mods: u8) -> KeyInput {
    KeyInput {
        key,
        mods: Mods::from_bits(mods),
        ..KeyInput::default()
    }
}

fn a11y(ui: &mut Ui, action: Action, id: u32) -> Vec<UiEvent> {
    ui.a11y_action(&ActionRequest {
        action,
        target_tree: TreeId::ROOT,
        target_node: crate::a11y::aid(NodeId(id)),
        data: None,
    });
    ui.take_events()
}

#[derive(Debug, PartialEq)]
enum P {
    In(u32),
    Out(u32),
    Cancel(u32),
    /// Node, source.
    Activate(u32, u32),
}

/// The press and activate events, in order.
fn presses(events: &[UiEvent]) -> Vec<P> {
    events
        .iter()
        .filter_map(|e| match e.kind {
            out_kind::PRESS => Some(match (e.key >> 4) & 3 {
                press_phase::IN => P::In(e.node),
                press_phase::OUT => P::Out(e.node),
                _ => P::Cancel(e.node),
            }),
            out_kind::ACTIVATE => Some(P::Activate(e.node, (e.key >> 4) & 3)),
            _ => None,
        })
        .collect()
}

fn nodes_of(events: &[UiEvent], kind: u8) -> Vec<u32> {
    events
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.node)
        .collect()
}

const POINTER: u32 = activate_source::POINTER;
const KEY: u32 = activate_source::KEY;

/// A press on Archive presses Archive only; the row's raw pointer
/// listener still hears the release. A press on the row's text presses
/// the row.
#[test]
fn presses_go_to_the_innermost_pressable() {
    let mut ui = app();
    let e = click(&mut ui, 350.0, 20.0);
    assert_eq!(presses(&e), [P::In(3), P::Out(3), P::Activate(3, POINTER)]);
    assert_eq!(nodes_of(&e, out_kind::POINTER_UP), [3, 1], "raw: the path");
    let e = click(&mut ui, 50.0, 20.0);
    assert_eq!(presses(&e), [P::In(1), P::Out(1), P::Activate(1, POINTER)]);

    // The activation carries the release point, node-relative too, the
    // primary button and the press's modifiers.
    ui.dispatch(&Event::PointerDown {
        x: 350.0,
        y: 20.0,
        button: Button::Primary,
        mods: Mods::from_bits(Mods::META),
    });
    up(&mut ui, 360.0, 30.0);
    let e = ui.take_events();
    let a = e.iter().find(|e| e.kind == out_kind::ACTIVATE).unwrap();
    assert_eq!((a.x, a.y, a.a, a.b), (360.0, 30.0, 60.0, 30.0));
    assert_eq!(a.key & 0xf, Mods::META as u32);
    assert_eq!((a.key >> 8) & 0xff, 1);
}

/// A disabled pressable swallows its presses: nothing fires on it or on
/// the row around it, from a pointer or assistive technology.
#[test]
fn a_disabled_pressable_swallows_presses() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.interaction_press(3, LISTEN, false, press::PRESSABLE | press::DISABLED);
    });
    let e = click(&mut ui, 350.0, 20.0);
    assert_eq!(presses(&e), []);
    assert_eq!(
        nodes_of(&e, out_kind::POINTER_UP),
        [3, 1],
        "raw listeners still hear it"
    );
    assert_eq!(presses(&a11y(&mut ui, Action::Click, 3)), []);
    // Disabled mid-press: the press cancels, with no activation.
    apply(&mut ui, |t| {
        t.interaction_press(3, LISTEN, true, press::PRESSABLE);
    });
    down(&mut ui, 350.0, 20.0);
    apply(&mut ui, |t| {
        t.interaction_press(3, LISTEN, false, press::PRESSABLE | press::DISABLED);
    });
    up(&mut ui, 350.0, 20.0);
    assert_eq!(presses(&ui.take_events()), [P::In(3), P::Cancel(3)]);
}

/// The web's click rule: a release activates only over the pressed node
/// or inside it, wherever the pointer went meanwhile. Only the primary
/// button presses.
#[test]
fn a_release_activates_only_over_the_pressed_node() {
    let mut ui = app();
    down(&mut ui, 350.0, 20.0);
    ui.dispatch(&Event::PointerMove { x: 100.0, y: 150.0 });
    up(&mut ui, 100.0, 150.0);
    assert_eq!(presses(&ui.take_events()), [P::In(3), P::Out(3)]);

    // Out and back in: it activates.
    down(&mut ui, 350.0, 20.0);
    ui.dispatch(&Event::PointerMove { x: 100.0, y: 150.0 });
    ui.dispatch(&Event::PointerMove { x: 360.0, y: 20.0 });
    up(&mut ui, 360.0, 20.0);
    assert_eq!(
        presses(&ui.take_events()),
        [P::In(3), P::Out(3), P::Activate(3, POINTER)]
    );

    // Pressed on the row, released on Archive (inside the row): the row.
    down(&mut ui, 50.0, 20.0);
    up(&mut ui, 350.0, 20.0);
    assert_eq!(
        presses(&ui.take_events()),
        [P::In(1), P::Out(1), P::Activate(1, POINTER)]
    );
    // Pressed on Archive, released on the row: outside Archive.
    down(&mut ui, 350.0, 20.0);
    up(&mut ui, 50.0, 20.0);
    assert_eq!(presses(&ui.take_events()), [P::In(3), P::Out(3)]);

    // Secondary and middle presses press nothing.
    for b in [Button::Secondary, Button::Middle] {
        down_with(&mut ui, 350.0, 20.0, b);
        ui.dispatch(&Event::PointerUp {
            x: 350.0,
            y: 20.0,
            button: b,
        });
        assert_eq!(presses(&ui.take_events()), [], "{b:?}");
    }
}

/// Enter on key down and Space on key up activate a focused pressable,
/// bare keys only; Space holds the pressed state meanwhile.
#[test]
fn enter_and_space_activate_the_focused_pressable() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
    });
    ui.take_events();

    ui.dispatch(&Event::KeyDown(named(Key::Enter, 0)));
    assert_eq!(presses(&ui.take_events()), [P::Activate(3, KEY)]);
    let repeat = KeyInput {
        repeat: true,
        ..named(Key::Enter, 0)
    };
    ui.dispatch(&Event::KeyDown(repeat));
    assert_eq!(
        presses(&ui.take_events()),
        [P::Activate(3, KEY)],
        "as browsers"
    );
    ui.dispatch(&Event::KeyDown(named(Key::Enter, Mods::SHIFT)));
    assert_eq!(presses(&ui.take_events()), [], "a modified Enter");

    ui.dispatch(&Event::KeyDown(named(Key::Space, 0)));
    assert_eq!(presses(&ui.take_events()), []);
    assert!(
        ui.state_bits(NodeId(3)) & PRESSED != 0,
        "Space holds it pressed"
    );
    ui.dispatch(&Event::KeyUp(named(Key::Space, 0)));
    assert_eq!(presses(&ui.take_events()), [P::Activate(3, KEY)]);
    assert!(ui.state_bits(NodeId(3)) & PRESSED == 0);

    // Focus moving away while Space is held ends the key press.
    ui.dispatch(&Event::KeyDown(named(Key::Space, 0)));
    apply(&mut ui, |t| {
        t.command(1, Command::Focus);
    });
    ui.dispatch(&Event::KeyUp(named(Key::Space, 0)));
    assert_eq!(presses(&ui.take_events()), []);

    // The activation is at the node's center.
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
    });
    ui.dispatch(&Event::KeyDown(named(Key::Enter, 0)));
    let e = ui.take_events();
    let a = e.iter().find(|e| e.kind == out_kind::ACTIVATE).unwrap();
    assert_eq!(
        (a.x, a.y, a.a, a.b, (a.key >> 8) & 0xff),
        (350.0, 20.0, 50.0, 20.0, 0)
    );
}

/// A key claim on the chord wins over activation: on Enter's key down,
/// and on Space's key down (so its key up activates nothing).
#[test]
fn a_key_claim_beats_activation() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(
            1,
            4,
            &[Claim::named(0, Key::Enter), Claim::named(0, Key::Space)],
        );
        t.command(3, Command::Focus);
    });
    ui.take_events();
    ui.dispatch(&Event::KeyDown(named(Key::Enter, 0)));
    let e = ui.take_events();
    assert_eq!(presses(&e), []);
    assert_eq!(nodes_of(&e, out_kind::CLAIM), [1]);
    ui.dispatch(&Event::KeyDown(named(Key::Space, 0)));
    ui.dispatch(&Event::KeyUp(named(Key::Space, 0)));
    let e = ui.take_events();
    assert_eq!(presses(&e), []);
    assert_eq!(nodes_of(&e, out_kind::CLAIM), [1]);
    assert!(ui.state_bits(NodeId(3)) & PRESSED == 0);
}

/// An accessibility click activates the pressable itself (or the
/// nearest one around the node), with no hit test: the node drawn over
/// it hears nothing.
#[test]
fn an_accessibility_click_activates_with_no_hit_test() {
    let mut ui = app();
    let e = a11y(&mut ui, Action::Click, 6);
    assert_eq!(
        presses(&e),
        [P::Activate(6, activate_source::ACCESSIBILITY)]
    );
    assert!(nodes_of(&e, out_kind::POINTER_DOWN).is_empty(), "{e:?}");
    assert!(nodes_of(&e, out_kind::POINTER_UP).is_empty(), "{e:?}");
    let a = &e[0];
    assert_eq!((a.x, a.y), (200.0, 275.0), "at its center");
    assert_eq!(
        presses(&a11y(&mut ui, Action::Click, 2)),
        [P::Activate(1, activate_source::ACCESSIBILITY)],
        "the text's pressable row"
    );
    assert_eq!(
        presses(&a11y(&mut ui, Action::Click, 7)),
        [],
        "no pressable"
    );
    assert_eq!(ui.focused(), None, "focus stays");

    // Every pressable offers the click, whatever its role.
    let tree = ui.a11y_tree(Size::new(400.0, 300.0));
    let row = crate::a11y::aid(NodeId(1));
    let node = tree.nodes.iter().find(|(id, _)| *id == row).unwrap();
    assert!(node.1.supports_action(Action::Click));
}

/// Losing window focus mid-press cancels the press and ends the hover,
/// with events; the release after it activates nothing.
#[test]
fn window_focus_loss_cancels_the_press_and_the_hover() {
    let mut ui = app();
    ui.dispatch(&Event::PointerMove { x: 350.0, y: 20.0 });
    down(&mut ui, 350.0, 20.0);
    ui.take_events();
    ui.dispatch(&Event::Focus(false));
    let e = ui.take_events();
    assert_eq!(presses(&e), [P::Cancel(3)]);
    assert_eq!(nodes_of(&e, out_kind::POINTER_LEAVE), [3, 1]);
    assert!(ui.state_bits(NodeId(3)) & PRESSED == 0);
    up(&mut ui, 350.0, 20.0);
    assert_eq!(presses(&ui.take_events()), []);

    // A held Space ends too.
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
    });
    ui.dispatch(&Event::KeyDown(named(Key::Space, 0)));
    ui.dispatch(&Event::Focus(false));
    ui.dispatch(&Event::KeyUp(named(Key::Space, 0)));
    assert_eq!(presses(&ui.take_events()), []);
}

/// A pressable leaving the tree mid-press cancels its press.
#[test]
fn a_detached_pressable_cancels_its_press() {
    let mut ui = app();
    down(&mut ui, 350.0, 20.0);
    ui.take_events();
    apply(&mut ui, |t| {
        t.detach(1);
    });
    assert_eq!(presses(&ui.take_events()), [P::Cancel(3)]);
    up(&mut ui, 350.0, 20.0);
    assert_eq!(presses(&ui.take_events()), []);
}

/// `preventFocusOnPress`: a press on the mention neither moves nor
/// clears focus, and leaves the composer's caret; it still activates.
#[test]
fn a_keep_focus_press_leaves_focus_alone() {
    let mut ui = app();
    click(&mut ui, 100.0, 120.0);
    assert_eq!(ui.focused(), Some(NodeId(4)));
    for c in ["h", "i"] {
        ui.dispatch(&Event::KeyDown(KeyInput {
            text: Some(c.into()),
            char: Some(c.into()),
            ..KeyInput::default()
        }));
    }
    let caret = *ui.inputs.get(4).unwrap().editor.selection();
    let e = click(&mut ui, 50.0, 220.0);
    assert_eq!(presses(&e), [P::In(5), P::Out(5), P::Activate(5, POINTER)]);
    assert_eq!(ui.focused(), Some(NodeId(4)));
    assert!(nodes_of(&e, out_kind::BLUR).is_empty());
    assert_eq!(*ui.inputs.get(4).unwrap().editor.selection(), caret);

    // Without the flag, the same press blurs.
    apply(&mut ui, |t| {
        t.interaction_press(5, LISTEN, false, press::PRESSABLE);
    });
    click(&mut ui, 50.0, 220.0);
    assert_eq!(ui.focused(), None);
}

/// Focus-visible, the browsers' rules: keyboard navigation turns it on,
/// a pointer press turns it off, a text input shows it however it was
/// focused, programmatic focus keeps the last mode, and assistive
/// technology changes nothing.
#[test]
fn focus_visible_follows_the_browsers() {
    let mut ui = app();
    let visible = |ui: &Ui, id: u32| ui.state_bits(NodeId(id)) & FOCUS_VISIBLE != 0;

    // Keyboard navigation.
    ui.dispatch(&Event::KeyDown(named(Key::Tab, 0)));
    assert_eq!(ui.focused(), Some(NodeId(1)));
    assert!(visible(&ui, 1));
    // Programmatic focus after it: visible.
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
    });
    assert!(visible(&ui, 3));
    // An accessibility click keeps the mode.
    a11y(&mut ui, Action::Click, 6);
    assert!(visible(&ui, 3));

    // A pointer press: off, on the node it focuses.
    click(&mut ui, 50.0, 20.0);
    assert_eq!(ui.focused(), Some(NodeId(1)));
    assert!(!visible(&ui, 1));
    // Programmatic focus after it: not visible.
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
    });
    assert!(!visible(&ui, 3));
    // Activating with a key is keyboard use.
    ui.dispatch(&Event::KeyDown(named(Key::Enter, 0)));
    assert!(visible(&ui, 3));
    // A keep-focus press is a pointer press too.
    click(&mut ui, 50.0, 220.0);
    assert_eq!(ui.focused(), Some(NodeId(3)));
    assert!(!visible(&ui, 3));

    // A text input shows it, focused by pointer or program.
    click(&mut ui, 100.0, 120.0);
    assert!(visible(&ui, 4));
    apply(&mut ui, |t| {
        t.command(3, Command::Focus);
        t.command(4, Command::Focus);
    });
    assert!(visible(&ui, 4));
}

/// A nested Text with `onPress` is a pressable span: a press on it
/// presses its text node (with the span), a press elsewhere on the text
/// reaches the pressable around it, and a release on another span
/// activates nothing.
#[test]
fn a_pressable_span_presses_its_text() {
    let mut ui = app();
    let plain = TextSpan::default();
    let link = TextSpan {
        start: 7,
        pressable: true,
        ..TextSpan::default()
    };
    apply(&mut ui, |t| {
        t.paragraph(2, "Deploy failed", &[plain, link]);
        t.interaction(2, mask::ACTIVATE | mask::PRESS, false);
    });
    ui.render(Size::new(400.0, 300.0));
    let layout = ui.text_layout(NodeId(2)).unwrap();
    let (on_plain, on_link) = (layout.caret(1).x, layout.caret(9).x);
    let line = layout.caret(1).top + layout.caret(1).height / 2.0;
    let e = click(&mut ui, on_link, line);
    assert_eq!(presses(&e), [P::In(2), P::Out(2), P::Activate(2, POINTER)]);
    let a = e.iter().find(|e| e.kind == out_kind::ACTIVATE).unwrap();
    assert_eq!(a.key >> 16, 2, "the span, plus one");
    assert_eq!(a.revision, ui.host.paragraph(NodeId(2)).unwrap().revision);

    let e = click(&mut ui, on_plain, line);
    assert_eq!(presses(&e), [P::In(1), P::Out(1), P::Activate(1, POINTER)]);

    down(&mut ui, on_link, line);
    up(&mut ui, on_plain, line);
    assert_eq!(presses(&ui.take_events()), [P::In(2), P::Out(2)]);
}

/// The press flags and the span flag round-trip; the flag bits no one
/// owns yet are rejected.
#[test]
fn press_flags_round_trip() {
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::Text).interaction_press(
        0,
        mask::ACTIVATE,
        false,
        press::PRESSABLE | press::DISABLED | press::KEEP_FOCUS,
    );
    t.paragraph(
        0,
        "ab",
        &[
            TextSpan::default(),
            TextSpan {
                start: 1,
                pressable: true,
                ..TextSpan::default()
            },
        ],
    );
    let buf = wire::encode(&t);
    let back = wire::decode(&buf).unwrap();
    assert_eq!(wire::encode(&back), buf);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    assert_eq!(ui.host.interaction(NodeId(0)).press, press::ALL);
    assert!(ui.host.paragraph(NodeId(0)).unwrap().spans[1].pressable);

    let i = buf
        .windows(9)
        .position(|w| w[0] == wire::op::INTERACTION && w[1..5] == 0u32.to_le_bytes())
        .expect("the interaction op");
    for bad in [1u8 << 7, 1 << 2, 1 << 3] {
        let mut b = buf.clone();
        b[i + 9] = bad;
        assert!(
            matches!(
                wire::decode(&b),
                Err(WireError::BadRef("interaction flags"))
            ),
            "{bad:#x}"
        );
    }
}
