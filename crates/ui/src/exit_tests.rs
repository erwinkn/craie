//! Exits through `Ui` (`exit.rs`): a detached node with a declared exit
//! stays laid out and drawn but inert, collapses its siblings with its
//! size, and goes with one `EXIT_END`; its parent leaving ends it at
//! once; only the detached root's exit runs; the wire carries it.

use std::sync::Arc;

use crate::a11y::aid;
use crate::animation::{Prop, Timing, Transition, end_reason};
use crate::events::{Event, UiEvent, mask, out_kind};
use crate::geom::Size;
use crate::host::NodeId;
use crate::keyframes::{Animation, Easing, Frame, Keyframes, Sample, Trigger, frame_field};
use crate::mutation::{Command, NodeKind, Transaction, trap_flag};
use crate::scene::Resolved;
use crate::states::{TermDecl, VariantDecl, state_bit, value_field};
use crate::ui::Ui;
use crate::wire;

const NIL: u32 = u32::MAX;
const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};
const TOAST: u32 = 0x2233_44FF;

fn at(ui: &mut Ui, t: f64) {
    ui.set_time(t);
    ui.render(VIEW);
}

fn apply(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).unwrap();
}

fn rejects(ui: &mut Ui, f: impl FnOnce(&mut Transaction<'static>)) -> bool {
    let mut t = Transaction::new(ui.seq + 1);
    f(&mut t);
    ui.apply_txn(&t).is_err()
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

fn column() -> taffy::Style {
    taffy::Style {
        flex_direction: taffy::FlexDirection::Column,
        ..sized(400.0, 300.0)
    }
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

/// The toast's exit: to opacity 0 and height 0 over 200 ms, held.
fn fade_and_collapse() -> Animation {
    let k = Keyframes::new(vec![frame(
        1.0,
        value_field::OPACITY | frame_field::HEIGHT,
        |s| {
            s.opacity = 0.0;
            s.size[1] = 0.0;
        },
    )]);
    Animation {
        fill: crate::keyframes::Fill::Forwards,
        ..Animation::new(Arc::new(k), 0.2, Easing::LINEAR)
    }
}

/// Root 0, a column holding toasts 1, 2, 3 (100 × 40, stacked). Toast 2
/// holds button 20 and declares its exit.
fn toasts() -> Ui {
    let mut ui = Ui::new(1.0);
    apply(&mut ui, |t| {
        t.create(0, NodeKind::View)
            .layout(0, &column())
            .place(NIL, 0, NIL);
        for id in [1, 2, 3] {
            t.create(id, NodeKind::View)
                .layout(id, &sized(100.0, 40.0))
                .fill(id, if id == 2 { TOAST } else { 0x0000_00FF })
                .append(0, id);
        }
        t.create(20, NodeKind::View)
            .layout(20, &sized(100.0, 40.0))
            .interaction(20, mask::FOCUS | mask::POINTER_ENTER_LEAVE, true)
            .append(2, 20);
        t.animation(2, Trigger::Exit, false, &[fade_and_collapse()]);
    });
    at(&mut ui, 0.0);
    ui
}

fn detach(ui: &mut Ui, id: u32) {
    apply(ui, |t| {
        t.detach(id);
    });
}

fn drawn(ui: &Ui, color: u32) -> bool {
    let all: Vec<Resolved> = ui.scene().resolve(&|r| r.0 as u64);
    all.iter().any(|p| p.color == color)
}

fn in_a11y(ui: &Ui, id: u32) -> bool {
    let tree = ui.a11y_tree(VIEW);
    tree.nodes.iter().any(|(n, _)| *n == aid(NodeId(id)))
}

fn y(ui: &Ui, id: u32) -> f32 {
    ui.layouts.rect(NodeId(id)).origin.y
}

fn exit_ends(events: &[UiEvent]) -> Vec<(u32, u16, u32)> {
    events
        .iter()
        .filter(|e| e.kind == out_kind::EXIT_END)
        .map(|e| (e.node, e.generation, e.key))
        .collect()
}

fn generation(ui: &Ui, id: u32) -> u16 {
    ui.host.node(NodeId(id)).unwrap().generation
}

/// While it exits, toast 2 keeps its place and its paint, and leaves
/// hit testing, the tab order and the accessibility tree; the focus on
/// its button leaves at the detach.
#[test]
fn an_exiting_subtree_stays_drawn_but_inert() {
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.command(20, Command::Focus);
    });
    assert_eq!(ui.focused(), Some(NodeId(20)));
    assert!(in_a11y(&ui, 2) && in_a11y(&ui, 20));
    assert_eq!(ui.hit_test(50.0, 60.0), Some(NodeId(20)));

    detach(&mut ui, 2);
    assert_eq!(ui.focused(), None, "the focus inside leaves at the detach");
    at(&mut ui, 0.05);
    let kids: Vec<u32> = ui.host.children(NodeId(0)).iter().map(|n| n.0).collect();
    assert_eq!(kids, [1, 2, 3], "it keeps its place");
    assert!(drawn(&ui, TOAST), "and its paint");
    assert!(!matches!(ui.hit_test(50.0, 50.0), Some(NodeId(2 | 20))));
    assert!(!in_a11y(&ui, 2) && !in_a11y(&ui, 20));
    apply(&mut ui, |t| {
        t.command(20, Command::Focus);
    });
    assert_eq!(ui.focused(), None, "no focus goes in");
    // The blur of the focus leaving is dropped with the subtree's
    // events.
    let events = ui.take_events();
    assert!(events.iter().all(|e| e.node != 20), "{events:?}");
}

/// Height to 0 moves the next sibling up as it runs; the end frees the
/// subtree and sends one `EXIT_END`, the root's generation at the
/// start.
#[test]
fn a_collapse_moves_the_next_sibling_up_then_frees() {
    let mut ui = toasts();
    assert_eq!(y(&ui, 3), 80.0);
    let generation = generation(&ui, 2);
    detach(&mut ui, 2);
    at(&mut ui, 0.1);
    assert_eq!(ui.layouts.rect(NodeId(2)).size.height, 20.0);
    assert_eq!(y(&ui, 3), 60.0);
    assert!(ui.host.is_live(NodeId(20)));
    assert!(exit_ends(&ui.take_events()).is_empty());

    at(&mut ui, 0.25);
    assert!(!ui.host.is_live(NodeId(2)) && !ui.host.is_live(NodeId(20)));
    assert_eq!(y(&ui, 3), 40.0);
    let events = ui.take_events();
    assert_eq!(exit_ends(&events), [(2, generation, end_reason::FINISHED)]);
    assert_eq!(events.len(), 1, "nothing else of the subtree: {events:?}");
    at(&mut ui, 0.4);
    assert!(ui.take_events().is_empty());
    assert!(ui.host.exiting.is_empty() && ui.motion().node_count() == 0);
    // The ids come back: a new node takes slot 2.
    apply(&mut ui, |t| {
        t.create(2, NodeKind::View).append(0, 2);
    });
    assert_eq!(
        ui.host.children(NodeId(0)),
        [NodeId(1), NodeId(3), NodeId(2)]
    );
}

/// A new sibling goes next to the one the app names: before 3, after
/// the exiting 2.
#[test]
fn a_new_sibling_places_by_the_named_one() {
    let mut ui = toasts();
    detach(&mut ui, 2);
    apply(&mut ui, |t| {
        t.create(4, NodeKind::View).place(0, 4, 3);
    });
    let kids: Vec<u32> = ui.host.children(NodeId(0)).iter().map(|n| n.0).collect();
    assert_eq!(kids, [1, 2, 4, 3]);

    // In the transaction of the detach too.
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.animation(3, Trigger::Exit, false, &[fade_and_collapse()])
            .detach(3)
            .create(4, NodeKind::View)
            .place(0, 4, 3);
    });
    assert_eq!(ui.host.children(NodeId(0)), [1, 2, 4, 3].map(NodeId));
}

/// The parent going, removed or detached, ends the exit at once.
#[test]
fn a_parent_gone_frees_at_once() {
    for remove in [true, false] {
        let mut ui = toasts();
        let generation = generation(&ui, 2);
        detach(&mut ui, 2);
        at(&mut ui, 0.05);
        ui.take_events();
        apply(&mut ui, |t| {
            if remove {
                t.remove(0);
            } else {
                t.detach(0);
            }
        });
        assert!(!ui.host.is_live(NodeId(2)) && !ui.host.is_live(NodeId(20)));
        assert_eq!(
            exit_ends(&ui.take_events()),
            [(2, generation, end_reason::PARENT_GONE)],
            "remove {remove}"
        );
    }
}

/// A remove of the root cuts the exit short; a node with an exit that
/// is out of the tree skips it, or is cut short when removed in the
/// same transaction. All free and report.
#[test]
fn removes_and_skips_end_at_once() {
    let mut ui = toasts();
    detach(&mut ui, 2);
    apply(&mut ui, |t| {
        t.remove(2);
    });
    assert!(!ui.host.is_live(NodeId(20)));
    assert_eq!(exit_ends(&ui.take_events())[0].2, end_reason::REMOVED);

    apply(&mut ui, |t| {
        t.create(7, NodeKind::View)
            .create(8, NodeKind::View)
            .append(7, 8)
            .animation(7, Trigger::Exit, false, &[fade_and_collapse()])
            .detach(7);
    });
    assert!(!ui.host.is_live(NodeId(7)) && !ui.host.is_live(NodeId(8)));
    assert_eq!(exit_ends(&ui.take_events())[0].2, end_reason::SKIPPED);

    apply(&mut ui, |t| {
        t.create(7, NodeKind::View)
            .create(8, NodeKind::View)
            .append(7, 8)
            .animation(7, Trigger::Exit, false, &[fade_and_collapse()])
            .detach(7)
            .remove(7);
    });
    assert!(!ui.host.is_live(NodeId(7)) && !ui.host.is_live(NodeId(8)));
    let ends = exit_ends(&ui.take_events());
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].2, end_reason::REMOVED);
}

/// `END_EXIT` cuts a running exit short (`removed`) and does nothing
/// once it has ended: JS may send it before it sees the `EXIT_END`. A
/// plain `REMOVE` of the freed id stays an error, and only an exit's
/// root takes an `END_EXIT`.
#[test]
fn end_exit_is_idempotent() {
    let mut ui = toasts();
    detach(&mut ui, 2);
    at(&mut ui, 0.05);
    apply(&mut ui, |t| {
        t.end_exit(2).end_exit(2);
    });
    assert!(!ui.host.is_live(NodeId(2)) && !ui.host.is_live(NodeId(20)));
    let ends = exit_ends(&ui.take_events());
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].2, end_reason::REMOVED);

    // Finished, its end on its way to JS.
    let mut ui = toasts();
    detach(&mut ui, 2);
    at(&mut ui, 0.25);
    assert_eq!(exit_ends(&ui.take_events())[0].2, end_reason::FINISHED);
    apply(&mut ui, |t| {
        t.end_exit(2);
    });
    assert!(exit_ends(&ui.take_events()).is_empty());
    assert!(rejects(&mut ui, |t| {
        t.remove(2);
    }));
    assert!(rejects(&mut ui, |t| {
        t.end_exit(1);
    }));
}

/// Validation frees an exit's subtree with its root, as execution does:
/// nothing later in the batch may name a node inside. A node moved out
/// first stays.
#[test]
fn a_cut_exit_frees_its_subtree_in_validation() {
    let mut ui = toasts();
    detach(&mut ui, 2);
    assert!(rejects(&mut ui, |t| {
        t.remove(2).place(NIL, 20, NIL);
    }));
    assert!(rejects(&mut ui, |t| {
        t.end_exit(2).place(NIL, 20, NIL);
    }));
    // A child the batch placed goes too.
    assert!(rejects(&mut ui, |t| {
        t.create(9, NodeKind::View)
            .append(1, 9)
            .animation(1, Trigger::Exit, false, &[fade_and_collapse()])
            .detach(1)
            .remove(1)
            .place(0, 9, NIL);
    }));
    apply(&mut ui, |t| {
        t.place(0, 20, NIL).remove(2);
    });
    assert!(ui.host.is_live(NodeId(20)));
    assert_eq!(ui.host.children(NodeId(0)), [1, 3, 20].map(NodeId));
}

/// Fades out over 0.5 s: only an end frame.
fn fade_out() -> Animation {
    let k = Keyframes::new(vec![frame(1.0, value_field::OPACITY, |s| s.opacity = 0.0)]);
    Animation {
        fill: crate::keyframes::Fill::Forwards,
        ..Animation::new(Arc::new(k), 0.5, Easing::LINEAR)
    }
}

fn opacity(ui: &Ui, id: u32) -> f32 {
    ui.host.spatial[NodeId(id).index()].opacity
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

/// An exit's omitted start is what showed at the detach, held while what
/// runs under it moves on unseen (Framer's `AnimatePresence`). Detached
/// at 0.2 s, a fade out over 0.5 s shows 80% of it at 0.3 s, whether an
/// enter fading in, a transition or a hover animation showed it.
#[test]
fn an_exit_starts_from_what_showed() {
    // From 0 to what lies under (1) over 1 s.
    let fade_in = Animation::new(
        Arc::new(Keyframes::new(vec![frame(
            0.0,
            value_field::OPACITY,
            |s| s.opacity = 0.0,
        )])),
        1.0,
        Easing::LINEAR,
    );

    // An interrupted enter: 0.2 at the detach (not the 0.3 it would
    // reach by 0.3 s).
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.create(5, NodeKind::View)
            .layout(5, &sized(100.0, 40.0))
            .append(0, 5)
            .animation(5, Trigger::Enter, false, std::slice::from_ref(&fade_in))
            .animation(5, Trigger::Exit, false, &[fade_out()]);
    });
    at(&mut ui, 0.2);
    assert!(near(opacity(&ui, 5), 0.2));
    detach(&mut ui, 5);
    at(&mut ui, 0.3);
    assert!(near(opacity(&ui, 5), 0.16), "{}", opacity(&ui, 5));

    // A transition from 1 to 0 over 1 s: 0.8 at the detach.
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.transition(
            1,
            &[Transition {
                prop: Prop::Opacity,
                timing: Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
            }],
        )
        .animation(1, Trigger::Exit, false, &[fade_out()]);
    });
    apply(&mut ui, |t| {
        t.opacity(1, 0.0);
    });
    at(&mut ui, 0.2);
    assert!(near(opacity(&ui, 1), 0.8));
    detach(&mut ui, 1);
    at(&mut ui, 0.3);
    assert!(near(opacity(&ui, 1), 0.64), "{}", opacity(&ui, 1));

    // A hover animation: 0.2 at the detach, which also ends the hover
    // (and the animation under the exit with it).
    let mut ui = toasts();
    let hovered = VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER,
        }],
        animations: vec![fade_in],
        ..Default::default()
    };
    apply(&mut ui, |t| {
        t.interaction(1, mask::POINTER_ENTER_LEAVE, true)
            .states(1, 0)
            .variants(1, &[hovered])
            .animation(1, Trigger::Exit, false, &[fade_out()]);
    });
    ui.dispatch(&Event::PointerMove { x: 50.0, y: 20.0 });
    assert!(ui.state_bits(NodeId(1)) & state_bit::HOVER != 0);
    at(&mut ui, 0.2);
    assert!(near(opacity(&ui, 1), 0.2));
    detach(&mut ui, 1);
    at(&mut ui, 0.3);
    assert!(near(opacity(&ui, 1), 0.16), "{}", opacity(&ui, 1));
}

/// An exiting subtree counts as drawn: a loop inside keeps running with
/// the exit.
#[test]
fn loops_inside_run_through_the_exit() {
    let mut ui = toasts();
    let spin = Keyframes::new(vec![frame(1.0, value_field::ROTATE, |s| {
        s.rotate = std::f32::consts::TAU
    })]);
    let spin = Animation {
        iterations: f32::INFINITY,
        ..Animation::new(Arc::new(spin), 1.0, Easing::LINEAR)
    };
    apply(&mut ui, |t| {
        t.animation(20, Trigger::Base, false, &[spin]);
    });
    at(&mut ui, 0.0);
    detach(&mut ui, 2);
    at(&mut ui, 0.1);
    // The exit's animation and the loop.
    assert_eq!(ui.motion().live(), 2);
    assert!(ui.host.spatial[NodeId(20).index()].parts.rotate > 0.0);
}

/// A hidden exit (`display: none` on an ancestor) cannot run: it ends as
/// skipped on the next frame, whether hidden before its detach or while
/// it runs, rather than wait parked with its ids held. Until then it
/// keeps its place, as validation expects.
#[test]
fn a_hidden_exit_is_skipped() {
    let hide = |ui: &mut Ui| {
        apply(ui, |t| {
            t.layout(
                0,
                &taffy::Style {
                    display: taffy::Display::None,
                    ..column()
                },
            );
        });
    };
    let mut ui = toasts();
    hide(&mut ui);
    at(&mut ui, 0.05);
    apply(&mut ui, |t| {
        t.detach(2).create(4, NodeKind::View).place(0, 4, 2);
    });
    assert!(ui.host.is_live(NodeId(2)));
    at(&mut ui, 0.1);
    assert!(!ui.host.is_live(NodeId(2)));
    assert_eq!(exit_ends(&ui.take_events())[0].2, end_reason::SKIPPED);
    assert_eq!(ui.host.children(NodeId(0)), [1, 4, 3].map(NodeId));

    let mut ui = toasts();
    detach(&mut ui, 2);
    at(&mut ui, 0.05);
    hide(&mut ui);
    at(&mut ui, 0.1);
    assert!(!ui.host.is_live(NodeId(2)) && !ui.host.is_live(NodeId(20)));
    let ends = exit_ends(&ui.take_events());
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].2, end_reason::SKIPPED);
}

/// Only the detached root's exit runs: its descendants' stay declared
/// and go with the subtree, unreported.
#[test]
fn a_descendant_exit_does_not_run() {
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.animation(20, Trigger::Exit, false, &[fade_and_collapse()]);
    });
    detach(&mut ui, 2);
    at(&mut ui, 0.1);
    assert!(!ui.exit_running(NodeId(20)));
    assert_eq!(ui.layouts.rect(NodeId(20)).size.height, 40.0);
    at(&mut ui, 0.25);
    let ends = exit_ends(&ui.take_events());
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0, 2);
}

/// An exit inside an exit: the outer's end ends the inner (parent gone).
#[test]
fn an_inner_exit_ends_with_the_outer() {
    let mut ui = toasts();
    let long = Animation {
        duration: 1.0,
        ..fade_and_collapse()
    };
    apply(&mut ui, |t| {
        t.animation(20, Trigger::Exit, false, &[long]);
    });
    detach(&mut ui, 20);
    detach(&mut ui, 2);
    at(&mut ui, 0.25);
    let mut ends = exit_ends(&ui.take_events());
    ends.sort();
    assert_eq!(
        ends.iter().map(|e| (e.0, e.2)).collect::<Vec<_>>(),
        [(2, end_reason::FINISHED), (20, end_reason::PARENT_GONE)]
    );
}

/// A modal trap inside the exiting subtree releases: the app behind it
/// is reachable again, and the focus it held leaves.
#[test]
fn a_trap_inside_releases() {
    let mut ui = toasts();
    apply(&mut ui, |t| {
        t.interaction(1, mask::POINTER_ENTER_LEAVE, false).trap(
            20,
            trap_flag::ACTIVE | trap_flag::MODAL | trap_flag::AUTO_FOCUS,
        );
    });
    at(&mut ui, 0.0);
    assert!(ui.traps.top_modal().is_some());
    assert_ne!(ui.hit_test(50.0, 20.0), Some(NodeId(1)), "the modal gates");
    detach(&mut ui, 2);
    at(&mut ui, 0.05);
    assert_eq!(ui.traps.top_modal(), None);
    assert_eq!(ui.hit_test(50.0, 20.0), Some(NodeId(1)));
    assert_eq!(ui.focused(), None);
}

/// An exiting node never comes back, nor takes children; an exit ends;
/// only an exit sets a size.
#[test]
fn validation_keeps_exits_one_way() {
    let mut ui = toasts();
    detach(&mut ui, 2);
    assert!(rejects(&mut ui, |t| {
        t.place(0, 2, NIL);
    }));
    assert!(rejects(&mut ui, |t| {
        t.detach(2);
    }));
    assert!(rejects(&mut ui, |t| {
        t.create(9, NodeKind::View).append(20, 9);
    }));
    // In one transaction too: a detach starts it.
    assert!(rejects(&mut ui, |t| {
        t.animation(1, Trigger::Exit, false, &[fade_and_collapse()])
            .detach(1)
            .place(0, 1, NIL);
    }));
    // A cleared exit detaches plainly.
    apply(&mut ui, |t| {
        t.animation(1, Trigger::Exit, false, &[fade_and_collapse()])
            .animation(1, Trigger::Exit, false, &[])
            .detach(1)
            .place(0, 1, NIL);
    });
    let forever = Animation {
        iterations: f32::INFINITY,
        ..fade_and_collapse()
    };
    assert!(rejects(&mut ui, |t| {
        t.animation(3, Trigger::Exit, false, &[forever]);
    }));
    assert!(rejects(&mut ui, |t| {
        t.animation(3, Trigger::Base, false, &[fade_and_collapse()]);
    }));
}

/// The exit trigger, the size channels and `END_EXIT` round-trip.
#[test]
fn exit_ops_round_trip() {
    let width = Keyframes::new(vec![frame(0.5, frame_field::SIZE, |s| {
        s.size = [10.0, 0.0]
    })]);
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View).animation(
        1,
        Trigger::Exit,
        false,
        &[
            fade_and_collapse(),
            Animation {
                index: 1,
                ..Animation::new(Arc::new(width), 0.1, Easing::LINEAR)
            },
        ],
    );
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);
    let mut ui = Ui::new(1.0);
    ui.apply(&buf).unwrap();
    assert_eq!(ui.host.exits[&1].len(), 2);
    let mut end = Transaction::new(2);
    end.end_exit(1);
    assert_eq!(
        wire::decode(&wire::encode(&end)).unwrap().mutations,
        end.mutations
    );
}
