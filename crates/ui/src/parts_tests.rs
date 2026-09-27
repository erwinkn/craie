//! Transform parts through `Ui`: translate, rotate, scale and the free
//! matrix compose in CSS order about the border-box center, each part
//! tweens on its own (rotate by angle), variants set parts without
//! clobbering the others, a percent translate follows the node's size,
//! the hit test reads the composed matrix, and the wire carries parts.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::animation::{Prop, Timing, Transition, Value};
use crate::events::mask;
use crate::geom::Size;
use crate::host::{NodeId, Parts};
use crate::mutation::{Mutation, NodeKind, SpatialPatch, Transaction};
use crate::states::{TermDecl, Values, VariantDecl, state_bit, value_field};
use crate::ui::Ui;
use crate::wire;
use craie_core::geom::{Affine, Point};

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

fn linear(secs: f32) -> Timing {
    Timing::curve(secs, [0.0, 0.0, 1.0, 1.0])
}

fn at(ui: &mut Ui, t: f64) {
    ui.set_time(t);
    ui.render(VIEW);
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

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

/// Root 0 (400 × 300) centering box 1 (`w` × `h`), a hover scope.
fn centered(w: f32, h: f32) -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let root = taffy::Style {
        justify_content: Some(taffy::JustifyContent::CENTER),
        align_items: Some(taffy::AlignItems::CENTER),
        ..sized(400.0, 300.0)
    };
    t.create(0, NodeKind::View)
        .layout(0, &root)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(w, h))
        .fill(1, 0x0000_00FF)
        .interaction(1, mask::POINTER_ENTER_LEAVE, true)
        .states(1, 0)
        .place(0, 1, NIL);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    ui
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn near_m(a: Affine, b: Affine) -> bool {
    a.0.iter().zip(b.0).all(|(x, y)| near(*x, y))
}

fn parts(ui: &Ui, id: usize) -> Parts {
    ui.host.spatial[id].parts
}

fn hover(scale: f32) -> VariantDecl {
    VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER,
        }],
        env: 0,
        values: Values {
            mask: value_field::SCALE_X | value_field::SCALE_Y,
            parts: Parts {
                scale: [scale, scale],
                ..Parts::IDENTITY
            },
            ..Values::default()
        },
    }
}

/// translate(10, 20) rotate(90deg) scale(2, 3) matrix(shear): the
/// matrix applies first, the translate last. By hand: M = [1 1; 0 1],
/// S·M = [2 2; 0 3], R·S·M = [0 -3; 2 2], so (a, b, c, d) =
/// (0, 2, -3, 2) and (e, f) = (10, 20). The ops' order in the
/// transaction doesn't matter; the parts do.
#[test]
fn parts_compose_in_css_order() {
    let shear = Affine([1.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
    let want = Affine([0.0, 2.0, -3.0, 2.0, 10.0, 20.0]);
    let p = Parts {
        translate: [10.0, 20.0, 0.0, 0.0],
        rotate: FRAC_PI_2,
        scale: [2.0, 3.0],
        matrix: shear,
    };
    assert!(near_m(p.compose(), want), "{:?}", p.compose());
    // (1, 0): sheared (1, 0), scaled (2, 0), turned (0, 2), moved (10, 22).
    let q = p.compose().apply(Point::new(1.0, 0.0));
    assert!(near(q.x, 10.0) && near(q.y, 22.0), "{q:?}");

    let mut ui = centered(100.0, 40.0);
    apply(&mut ui, |t| {
        t.scale(1, 2.0, 3.0)
            .transform(1, shear)
            .translate(1, [10.0, 20.0, 0.0, 0.0])
            .rotate(1, FRAC_PI_2);
    });
    let s = ui.host.spatial[1];
    assert!(near_m(s.composed, want), "{:?}", s.composed);
    // About the center (50, 20): local = T(c) · want · T(-c).
    let local = s.local(Size {
        width: 100.0,
        height: 40.0,
    });
    assert!(near_m(local, want.about(Point::new(50.0, 20.0))));
}

/// Each part runs its own tween: rotate over 1 s, scale over 0.5 s, and
/// a translate set with no transition jumps without stopping either.
/// Translate tweens between points and a percentage component-wise.
#[test]
fn each_part_tweens_on_its_own() {
    let mut ui = centered(100.0, 40.0);
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[
                Transition {
                    prop: Prop::Rotate,
                    timing: linear(1.0),
                },
                Transition {
                    prop: Prop::Scale,
                    timing: linear(0.5),
                },
            ],
        );
    });
    apply(&mut ui, |t| {
        t.rotate(1, FRAC_PI_2).scale(1, 2.0, 3.0);
    });
    at(&mut ui, 0.25);
    let p = parts(&ui, 1);
    assert!(near(p.rotate, FRAC_PI_2 / 4.0), "{p:?}");
    assert!(near(p.scale[0], 1.5) && near(p.scale[1], 2.0), "{p:?}");
    apply(&mut ui, |t| {
        t.translate(1, [7.0, 0.0, 0.0, 0.0]);
    });
    assert_eq!(parts(&ui, 1).translate, [7.0, 0.0, 0.0, 0.0]);
    at(&mut ui, 0.5);
    let p = parts(&ui, 1);
    assert!(near(p.rotate, FRAC_PI_2 / 2.0), "{p:?}");
    assert_eq!(p.scale, [2.0, 3.0]);
    assert_eq!(p.translate, [7.0, 0.0, 0.0, 0.0]);
    assert!(ui.animating());
    at(&mut ui, 1.0);
    assert_eq!(parts(&ui, 1).rotate, FRAC_PI_2);
    assert!(!ui.animating());

    // 10pt -> 100%: halfway is 5pt + 50%.
    apply(&mut ui, |t| {
        t.translate(1, [10.0, 0.0, 0.0, 0.0]);
    });
    apply(&mut ui, |t| {
        t.animate(
            1,
            Prop::Translate,
            Value::Translate([0.0, 0.0, 1.0, 0.0]),
            linear(1.0),
        );
    });
    at(&mut ui, 1.5);
    let [x, y, fx, fy] = parts(&ui, 1).translate;
    assert!(near(x, 5.0) && near(fx, 0.5) && y == 0.0 && fy == 0.0);
}

/// Rotate tweens by angle: 0 to 360deg is a full turn, upside down at
/// the midpoint. (The free matrix can't: 0 and 360deg are one matrix.)
#[test]
fn rotate_tweens_a_full_turn() {
    let mut ui = centered(100.0, 40.0);
    apply(&mut ui, |t| {
        t.animate(1, Prop::Rotate, Value::Rotate(TAU), linear(1.0));
    });
    at(&mut ui, 0.5);
    let s = ui.host.spatial[1];
    assert!(near(s.parts.rotate, PI), "{:?}", s.parts);
    assert!(near_m(s.composed, Affine::rotate(PI)), "{:?}", s.composed);
    assert!(near(s.composed.0[0], -1.0));
    at(&mut ui, 1.0);
    assert_eq!(ui.host.spatial[1].parts.rotate, TAU);
}

/// The Pressable example: a base rotate, a hover scale with its own
/// transition. The variant moves scale alone; rotate stays at the base,
/// and an `animate` on translate runs alongside both.
#[test]
fn a_variant_scale_keeps_the_base_rotate() {
    let mut ui = centered(100.0, 40.0);
    let twelve = 12f32.to_radians();
    apply(&mut ui, |t| {
        t.rotate(1, twelve)
            .transition(
                1,
                &[Transition {
                    prop: Prop::Scale,
                    timing: linear(0.1),
                }],
            )
            .variants(1, &[hover(1.5)]);
    });
    at(&mut ui, 1.0);
    apply(&mut ui, |t| {
        t.animate(
            1,
            Prop::Translate,
            Value::Translate([20.0, 0.0, 0.0, 0.0]),
            linear(1.0),
        );
    });
    ui.dispatch(&crate::events::Event::PointerMove { x: 200.0, y: 150.0 });
    assert!(ui.state_bits(NodeId(1)) & state_bit::HOVER != 0);
    at(&mut ui, 1.05);
    let p = parts(&ui, 1);
    assert!(near(p.scale[0], 1.25) && near(p.scale[1], 1.25), "{p:?}");
    assert_eq!(p.rotate, twelve);
    assert!(near(p.translate[0], 1.0), "{p:?}");
    at(&mut ui, 1.2);
    let p = parts(&ui, 1);
    assert_eq!((p.scale, p.rotate), ([1.5, 1.5], twelve));
    ui.dispatch(&crate::events::Event::PointerMove { x: 5.0, y: 5.0 });
    at(&mut ui, 2.0);
    let p = parts(&ui, 1);
    assert_eq!(
        (p.scale, p.rotate, p.translate),
        ([1.0; 2], twelve, [20.0, 0.0, 0.0, 0.0])
    );
}

/// A 50% translate is half the node's own width, and follows the width.
#[test]
fn percent_translate_follows_the_size() {
    let mut ui = centered(100.0, 40.0);
    // Box 1 spans x 150..250; a 50% translate moves it to 200..300.
    apply(&mut ui, |t| {
        t.translate(1, [0.0, 0.0, 0.5, 0.0]);
    });
    at(&mut ui, 0.0);
    assert_eq!(ui.hit_test(160.0, 150.0), Some(NodeId(0)));
    assert_eq!(ui.hit_test(290.0, 150.0), Some(NodeId(1)));
    // 200 wide spans 100..300, translated by 100 to 200..400.
    apply(&mut ui, |t| {
        t.layout(1, &sized(200.0, 40.0));
    });
    at(&mut ui, 0.0);
    for (x, want) in [(190.0, 0), (210.0, 1), (390.0, 1)] {
        assert_eq!(ui.hit_test(x, 150.0), Some(NodeId(want)), "x {x}");
        assert_eq!(ui.hit_test(x, 150.0), ui.hit_test_walk(x, 150.0));
    }
}

/// A 100 × 20 box turned 90deg and scaled 2x stands 40 wide and 200
/// tall about its center (200, 150): x 180..220, y 50..250.
#[test]
fn hit_test_reads_rotate_and_scale() {
    let mut ui = centered(100.0, 20.0);
    apply(&mut ui, |t| {
        t.rotate(1, FRAC_PI_2).scale(1, 2.0, 2.0);
    });
    at(&mut ui, 0.0);
    for (x, y, want) in [
        (200.0, 55.0, 1),
        (200.0, 245.0, 1),
        (215.0, 150.0, 1),
        (225.0, 150.0, 0),
        (240.0, 150.0, 0),
        (200.0, 45.0, 0),
    ] {
        assert_eq!(ui.hit_test(x, y), Some(NodeId(want)), "({x}, {y})");
        assert_eq!(ui.hit_test(x, y), ui.hit_test_walk(x, y));
    }
    // The scene draws the same box: its rect's bounds turn with it.
    let drawn = ui.scene().resolve(&|r| r.0 as u64);
    let rect = drawn.iter().find(|r| r.kind == 0 && r.color == 0x0000_00FF);
    let b = rect.expect("box 1's fill").bounds;
    let got = [b.origin.x, b.origin.y, b.max_x(), b.max_y()];
    assert!(
        got.iter()
            .zip([180.0, 50.0, 220.0, 250.0])
            .all(|(a, b)| (a - b).abs() < 1e-3),
        "{got:?}"
    );
}

/// Parts survive the wire in ops, animations and variants; unknown
/// spatial bits reject.
#[test]
fn parts_round_trip_the_wire() {
    let mut t = Transaction::new(3);
    t.spatial(
        1,
        SpatialPatch {
            translate: Some([1.0, 2.0, 0.5, -0.25]),
            rotate: Some(0.3),
            scale: Some([1.5, 0.5]),
            matrix: Some(Affine([1.0, 0.0, 1.0, 1.0, 0.0, 0.0])),
            opacity: Some(0.5),
        },
    )
    .rotate(2, -TAU)
    .animate(
        1,
        Prop::Translate,
        Value::Translate([0.0, 1.0, 0.5, 0.0]),
        linear(0.2),
    )
    .animate(1, Prop::Rotate, Value::Rotate(PI), linear(0.2))
    .animate(1, Prop::Scale, Value::Scale([2.0, 3.0]), linear(0.2));
    let mut v = hover(1.02);
    v.values.mask |= value_field::PARTS;
    v.values.parts.translate = [3.0, 4.0, 0.5, 0.0];
    v.values.parts.rotate = 1.0;
    t.variants(1, &[v, hover(0.98)]);
    let buf = wire::encode(&t);
    let d = wire::decode(&buf).unwrap();
    assert_eq!(d.mutations, t.mutations);
    assert!(matches!(
        d.mutations[1],
        Mutation::Spatial {
            patch: SpatialPatch {
                rotate: Some(r),
                translate: None,
                ..
            },
            ..
        } if r == -TAU
    ));

    // A spatial mask bit past scale rejects. Node 1's op sets every
    // field but z: mask 0b11_1011.
    let mut bad = buf.clone();
    let at = bad
        .windows(6)
        .position(|w| w == [wire::op::SPATIAL, 1, 0, 0, 0, 0x3b])
        .expect("node 1's spatial op");
    bad[at + 5] |= 1 << 6;
    assert!(wire::decode(&bad).is_err());
}
