//! State styles through `Ui`: which variants apply (specificity, the
//! bits native owns, the environment), what a restyle declares (rows,
//! transitions, inherited text color), the base a tabled node's own ops
//! set, scope lifetimes, hover at rest, and the wire.

use craie_layout::LayoutRow;
use craie_scene::PaintSlot;

use crate::animation::{Prop, Timing, Transition, Value};
use crate::events::{Button, Event, Key, KeyInput, Mods, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{Mutation, NodeKind, TextSpan, Transaction};
use crate::states::{TermDecl, Values, VariantDecl, env_bit, state_bit, value_field};
use crate::ui::Ui;
use crate::wire::{self, WireError, field};

const NIL: u32 = u32::MAX;
const WIDE: Size = Size {
    width: 1200.0,
    height: 300.0,
};

const A: u32 = 0xAA00_00FF;
const B: u32 = 0x00BB_00FF;
const C: u32 = 0x0000_CCFF;
const BASE: u32 = 0x1111_11FF;

fn sized(w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        flex_shrink: 0.0,
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

/// Root 0 (a column) holding: row 1 (200 × 40, a scope) with dot 2
/// (20 × 20) and text 3; focusable box 4 (100 × 40); input 5
/// (100 × 30).
fn app() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let root = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        align_items: Some(taffy::AlignItems::START),
        ..sized(1200.0, 300.0)
    };
    let row = taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        ..sized(200.0, 40.0)
    };
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &row)
        .fill(1, BASE)
        .interaction(1, mask::POINTER_ENTER_LEAVE, true)
        .states(1, 0)
        .place(0, 1, NIL);
    t.create(2, NodeKind::View)
        .layout(2, &sized(20.0, 20.0))
        .place(1, 2, NIL);
    t.create(3, NodeKind::Text)
        .paragraph(
            3,
            "Hi",
            &[TextSpan {
                font_size: 16.0,
                color: 0xFF00_00FF,
                inherit_color: true,
                ..TextSpan::default()
            }],
        )
        .place(1, 3, NIL);
    t.create(4, NodeKind::View)
        .layout(4, &sized(100.0, 40.0))
        .fill(4, BASE)
        .interaction(4, 0, true)
        .states(4, 0)
        .place(0, 4, NIL);
    t.create(5, NodeKind::Input)
        .layout(5, &sized(100.0, 30.0))
        .input_config(5, 16.0, 0xFFFF_FFFF, "", false)
        .states(5, 0)
        .place(0, 5, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(WIDE);
    ui.take_events();
    ui
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'_>)) {
    let mut t = Transaction::new(2);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

fn fill(c: u32) -> Values {
    Values {
        mask: value_field::FILL,
        fill: c,
        ..Values::default()
    }
}

fn color(c: u32) -> Values {
    Values {
        mask: value_field::COLOR,
        color: Some(c),
        ..Values::default()
    }
}

/// A size (both axes: the wire's field).
fn size(w: f32, h: f32) -> Values {
    let mut s = crate::host::default_style().to_taffy();
    s.size = sized(w, h).size;
    Values {
        mask: value_field::LAYOUT,
        layout_mask: field::SIZE,
        layout: LayoutRow::from(&s),
        ..Values::default()
    }
}

/// Values that apply while scope `scope` holds every bit of `bits`.
fn on(scope: u32, bits: u64, values: Values) -> VariantDecl {
    VariantDecl {
        terms: vec![TermDecl { scope, mask: bits }],
        env: 0,
        values,
    }
}

fn env(bits: u8, values: Values) -> VariantDecl {
    VariantDecl {
        terms: Vec::new(),
        env: bits,
        values,
    }
}

fn move_to(ui: &mut Ui, x: f32, y: f32) {
    ui.dispatch(&Event::PointerMove { x, y });
}

fn fill_of(ui: &Ui, id: usize) -> u32 {
    ui.host.paint[id].fill
}

fn has(ui: &Ui, id: u32, bits: u64) -> bool {
    ui.state_bits(NodeId(id)) & bits == bits
}

/// `_hover: A`, `_selected: B`, `_selected: { _hover: C }`: the deeper
/// variant wins, then the later rank (selected over hover).
#[test]
fn specificity_orders_variants() {
    use state_bit::{HOVER, SELECTED};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(
            1,
            &[
                on(1, HOVER, fill(A)),
                on(1, SELECTED, fill(B)),
                on(1, SELECTED | HOVER, fill(C)),
            ],
        );
    });
    assert_eq!(fill_of(&ui, 1), BASE);
    move_to(&mut ui, 100.0, 30.0);
    assert_eq!(fill_of(&ui, 1), A, "hovered");
    apply(&mut ui, |t| {
        t.states(1, SELECTED);
    });
    assert_eq!(fill_of(&ui, 1), C, "selected and hovered");
    move_to(&mut ui, 100.0, 290.0);
    assert_eq!(fill_of(&ui, 1), B, "selected");

    // Equal depth: the later rank wins, whatever the order.
    move_to(&mut ui, 100.0, 30.0);
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, SELECTED, fill(B)), on(1, HOVER, fill(A))]);
    });
    assert_eq!(fill_of(&ui, 1), B, "selected outranks hover");
    // Equal bits: the later declaration wins.
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, HOVER, fill(A)), on(1, HOVER, fill(C))]);
    });
    assert_eq!(fill_of(&ui, 1), C, "later declaration");
    // Property by property: a more specific variant overrides only what
    // it sets.
    let radius = Values {
        mask: value_field::RADIUS,
        radius: 8.0,
        ..Values::default()
    };
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, HOVER, fill(A)), on(1, SELECTED | HOVER, radius)]);
    });
    assert_eq!((fill_of(&ui, 1), ui.host.paint[1].radius), (A, 8.0));
}

/// Disabled stops hover and pressed but keeps selected; touch stops
/// hover.
#[test]
fn disabled_and_touch_mask_hover() {
    use state_bit::{DISABLED, HOVER, PRESSED, SELECTED};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, HOVER, fill(A)), on(1, SELECTED, fill(B))]);
        t.states(1, DISABLED);
    });
    move_to(&mut ui, 100.0, 30.0);
    ui.dispatch(&Event::PointerDown {
        x: 100.0,
        y: 30.0,
        button: Button::Primary,
        mods: Mods::default(),
    });
    assert!(!has(&ui, 1, HOVER) && !has(&ui, 1, PRESSED));
    assert_eq!(fill_of(&ui, 1), BASE);
    apply(&mut ui, |t| {
        t.states(1, DISABLED | SELECTED);
    });
    assert_eq!(fill_of(&ui, 1), B);
    apply(&mut ui, |t| {
        t.states(1, 0);
    });
    assert!(has(&ui, 1, HOVER | PRESSED));
    assert_eq!(fill_of(&ui, 1), A);
    ui.set_touch(true);
    ui.render(WIDE);
    assert!(!has(&ui, 1, HOVER));
    assert_eq!(fill_of(&ui, 1), BASE);
    assert_eq!(ui.env_bits() & env_bit::TOUCH, env_bit::TOUCH);
}

/// A scope is hovered while the pointer is over it or any descendant;
/// pressed follows the primary button only.
#[test]
fn hover_and_press_follow_the_pointer() {
    use state_bit::{HOVER, PRESSED};
    let mut ui = app();
    move_to(&mut ui, 5.0, 5.0);
    assert_eq!(ui.hover, Some(NodeId(2)));
    assert!(has(&ui, 1, HOVER), "hovered through its child");
    let down = |ui: &mut Ui, button| {
        ui.dispatch(&Event::PointerDown {
            x: 5.0,
            y: 5.0,
            button,
            mods: Mods::default(),
        })
    };
    let up = |ui: &mut Ui, button| {
        ui.dispatch(&Event::PointerUp {
            x: 5.0,
            y: 5.0,
            button,
        })
    };
    down(&mut ui, Button::Secondary);
    assert!(!has(&ui, 1, PRESSED), "secondary press");
    up(&mut ui, Button::Secondary);
    down(&mut ui, Button::Primary);
    assert!(has(&ui, 1, PRESSED));
    up(&mut ui, Button::Primary);
    assert!(!has(&ui, 1, PRESSED));
    move_to(&mut ui, 500.0, 290.0);
    assert!(!has(&ui, 1, HOVER));
    // Losing window focus drops hover.
    move_to(&mut ui, 5.0, 5.0);
    ui.dispatch(&Event::Focus(false));
    assert!(!has(&ui, 1, HOVER));
}

/// Focus is visible after a key, not after a click, except in an input.
/// Within bits reach the ancestors' scopes.
#[test]
fn focus_visibility_follows_modality() {
    use state_bit::{FOCUS_VISIBLE, FOCUS_VISIBLE_WITHIN, FOCUS_WITHIN};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.states(0, 0);
    });
    ui.dispatch(&Event::KeyDown(KeyInput {
        key: Key::Tab,
        ..KeyInput::default()
    }));
    let f = ui.focused().expect("tab focuses").0;
    assert!(has(
        &ui,
        f,
        FOCUS_VISIBLE | FOCUS_WITHIN | FOCUS_VISIBLE_WITHIN
    ));
    assert!(has(&ui, 0, FOCUS_WITHIN | FOCUS_VISIBLE_WITHIN));
    assert!(!has(&ui, 0, FOCUS_VISIBLE));

    let click = |ui: &mut Ui, x, y| {
        ui.dispatch(&Event::PointerDown {
            x,
            y,
            button: Button::Primary,
            mods: Mods::default(),
        });
        ui.dispatch(&Event::PointerUp {
            x,
            y,
            button: Button::Primary,
        });
    };
    click(&mut ui, 50.0, 60.0);
    assert_eq!(ui.focused(), Some(NodeId(4)));
    assert!(has(&ui, 4, FOCUS_WITHIN) && !has(&ui, 4, FOCUS_VISIBLE));
    assert!(!has(&ui, 0, FOCUS_VISIBLE_WITHIN));
    assert!(!has(&ui, f, FOCUS_WITHIN), "the old focus lost its bits");
    click(&mut ui, 50.0, 95.0);
    assert_eq!(ui.focused(), Some(NodeId(5)));
    assert!(has(&ui, 5, FOCUS_VISIBLE), "an input shows focus");
}

/// A `_narrow` size relayouts when the window crosses the breakpoint,
/// with no op; `ENVIRONMENT` moves the breakpoint.
#[test]
fn narrow_layout_relayouts_without_ops() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(4, &[env(env_bit::NARROW, size(100.0, 60.0))]);
    });
    ui.render(WIDE);
    assert_eq!(ui.layouts.rect(NodeId(4)).size.height, 40.0);
    let narrow = Size::new(800.0, 300.0);
    ui.render(narrow);
    assert_eq!(ui.env_bits() & env_bit::NARROW, env_bit::NARROW);
    assert_eq!(ui.layouts.rect(NodeId(4)).size.height, 60.0);
    apply(&mut ui, |t| {
        t.environment(700.0, 300.0);
    });
    assert!(ui.needs_paint());
    ui.render(narrow);
    assert_eq!(ui.env_bits(), 0);
    assert_eq!(ui.layouts.rect(NodeId(4)).size.height, 40.0);
}

/// A hover color on the row reaches its text's inheriting spans as a
/// paint patch: no layout, no shaping, no chunk rebuild.
#[test]
fn inherited_hover_color_reaches_spans() {
    const GREY: u32 = 0x9AA0_AAFF;
    const WHITE: u32 = 0xFFFF_FFFF;
    let mut ui = app();
    let span = |ui: &Ui| ui.scene().paint(3, PaintSlot(0));
    assert_eq!(span(&ui), Some(0xFF00_00FF), "no inherited color: its own");
    apply(&mut ui, |t| {
        t.color(1, Some(GREY));
        t.variants(1, &[on(1, state_bit::HOVER, color(WHITE))]);
    });
    ui.render(WIDE);
    assert_eq!(span(&ui), Some(GREY));
    let before = ui.counters();
    move_to(&mut ui, 150.0, 30.0);
    ui.render(WIDE);
    assert_eq!(span(&ui), Some(WHITE));
    let spent = ui.counters().since(&before);
    assert_eq!(
        (spent.layout_passes, spent.shapes, spent.chunks_built),
        (0, 0, 0)
    );
    assert!(spent.paints_patched > 0);
    // A nearer color wins; clearing it falls back to the row's.
    apply(&mut ui, |t| {
        t.color(3, Some(A));
    });
    ui.render(WIDE);
    assert_eq!(span(&ui), Some(A));
    apply(&mut ui, |t| {
        t.color(3, None);
    });
    ui.render(WIDE);
    assert_eq!(span(&ui), Some(WHITE));
    // Moved under a parent without a color: its own again.
    apply(&mut ui, |t| {
        t.place(4, 3, NIL);
    });
    ui.render(WIDE);
    assert_eq!(span(&ui), Some(0xFF00_00FF));
}

/// A state change declares through transitions: a hover fill tweens,
/// and so does an inherited color between two set colors.
#[test]
fn transition_tweens_a_hover_fill() {
    let mut ui = app();
    let linear = Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]);
    apply(&mut ui, |t| {
        t.fill(1, 0x0000_00FF);
        t.color(1, Some(0x0000_00FF));
        t.transition(
            1,
            &[
                Transition {
                    prop: Prop::Fill,
                    timing: linear,
                },
                Transition {
                    prop: Prop::Color,
                    timing: linear,
                },
            ],
        );
        t.variants(
            1,
            &[on(
                1,
                state_bit::HOVER,
                Values {
                    mask: value_field::FILL | value_field::COLOR,
                    fill: 0xFF00_00FF,
                    color: Some(0x00FF_00FF),
                    ..Values::default()
                },
            )],
        );
    });
    ui.set_time(0.0);
    move_to(&mut ui, 150.0, 30.0);
    assert_eq!(
        fill_of(&ui, 1),
        0x0000_00FF,
        "the row holds the value on screen"
    );
    ui.set_time(0.5);
    ui.render(WIDE);
    assert_eq!(fill_of(&ui, 1), 0x8000_00FF);
    assert_eq!(ui.host.colors[&1], 0x0080_00FF);
    assert_eq!(ui.scene().paint(3, PaintSlot(0)), Some(0x0080_00FF));
    ui.set_time(1.0);
    ui.render(WIDE);
    assert_eq!(fill_of(&ui, 1), 0xFF00_00FF);
    assert!(!ui.animating());
}

/// A PAINT op on a tabled node sets the base: an active variant keeps
/// the row until it stops applying. Removing the table restores the
/// base.
#[test]
fn ops_on_a_tabled_node_set_the_base() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, state_bit::HOVER, fill(A))]);
    });
    move_to(&mut ui, 150.0, 30.0);
    apply(&mut ui, |t| {
        t.fill(1, B);
        t.opacity(1, 0.5);
    });
    assert_eq!(fill_of(&ui, 1), A, "the variant holds");
    assert_eq!(ui.host.spatial[1].opacity, 0.5, "not overridden: applies");
    move_to(&mut ui, 150.0, 290.0);
    assert_eq!(fill_of(&ui, 1), B, "the new base");
    move_to(&mut ui, 150.0, 30.0);
    assert_eq!(fill_of(&ui, 1), A);
    apply(&mut ui, |t| {
        t.variants(1, &[]);
    });
    assert!(!ui.has_variants(NodeId(1)));
    assert_eq!(fill_of(&ui, 1), B, "count 0 restores the base");
    // Animate on a tabled node sets the base too; the variant wins.
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, state_bit::HOVER, fill(A))]);
        t.animate(
            1,
            Prop::Fill,
            Value::Color(C),
            Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
        );
    });
    assert_eq!(fill_of(&ui, 1), A);
    assert!(!ui.animating());
    move_to(&mut ui, 150.0, 290.0);
    assert_eq!(fill_of(&ui, 1), C);
}

/// A removed scope makes its terms false, even when its id comes back;
/// a removed dependent leaves no entry behind.
#[test]
fn scopes_die_with_their_node() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(4, &[on(1, state_bit::SELECTED, fill(A))]);
        t.variants(2, &[on(1, state_bit::SELECTED, fill(B))]);
        t.states(1, state_bit::SELECTED);
    });
    assert_eq!(fill_of(&ui, 4), A);
    assert_eq!(ui.states.scopes[&1].dependents.len(), 2);
    apply(&mut ui, |t| {
        t.remove(2);
    });
    assert_eq!(ui.states.scopes[&1].dependents, [4]);
    assert!(!ui.has_variants(NodeId(2)));
    apply(&mut ui, |t| {
        t.remove(1);
    });
    assert_eq!(fill_of(&ui, 4), BASE, "a dead scope's terms are false");
    apply(&mut ui, |t| {
        t.create(1, NodeKind::View)
            .place(0, 1, NIL)
            .states(1, state_bit::SELECTED);
    });
    assert_eq!(fill_of(&ui, 4), BASE, "a new occupant is another scope");
    assert!(ui.states.scopes[&1].dependents.is_empty());
}

/// Hover follows geometry under a still pointer: at once after a
/// layout change, at `settle` after a scroll.
#[test]
fn hover_at_rest() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(1, &[on(1, state_bit::HOVER, fill(A))]);
    });
    move_to(&mut ui, 150.0, 50.0);
    assert_eq!(ui.hover, Some(NodeId(0)));
    ui.set_time(1.0);
    ui.settle();
    ui.render(WIDE);
    ui.take_events();
    apply(&mut ui, |t| {
        t.layout(
            1,
            &taffy::Style {
                flex_direction: taffy::FlexDirection::Row,
                ..sized(200.0, 80.0)
            },
        );
    });
    ui.render(WIDE);
    assert_eq!(ui.hover, Some(NodeId(1)), "the row grew under the pointer");
    let events = ui.take_events();
    assert!(
        events
            .iter()
            .any(|e| e.kind == out_kind::POINTER_ENTER && e.node == 1)
    );
    assert!(ui.needs_paint(), "the restyle wants a frame");
    ui.render(WIDE);
    assert_eq!(fill_of(&ui, 1), A);

    // A scroll moves content under the pointer: held until it rests.
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let scroller = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        overflow: taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        },
        ..sized(100.0, 100.0)
    };
    t.create(0, NodeKind::View)
        .layout(0, &scroller)
        .place(NIL, 0, NIL);
    for id in 1..=4 {
        t.create(id, NodeKind::View)
            .layout(id, &sized(100.0, 50.0))
            .states(id, 0)
            .place(0, id, NIL);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(WIDE);
    ui.set_time(1.0);
    ui.settle();
    ui.render(WIDE);
    move_to(&mut ui, 10.0, 10.0);
    assert!(has(&ui, 1, state_bit::HOVER));
    ui.dispatch(&Event::Wheel {
        x: 10.0,
        y: 10.0,
        dx: 0.0,
        dy: 60.0,
    });
    ui.render(WIDE);
    assert_eq!(ui.hover, Some(NodeId(1)), "held while moving");
    let rest = ui.next_settle().expect("the scroll is moving");
    ui.set_time(rest + 0.01);
    assert!(ui.settle());
    assert_eq!(ui.hover, Some(NodeId(2)));
    assert!(has(&ui, 2, state_bit::HOVER) && !has(&ui, 1, state_bit::HOVER));
}

#[test]
fn state_ops_are_validated() {
    let mut ui = app();
    let rejects = |ui: &mut Ui, f: &dyn Fn(&mut Transaction<'_>)| {
        let mut t = Transaction::new(3);
        f(&mut t);
        matches!(ui.apply_txn(&t), Err(WireError::Invalid(_)))
    };
    assert!(rejects(&mut ui, &|t| {
        t.states(1, state_bit::HOVER);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.states(9, 0);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.variants(3, &[on(1, state_bit::HOVER, fill(A))]);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.variants(4, &[on(9, state_bit::HOVER, fill(A))]);
    }));
    assert!(rejects(&mut ui, &|t| {
        let o = Values {
            mask: value_field::OPACITY,
            opacity: 2.0,
            ..Values::default()
        };
        t.variants(4, &[on(1, state_bit::HOVER, o)]);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.variants(4, &[env(1 << 5, fill(A))]);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.environment(f32::NAN, 0.0);
    }));
    assert!(rejects(&mut ui, &|t| {
        t.color(9, None);
    }));
    // A scope created in the same batch is live.
    let mut t = Transaction::new(3);
    t.create(6, NodeKind::View)
        .place(0, 6, NIL)
        .variants(3, &[on(6, state_bit::SELECTED, color(A))]);
    ui.apply_txn(&t).unwrap();
}

#[test]
fn wire_round_trip() {
    let mut t = Transaction::new(7);
    let mut both = on(1, state_bit::SELECTED | state_bit::HOVER, size(10.0, 20.0));
    both.terms.push(TermDecl {
        scope: 2,
        mask: 1 << 3,
    });
    both.env = env_bit::NARROW | env_bit::TOUCH;
    both.values.mask |= value_field::ALL;
    both.values.fill = A;
    both.values.border = (B, 2.0);
    both.values.radius = 4.0;
    both.values.color = None;
    both.values.opacity = 0.25;
    both.values.transform = craie_core::geom::Affine::rotate(0.5);
    t.states(1, state_bit::SELECTED | 5)
        .variants(4, &[both, on(1, state_bit::HOVER, color(C))])
        .variants(5, &[])
        .environment(900.0, 500.0)
        .color(3, Some(B))
        .color(1, None)
        .animate(
            1,
            Prop::Color,
            Value::Color(A),
            Timing::curve(0.2, [0.0; 4]),
        )
        .paragraph(
            3,
            "x",
            &[TextSpan {
                inherit_color: true,
                ..TextSpan::default()
            }],
        );
    let buf = wire::encode(&t);
    let d = wire::decode(&buf).unwrap();
    assert_eq!(d.mutations, t.mutations);
    assert_eq!(d.spans, t.spans);
    assert!(matches!(
        d.mutations[5],
        Mutation::Color { color: None, .. }
    ));

    // Unknown value and span bits reject.
    let mut bad = Transaction::new(1);
    bad.variants(1, &[on(1, 0, fill(A))]);
    let mut buf = wire::encode(&bad);
    let at = buf.len() - 5;
    assert_eq!(buf[at], value_field::FILL);
    buf[at] = 1 << 7;
    assert!(wire::decode(&buf).is_err());
}
