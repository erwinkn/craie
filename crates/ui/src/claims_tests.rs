//! Claims through `Ui::dispatch`: which node a key, clipboard, drop or
//! context-menu gesture reaches, what native no longer does itself, and
//! what the `CLAIM` event carries.

use crate::claims::{Claim, chord_flag, claim_kind};
use crate::events::{Button, Event, Key, KeyInput, Mods, UiEvent, key_bits, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::input::SubmitKey;
use crate::mutation::{Command, NodeKind, Transaction};
use crate::ui::Ui;
use crate::wire::{self, WireError};

const NIL: u32 = u32::MAX;

/// A 400 × 100 row: input 1 (200 wide), button 2 (50 × 20), and a
/// selectable domain 3 holding text 4, all in root 0.
fn app() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let size = |w: f32, h: f32| taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::View)
        .layout(0, &size(400.0, 100.0))
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::Input)
        .layout(1, &size(200.0, 30.0))
        .input_config(1, 16.0, "", false)
        .interaction(1, mask::INPUT | mask::KEY, true)
        .place(0, 1, NIL);
    t.create(2, NodeKind::View)
        .layout(2, &size(50.0, 20.0))
        .interaction(2, mask::FOCUS | mask::KEY | mask::POINTER_DOWN, true)
        .place(0, 2, NIL);
    t.create(3, NodeKind::View)
        .layout(3, &size(150.0, 30.0))
        .interaction_flags(3, 0, false, true)
        .place(0, 3, NIL);
    t.create(4, NodeKind::Text)
        .text(4, "Hello world", 16.0, 0xFFFF_FFFF)
        .place(3, 4, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(Size::new(400.0, 100.0));
    ui.take_events();
    ui
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'_>)) {
    let mut t = Transaction::new(2);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

/// A character key as the platform reports it: text only without a
/// command modifier; the physical key is the character's (US layout).
fn press(c: &str, mods: u8) -> KeyInput {
    let command = mods & (Mods::CTRL | Mods::META) != 0;
    KeyInput {
        text: (!command).then(|| c.into()),
        char: Some(c.into()),
        code: c.chars().next().map(|c| c.to_ascii_lowercase()),
        mods: Mods::from_bits(mods),
        ..KeyInput::default()
    }
}

fn named(key: Key, mods: u8) -> KeyInput {
    KeyInput {
        key,
        mods: Mods::from_bits(mods),
        ..KeyInput::default()
    }
}

fn key(ui: &mut Ui, k: KeyInput) -> Vec<UiEvent> {
    ui.dispatch(&Event::KeyDown(k));
    ui.take_events()
}

/// The one event is a claim: (claimer, kind, index, version).
fn only_claim(events: &[UiEvent]) -> (u32, u8, u32, u32) {
    let kinds: Vec<u8> = events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, [out_kind::CLAIM], "{events:?}");
    let e = &events[0];
    (e.node, e.key as u8, e.key >> 8, e.revision)
}

fn has(events: &[UiEvent], kind: u8) -> bool {
    events.iter().any(|e| e.kind == kind)
}

/// The focused node and its ancestors claim first, then the window
/// list. A claimed key gets no default and no KEY_DOWN; with nothing
/// focused, an unclaimed key reaches no one.
#[test]
fn focus_path_claims_beat_the_window_list() {
    let mut ui = app();
    let search = Claim::char(Mods::COMMAND, 'k');
    apply(&mut ui, |t| {
        t.claims(0, 5, &[search]);
        t.claims(NIL, 9, &[search, Claim::char(0, '/')]);
        t.command(2, Command::Focus);
    });
    ui.take_events();

    assert_eq!(
        only_claim(&key(&mut ui, press("k", Mods::COMMAND))),
        (0, claim_kind::KEY, 0, 5)
    );
    assert_eq!(
        only_claim(&key(&mut ui, press("/", 0))),
        (NIL, claim_kind::KEY, 1, 9)
    );
    let e = key(&mut ui, press("q", 0));
    assert!(e.len() == 1 && e[0].kind == out_kind::KEY_DOWN && e[0].node == 2);

    apply(&mut ui, |t| {
        t.command(2, Command::Blur);
    });
    ui.take_events();
    assert_eq!(
        only_claim(&key(&mut ui, press("k", Mods::COMMAND))),
        (NIL, claim_kind::KEY, 0, 9)
    );
    assert!(key(&mut ui, press("q", 0)).is_empty(), "no focus, no one");

    // An empty set removes the node's claims.
    apply(&mut ui, |t| {
        t.claims(0, 6, &[]);
        t.command(2, Command::Focus);
    });
    ui.take_events();
    assert_eq!(
        only_claim(&key(&mut ui, press("k", Mods::COMMAND))),
        (NIL, claim_kind::KEY, 0, 9)
    );
}

/// While a text input has focus, the window list matches only claims
/// that allow it: `/` types a slash, `mod+k` still opens the palette.
#[test]
fn window_claims_in_inputs() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(
            NIL,
            1,
            &[
                Claim::char(0, '/'),
                Claim::char(Mods::COMMAND, 'k').with(chord_flag::IN_INPUT),
            ],
        );
        t.command(1, Command::Focus);
    });
    ui.take_events();

    let e = key(&mut ui, press("/", 0));
    assert!(!has(&e, out_kind::CLAIM) && has(&e, out_kind::CHANGE));
    assert_eq!(ui.inputs.text(1), "/");
    assert_eq!(
        only_claim(&key(&mut ui, press("k", Mods::COMMAND))),
        (NIL, claim_kind::KEY, 1, 1)
    );
    assert_eq!(ui.inputs.text(1), "/");

    // Escape no longer blurs: an app that wants it claims it.
    key(&mut ui, named(Key::Escape, 0));
    assert_eq!(ui.focused(), Some(NodeId(1)));
}

/// A claim replaces native's default: Tab keeps focus, Enter does not
/// submit, `mod+a` does not select.
#[test]
fn claims_beat_defaults() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(
            1,
            2,
            &[
                Claim::named(0, Key::Tab),
                Claim::named(0, Key::Enter),
                Claim::char(Mods::COMMAND, 'a'),
            ],
        );
        t.command(1, Command::SetText("hi".into()));
        t.command(1, Command::Focus);
    });
    ui.take_events();
    key(&mut ui, named(Key::End, 0));

    assert_eq!(
        only_claim(&key(&mut ui, named(Key::Tab, 0))),
        (1, claim_kind::KEY, 0, 2)
    );
    assert_eq!(ui.focused(), Some(NodeId(1)));
    assert_eq!(
        only_claim(&key(&mut ui, named(Key::Enter, 0))),
        (1, claim_kind::KEY, 1, 2)
    );
    assert_eq!(
        only_claim(&key(&mut ui, press("a", Mods::COMMAND))),
        (1, claim_kind::KEY, 2, 2)
    );
    // Nothing was selected: typing appends.
    key(&mut ui, press("x", 0));
    assert_eq!(ui.inputs.text(1), "hix");
    // Shift+Tab is another chord: it moves focus back as usual.
    key(&mut ui, named(Key::Tab, Mods::SHIFT));
    assert_ne!(ui.focused(), Some(NodeId(1)));
}

/// A `NO_REPEAT` claim swallows the key's auto-repeat; nothing matches
/// while an IME composes, and the KEY_DOWN says it is composing.
#[test]
fn repeats_and_composition() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(1, 1, &[Claim::char(0, 'j').with(chord_flag::NO_REPEAT)]);
        t.command(1, Command::Focus);
    });
    ui.take_events();

    let mut j = press("j", 0);
    assert_eq!(
        only_claim(&key(&mut ui, j.clone())),
        (1, claim_kind::KEY, 0, 1)
    );
    j.repeat = true;
    assert!(key(&mut ui, j).is_empty(), "the repeat is swallowed");
    assert_eq!(ui.inputs.text(1), "", "and never typed");

    ui.dispatch(&Event::ImePreedit {
        text: "か".into(),
        cursor: Some((3, 3)),
    });
    ui.take_events();
    let e = key(&mut ui, named(Key::Unknown, 0));
    let down = e.iter().find(|e| e.kind == out_kind::KEY_DOWN).unwrap();
    assert!(!has(&e, out_kind::CLAIM));
    assert_ne!(down.key & 1 << 5, 0, "composing");
    let mut j = press("j", 0);
    j.text = None;
    assert!(!has(&key(&mut ui, j), out_kind::CLAIM));
    ui.dispatch(&Event::ImeCommit("か".into()));
    assert_eq!(ui.inputs.text(1), "か");
}

/// Enter per the input's submit key.
#[test]
fn submit_keys() {
    let mut ui = app();
    let config = |ui: &mut Ui, multiline: bool, submit: SubmitKey| {
        apply(ui, |t| {
            t.input_config_submit(1, 16.0, "", multiline, submit);
            t.command(1, Command::SetText("".into()));
            t.command(1, Command::Focus);
        });
        ui.take_events();
    };
    let submits = |ui: &mut Ui, mods: u8| has(&key(ui, named(Key::Enter, mods)), out_kind::SUBMIT);

    // A chat box: Enter breaks the line, mod+Enter sends.
    config(&mut ui, true, SubmitKey::ModEnter);
    assert!(!submits(&mut ui, 0));
    assert_eq!(ui.inputs.text(1), "\n");
    assert!(submits(&mut ui, Mods::COMMAND));
    assert_eq!(ui.inputs.text(1), "\n");

    // A multiline input that submits on Enter breaks lines on Shift+Enter.
    config(&mut ui, true, SubmitKey::Enter);
    assert!(submits(&mut ui, 0));
    assert!(!submits(&mut ui, Mods::SHIFT));
    assert_eq!(ui.inputs.text(1), "\n");

    // A form field submits on exactly Enter (Marbre's rule).
    config(&mut ui, false, SubmitKey::Enter);
    assert!(submits(&mut ui, 0));
    assert!(!submits(&mut ui, Mods::SHIFT));
    assert!(!submits(&mut ui, Mods::COMMAND));
    assert_eq!(ui.inputs.text(1), "");

    // No onSubmit: Enter does nothing in a single line.
    config(&mut ui, false, SubmitKey::None);
    assert!(!submits(&mut ui, 0));
    assert!(!submits(&mut ui, Mods::SHIFT));
    assert_eq!(ui.inputs.text(1), "");
}

/// Paste, copy and cut claims carry the clipboard's or the selection's
/// text and leave the buffer and the clipboard alone; JS answers with
/// `InsertText` and `WriteClipboard`.
#[test]
fn clipboard_claims() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(
            0,
            4,
            &[
                Claim::of(claim_kind::PASTE),
                Claim::of(claim_kind::COPY),
                Claim::of(claim_kind::CUT),
            ],
        );
        t.command(1, Command::SetText("hello".into()));
        t.command(1, Command::Focus);
    });
    ui.inputs.clipboard.set("CLIP");
    key(&mut ui, press("a", Mods::COMMAND));

    let e = key(&mut ui, press("v", Mods::COMMAND));
    assert_eq!(only_claim(&e), (0, claim_kind::PASTE, 0, 4));
    assert_eq!(e[0].text, "CLIP");
    assert_eq!(ui.inputs.text(1), "hello");
    // The answer goes to the claimer and lands in the focused input
    // inside it, as typed: a change event.
    apply(&mut ui, |t| {
        t.command(0, Command::InsertText("PASTED".into()));
    });
    assert_eq!(ui.inputs.text(1), "PASTED");
    assert!(has(&ui.take_events(), out_kind::CHANGE));

    key(&mut ui, press("a", Mods::COMMAND));
    let e = key(&mut ui, press("c", Mods::COMMAND));
    assert_eq!(only_claim(&e), (0, claim_kind::COPY, 1, 4));
    assert_eq!(e[0].text, "PASTED");
    let e = key(&mut ui, press("x", Mods::COMMAND));
    assert_eq!(only_claim(&e), (0, claim_kind::CUT, 2, 4));
    assert_eq!(e[0].text, "PASTED");
    assert_eq!(ui.inputs.clipboard.get().as_deref(), Some("CLIP"));
    assert_eq!(ui.inputs.text(1), "PASTED");
    apply(&mut ui, |t| {
        t.command(NIL, Command::WriteClipboard("cut".into()));
        t.command(0, Command::InsertText("".into()));
    });
    assert_eq!(ui.inputs.clipboard.get().as_deref(), Some("cut"));
    assert_eq!(ui.inputs.text(1), "");

    // Text for a node that does not hold the focused input goes nowhere.
    apply(&mut ui, |t| {
        t.command(2, Command::InsertText("zzz".into()));
    });
    assert_eq!(ui.inputs.text(1), "");

    // With nothing focused, copy is claimed on the text selection's
    // domain.
    apply(&mut ui, |t| {
        t.command(1, Command::Blur);
        t.claims(3, 1, &[Claim::of(claim_kind::COPY)]);
    });
    ui.dispatch(&Event::PointerDown {
        x: 260.0,
        y: 8.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    ui.take_events();
    key(&mut ui, press("a", Mods::COMMAND));
    let e = key(&mut ui, press("c", Mods::COMMAND));
    assert_eq!(only_claim(&e), (3, claim_kind::COPY, 0, 1));
    assert_eq!(e[0].text, "Hello world");
    assert_eq!(ui.inputs.clipboard.get().as_deref(), Some("cut"));
}

/// Drops and secondary presses are claimed on the path under the
/// pointer (drops at an unknown position on the focus path); the
/// context-menu keys on the focus path, at the focused node's center.
#[test]
fn drop_and_context_menu() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.claims(
            0,
            3,
            &[
                Claim::of(claim_kind::DROP),
                Claim::of(claim_kind::CONTEXT_MENU),
            ],
        );
    });
    ui.dispatch(&Event::Drop {
        x: 210.0,
        y: 5.0,
        paths: vec!["/a.png".into(), "/b c.txt".into()],
    });
    let e = ui.take_events();
    assert_eq!(only_claim(&e), (0, claim_kind::DROP, 0, 3));
    assert_eq!(
        (e[0].x, e[0].y, e[0].text.as_str()),
        (210.0, 5.0, "/a.png\0/b c.txt")
    );

    // The press itself still focuses and reaches its listeners, first.
    ui.dispatch(&Event::PointerDown {
        x: 212.0,
        y: 6.0,
        button: Button::Secondary,
        mods: Mods::default(),
    });
    let e = ui.take_events();
    let kinds: Vec<(u8, u32)> = e.iter().map(|e| (e.kind, e.node)).collect();
    assert_eq!(
        kinds,
        [
            (out_kind::FOCUS, 2),
            (out_kind::POINTER_DOWN, 2),
            (out_kind::CLAIM, 0)
        ]
    );
    assert_eq!(
        (e[2].key, e[2].x, e[2].y),
        (claim_kind::CONTEXT_MENU as u32 | 1 << 8, 212.0, 6.0)
    );
    assert_eq!(ui.focused(), Some(NodeId(2)));

    for k in [named(Key::F(10), Mods::SHIFT), named(Key::ContextMenu, 0)] {
        let e = key(&mut ui, k);
        assert_eq!(only_claim(&e), (0, claim_kind::CONTEXT_MENU, 1, 3));
        assert_eq!((e[0].x, e[0].y), (225.0, 10.0), "button 2's center");
    }
    // A drop at an unknown position goes to the focus path.
    apply(&mut ui, |t| {
        t.claims(2, 1, &[Claim::of(claim_kind::DROP)]);
    });
    ui.dispatch(&Event::Drop {
        x: -1.0,
        y: -1.0,
        paths: vec!["/a.png".into()],
    });
    assert_eq!(only_claim(&ui.take_events()), (2, claim_kind::DROP, 0, 1));

    // F10 alone is not the menu key.
    assert_eq!(
        key(&mut ui, named(Key::F(10), 0))[0].kind,
        out_kind::KEY_DOWN
    );

    // Unclaimed, the gestures are plain events or nothing.
    apply(&mut ui, |t| {
        t.claims(0, 4, &[]);
        t.claims(2, 2, &[]);
    });
    assert_eq!(
        key(&mut ui, named(Key::ContextMenu, 0))[0].kind,
        out_kind::KEY_DOWN
    );
    ui.dispatch(&Event::Drop {
        x: 210.0,
        y: 5.0,
        paths: vec!["/a.png".into()],
    });
    assert!(ui.take_events().is_empty());
}

/// KEY_DOWN's key field: modifiers, repeat, composing, the named key and
/// the physical key's US character.
#[test]
fn key_records() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.command(2, Command::Focus);
    });
    ui.take_events();

    let mut k = press("A", Mods::CTRL | Mods::SHIFT);
    k.repeat = true;
    let e = key(&mut ui, k);
    assert_eq!(e[0].kind, out_kind::KEY_DOWN);
    assert_eq!(
        e[0].key,
        (Mods::CTRL | Mods::SHIFT) as u32 | 1 << 4 | ('a' as u32) << 16
    );
    assert_eq!(e[0].text, "A");

    let e = key(&mut ui, named(Key::F(5), Mods::ALT));
    assert_eq!(e[0].key, Mods::ALT as u32 | 36 << 8);

    // A physical key off the US layout has no code.
    let mut k = named(Key::Unknown, 0);
    k.code = Some('é');
    assert_eq!(key_bits(&k, true), 1 << 5);
}

/// The executor refuses claims for absent nodes, window claims that are
/// not keys, and malformed claims, and a node's claims go with it.
#[test]
fn claims_are_validated() {
    let mut ui = app();
    let fails = |ui: &mut Ui, f: &dyn Fn(&mut Transaction<'_>)| {
        let mut t = Transaction::new(3);
        f(&mut t);
        ui.apply_txn(&t).is_err()
    };
    let k = Claim::char(0, 'k');
    assert!(fails(&mut ui, &|t| {
        t.claims(0, 1, &[k]).claims(99, 1, &[k]);
    }));
    assert!(
        ui.host.claims.is_empty(),
        "a failed transaction changes nothing"
    );
    assert!(fails(&mut ui, &|t| {
        t.claims(NIL, 1, &[Claim::of(claim_kind::PASTE)]);
    }));
    assert!(fails(&mut ui, &|t| {
        t.claims(0, 1, &[Claim::of(9)]);
    }));
    assert!(fails(&mut ui, &|t| {
        t.command(NIL, Command::InsertText("x".into()));
    }));
    assert!(!fails(&mut ui, &|t| {
        t.command(NIL, Command::WriteClipboard("x".into()));
    }));

    apply(&mut ui, |t| {
        t.claims(2, 1, &[k]);
    });
    assert_eq!(ui.host.claims[&2].version, 1);
    apply(&mut ui, |t| {
        t.remove(2);
    });
    assert!(!ui.host.claims.contains_key(&2));

    // The wire rejects a malformed claim at decode. The op is last, so
    // the final 8 bytes are the claim: kind, flags, mods, pad, key.
    let mut t = Transaction::new(4);
    t.claims(0, 1, &[k]);
    let good = wire::encode(&t);
    let n = good.len();
    let patched = |f: &dyn Fn(&mut [u8])| {
        let mut buf = good.clone();
        f(&mut buf);
        wire::decode(&buf).err()
    };
    assert_eq!(patched(&|_| {}), None);
    let claim = Some(WireError::BadRef("claim"));
    assert_eq!(patched(&|b| b[n - 8] = 9), claim, "unknown kind");
    assert_eq!(patched(&|b| b[n - 7] |= 0x80), claim, "reserved flag bit");
    assert_eq!(patched(&|b| b[n - 6] = 16), claim, "mods past bit 3");
    assert_eq!(patched(&|b| b[n - 4..].fill(0)), claim, "NUL character key");
    assert_eq!(
        wire::decode(&good[..n - 3]).err(),
        Some(WireError::Truncated)
    );
    // The count says two claims, the buffer holds one.
    assert_eq!(patched(&|b| b[n - 10] = 2), Some(WireError::Truncated));

    // Submit bits 3 name no submit key.
    let mut t = Transaction::new(5);
    t.input_config_submit(1, 16.0, "", false, SubmitKey::ModEnter);
    let mut buf = wire::encode(&t);
    let flags = buf.last_mut().unwrap();
    assert_eq!(*flags, (SubmitKey::ModEnter as u8) << 1);
    *flags = 3 << 1;
    assert_eq!(
        wire::decode(&buf).err(),
        Some(WireError::BadRef("input flags"))
    );
}
