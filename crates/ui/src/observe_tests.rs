//! Observations through `Ui` (`observe.rs`): layout events for nodes
//! that listen, measure answers, the window's state, and `Present`
//! answers once a frame (at rest, if asked) is presented; the wire
//! carries the commands.

use crate::animation::{Prop, Timing, Value};
use crate::events::{UiEvent, mask, out_kind, window_bit};
use crate::geom::{Affine, Size};
use crate::mutation::{Command, NIL, NodeKind, Transaction};
use crate::observe::{PresentRequest, WindowState};
use crate::ui::Ui;
use crate::wire;

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

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

/// A 400×300 column with 10 points of padding (node 1), holding a
/// 100×40 box (2) and a 50×20 box (3) that listens for layout.
fn column(ui: &mut Ui) {
    let mut col = sized(400.0, 300.0);
    col.flex_direction = taffy::FlexDirection::Column;
    col.padding = taffy::Rect::length(10.0);
    apply(ui, |t| {
        t.create(1, NodeKind::View).layout(1, &col).append(NIL, 1);
        t.create(2, NodeKind::View)
            .layout(2, &sized(100.0, 40.0))
            .append(1, 2);
        t.create(3, NodeKind::View)
            .layout(3, &sized(50.0, 20.0))
            .interaction(3, mask::LAYOUT, false)
            .append(1, 3);
    });
}

fn of(events: &[UiEvent], kind: u8) -> Vec<UiEvent> {
    events.iter().filter(|e| e.kind == kind).cloned().collect()
}

fn layouts(ui: &mut Ui) -> Vec<(u32, [f32; 4])> {
    of(&ui.take_events(), out_kind::LAYOUT)
        .iter()
        .map(|e| (e.node, [e.x, e.y, e.a, e.b]))
        .collect()
}

/// A listener hears its box once layout places it, relative to its
/// parent's border box, then only when the box changes; nodes without
/// a listener say nothing.
#[test]
fn layout_events_report_changes_only() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    assert_eq!(layouts(&mut ui), vec![], "nothing before layout");
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![(3, [10.0, 50.0, 50.0, 20.0])]);
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![], "an unchanged frame");
    // Growing the box above moves the listener; a paint change does not.
    apply(&mut ui, |t| {
        t.layout(2, &sized(100.0, 60.0)).fill(3, 0xFF00_00FF);
    });
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![(3, [10.0, 70.0, 50.0, 20.0])]);
    apply(&mut ui, |t| {
        t.fill(3, 0x00FF_00FF);
    });
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![]);
    // A resize of its own.
    apply(&mut ui, |t| {
        t.layout(3, &sized(80.0, 20.0));
    });
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![(3, [10.0, 70.0, 80.0, 20.0])]);
    // A transform is not layout.
    apply(&mut ui, |t| {
        t.transform(3, Affine::translate(30.0, 0.0));
    });
    ui.render(VIEW);
    assert_eq!(layouts(&mut ui), vec![]);
}

/// A listener set on a node already laid out hears its box at once,
/// with no frame (nothing else owes one); cleared, it hears no more.
#[test]
fn a_new_listener_reports_without_a_frame() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    ui.render(VIEW);
    ui.take_events();
    apply(&mut ui, |t| {
        t.interaction(2, mask::LAYOUT, false);
    });
    assert!(!ui.needs_paint(), "a listener draws nothing");
    assert_eq!(layouts(&mut ui), vec![(2, [10.0, 10.0, 100.0, 40.0])]);
    apply(&mut ui, |t| {
        t.interaction(2, 0, false).layout(2, &sized(100.0, 50.0));
    });
    ui.render(VIEW);
    assert_eq!(
        layouts(&mut ui),
        vec![(3, [10.0, 60.0, 50.0, 20.0])],
        "only the listener that remains"
    );
}

/// A slot's new occupant starts with nothing sent: its listener reports
/// even when its box equals the old node's.
#[test]
fn a_recycled_id_reports_again() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    ui.render(VIEW);
    ui.take_events();
    apply(&mut ui, |t| {
        t.detach(3).remove(3);
        t.create(3, NodeKind::View)
            .layout(3, &sized(50.0, 20.0))
            .interaction(3, mask::LAYOUT, false)
            .append(1, 3);
    });
    ui.render(VIEW);
    let events = of(&ui.take_events(), out_kind::LAYOUT);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].generation,
        ui.host.node(crate::host::NodeId(3)).unwrap().generation
    );
}

fn measures(ui: &mut Ui) -> Vec<(u32, u32, [f32; 4])> {
    of(&ui.take_events(), out_kind::MEASURE)
        .iter()
        .map(|e| (e.key, e.revision, [e.x, e.y, e.a, e.b]))
        .collect()
}

/// A measure answers from current layout at once, or after the frame
/// that lays out what its batch changed; in window space, transforms and
/// scroll offsets applied. A node not displayed answers "not measured".
#[test]
fn measures_answer_from_current_layout() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    apply(&mut ui, |t| {
        t.command(2, Command::Measure(7));
    });
    assert_eq!(measures(&mut ui), vec![], "layout is owed");
    ui.render(VIEW);
    assert_eq!(measures(&mut ui), vec![(7, 1, [10.0, 10.0, 100.0, 40.0])]);
    // Current layout: at once.
    apply(&mut ui, |t| {
        t.command(3, Command::Measure(8));
    });
    assert_eq!(measures(&mut ui), vec![(8, 1, [10.0, 50.0, 50.0, 20.0])]);
    // The transform of this batch is applied by the frame.
    apply(&mut ui, |t| {
        t.transform(1, Affine::translate(5.0, 7.0))
            .command(3, Command::Measure(9));
    });
    assert_eq!(measures(&mut ui), vec![]);
    ui.render(VIEW);
    assert_eq!(measures(&mut ui), vec![(9, 1, [15.0, 57.0, 50.0, 20.0])]);
    // Hidden: answered, not measured.
    let mut hidden = sized(400.0, 300.0);
    hidden.display = taffy::Display::None;
    apply(&mut ui, |t| {
        t.layout(1, &hidden).command(3, Command::Measure(10));
    });
    ui.render(VIEW);
    assert_eq!(measures(&mut ui), vec![(10, 0, [0.0; 4])]);
}

/// A node removed in the batch that measures it still gets an answer
/// (a promise waits on it): not measured.
#[test]
fn a_removed_node_answers_unmeasured() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    ui.render(VIEW);
    ui.take_events();
    apply(&mut ui, |t| {
        t.command(3, Command::Measure(1)).detach(3).remove(3);
    });
    ui.render(VIEW);
    assert_eq!(measures(&mut ui), vec![(1, 0, [0.0; 4])]);
}

/// The window reports each change once, and sets the breakpoints.
#[test]
fn window_state_reports_changes() {
    let mut ui = Ui::new(2.0);
    let w = WindowState {
        size: Size::new(800.0, 600.0),
        scale: 2.0,
        focused: true,
        visible: true,
        dark: false,
    };
    ui.set_window(w);
    ui.set_window(w);
    let events = of(&ui.take_events(), out_kind::WINDOW);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!((e.node, e.x, e.y, e.a), (NIL, 800.0, 600.0, 2.0));
    assert_eq!(e.key, window_bit::FOCUSED | window_bit::VISIBLE);
    ui.set_window(WindowState {
        visible: false,
        dark: true,
        ..w
    });
    let events = of(&ui.take_events(), out_kind::WINDOW);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].key, window_bit::FOCUSED | window_bit::DARK);
}

/// A present owes a frame even when nothing changed; the next presented
/// frame answers it. One waiting for rest waits out a tween.
#[test]
fn presents_wait_for_their_frame() {
    let mut ui = Ui::new(1.0);
    column(&mut ui);
    ui.render(VIEW);
    assert!(!ui.needs_paint());
    apply(&mut ui, |t| {
        t.command(
            NIL,
            Command::Present {
                request: 1,
                rest: false,
                path: None,
            },
        );
    });
    assert!(ui.needs_paint(), "a present owes a frame");
    ui.render(VIEW);
    let (frame, due) = ui.frame_presented();
    assert_eq!(frame, 1);
    assert_eq!(
        due,
        vec![PresentRequest {
            request: 1,
            rest: false,
            path: None
        }]
    );
    assert!(!ui.needs_paint(), "answered: idle again");

    ui.set_time(0.0);
    apply(&mut ui, |t| {
        t.animate(
            2,
            Prop::Opacity,
            Value::Opacity(0.5),
            Timing::curve(0.2, [0.0, 0.0, 1.0, 1.0]),
        )
        .command(
            NIL,
            Command::Present {
                request: 2,
                rest: true,
                path: Some("a.png".into()),
            },
        )
        .command(
            NIL,
            Command::Present {
                request: 3,
                rest: false,
                path: None,
            },
        );
    });
    ui.render(VIEW);
    let (frame, due) = ui.frame_presented();
    assert_eq!(frame, 2);
    assert_eq!(due.iter().map(|p| p.request).collect::<Vec<_>>(), vec![3]);
    assert!(ui.needs_paint());
    ui.set_time(0.3);
    ui.render(VIEW);
    let (frame, due) = ui.frame_presented();
    assert_eq!(frame, 3);
    assert_eq!(due.iter().map(|p| p.request).collect::<Vec<_>>(), vec![2]);
    assert_eq!(due[0].path.as_deref(), Some("a.png"));
    ui.presented(2, frame, (400, 300), None);
    let e = &of(&ui.take_events(), out_kind::PRESENTED)[0];
    assert_eq!((e.key, e.revision, e.x, e.y), (2, 3, 400.0, 300.0));
    assert!(e.text.is_empty());
}

/// The commands cross the wire; a present names no node, and its flag
/// bits are strict.
#[test]
fn commands_round_trip_and_validate() {
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View)
        .append(NIL, 1)
        .command(1, Command::Measure(42))
        .command(
            NIL,
            Command::Present {
                request: 5,
                rest: true,
                path: Some("/tmp/x.png".into()),
            },
        )
        .command(
            NIL,
            Command::Present {
                request: 6,
                rest: false,
                path: None,
            },
        );
    let buf = wire::encode(&t);
    assert_eq!(wire::decode(&buf).unwrap().mutations, t.mutations);

    let mut ui = Ui::new(1.0);
    column(&mut ui);
    assert!(rejects(&mut ui, |t| {
        t.command(
            1,
            Command::Present {
                request: 1,
                rest: false,
                path: None,
            },
        );
    }));
    assert!(rejects(&mut ui, |t| {
        t.command(9, Command::Measure(1));
    }));
    // Unknown present flags.
    let mut t = Transaction::new(ui.seq + 1);
    t.command(
        NIL,
        Command::Present {
            request: 1,
            rest: true,
            path: None,
        },
    );
    let mut buf = wire::encode(&t);
    let at = buf
        .windows(6)
        .position(|w| w == [wire::cmd::PRESENT, 1, 0, 0, 0, 1])
        .unwrap();
    buf[at + 5] = 3;
    assert!(wire::decode(&buf).is_err());
}
