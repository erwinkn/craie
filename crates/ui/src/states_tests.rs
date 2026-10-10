//! State styles through `Ui`: which variants apply (specificity, the
//! bits native owns, the environment), what a restyle declares (rows,
//! transitions, inherited color), the base a tabled node's own ops
//! set, scope lifetimes, hover at rest, and the wire.

use craie_layout::LayoutRow;
use craie_scene::PaintSlot;

use crate::animation::{Prop, Timing, Transition, Value};
use crate::events::{Button, Event, Key, KeyInput, Mods, mask, out_kind};
use crate::geom::{Point, Size};
use crate::host::NodeId;
use crate::mutation::{Mutation, NodeKind, TextSpan, Transaction};
use crate::states::{TermDecl, Values, VariantDecl, env_bit, layout_key, state_bit, value_field};
use crate::ui::Ui;
use crate::wire::{self, WireError};

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
        .input_config(5, 16.0, "", false)
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

/// A size (both axes).
fn size(w: f32, h: f32) -> Values {
    let mut s = crate::host::default_style().to_taffy();
    s.size = sized(w, h).size;
    Values {
        mask: value_field::LAYOUT,
        layout_keys: layout_key::WIDTH | layout_key::HEIGHT,
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
        ..Default::default()
    }
}

fn env(bits: u8, values: Values) -> VariantDecl {
    VariantDecl {
        terms: Vec::new(),
        env: bits,
        values,
        ..Default::default()
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

/// DF-24, DF-14: the same hover recolors a `currentColor` icon and an
/// input without a color of their own, as it does the text: paint
/// patches only, no layout, no shaping, no chunk rebuild, no new
/// tessellation.
#[test]
fn inherited_hover_color_reaches_drawings_and_inputs() {
    use craie_vector::svg::{CURRENT_STROKE, Drawing, Shape};
    const GREY: u32 = 0x9AA0_AAFF;
    const WHITE: u32 = 0xFFFF_FFFF;
    let mut ui = app();
    let icon = Drawing {
        view_box: "0 0 24 24".into(),
        shapes: vec![Shape {
            geometry: "M4 12H20".into(),
            fill: 0,
            stroke: WHITE,
            current: CURRENT_STROKE,
            ..Shape::default()
        }],
    };
    apply(&mut ui, |t| {
        t.create(6, NodeKind::Vector)
            .layout(6, &sized(20.0, 20.0))
            .drawing(6, icon)
            .place(1, 6, NIL);
        t.create(7, NodeKind::Input)
            .layout(7, &sized(60.0, 30.0))
            .input_config(7, 16.0, "", false)
            .place(1, 7, NIL);
        t.color(1, Some(GREY));
        t.variants(1, &[on(1, state_bit::HOVER, color(WHITE))]);
    });
    ui.render(WIDE);
    // The icon's stroke is its first slot after the box's two.
    let icon = |ui: &Ui| ui.scene().paint(6, PaintSlot(2));
    let input = |ui: &Ui| [2, 3, 5].map(|s| ui.scene().paint(7, PaintSlot(s)));
    assert_eq!(icon(&ui), Some(GREY));
    // Text, placeholder (half alpha), caret.
    assert_eq!(input(&ui), [Some(GREY), Some(0x9AA0_AA7F), Some(GREY)]);
    let mesh = ui.vector_meshes.items(6).unwrap().as_ptr();
    let before = ui.counters();
    move_to(&mut ui, 150.0, 30.0);
    ui.render(WIDE);
    assert!(has(&ui, 1, state_bit::HOVER));
    assert_eq!(icon(&ui), Some(WHITE));
    assert_eq!(input(&ui), [Some(WHITE), Some(0xFFFF_FF7F), Some(WHITE)]);
    let spent = ui.counters().since(&before);
    assert_eq!(
        (spent.layout_passes, spent.shapes, spent.chunks_built),
        (0, 0, 0)
    );
    assert_eq!(ui.vector_meshes.items(6).unwrap().as_ptr(), mesh);
    // Their own color wins over the row's; clearing it falls back.
    apply(&mut ui, |t| {
        t.color(6, Some(A)).color(7, Some(B));
    });
    ui.render(WIDE);
    assert_eq!((icon(&ui), input(&ui)[0]), (Some(A), Some(B)));
    apply(&mut ui, |t| {
        t.color(6, None).color(7, None);
    });
    ui.render(WIDE);
    assert_eq!((icon(&ui), input(&ui)[0]), (Some(WHITE), Some(WHITE)));
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

/// `animate` on a tabled node runs unless a variant in effect sets the
/// same layout key: a height variant leaves a width animation alone, a
/// width variant cancels it (`LEDGER.md` DF-28).
#[test]
fn animate_under_a_layout_variant() {
    use crate::animation::end_reason::CANCELLED;
    use crate::events::out_kind::ANIMATION_END;
    let narrow = Size::new(800.0, 300.0);
    let dim = taffy::Dimension::length;
    let mut ui = app();
    ui.set_time(0.0);
    apply(&mut ui, |t| {
        t.variants(
            4,
            &[env(
                env_bit::NARROW,
                layout(layout_key::HEIGHT, |s| s.size.height = dim(60.0)),
            )],
        );
    });
    ui.render(narrow);
    let width = |ui: &mut Ui| {
        apply(ui, |t| {
            t.animate(
                4,
                Prop::Width,
                Value::Size(dim(160.0)),
                Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
            );
        });
    };
    width(&mut ui);
    ui.set_time(0.5);
    ui.render(narrow);
    assert_eq!(ui.layouts.rect(NodeId(4)).size, Size::new(130.0, 60.0));
    ui.set_time(1.0);
    ui.render(narrow);
    assert_eq!(ui.layouts.rect(NodeId(4)).size, Size::new(160.0, 60.0));
    ui.take_events();

    apply(&mut ui, |t| {
        t.variants(
            4,
            &[env(
                env_bit::NARROW,
                layout(layout_key::WIDTH, |s| s.size.width = dim(60.0)),
            )],
        );
    });
    ui.render(narrow);
    width(&mut ui);
    ui.render(narrow);
    assert_eq!(ui.layouts.rect(NodeId(4)).size.width, 60.0);
    assert!(!ui.animating());
    let ends: Vec<u32> = ui
        .take_events()
        .iter()
        .filter(|e| e.kind == ANIMATION_END)
        .map(|e| e.key >> 8)
        .collect();
    assert_eq!(ends, [CANCELLED]);
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
            .interaction(id, mask::POINTER_ENTER_LEAVE, false)
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
    both.values.parts.matrix = craie_core::geom::Affine::rotate(0.5);
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

    // Unknown layout keys reject.
    let mut bad = Transaction::new(1);
    let mut v = size(1.0, 1.0);
    v.layout_keys = 1 << 40;
    bad.variants(1, &[on(1, 0, v)]);
    assert!(wire::decode(&wire::encode(&bad)).is_err());
}

fn layout(keys: u64, f: impl FnOnce(&mut taffy::Style)) -> Values {
    let mut s = crate::host::default_style().to_taffy();
    f(&mut s);
    Values {
        mask: value_field::LAYOUT,
        layout_keys: keys,
        layout: LayoutRow::from(&s),
        ..Values::default()
    }
}

fn linear(prop: Prop) -> Transition {
    Transition {
        prop,
        timing: Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
    }
}

fn press(ui: &mut Ui, x: f32, y: f32) {
    ui.dispatch(&Event::PointerDown {
        x,
        y,
        button: Button::Primary,
        mods: Mods::default(),
    });
}

fn key(ui: &mut Ui, k: KeyInput) {
    ui.dispatch(&Event::KeyDown(k));
}

fn focus(ui: &mut Ui, id: u32) {
    use accesskit::{Action, ActionRequest, TreeId};
    ui.a11y_action(&ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: crate::a11y::aid(NodeId(id)),
        data: None,
    });
}

/// A table's first values were never on screen: they go to the rows
/// with no transition, at mount and at the first frame's breakpoints.
#[test]
fn first_resolution_does_not_transition() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.transition(4, &[linear(Prop::Fill)])
            .states(4, state_bit::SELECTED)
            .variants(4, &[on(4, state_bit::SELECTED, fill(A))]);
    });
    assert_eq!(fill_of(&ui, 4), A, "mounted selected");
    assert!(!ui.animating());
    // Later changes do transition.
    apply(&mut ui, |t| {
        t.states(4, 0);
    });
    assert!(ui.animating());

    // A `_narrow` fill applied before the first frame, which is narrow.
    let narrow = Size::new(800.0, 300.0);
    let mount = |t: &mut Transaction<'_>| {
        t.create(0, NodeKind::View)
            .layout(0, &sized(100.0, 40.0))
            .fill(0, BASE)
            .transition(0, &[linear(Prop::Fill)])
            .variants(0, &[env(env_bit::NARROW, fill(A))])
            .place(NIL, 0, NIL);
    };
    let mut ui = Ui::new(1.0);
    apply(&mut ui, mount);
    ui.render(narrow);
    assert_eq!(fill_of(&ui, 0), A, "the first frame snaps");
    assert!(!ui.animating());
    ui.render(WIDE);
    assert!(ui.animating(), "the next crossing transitions");
    // The platform gives the size first: the first transaction resolves
    // narrow.
    let mut ui = Ui::new(1.0);
    ui.set_window_size(narrow);
    apply(&mut ui, mount);
    assert_eq!(fill_of(&ui, 0), A);
    assert!(!ui.animating());
}

/// Layout resolves per key: two variants setting different sides of
/// padding both apply, and a size axis from each of two variants
/// composes.
#[test]
fn layout_keys_compose() {
    use layout_key::{HEIGHT, PADDING_BOTTOM, PADDING_LEFT, PADDING_RIGHT, PADDING_TOP, WIDTH};
    use taffy::LengthPercentage as Lp;
    let mut ui = app();
    let pad = |x: f32, y: f32| taffy::Rect {
        left: Lp::length(x),
        right: Lp::length(x),
        top: Lp::length(y),
        bottom: Lp::length(y),
    };
    // px: 16, py: 12, _narrow: { px: 4 }, _compact: { py: 6 }
    apply(&mut ui, |t| {
        t.layout(
            4,
            &taffy::Style {
                padding: pad(16.0, 12.0),
                ..sized(100.0, 40.0)
            },
        )
        .variants(
            4,
            &[
                env(
                    env_bit::NARROW,
                    layout(PADDING_LEFT | PADDING_RIGHT, |s| s.padding = pad(4.0, 0.0)),
                ),
                env(
                    env_bit::COMPACT,
                    layout(PADDING_TOP | PADDING_BOTTOM, |s| s.padding = pad(0.0, 6.0)),
                ),
            ],
        );
    });
    ui.render(Size::new(500.0, 300.0));
    assert_eq!(ui.host.layout[4].to_taffy().padding, pad(4.0, 6.0));
    ui.render(Size::new(800.0, 300.0));
    assert_eq!(ui.host.layout[4].to_taffy().padding, pad(4.0, 12.0));

    // _narrow: { height: 60 }, _hover: { width: 300 }
    apply(&mut ui, |t| {
        t.variants(
            1,
            &[
                env(
                    env_bit::NARROW,
                    layout(HEIGHT, |s| s.size.height = taffy::Dimension::length(60.0)),
                ),
                on(
                    1,
                    state_bit::HOVER,
                    layout(WIDTH, |s| s.size.width = taffy::Dimension::length(300.0)),
                ),
            ],
        );
    });
    let narrow = Size::new(800.0, 300.0);
    ui.render(narrow);
    assert_eq!(ui.layouts.rect(NodeId(1)).size, Size::new(200.0, 60.0));
    move_to(&mut ui, 100.0, 30.0);
    ui.render(narrow);
    assert_eq!(ui.layouts.rect(NodeId(1)).size, Size::new(300.0, 60.0));
}

/// Border color and width are separate values: one variant's color and
/// another's width both apply, and the width lays out.
#[test]
fn border_color_and_width_compose() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(
            1,
            &[
                on(
                    1,
                    state_bit::HOVER,
                    Values {
                        mask: value_field::BORDER_COLOR,
                        border: (A, 0.0),
                        ..Values::default()
                    },
                ),
                on(
                    1,
                    state_bit::SELECTED,
                    Values {
                        mask: value_field::BORDER_WIDTH,
                        border: (0, 3.0),
                        ..Values::default()
                    },
                ),
            ],
        )
        .states(1, state_bit::SELECTED);
    });
    // The variant's width lays out: the dot sits inside the border.
    ui.render(WIDE);
    assert_eq!(ui.layouts.rect(NodeId(2)).origin, Point::new(3.0, 3.0));
    move_to(&mut ui, 100.0, 30.0);
    let p = ui.host.paint[1];
    assert_eq!((p.border_color, p.border_width), (A, 3.0));
    apply(&mut ui, |t| {
        t.states(1, 0);
    });
    ui.render(WIDE);
    assert_eq!(ui.layouts.rect(NodeId(2)).origin, Point::new(0.0, 0.0));
}

/// Equal depth compares only the latest rank; below it, declaration
/// order decides. `_hover._pressed` and `_selected._pressed` both
/// latest-rank at pressed: the later declared wins.
#[test]
fn specificity_compares_the_latest_rank() {
    use state_bit::{HOVER, PRESSED, SELECTED};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(
            1,
            &[
                on(1, SELECTED | PRESSED, fill(B)),
                on(1, HOVER | PRESSED, fill(A)),
            ],
        )
        .states(1, SELECTED);
    });
    move_to(&mut ui, 100.0, 30.0);
    press(&mut ui, 100.0, 30.0);
    assert_eq!(fill_of(&ui, 1), A);
}

/// Detaching a subtree ends a press inside it; focus stays on a node
/// that moves, and its ancestors' bits follow it.
#[test]
fn detach_ends_a_press_and_focus_follows_moves() {
    use state_bit::{FOCUS_WITHIN, PRESSED};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.states(0, 0);
    });
    press(&mut ui, 5.0, 5.0);
    assert!(has(&ui, 1, PRESSED));
    apply(&mut ui, |t| {
        t.detach(1);
    });
    assert_eq!(ui.pressed, None);
    assert!(!has(&ui, 1, PRESSED));
    apply(&mut ui, |t| {
        t.place(0, 1, NIL);
    });
    assert!(!has(&ui, 1, PRESSED), "placed back: still released");
    ui.dispatch(&Event::PointerUp {
        x: 5.0,
        y: 5.0,
        button: Button::Primary,
    });

    focus(&mut ui, 4);
    assert!(has(&ui, 0, FOCUS_WITHIN));
    apply(&mut ui, |t| {
        t.detach(4);
    });
    assert_eq!(ui.focused(), Some(NodeId(4)));
    assert!(!has(&ui, 0, FOCUS_WITHIN), "outside the tree");
    apply(&mut ui, |t| {
        t.place(1, 4, NIL);
    });
    assert!(has(&ui, 0, FOCUS_WITHIN) && has(&ui, 1, FOCUS_WITHIN));
}

/// Leaving the window ends hover and forgets the position; a wheel
/// event gives one.
#[test]
fn pointer_leave_and_wheel_position() {
    let mut ui = app();
    move_to(&mut ui, 100.0, 30.0);
    ui.take_events();
    ui.dispatch(&Event::PointerLeave);
    assert!(!has(&ui, 1, state_bit::HOVER));
    assert_eq!(ui.last_pointer, None);
    let leaves: Vec<u32> = ui
        .take_events()
        .iter()
        .filter(|e| e.kind == out_kind::POINTER_LEAVE)
        .map(|e| e.node)
        .collect();
    assert_eq!(leaves, [1]);
    // Geometry moves under where the pointer was: nothing hovers.
    apply(&mut ui, |t| {
        t.layout(1, &sized(300.0, 60.0));
    });
    ui.render(WIDE);
    assert_eq!(ui.hover, None);

    ui.dispatch(&Event::Wheel {
        x: 250.0,
        y: 30.0,
        dx: 0.0,
        dy: 0.0,
    });
    apply(&mut ui, |t| {
        t.layout(1, &sized(400.0, 60.0));
    });
    ui.render(WIDE);
    assert!(
        has(&ui, 1, state_bit::HOVER),
        "hover at rest from the wheel"
    );
}

/// An assistive technology's focus restyles at once.
#[test]
fn a11y_focus_restyles() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(4, &[on(4, state_bit::FOCUS_WITHIN, fill(A))]);
    });
    focus(&mut ui, 4);
    assert_eq!(fill_of(&ui, 4), A);
}

/// Bare modifiers and command chords keep the pointer's modality.
#[test]
fn modifier_keys_keep_the_modality() {
    use state_bit::FOCUS_VISIBLE;
    let mut ui = app();
    press(&mut ui, 50.0, 60.0);
    assert_eq!(ui.focused(), Some(NodeId(4)));
    let shift = Mods {
        shift: true,
        ..Mods::default()
    };
    let meta = Mods {
        meta: true,
        ..Mods::default()
    };
    let ch = |c: &str, mods| KeyInput {
        char: Some(c.into()),
        mods,
        ..KeyInput::default()
    };
    key(
        &mut ui,
        KeyInput {
            mods: shift,
            ..KeyInput::default()
        },
    );
    assert!(!has(&ui, 4, FOCUS_VISIBLE), "bare shift");
    key(&mut ui, ch("c", meta));
    assert!(!has(&ui, 4, FOCUS_VISIBLE), "Cmd+C");
    key(&mut ui, ch("a", Mods::default()));
    assert!(has(&ui, 4, FOCUS_VISIBLE));
}

/// Disabled masks focus visible too, and assistive technology reports
/// it.
#[test]
fn disabled_masks_focus_visible() {
    use state_bit::{DISABLED, FOCUS_VISIBLE, FOCUS_WITHIN};
    let mut ui = app();
    apply(&mut ui, |t| {
        t.states(4, DISABLED);
    });
    focus(&mut ui, 4);
    key(
        &mut ui,
        KeyInput {
            char: Some("a".into()),
            ..KeyInput::default()
        },
    );
    assert!(has(&ui, 4, FOCUS_WITHIN) && !has(&ui, 4, FOCUS_VISIBLE));
    let tree = ui.a11y_tree(WIDE);
    let node = |id: u32| {
        tree.nodes
            .iter()
            .find(|(n, _)| *n == crate::a11y::aid(NodeId(id)))
            .map(|(_, n)| n.clone())
            .unwrap()
    };
    assert!(node(4).is_disabled());
    assert!(!node(1).is_disabled());
}

/// Touch masks hover even inside a touch variant; the touch variant
/// alone applies.
#[test]
fn touch_masks_hover_in_touch_variants() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.variants(
            1,
            &[
                env(env_bit::TOUCH, fill(A)),
                VariantDecl {
                    terms: vec![TermDecl {
                        scope: 1,
                        mask: state_bit::HOVER,
                    }],
                    env: env_bit::TOUCH,
                    values: fill(C),
                    ..Default::default()
                },
            ],
        );
    });
    ui.set_touch(true);
    move_to(&mut ui, 100.0, 30.0);
    ui.render(WIDE);
    assert_eq!(fill_of(&ui, 1), A);
}

/// A table holds at most 256 variants of at most 8 terms.
#[test]
fn table_size_limits() {
    let mut ui = app();
    let ok = |ui: &mut Ui, v: Vec<VariantDecl>| {
        let mut t = Transaction::new(3);
        t.variants(4, &v);
        ui.apply_txn(&t).is_ok()
    };
    let one = on(1, state_bit::HOVER, fill(A));
    assert!(ok(&mut ui, vec![one.clone(); 256]));
    assert!(!ok(&mut ui, vec![one.clone(); 257]));
    let mut wide = one.clone();
    wide.terms = vec![wide.terms[0]; 8];
    assert!(ok(&mut ui, vec![wide.clone()]));
    wide.terms.push(wide.terms[0]);
    assert!(!ok(&mut ui, vec![wide]));
}

/// With no hover variant and no hover listener, hover at rest does not
/// hit-test; the first reader brings it up to date.
#[test]
fn hover_at_rest_waits_for_a_reader() {
    let mut ui = app();
    apply(&mut ui, |t| {
        t.interaction(1, 0, true);
    });
    move_to(&mut ui, 5.0, 5.0);
    assert_eq!(ui.hover, Some(NodeId(2)));
    // The dot moves to the top left, still under the pointer.
    apply(&mut ui, |t| {
        t.place(0, 2, 1);
    });
    ui.render(WIDE);
    assert_eq!(ui.hover, Some(NodeId(1)), "not re-tested");
    apply(&mut ui, |t| {
        t.variants(4, &[on(1, state_bit::HOVER, fill(A))]);
    });
    ui.render(WIDE);
    assert_eq!(ui.hover, Some(NodeId(2)), "re-tested for the reader");
}

/// Inherited color follows moves, and a text added during a color
/// tween takes the tweened value.
#[test]
fn inherited_color_follows_the_tree() {
    const GREY: u32 = 0x8080_80FF;
    let mut ui = app();
    let span = |ui: &Ui, id| ui.scene().paint(id, PaintSlot(0));
    apply(&mut ui, |t| {
        t.color(1, Some(0x0000_00FF))
            .color(4, Some(A))
            .transition(1, &[linear(Prop::Color)]);
    });
    ui.render(WIDE);
    assert_eq!(span(&ui, 3), Some(0x0000_00FF));
    apply(&mut ui, |t| {
        t.place(4, 3, NIL);
    });
    ui.render(WIDE);
    assert_eq!(span(&ui, 3), Some(A), "moved under another color");
    apply(&mut ui, |t| {
        t.place(1, 3, NIL);
    });
    ui.set_time(0.0);
    apply(&mut ui, |t| {
        t.color(1, Some(0xFFFF_FFFF));
    });
    ui.set_time(0.5);
    ui.render(WIDE);
    assert_eq!(span(&ui, 3), Some(GREY));
    apply(&mut ui, |t| {
        t.create(7, NodeKind::Text)
            .paragraph(
                7,
                "new",
                &[TextSpan {
                    font_size: 16.0,
                    inherit_color: true,
                    ..TextSpan::default()
                }],
            )
            .place(1, 7, NIL);
    });
    ui.set_time(0.75);
    ui.render(WIDE);
    assert_eq!(span(&ui, 7), span(&ui, 3));
    assert_eq!(span(&ui, 3), Some(0xBFBF_BFFF));
}
