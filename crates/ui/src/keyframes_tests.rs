//! Keyframe animations through `Ui`: `enter` starts with the node's
//! creation only; an animation is its author's position in its list, so
//! entries around it come and go without restarting it; changed
//! keyframes restart it, changed timing applies in place, and a done one
//! never replays while declared. Variant animations start and stop with
//! their variant, a variant's transition list replaces the node's, an
//! animation runs over a transition's tween, nodes not drawn run
//! nothing, ends are reported, and the wire carries it all.

use std::f32::consts::TAU;
use std::sync::Arc;

use crate::animation::{Prop, Timing, Transition, end_reason};
use crate::events::{Event, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::keyframes::{Animation, Easing, Fill, Frame, Keyframes, Sample, Trigger};
use crate::mutation::{NodeKind, Transaction};
use crate::states::{TermDecl, Values, VariantDecl, state_bit, value_field};
use crate::ui::Ui;
use crate::wire;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

fn at(ui: &mut Ui, t: f64) {
    ui.set_time(t);
    ui.render(VIEW);
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
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

/// Root 0 (400 × 300) holding box 1 (100 × 100 at the origin), a hover
/// scope.
fn boxed() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &sized(400.0, 300.0))
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 100.0))
        .fill(1, 0x0000_00FF)
        .interaction(1, mask::POINTER_ENTER_LEAVE, true)
        .states(1, 0)
        .place(0, 1, NIL);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    ui
}

fn frame(at: f32, mask: u16, f: impl FnOnce(&mut Sample)) -> Frame {
    let mut values = Sample::default();
    f(&mut values);
    Frame {
        at,
        easing: None,
        mask,
        values,
    }
}

fn linear(frames: Vec<Frame>, secs: f32) -> Animation {
    Animation::new(Arc::new(Keyframes::new(frames)), secs, Easing::LINEAR)
}

/// `a` at position `index` in its author's list.
fn nth(index: u8, a: Animation) -> Animation {
    Animation { index, ..a }
}

/// A full turn every `secs`, forever.
fn spin(secs: f32) -> Animation {
    Animation {
        iterations: f32::INFINITY,
        ..linear(
            vec![
                frame(0.0, value_field::ROTATE, |s| s.rotate = 0.0),
                frame(1.0, value_field::ROTATE, |s| s.rotate = TAU),
            ],
            secs,
        )
    }
}

/// From opacity `o` to the underlying value.
fn fade_from(o: f32, secs: f32) -> Animation {
    linear(
        vec![frame(0.0, value_field::OPACITY, |s| s.opacity = o)],
        secs,
    )
}

fn hover(values: Values) -> VariantDecl {
    VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER,
        }],
        env: 0,
        values,
        ..Default::default()
    }
}

fn enter_box(ui: &mut Ui) {
    ui.dispatch(&Event::PointerMove { x: 50.0, y: 50.0 });
    assert!(ui.state_bits(NodeId(1)) & state_bit::HOVER != 0);
}

fn leave_box(ui: &mut Ui) {
    ui.dispatch(&Event::PointerMove { x: 300.0, y: 250.0 });
    assert!(ui.state_bits(NodeId(1)) & state_bit::HOVER == 0);
}

fn ends(ui: &mut Ui) -> Vec<(u32, u32, u32)> {
    ui.take_events()
        .into_iter()
        .filter(|e| e.kind == out_kind::ANIMATION_END)
        .map(|e| (e.node, e.key & !0xFF00, e.key >> 8 & 0xFF))
        .collect()
}

/// The target example's `enter`: from opacity 0 and 8 points down to
/// the declared 0.6 and no offset, in the creation batch only. A
/// re-send of the op later starts nothing.
#[test]
fn enter_runs_with_the_creation_batch_only() {
    let mut ui = boxed();
    let mut fade_up = linear(
        vec![frame(
            0.0,
            value_field::OPACITY | value_field::TRANSLATE_Y,
            |s| {
                s.opacity = 0.0;
                s.translate[1] = 8.0;
            },
        )],
        0.2,
    );
    fade_up.fill = Fill::Backwards;
    fade_up.delay = 0.1;
    ui.set_time(1.0);
    apply(&mut ui, |t| {
        // The op precedes the style: the declared opacity goes under.
        t.create(2, NodeKind::View)
            .animation(2, Trigger::Enter, false, std::slice::from_ref(&fade_up))
            .layout(2, &sized(10.0, 10.0))
            .opacity(2, 0.6)
            .place(0, 2, NIL);
    });
    let s = |ui: &Ui| ui.host.spatial[2];
    // The backwards fill holds the first frame through the delay.
    at(&mut ui, 1.05);
    assert_eq!((s(&ui).opacity, s(&ui).parts.translate[1]), (0.0, 8.0));
    at(&mut ui, 1.2);
    assert!(near(s(&ui).opacity, 0.3) && near(s(&ui).parts.translate[1], 4.0));
    at(&mut ui, 1.31);
    assert_eq!((s(&ui).opacity, s(&ui).parts.translate[1]), (0.6, 0.0));
    assert_eq!(ui.motion.node_count(), 0);
    assert!(!ui.animating());

    apply(&mut ui, |t| {
        t.animation(2, Trigger::Enter, false, &[fade_up]);
    });
    assert_eq!(ui.motion.live(), 0);
    at(&mut ui, 1.4);
    assert_eq!(s(&ui).opacity, 0.6);
}

/// An animation keeps running through an equal re-send, takes changed
/// timing in place (CSS), restarts on changed keyframes, and without a
/// fill lets go at once when it ends.
#[test]
fn a_list_restarts_on_changed_keyframes_only() {
    let mut ui = boxed();
    let o = |ui: &Ui| ui.host.spatial[1].opacity;
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.0, 1.0)]);
    });
    at(&mut ui, 0.5);
    assert!(near(o(&ui), 0.5));
    // An equal list (a fresh `Arc`, equal frames) is the same animation.
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.0, 1.0)]);
    });
    at(&mut ui, 0.75);
    assert!(near(o(&ui), 0.75));
    assert_eq!(ends(&mut ui), []);
    // New timing applies in place: 2 s from the same start.
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.0, 2.0)]);
    });
    at(&mut ui, 1.0);
    assert!(near(o(&ui), 0.5));
    assert_eq!(ends(&mut ui), []);
    // New keyframes restart it from its first frame.
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.2, 1.0)]);
    });
    let key = 1 << 17;
    assert_eq!(ends(&mut ui), [(1, key, end_reason::RETARGETED)]);
    at(&mut ui, 1.5);
    assert!(near(o(&ui), 0.6));
    at(&mut ui, 2.75);
    assert_eq!(o(&ui), 1.0);
    assert_eq!(ends(&mut ui), [(1, key, end_reason::FINISHED)]);
    assert!(!ui.animating());

    // An empty list stops it, reported as cancelled; the value returns
    // at once.
    apply(&mut ui, |t| {
        t.opacity(1, 0.8)
            .animation(1, Trigger::Base, true, &[fade_from(0.0, 1.0)]);
    });
    at(&mut ui, 2.75);
    assert_eq!(o(&ui), 0.0);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[]);
    });
    assert_eq!(o(&ui), 0.8);
    assert_eq!(ends(&mut ui), [(1, key, end_reason::CANCELLED)]);

    // Removed with its node; a loop is never reported.
    apply(&mut ui, |t| {
        t.animation(
            1,
            Trigger::Base,
            true,
            &[fade_from(0.0, 1.0), nth(1, spin(1.0))],
        );
    });
    apply(&mut ui, |t| {
        t.remove(1);
    });
    assert_eq!(ends(&mut ui), [(1, key, end_reason::REMOVED)]);
    assert_eq!(ui.motion.node_count(), 0);
}

/// A running animation holds the node's transform record and opacity
/// layer through identity and opacity 1 (a loop's iteration boundaries),
/// so its frames change no draw topology; the hold goes when it ends.
#[test]
fn a_loop_holds_its_record_and_layer() {
    let mut ui = boxed();
    let held = |ui: &Ui| {
        let s = &ui.host.spatial[1];
        (s.transformed(), s.layered())
    };
    let pulse = Animation {
        iterations: f32::INFINITY,
        ..linear(
            vec![frame(0.5, value_field::OPACITY, |s| s.opacity = 0.5)],
            1.0,
        )
    };
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[spin(1.0), nth(1, pulse)]);
    });
    at(&mut ui, 2.0);
    let s = ui.host.spatial[1];
    assert_eq!((s.parts.rotate, s.opacity), (0.0, 1.0));
    assert_eq!(held(&ui), (true, true));
    let structure = ui.host.revs.structure;
    at(&mut ui, 2.5);
    at(&mut ui, 3.0);
    assert_eq!(ui.host.revs.structure, structure);

    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[fade_from(0.5, 1.0)]);
    });
    at(&mut ui, 3.5);
    assert_eq!(held(&ui), (false, true));
    // At its end it lets go: opacity 1, no layer.
    at(&mut ui, 4.0);
    assert_eq!(held(&ui), (false, false));
    // A finite one holds too, through 100 iterations of its boundaries.
    let blink = Animation {
        iterations: 100.0,
        ..linear(
            vec![frame(0.5, value_field::OPACITY, |s| s.opacity = 0.5)],
            1.0,
        )
    };
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[blink]);
    });
    at(&mut ui, 5.0);
    assert_eq!(
        (ui.host.spatial[1].opacity, held(&ui)),
        (1.0, (false, true))
    );
    at(&mut ui, 104.0);
    assert_eq!(held(&ui), (false, false));
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[spin(1.0)]);
    });
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[]);
    });
    assert_eq!(held(&ui), (false, false));
}

/// A forwards fill holds the last frame after the end; the node's own
/// value still changes underneath and shows when the list goes.
#[test]
fn a_forwards_fill_holds_over_the_value() {
    let mut ui = boxed();
    let o = |ui: &Ui| ui.host.spatial[1].opacity;
    let mut hold = linear(
        vec![
            frame(0.0, value_field::OPACITY, |s| s.opacity = 1.0),
            frame(1.0, value_field::OPACITY, |s| s.opacity = 0.2),
        ],
        1.0,
    );
    hold.fill = Fill::Forwards;
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[hold]);
    });
    at(&mut ui, 2.0);
    assert_eq!(o(&ui), 0.2);
    assert!(!ui.animating(), "a holding fill needs no frames");
    apply(&mut ui, |t| {
        t.opacity(1, 0.7);
    });
    at(&mut ui, 2.1);
    assert_eq!(o(&ui), 0.2);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[]);
    });
    assert_eq!(o(&ui), 0.7);
}

/// The target example's `_streaming`-style variant animation, here on
/// hover: native starts it when the variant holds and stops it when it
/// lets go, the rotation back at its value at once.
#[test]
fn variant_animations_follow_their_variant() {
    let mut ui = boxed();
    let spinning = VariantDecl {
        animations: vec![spin(1.0)],
        ..hover(Values::default())
    };
    apply(&mut ui, |t| {
        t.rotate(1, 0.5).variants(1, &[spinning]);
    });
    at(&mut ui, 1.0);
    assert_eq!(ui.motion.live(), 0);
    enter_box(&mut ui);
    at(&mut ui, 1.0);
    at(&mut ui, 1.25);
    assert!(near(ui.host.spatial[1].parts.rotate, TAU / 4.0));
    at(&mut ui, 3.5);
    assert!(near(ui.host.spatial[1].parts.rotate, TAU / 2.0));
    leave_box(&mut ui);
    at(&mut ui, 3.6);
    assert_eq!(ui.host.spatial[1].parts.rotate, 0.5);
    assert_eq!(ui.motion.node_count(), 0);
    assert!(!ui.animating());
    // Variant ends are not reported (DF-56).
    assert_eq!(ends(&mut ui), []);
}

/// DF-22: moving into a variant uses its transition, moving out the
/// base's. Hover scales to 2 in 0.1 s; leaving takes the base's 1 s.
#[test]
fn transitions_come_from_the_entered_style() {
    let mut ui = boxed();
    let scale = |ui: &Ui| ui.host.spatial[1].parts.scale[0];
    let grow = VariantDecl {
        transitions: Some(vec![Transition {
            prop: Prop::Scale,
            timing: Timing::curve(0.1, [0.0, 0.0, 1.0, 1.0]),
        }]),
        ..hover(Values {
            mask: value_field::SCALE_X | value_field::SCALE_Y,
            parts: crate::host::Parts {
                scale: [2.0, 2.0],
                ..crate::host::Parts::IDENTITY
            },
            ..Values::default()
        })
    };
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[Transition {
                prop: Prop::Scale,
                timing: Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
            }],
        )
        .variants(1, &[grow]);
    });
    at(&mut ui, 1.0);
    enter_box(&mut ui);
    at(&mut ui, 1.0);
    at(&mut ui, 1.05);
    assert!(near(scale(&ui), 1.5), "{}", scale(&ui));
    at(&mut ui, 1.1);
    assert_eq!(scale(&ui), 2.0);
    ui.set_time(2.0);
    leave_box(&mut ui);
    at(&mut ui, 2.5);
    assert!(near(scale(&ui), 1.5), "{}", scale(&ui));
    at(&mut ui, 3.0);
    assert_eq!(scale(&ui), 1.0);
}

/// Rotate loops while scale tweens on the same node: separate parts,
/// neither disturbs the other. And an animation over a transitioned
/// property wins, sampling over the tween: its implicit end frame
/// follows the tweening value.
#[test]
fn animations_run_over_transitions() {
    let mut ui = boxed();
    let p = |ui: &Ui| ui.host.spatial[1].parts;
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[
                Transition {
                    prop: Prop::Scale,
                    timing: Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
                },
                Transition {
                    prop: Prop::Opacity,
                    timing: Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
                },
            ],
        )
        .animation(1, Trigger::Base, false, &[spin(4.0)]);
    });
    at(&mut ui, 0.0);
    apply(&mut ui, |t| {
        t.scale(1, 2.0, 2.0);
    });
    at(&mut ui, 0.5);
    assert!(near(p(&ui).scale[0], 1.5) && near(p(&ui).rotate, TAU / 8.0));
    at(&mut ui, 1.0);
    assert!(near(p(&ui).scale[0], 2.0) && near(p(&ui).rotate, TAU / 4.0));

    // Opacity tweens 1 -> 0 over [1, 2]; the fade from 0 samples over it.
    apply(&mut ui, |t| {
        t.opacity(1, 0.0).animation(
            1,
            Trigger::Base,
            false,
            &[spin(4.0), nth(1, fade_from(0.0, 1.0))],
        );
    });
    at(&mut ui, 1.5);
    // Under 0.5, halfway from 0: 0.25.
    assert!(near(ui.host.spatial[1].opacity, 0.25));
    at(&mut ui, 2.0);
    assert_eq!(ui.host.spatial[1].opacity, 0.0);
    // The spin kept its start through the list change (same position,
    // same keyframes).
    assert!(near(p(&ui).rotate, TAU / 2.0));
}

/// Later wins per channel: the node's list over `enter`, a variant over
/// the list.
#[test]
fn later_animations_win_per_channel() {
    let mut ui = boxed();
    let o = |ui: &Ui| ui.host.spatial[1].opacity;
    let dim = VariantDecl {
        animations: vec![Animation {
            iterations: f32::INFINITY,
            ..linear(
                vec![
                    frame(0.0, value_field::OPACITY, |s| s.opacity = 0.1),
                    frame(1.0, value_field::OPACITY, |s| s.opacity = 0.1),
                ],
                1.0,
            )
        }],
        ..hover(Values::default())
    };
    apply(&mut ui, |t| {
        t.animation(
            1,
            Trigger::Base,
            false,
            &[Animation {
                iterations: f32::INFINITY,
                ..fade_from(0.5, 1.0)
            }],
        )
        .variants(1, &[dim]);
    });
    at(&mut ui, 0.0);
    assert_eq!(o(&ui), 0.5);
    enter_box(&mut ui);
    at(&mut ui, 0.5);
    assert_eq!(o(&ui), 0.1);
    leave_box(&mut ui);
    at(&mut ui, 0.5);
    assert!(near(o(&ui), 0.75));
}

/// A looping animation keeps frames coming; the reduced-motion setting
/// reaches JS as an environment event.
#[test]
fn reduced_motion_reaches_js() {
    let mut ui = boxed();
    ui.take_events();
    ui.set_reduced_motion(true);
    ui.set_reduced_motion(true);
    let env: Vec<u32> = ui
        .take_events()
        .into_iter()
        .filter(|e| e.kind == out_kind::ENVIRONMENT)
        .map(|e| e.key)
        .collect();
    let bit = crate::states::env_bit::REDUCED_MOTION as u32;
    assert_eq!(env.len(), 1);
    assert!(env[0] & bit != 0);
}

/// The ops round-trip: `ANIMATION` with shared keyframes interned once
/// per transaction, and variants carrying transitions and animations.
#[test]
fn animation_ops_round_trip_and_validate() {
    let mut shared = spin(1.0);
    shared.easing = Easing::spring(170.0, 26.0, 1.0);
    shared = Animation::new(shared.keyframes.clone(), 0.0, shared.easing.clone());
    let mut stepped = fade_from(0.0, 0.3);
    stepped.easing = Easing::Steps { n: 4, jump: 1 };
    stepped.delay = 0.1;
    stepped.iterations = 2.5;
    stepped.direction = crate::keyframes::Direction::Alternate;
    stepped.fill = Fill::Both;
    let mut bumpy = linear(
        vec![
            frame(0.0, value_field::FILL | value_field::COLOR, |s| {
                s.fill = 0xFF00_00FF;
                s.color = Some(0x00FF_00FF);
            }),
            Frame {
                easing: Some(Easing::Linear(
                    vec![[0.0, 0.0], [0.5, 0.9], [1.0, 1.0]].into(),
                )),
                ..frame(
                    0.5,
                    value_field::TRANSLATE_X | value_field::SCALE_X | value_field::BORDER_COLOR,
                    |s| {
                        s.translate = [4.0, 0.0, 0.5, 0.0];
                        s.scale[0] = 1.2;
                        s.border = 0x1234_5678;
                    },
                )
            },
        ],
        0.4,
    );
    bumpy.easing = Easing::Bezier([0.23, 1.0, 0.32, 1.0]);
    let variant = VariantDecl {
        transitions: Some(vec![Transition {
            prop: Prop::Scale,
            timing: Timing::curve(0.12, [0.0, 0.0, 1.0, 1.0]),
        }]),
        animations: vec![shared.clone()],
        ..hover(Values {
            mask: value_field::OPACITY,
            opacity: 0.5,
            ..Values::default()
        })
    };
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View)
        .animation(1, Trigger::Enter, false, &[stepped, nth(1, bumpy)])
        .animation(1, Trigger::Base, true, &[shared.clone(), nth(3, shared)])
        .variants(1, &[variant]);
    let buf = wire::encode(&t);
    let keyframes = buf.iter().filter(|&&b| b == wire::op::KEYFRAMES).count();
    assert!(keyframes >= 3);
    let decoded = wire::decode(&buf).unwrap();
    assert_eq!(decoded.mutations, t.mutations);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap_or_else(|e| panic!("{e:?}"));

    // Out of range: rejected whole, before anything applies.
    let bad: Vec<Animation> = vec![
        Animation {
            duration: f32::NAN,
            ..fade_from(0.0, 1.0)
        },
        Animation {
            iterations: -1.0,
            ..fade_from(0.0, 1.0)
        },
        Animation {
            easing: Easing::Steps { n: 0, jump: 1 },
            ..fade_from(0.0, 1.0)
        },
        linear(
            vec![frame(0.0, value_field::OPACITY, |s| s.opacity = 2.0)],
            1.0,
        ),
    ];
    for (k, a) in bad.into_iter().enumerate() {
        let mut t = Transaction::new(ui.seq + 1);
        t.animation(1, Trigger::Base, false, &[a]);
        assert!(ui.apply_txn(&t).is_err(), "case {k}");
        assert!(ui.apply(&wire::encode(&t)).is_err(), "case {k} on the wire");
    }
    let many = vec![fade_from(0.0, 1.0); crate::keyframes::MAX_ANIMATIONS + 1];
    let mut t = Transaction::new(ui.seq + 1);
    t.animation(1, Trigger::Base, false, &many);
    assert!(ui.apply_txn(&t).is_err());
    // A paint channel needs a box: text has none.
    let mut t = Transaction::new(ui.seq + 1);
    t.create(9, NodeKind::Text).animation(
        9,
        Trigger::Base,
        false,
        &[linear(
            vec![frame(0.0, value_field::FILL, |s| s.fill = 0)],
            1.0,
        )],
    );
    assert!(ui.apply_txn(&t).is_err());
}

/// M1: an animation is its position in the author's list. In
/// `[busy && fade, spin]`, `busy` going false leaves the spin's phase
/// alone (so does a reduced-motion drop: the same op); a variant block
/// appearing before a looping one leaves its loop alone.
#[test]
fn entries_keep_their_phase_as_others_come_and_go() {
    let mut ui = boxed();
    let r = |ui: &Ui| ui.host.spatial[1].parts.rotate;
    apply(&mut ui, |t| {
        t.animation(
            1,
            Trigger::Base,
            false,
            &[fade_from(0.0, 10.0), nth(1, spin(4.0))],
        );
    });
    at(&mut ui, 1.0);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[nth(1, spin(4.0))]);
    });
    at(&mut ui, 2.0);
    assert!(near(r(&ui), TAU / 2.0), "{}", r(&ui));
    assert_eq!(ui.host.spatial[1].opacity, 1.0);

    // Variants: the loop's block is its identity, not its place in the
    // list sent (a block with nothing to send is left out).
    let looping = VariantDecl {
        block: 1,
        animations: vec![spin(4.0)],
        ..hover(Values::default())
    };
    let pressed = VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::PRESSED,
        }],
        block: 0,
        ..hover(Values {
            mask: value_field::OPACITY,
            opacity: 0.5,
            ..Values::default()
        })
    };
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, false, &[])
            .rotate(1, 0.0)
            .variants(1, std::slice::from_ref(&looping));
    });
    enter_box(&mut ui);
    at(&mut ui, 2.0);
    at(&mut ui, 3.0);
    assert!(near(r(&ui), TAU / 4.0), "{}", r(&ui));
    apply(&mut ui, |t| {
        t.variants(1, &[pressed, looping]);
    });
    at(&mut ui, 4.0);
    assert!(near(r(&ui), TAU / 2.0), "{}", r(&ui));
}

/// M2: a finished animation stays done while it is declared: (a) a
/// restyle of a variant that still holds, (b) a list that grows around
/// it and (c) a timing-only re-send (what a reduced-motion flip sends)
/// replay nothing and report no second end. It goes with its entry or
/// its variant; changed keyframes restart it.
#[test]
fn a_done_animation_never_replays_while_declared() {
    let mut ui = boxed();
    let o = |ui: &Ui| ui.host.spatial[1].opacity;
    // (a) Hover bumps once; an app state flipping on the scope restyles
    // the node while hover holds.
    let bump = VariantDecl {
        animations: vec![fade_from(0.0, 0.2)],
        ..hover(Values::default())
    };
    apply(&mut ui, |t| {
        t.variants(1, &[bump]);
    });
    enter_box(&mut ui);
    at(&mut ui, 0.1);
    assert!(near(o(&ui), 0.5));
    at(&mut ui, 0.3);
    assert_eq!((o(&ui), ui.motion.live()), (1.0, 0));
    apply(&mut ui, |t| {
        t.states(1, 1);
    });
    at(&mut ui, 0.4);
    assert_eq!((o(&ui), ui.motion.live()), (1.0, 0));
    // Leaving drops it; entering again plays it again.
    leave_box(&mut ui);
    at(&mut ui, 0.5);
    assert_eq!(ui.motion.node_count(), 0);
    enter_box(&mut ui);
    at(&mut ui, 0.6);
    assert!(near(o(&ui), 0.5));
    leave_box(&mut ui);
    apply(&mut ui, |t| {
        t.variants(1, &[]);
    });

    // (b) `[shake]` ends, then the list becomes `[shake, spin]`.
    let shake = || fade_from(0.0, 1.0);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[shake()]);
    });
    at(&mut ui, 2.0);
    assert_eq!(ends(&mut ui), [(1, 1 << 17, end_reason::FINISHED)]);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[shake(), nth(1, spin(4.0))]);
    });
    at(&mut ui, 2.5);
    assert_eq!(o(&ui), 1.0);

    // (c) Reduced motion on: the done one-shot goes as 0 s, then back
    // as 1 s; neither plays it or reports it again.
    for secs in [0.0, 1.0] {
        apply(&mut ui, |t| {
            t.animation(
                1,
                Trigger::Base,
                true,
                &[fade_from(0.0, secs), nth(1, spin(4.0))],
            );
        });
        at(&mut ui, 2.6);
        assert_eq!(o(&ui), 1.0);
    }
    assert_eq!(ends(&mut ui), []);
    // A running one-shot re-sent as 0 s ends once, finished.
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.5, 1.0)]);
    });
    at(&mut ui, 2.7);
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Base, true, &[fade_from(0.5, 0.0)]);
    });
    at(&mut ui, 2.8);
    assert_eq!(o(&ui), 1.0);
    assert_eq!(ends(&mut ui), [(1, 1 << 17, end_reason::FINISHED)]);
}

/// m2: a node that is not drawn (under `display: none`, detached) runs
/// nothing and needs no frames; drawn again, its animations start over.
#[test]
fn animations_run_only_on_drawn_nodes() {
    let mut ui = boxed();
    apply(&mut ui, |t| {
        t.create(2, NodeKind::View)
            .layout(2, &sized(10.0, 10.0))
            .animation(
                2,
                Trigger::Base,
                true,
                &[spin(4.0), nth(1, fade_from(0.0, 10.0))],
            )
            .place(1, 2, NIL);
    });
    let r = |ui: &Ui| ui.host.spatial[2].parts.rotate;
    at(&mut ui, 1.0);
    assert!(near(r(&ui), TAU / 4.0));
    ui.take_events();
    // `hidden` on the parent: display none.
    let hide = |t: &mut Transaction<'static>, on: bool| {
        let mut s = sized(100.0, 100.0);
        s.display = if on {
            taffy::Display::None
        } else {
            taffy::Display::Flex
        };
        t.layout(1, &s);
    };
    apply(&mut ui, |t| hide(t, true));
    at(&mut ui, 1.5);
    assert_eq!(ui.motion.live(), 0);
    assert!(!ui.animating());
    // The running one-shot is cancelled; its values go back.
    assert_eq!(ends(&mut ui), [(2, 1 | 1 << 17, end_reason::CANCELLED)]);
    assert_eq!((r(&ui), ui.host.spatial[2].opacity), (0.0, 1.0));
    ui.set_time(3.0);
    ui.render(VIEW);
    assert!(!ui.animating());
    // Shown at 5: both start over.
    ui.set_time(5.0);
    apply(&mut ui, |t| hide(t, false));
    at(&mut ui, 5.0);
    at(&mut ui, 6.0);
    assert!(near(r(&ui), TAU / 4.0), "{}", r(&ui));
    assert!(near(ui.host.spatial[2].opacity, 0.1));
    assert_eq!(ui.motion.live(), 2);
    // Detached: not drawn either.
    apply(&mut ui, |t| {
        t.detach(2);
    });
    at(&mut ui, 6.5);
    assert_eq!(ui.motion.live(), 0);
}

/// Two active variants animating one property: the more specific wins,
/// whatever the order declared.
#[test]
fn the_more_specific_variant_animation_wins() {
    let mut ui = boxed();
    let hold = |o: f32| Animation {
        iterations: f32::INFINITY,
        ..linear(
            vec![
                frame(0.0, value_field::OPACITY, |s| s.opacity = o),
                frame(1.0, value_field::OPACITY, |s| s.opacity = o),
            ],
            1.0,
        )
    };
    // Hover and app state 0: two bits, declared first.
    let specific = VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER | 1,
        }],
        block: 0,
        animations: vec![hold(0.1)],
        ..hover(Values::default())
    };
    let plain = VariantDecl {
        block: 1,
        animations: vec![hold(0.9)],
        ..hover(Values::default())
    };
    apply(&mut ui, |t| {
        t.states(1, 1).variants(1, &[specific, plain]);
    });
    enter_box(&mut ui);
    at(&mut ui, 0.5);
    assert_eq!(ui.host.spatial[1].opacity, 0.1);
    apply(&mut ui, |t| {
        t.states(1, 0);
    });
    at(&mut ui, 0.6);
    assert_eq!(ui.host.spatial[1].opacity, 0.9);
}

/// m1: a variant's transition list replaces the node's while it holds
/// (CSS): hover times scale only, so an opacity change during hover
/// jumps; an empty list times nothing; leaving uses the node's list.
#[test]
fn a_variant_transition_list_replaces_the_nodes() {
    let mut ui = boxed();
    let s = |ui: &Ui| {
        (
            ui.host.spatial[1].parts.scale[0],
            ui.host.spatial[1].opacity,
        )
    };
    let curve = |prop, secs| Transition {
        prop,
        timing: Timing::curve(secs, [0.0, 0.0, 1.0, 1.0]),
    };
    let values = Values {
        mask: value_field::SCALE_X | value_field::SCALE_Y,
        parts: crate::host::Parts {
            scale: [2.0, 2.0],
            ..crate::host::Parts::IDENTITY
        },
        ..Values::default()
    };
    let grow = |list: Vec<Transition>| VariantDecl {
        transitions: Some(list),
        ..hover(values)
    };
    apply(&mut ui, |t| {
        t.transition(1, &[curve(Prop::Scale, 1.0), curve(Prop::Opacity, 1.0)])
            .variants(1, &[grow(vec![curve(Prop::Scale, 0.5)])]);
    });
    at(&mut ui, 1.0);
    enter_box(&mut ui);
    at(&mut ui, 1.0);
    apply(&mut ui, |t| {
        t.opacity(1, 0.0);
    });
    at(&mut ui, 1.25);
    assert!(near(s(&ui).0, 1.5) && s(&ui).1 == 0.0, "{:?}", s(&ui));
    // Leaving: the node's 1 s for both.
    at(&mut ui, 2.0);
    leave_box(&mut ui);
    apply(&mut ui, |t| {
        t.opacity(1, 1.0);
    });
    at(&mut ui, 2.5);
    assert!(near(s(&ui).0, 1.5) && near(s(&ui).1, 0.5), "{:?}", s(&ui));
    // An empty list: nothing transitions into the variant.
    at(&mut ui, 4.0);
    apply(&mut ui, |t| {
        t.variants(1, &[grow(vec![])]);
    });
    enter_box(&mut ui);
    at(&mut ui, 4.0);
    assert_eq!(s(&ui).0, 2.0);
}

/// m7: a negative delay starts partway through (CSS), down to -600 s.
#[test]
fn a_negative_delay_starts_partway() {
    let mut ui = boxed();
    apply(&mut ui, |t| {
        t.animation(
            1,
            Trigger::Base,
            false,
            &[Animation {
                delay: -0.25,
                ..fade_from(0.0, 1.0)
            }],
        );
    });
    at(&mut ui, 0.0);
    assert!(near(ui.host.spatial[1].opacity, 0.25));
    at(&mut ui, 0.75);
    assert_eq!(ui.motion.live(), 0);
    let with = |delay| Animation {
        delay,
        ..fade_from(0.0, 1.0)
    };
    assert!(with(-600.0).is_valid());
    assert_eq!(with(-600.5).check(), Err("animation delay out of range"));
    assert_eq!(with(600.5).check(), Err("animation delay out of range"));
}

/// m6: the environment goes to JS once when the session starts (its
/// first transaction), so a host learns a setting made before it, then
/// on each reduced-motion change.
#[test]
fn the_session_starts_with_the_environment() {
    let env = |ui: &mut Ui| -> Vec<u32> {
        ui.take_events()
            .into_iter()
            .filter(|e| e.kind == out_kind::ENVIRONMENT)
            .map(|e| e.key)
            .collect()
    };
    let bit = crate::states::env_bit::REDUCED_MOTION as u32;
    let mut ui = Ui::new(1.0);
    ui.set_reduced_motion(true);
    assert_eq!(env(&mut ui), []);
    apply(&mut ui, |t| {
        t.create(0, NodeKind::View).place(NIL, 0, NIL);
    });
    assert_eq!(env(&mut ui), [bit]);
    apply(&mut ui, |t| {
        t.opacity(0, 0.5);
    });
    assert_eq!(env(&mut ui), []);
    ui.set_reduced_motion(false);
    assert_eq!(env(&mut ui), [0]);
}

/// m4: each malformed input says what is wrong.
#[test]
fn malformed_keyframes_say_why() {
    let o = |at: f32, v: f32| frame(at, value_field::OPACITY, |s| s.opacity = v);
    let check = |frames: Vec<Frame>| linear(frames, 1.0).check();
    assert_eq!(
        check(vec![o(0.5, 0.0), o(0.2, 1.0)]),
        Err("keyframe offsets out of order")
    );
    assert_eq!(
        check(vec![o(1.5, 0.0)]),
        Err("keyframe offset outside [0, 1]")
    );
    assert_eq!(check(vec![]), Err("keyframes without a frame"));
    assert_eq!(
        check(vec![
            frame(0.0, value_field::ROTATE, |s| s.rotate = f32::NAN)
        ]),
        Err("keyframe value not finite")
    );
    assert_eq!(
        check(vec![o(0.0, 2.0)]),
        Err("keyframe opacity outside [0, 1]")
    );
    // Through the executor, with the text.
    let mut ui = boxed();
    let mut t = Transaction::new(ui.seq + 1);
    t.animation(1, Trigger::Base, false, &[linear(vec![], 1.0)]);
    assert_eq!(
        ui.apply_txn(&t),
        Err(wire::WireError::Invalid("keyframes without a frame"))
    );
    // Indices must rise: they are identities.
    let mut t = Transaction::new(ui.seq + 1);
    t.animation(1, Trigger::Base, false, &[spin(1.0), spin(2.0)]);
    assert_eq!(
        ui.apply_txn(&t),
        Err(wire::WireError::Invalid("animation indices out of order"))
    );
}

/// The Rust encoder refuses a keyframes table index past u16.
#[test]
#[should_panic(expected = "65,536 keyframes lists")]
fn the_encoder_checks_its_keyframes_table() {
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View);
    let list: Vec<Animation> = (0..=u16::MAX as usize + 1)
        .map(|i| fade_from(i as f32 / 65537.0, 1.0))
        .collect();
    for chunk in list.chunks(16) {
        let chunk: Vec<Animation> = chunk
            .iter()
            .enumerate()
            .map(|(i, a)| nth(i as u8, a.clone()))
            .collect();
        t.animation(1, Trigger::Base, false, &chunk);
    }
    wire::encode(&t);
}
