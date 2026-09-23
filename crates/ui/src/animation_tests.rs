//! The animation driver through transactions: tweens write the rows
//! each frame, transform and opacity tweens neither lay out nor shape,
//! layout tweens reflow siblings, `auto` targets resolve by one probe
//! layout, and the wire carries both ops.

use crate::animation::{Prop, Timing, Transition, Value};
use crate::geom::Size;
use crate::host::NodeId;
use crate::mutation::{NodeKind, Transaction};
use crate::ui::Ui;
use crate::wire;
use craie_core::geom::Affine;

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

/// A row container (0, 300 wide) holding box 1 (100x40) and box 2
/// (50x40), and a text (3).
fn row_ui() -> Ui {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let row = taffy::Style {
        flex_direction: taffy::FlexDirection::Row,
        align_items: Some(taffy::AlignItems::START),
        size: taffy::Size {
            width: taffy::Dimension::length(300.0),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::View)
        .layout(0, &row)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 40.0))
        .fill(1, 0x0000_00FF)
        .place(0, 1, NIL);
    t.create(2, NodeKind::View)
        .layout(2, &sized(50.0, 40.0))
        .fill(2, 0xFFFF_FFFF)
        .place(0, 2, NIL);
    t.create(3, NodeKind::Text)
        .text(3, "moving words", 16.0, 0xFFFF_FFFF)
        .place(0, 3, NIL);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    ui
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

/// A transaction-building case.
type Case = Box<dyn Fn(&mut Transaction<'static>)>;

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

/// Transform and opacity tweens write the spatial row each frame and do
/// zero layouts and zero shapes (ARCHITECTURE.md section 12).
#[test]
fn spatial_tweens_neither_lay_out_nor_shape() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.transition(
            3,
            &[
                Transition {
                    prop: Prop::Opacity,
                    timing: linear(1.0),
                },
                Transition {
                    prop: Prop::Transform,
                    timing: linear(1.0),
                },
            ],
        );
    });
    at(&mut ui, 0.0);
    apply(&mut ui, |t| {
        t.opacity(3, 0.0)
            .transform(3, Affine::translate(100.0, 0.0));
    });
    // The rows keep the value on screen until a frame advances them.
    assert_eq!(ui.host.spatial[3].opacity, 1.0);
    let before = ui.counters();
    for (k, time) in [0.25, 0.5, 0.75].into_iter().enumerate() {
        assert!(ui.needs_paint(), "a running animation owes frames");
        at(&mut ui, time);
        let s = ui.host.spatial[3];
        let want = 0.25 * (k + 1) as f32;
        assert!(near(s.opacity, 1.0 - want), "{time}: {}", s.opacity);
        assert!(
            near(s.transform.0[4], 100.0 * want),
            "{time}: {:?}",
            s.transform
        );
    }
    let spent = ui.counters().since(&before);
    assert_eq!((spent.layout_passes, spent.shapes), (0, 0));
    at(&mut ui, 1.0);
    assert_eq!(ui.host.spatial[3].opacity, 0.0);
    assert_eq!(ui.host.spatial[3].transform, Affine::translate(100.0, 0.0));
    assert!(!ui.animating());
    let spent = ui.counters().since(&before);
    assert_eq!((spent.layout_passes, spent.shapes), (0, 0));
}

/// A width tween relayouts each frame: the sibling after it moves.
#[test]
fn width_tween_reflows_siblings() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[Transition {
                prop: Prop::Width,
                timing: linear(1.0),
            }],
        );
    });
    apply(&mut ui, |t| {
        t.layout(1, &sized(200.0, 40.0));
    });
    at(&mut ui, 0.5);
    let x2 = |ui: &Ui| ui.layouts.data(NodeId(2)).rect.origin.x;
    assert!(near(ui.layouts.data(NodeId(1)).rect.size.width, 150.0));
    assert!(near(x2(&ui), 150.0));
    at(&mut ui, 1.0);
    assert!(near(x2(&ui), 200.0));
    assert!(!ui.animating());
    assert_eq!(ui.host.layout[1], sized(200.0, 40.0));
}

/// A tween to `auto` finds its end by one probe layout and restores
/// `auto` on the final frame; one from `auto` starts at the laid-out
/// size.
#[test]
fn size_tweens_to_and_from_auto() {
    let mut ui = Ui::new(1.0);
    let column = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::length(300.0),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    };
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 40.0))
        .transition(
            1,
            &[Transition {
                prop: Prop::Width,
                timing: linear(1.0),
            }],
        )
        .place(0, 1, NIL);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    let auto_width = taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::length(40.0),
        },
        ..taffy::Style::default()
    };
    apply(&mut ui, |t| {
        t.layout(1, &auto_width);
    });
    let width = |ui: &Ui| ui.layouts.data(NodeId(1)).rect.size.width;
    at(&mut ui, 0.5);
    // Stretched in a 300-wide column: auto resolves to 300.
    assert!(near(width(&ui), 200.0), "{}", width(&ui));
    at(&mut ui, 1.0);
    assert!(near(width(&ui), 300.0));
    assert!(ui.host.layout[1].size.width.is_auto(), "auto restored");
    // Back from auto: starts at the laid-out 300.
    apply(&mut ui, |t| {
        t.layout(1, &sized(50.0, 40.0));
    });
    at(&mut ui, 1.5);
    assert!(near(width(&ui), 175.0), "{}", width(&ui));
    at(&mut ui, 2.0);
    assert_eq!(ui.host.layout[1], sized(50.0, 40.0));
}

/// A resent equal value leaves a tween running; a new one retargets it
/// from the value on screen.
#[test]
fn tweens_retarget_from_the_value_on_screen() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[Transition {
                prop: Prop::Opacity,
                timing: linear(1.0),
            }],
        );
    });
    apply(&mut ui, |t| {
        t.opacity(1, 0.0);
    });
    at(&mut ui, 0.5);
    apply(&mut ui, |t| {
        t.opacity(1, 0.0);
    });
    at(&mut ui, 0.75);
    assert!(near(ui.host.spatial[1].opacity, 0.25));
    apply(&mut ui, |t| {
        t.opacity(1, 1.0);
    });
    at(&mut ui, 1.25);
    assert!(
        near(ui.host.spatial[1].opacity, 0.625),
        "{}",
        ui.host.spatial[1].opacity
    );
    at(&mut ui, 1.75);
    assert_eq!(ui.host.spatial[1].opacity, 1.0);
    assert!(!ui.animating());
}

/// `Animate` tweens once; a plain set (no transition) cancels it and
/// jumps. Springs end on their target.
#[test]
fn animate_tweens_once_and_a_set_cancels_it() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.animate(1, Prop::Fill, Value::Color(0xFF00_00FF), linear(1.0));
    });
    at(&mut ui, 0.5);
    assert_eq!(ui.host.paint[1].fill, 0x8000_00FF);
    apply(&mut ui, |t| {
        t.fill(1, 0x00FF_00FF);
    });
    assert!(!ui.animating());
    assert_eq!(ui.host.paint[1].fill, 0x00FF_00FF);
    at(&mut ui, 0.75);
    assert_eq!(ui.host.paint[1].fill, 0x00FF_00FF);

    let spring = Timing::Spring {
        delay: 0.0,
        stiffness: 170.0,
        damping: 12.0,
        mass: 1.0,
    };
    let target = Affine::rotate(1.0).mul(&Affine::scale(2.0, 2.0));
    apply(&mut ui, |t| {
        t.animate(2, Prop::Transform, Value::Transform(target), spring);
    });
    let end = ui.animations_end().unwrap();
    // Started at the clock of its transaction (0.75).
    assert!((end - (0.75 + spring.run_secs())).abs() < 1e-9);
    at(&mut ui, 1.2);
    assert_ne!(ui.host.spatial[2].transform, target);
    at(&mut ui, end);
    assert_eq!(ui.host.spatial[2].transform, target);
    assert!(!ui.animating());
}

/// Removing a node drops its animations; its id reused starts clean.
#[test]
fn removed_nodes_drop_their_animations() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.animate(1, Prop::Opacity, Value::Opacity(0.0), linear(1.0));
    });
    at(&mut ui, 0.5);
    apply(&mut ui, |t| {
        t.remove(1).create(1, NodeKind::View).place(0, 1, NIL);
    });
    assert!(!ui.animating());
    at(&mut ui, 1.0);
    assert_eq!(ui.host.spatial[1].opacity, 1.0);
    assert!(ui.host.transitions.is_empty());
}

/// Both ops round-trip through the wire; invalid timings, duplicate
/// declarations, non-length targets, and paint animations on text
/// reject the transaction.
#[test]
fn animation_ops_round_trip_and_validate() {
    let spring = Timing::Spring {
        delay: 0.1,
        stiffness: 200.0,
        damping: 20.0,
        mass: 1.0,
    };
    let mut t = Transaction::new(1);
    t.transition(
        1,
        &[
            Transition {
                prop: Prop::Width,
                timing: linear(0.3).with_delay(0.05),
            },
            Transition {
                prop: Prop::Fill,
                timing: spring,
            },
        ],
    )
    .animate(
        1,
        Prop::Padding,
        Value::Padding([taffy::LengthPercentage::length(4.0); 4]),
        linear(1.0),
    )
    .animate(1, Prop::BorderColor, Value::Color(0x1234_5678), spring)
    .animate(
        1,
        Prop::Transform,
        Value::Transform(Affine::rotate(0.3)),
        Timing::curve(0.2, [0.25, 0.1, 0.25, 1.0]),
    );
    let bytes = wire::encode(&t);
    let decoded = wire::decode(&bytes).unwrap();
    assert_eq!(decoded.mutations, t.mutations);

    let mut ui = row_ui();
    let seq = ui.seq;
    let bad: Vec<Case> = vec![
        Box::new(|t| {
            t.animate(
                1,
                Prop::Opacity,
                Value::Opacity(0.0),
                Timing::Spring {
                    delay: 0.0,
                    stiffness: 100.0,
                    damping: 0.0,
                    mass: 1.0,
                },
            );
        }),
        Box::new(|t| {
            t.animate(
                1,
                Prop::Opacity,
                Value::Opacity(0.0),
                Timing::curve(1.0, [1.5, 0.0, 1.0, 1.0]),
            );
        }),
        Box::new(|t| {
            t.animate(1, Prop::Opacity, Value::Opacity(0.0), linear(f32::NAN));
        }),
        Box::new(|t| {
            let tr = Transition {
                prop: Prop::Width,
                timing: linear(1.0),
            };
            t.transition(1, &[tr, tr]);
        }),
        Box::new(|t| {
            t.animate(
                1,
                Prop::Width,
                Value::Size(taffy::Dimension::percent(0.5)),
                linear(1.0),
            );
        }),
        Box::new(|t| {
            t.animate(1, Prop::Width, Value::Opacity(1.0), linear(1.0));
        }),
        Box::new(|t| {
            t.animate(3, Prop::Fill, Value::Color(0xFF), linear(1.0));
        }),
        Box::new(|t| {
            t.animate(9, Prop::Opacity, Value::Opacity(0.0), linear(1.0));
        }),
    ];
    for (k, f) in bad.iter().enumerate() {
        let mut t = Transaction::new(seq + 1);
        f(&mut t);
        assert!(ui.apply_txn(&t).is_err(), "case {k}");
        // The wire encodes a target by its property: a mismatched value
        // (case 5) exists only in the direct API.
        if k != 5 {
            assert!(ui.apply(&wire::encode(&t)).is_err(), "case {k} on the wire");
        }
        assert_eq!(ui.seq, seq);
        assert!(!ui.animating());
    }
    // Unknown timing kinds and properties fail decoding.
    let mut t = Transaction::new(seq + 1);
    t.animate(1, Prop::Opacity, Value::Opacity(0.0), linear(1.0));
    let buf = wire::encode(&t);
    let at = buf.iter().rposition(|&b| b == wire::op::ANIMATE).unwrap();
    let mut prop = buf.clone();
    prop[at + 5] = 8;
    assert!(ui.apply(&prop).is_err());
    let mut kind = buf.clone();
    kind[at + 5 + 1 + 4] = 2;
    assert!(ui.apply(&kind).is_err());
    ui.apply(&buf).unwrap();
    assert!(ui.animating());
}

fn column(width: f32) -> taffy::Style {
    taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::length(width),
            height: taffy::Dimension::auto(),
        },
        ..taffy::Style::default()
    }
}

fn width_transition(id: u32, prop: Prop, t: &mut Transaction<'static>) {
    t.transition(
        id,
        &[Transition {
            prop,
            timing: linear(1.0),
        }],
    );
}

/// S4-01: the probe that resolves an `auto` target leaves scroll offsets
/// alone; the frame's layout clamps them against the frame's geometry.
#[test]
fn probe_keeps_scroll_offsets() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    let scroller = taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        overflow: taffy::Point {
            x: taffy::Overflow::Visible,
            y: taffy::Overflow::Scroll,
        },
        ..sized(200.0, 100.0)
    };
    t.create(0, NodeKind::View)
        .layout(0, &scroller)
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(200.0, 400.0))
        .place(0, 1, NIL);
    width_transition(1, Prop::Height, &mut t);
    t.create(2, NodeKind::View)
        .layout(2, &sized(200.0, 200.0))
        .place(1, 2, NIL);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    ui.scroll_to(NodeId(0), 0.0, 300.0);
    at(&mut ui, 0.0);
    assert_eq!(ui.host.spatial[0].scroll[1], 300.0);
    apply(&mut ui, |t| {
        let auto = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(200.0),
                height: taffy::Dimension::auto(),
            },
            ..sized(0.0, 0.0)
        };
        t.layout(1, &auto);
    });
    at(&mut ui, 0.0);
    assert_eq!(ui.layouts.data(NodeId(1)).rect.size.height, 400.0);
    assert_eq!(
        ui.host.spatial[0].scroll[1], 300.0,
        "the probe does not clamp"
    );
    at(&mut ui, 1.0);
    assert_eq!(ui.layouts.data(NodeId(1)).rect.size.height, 200.0);
    assert_eq!(ui.host.spatial[0].scroll[1], 100.0, "the frame clamps");
}

/// S4-02: targets that depend on other layout tweens resolve with those
/// at their declared values: a stretched child's `auto` height follows
/// its parent's target width (aspect ratio 2).
#[test]
fn probe_applies_every_declared_layout_target() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column(100.0))
        .place(NIL, 0, NIL);
    width_transition(0, Prop::Width, &mut t);
    let child = |h: taffy::Dimension| taffy::Style {
        aspect_ratio: Some(2.0),
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: h,
        },
        ..taffy::Style::default()
    };
    t.create(1, NodeKind::View)
        .layout(1, &child(taffy::Dimension::length(10.0)))
        .place(0, 1, NIL);
    width_transition(1, Prop::Height, &mut t);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    apply(&mut ui, |t| {
        t.layout(0, &column(300.0))
            .layout(1, &child(taffy::Dimension::auto()));
    });
    let h = |ui: &Ui| ui.layouts.data(NodeId(1)).rect.size.height;
    at(&mut ui, 0.5);
    assert!(near(h(&ui), 80.0), "{}", h(&ui));
    at(&mut ui, 0.999);
    assert!((h(&ui) - 150.0).abs() < 0.5, "{}", h(&ui));
    at(&mut ui, 1.0);
    assert!(near(h(&ui), 150.0));
}

/// S4-03: a padding target that is not a length jumps (DF-4).
#[test]
fn percent_padding_jumps() {
    let mut ui = Ui::new(1.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .layout(0, &column(300.0))
        .place(NIL, 0, NIL);
    t.create(1, NodeKind::View)
        .layout(1, &sized(100.0, 40.0))
        .place(0, 1, NIL);
    width_transition(1, Prop::Padding, &mut t);
    ui.apply_txn(&t).unwrap();
    at(&mut ui, 0.0);
    let pct = taffy::LengthPercentage::percent(0.1);
    apply(&mut ui, |t| {
        let s = taffy::Style {
            padding: taffy::Rect {
                left: pct,
                right: pct,
                top: pct,
                bottom: pct,
            },
            ..sized(100.0, 40.0)
        };
        t.layout(1, &s);
    });
    assert!(!ui.animating());
    at(&mut ui, 0.5);
    assert_eq!(ui.layouts.data(NodeId(1)).content, [30.0, 30.0]);
}

/// S4-04: a recycled id tweens like a fresh one: the previous occupant's
/// laid-out size is not a start value.
#[test]
fn recycled_ids_do_not_inherit_layout() {
    let halfway = |recycle: bool| {
        let mut ui = Ui::new(1.0);
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View)
            .layout(0, &column(300.0))
            .place(NIL, 0, NIL);
        t.create(1, NodeKind::View)
            .layout(1, &sized(100.0, 40.0))
            .place(0, 1, NIL);
        ui.apply_txn(&t).unwrap();
        at(&mut ui, 0.0);
        let id = if recycle { 1 } else { 2 };
        apply(&mut ui, |t| {
            t.remove(1)
                .create(id, NodeKind::View)
                .place(0, id, NIL)
                .animate(
                    id,
                    Prop::Width,
                    Value::Size(taffy::Dimension::length(200.0)),
                    linear(1.0),
                );
        });
        at(&mut ui, 0.5);
        ui.layouts.data(NodeId(id)).rect.size.width
    };
    assert_eq!(halfway(true), halfway(false));
}

/// S4-05: retargeting a tween whose `auto` target is not resolved yet
/// tweens from its start value.
#[test]
fn unresolved_targets_retarget_from_their_start() {
    let mut ui = row_ui();
    apply(&mut ui, |t| width_transition(1, Prop::Width, t));
    let auto = taffy::Style {
        flex_shrink: 0.0,
        size: taffy::Size {
            width: taffy::Dimension::auto(),
            height: taffy::Dimension::length(40.0),
        },
        ..taffy::Style::default()
    };
    apply(&mut ui, |t| {
        t.layout(1, &auto);
    });
    apply(&mut ui, |t| {
        t.layout(1, &sized(200.0, 40.0));
    });
    assert!(ui.animating());
    at(&mut ui, 0.5);
    assert!(near(ui.layouts.data(NodeId(1)).rect.size.width, 150.0));
}

/// S4-06: a retarget starts from the value the row shows (a spring's
/// overshoot clamped), and intermediate frames follow from there.
#[test]
fn retarget_starts_from_the_clamped_value() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.animate(
            1,
            Prop::Opacity,
            Value::Opacity(0.0),
            Timing::Spring {
                delay: 0.0,
                stiffness: 100.0,
                damping: 5.0,
                mass: 1.0,
            },
        );
    });
    at(&mut ui, 0.3);
    assert_eq!(ui.host.spatial[1].opacity, 0.0, "the overshoot clamps");
    apply(&mut ui, |t| {
        width_transition(1, Prop::Opacity, t);
        t.opacity(1, 1.0);
    });
    at(&mut ui, 0.8);
    assert!(
        near(ui.host.spatial[1].opacity, 0.5),
        "{}",
        ui.host.spatial[1].opacity
    );
}

/// S4-07: a large finite transform target tweens through finite rows.
#[test]
fn large_transforms_stay_finite() {
    let mut ui = row_ui();
    apply(&mut ui, |t| {
        t.animate(
            1,
            Prop::Transform,
            Value::Transform(Affine::scale(1e30, 1.0)),
            linear(1.0),
        );
    });
    at(&mut ui, 0.5);
    let m = ui.host.spatial[1].transform;
    assert!(m.0.iter().all(|v| v.is_finite()), "{m:?}");
    // Interpolated, not the last frame kept: x scale halfway.
    assert!((m.0[0] / 5e29 - 1.0).abs() < 1e-3, "{m:?}");
    at(&mut ui, 1.0);
    assert_eq!(ui.host.spatial[1].transform, Affine::scale(1e30, 1.0));
}

/// `Animate` tweens report their end to JS (DF-3, promoted into step 4):
/// finished, cancelled by a plain set, retargeted by another tween, or
/// removed with their node (the removed occupant's generation);
/// transition tweens report nothing.
#[test]
fn animate_reports_how_it_ended() {
    use crate::animation::end_reason::*;
    use crate::events::out_kind::ANIMATION_END;
    let ends = |ui: &mut Ui| -> Vec<(u32, u16, u32, u32)> {
        ui.take_events()
            .into_iter()
            .filter(|e| e.kind == ANIMATION_END)
            .map(|e| (e.node, e.generation, e.key & 0xFF, e.key >> 8))
            .collect()
    };
    let mut ui = row_ui();
    ui.take_events();
    let o = Prop::Opacity as u32;
    apply(&mut ui, |t| {
        t.animate(1, Prop::Opacity, Value::Opacity(0.0), linear(1.0));
    });
    at(&mut ui, 0.5);
    assert_eq!(ends(&mut ui), []);
    at(&mut ui, 1.0);
    assert_eq!(ends(&mut ui), [(1, 0, o, FINISHED)]);

    apply(&mut ui, |t| {
        t.animate(1, Prop::Opacity, Value::Opacity(1.0), linear(1.0));
    });
    apply(&mut ui, |t| {
        t.opacity(1, 0.5);
    });
    assert_eq!(ends(&mut ui), [(1, 0, o, CANCELLED)]);

    apply(&mut ui, |t| {
        t.animate(1, Prop::Opacity, Value::Opacity(1.0), linear(1.0));
    });
    apply(&mut ui, |t| {
        t.animate(1, Prop::Opacity, Value::Opacity(0.0), linear(1.0));
    });
    assert_eq!(ends(&mut ui), [(1, 0, o, RETARGETED)]);
    apply(&mut ui, |t| {
        t.remove(1).create(1, NodeKind::View).place(0, 1, NIL);
    });
    assert_eq!(ends(&mut ui), [(1, 0, o, REMOVED)]);

    // Nothing to tween from (never laid out): it ends at once.
    apply(&mut ui, |t| {
        t.animate(
            1,
            Prop::Width,
            Value::Size(taffy::Dimension::length(10.0)),
            linear(1.0),
        );
    });
    assert_eq!(ends(&mut ui), [(1, 1, Prop::Width as u32, FINISHED)]);

    // Transition tweens report nothing.
    apply(&mut ui, |t| {
        width_transition(2, Prop::Opacity, t);
        t.opacity(2, 0.0);
    });
    at(&mut ui, 5.0);
    assert!(!ui.animating());
    assert_eq!(ends(&mut ui), []);
}
