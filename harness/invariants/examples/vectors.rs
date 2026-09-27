//! Runtime vector shapes (work item 8): parse and tessellation cost of
//! icons and a sparkline, and what the caches save.
//!
//! - 200 distinct 24 px icons (eight Lucide icons, each at 25 stroke
//!   widths): parse alone, then mounted in one transaction (validate +
//!   parse, then the first frame's layout + tessellation).
//! - Cache hits: 200 more nodes drawing icons already mounted (source
//!   and meshes shared), and the same drawings sent again to the same
//!   nodes (a byte compare).
//! - A 2,000-point sparkline (a polyline, 600 x 100), solid and dashed.
//! - The per-drawing bounds' worst case: 4,096 shapes (`MAX_SHAPES`)
//!   sharing one 1 KiB path, 4 MiB of string references (`MAX_BYTES`).
//!
//! These measure the Rust direct API: transactions built in Rust and
//! applied with `apply_txn`, no wire encode or decode and no JS (the JS
//! side of a resend is in EXPERIMENTS.md). Display scale 2. Times are
//! medians of `RUNS` fresh `Ui`s.
//!
//!   cargo run --release -p craie-harness --example vectors

use std::time::Instant;

use craie_core::geom::Size;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;
use craie_vector::svg::{Drawing, Shape, ShapeKind};
use craie_vector::{LineCap, LineJoin, Stroke};

const RUNS: usize = 21;
const VIEW: Size = Size {
    width: 1200.0,
    height: 800.0,
};

fn circle(cx: f32, cy: f32, r: f32) -> String {
    format!(
        "M{} {cy}A{r} {r} 0 1 1 {} {cy}A{r} {r} 0 1 1 {} {cy}Z",
        cx + r,
        cx - r,
        cx + r
    )
}

/// Lucide icons (v0.4xx), shapes as the facade flattens them.
fn lucide() -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    vec![
        [s(&["m9 12 2 2 4-4"]), vec![circle(12.0, 12.0, 10.0)]].concat(),
        s(&[
            "M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8",
            "M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
        ]),
        [s(&["m21 21-4.34-4.34"]), vec![circle(11.0, 11.0, 8.0)]].concat(),
        [
            s(&[
                "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z",
            ]),
            vec![circle(12.0, 12.0, 3.0)],
        ]
        .concat(),
        s(&[
            "M10.268 21a2 2 0 0 0 3.464 0",
            "M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326",
        ]),
        [
            s(&["M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"]),
            vec![circle(12.0, 7.0, 4.0)],
        ]
        .concat(),
        s(&[
            "M8 2v4",
            "M16 2v4",
            "M5 4H19A2 2 0 0 1 21 6V20A2 2 0 0 1 19 22H5A2 2 0 0 1 3 20V6A2 2 0 0 1 5 4Z",
            "M3 10h18",
        ]),
        s(&["m9 18 6-6-6-6"]),
    ]
}

/// Icon `i` of 200: Lucide icon `i % 8` at stroke width 1.5 + i / 8 / 100.
fn icon(icons: &[Vec<String>], i: usize) -> Drawing<'static> {
    let line = Stroke {
        width: 1.5 + (i / 8) as f32 / 100.0,
        join: LineJoin::Round,
        cap: LineCap::Round,
        ..Stroke::default()
    };
    Drawing {
        view_box: "0 0 24 24".into(),
        shapes: icons[i % 8]
            .iter()
            .map(|d| Shape {
                geometry: d.clone().into(),
                fill: 0,
                stroke: 0xE8EA_F0FF,
                line,
                ..Shape::default()
            })
            .collect(),
    }
}

fn sparkline(dashes: &str) -> Drawing<'static> {
    let mut points = String::new();
    let mut y = 50.0f32;
    let mut seed = 0x2545_F491u32;
    for i in 0..2000 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        y = (y + (seed % 1000) as f32 / 100.0 - 5.0).clamp(4.0, 96.0);
        points.push_str(&format!("{:.2},{y:.2} ", i as f32 * 0.3));
    }
    Drawing {
        view_box: "0 0 600 100".into(),
        shapes: vec![Shape {
            kind: ShapeKind::Polyline,
            geometry: points.into(),
            fill: 0,
            stroke: 0x6DC7_FFFF,
            line: Stroke {
                width: 1.5,
                join: LineJoin::Round,
                ..Stroke::default()
            },
            dashes: dashes.to_string().into(),
            ..Shape::default()
        }],
    }
}

fn sized(w: f32, h: f32) -> taffy::Style {
    taffy::Style {
        size: taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        },
        ..taffy::Style::default()
    }
}

/// Creates vector nodes `ids`, each `w` x `h`, drawing `drawing(k)`.
fn mount(
    t: &mut Transaction<'static>,
    ids: std::ops::Range<u32>,
    w: f32,
    h: f32,
    drawing: impl Fn(usize) -> Drawing<'static>,
) {
    for (k, id) in ids.enumerate() {
        t.create(id, NodeKind::Vector)
            .layout(id, &sized(w, h))
            .drawing(id, drawing(k))
            .place(0, id, NIL);
    }
}

fn root() -> Ui {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    let row = taffy::Style {
        flex_wrap: taffy::FlexWrap::Wrap,
        align_content: Some(taffy::AlignContent::START),
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::percent(1.0),
        },
        ..taffy::Style::default()
    };
    t.create(0, NodeKind::View)
        .layout(0, &row)
        .place(NIL, 0, NIL);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui
}

fn us(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e6
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Median (apply, first frame) in µs of mounting `txn` on a fresh root.
fn mount_cost(txn: impl Fn() -> Transaction<'static>) -> (f64, f64) {
    let (mut apply, mut frame) = (Vec::new(), Vec::new());
    for _ in 0..RUNS {
        let mut ui = root();
        let t = txn();
        let t0 = Instant::now();
        ui.apply_txn(&t).unwrap();
        apply.push(us(t0));
        let t1 = Instant::now();
        ui.render(VIEW);
        frame.push(us(t1));
    }
    (median(apply), median(frame))
}

fn main() {
    let icons = lucide();
    let drawings: Vec<_> = (0..200).map(|i| icon(&icons, i)).collect();

    let parse = median(
        (0..RUNS)
            .map(|_| {
                let t = Instant::now();
                for d in &drawings {
                    std::hint::black_box(d.build().unwrap());
                }
                us(t)
            })
            .collect(),
    );
    println!(
        "200 icons, parse only: {parse:.0} µs ({:.2} µs each)",
        parse / 200.0
    );

    // A baseline: 200 empty boxes, so the frame's layout and scene cost
    // comes off.
    let (base_apply, base_frame) = mount_cost(|| {
        let mut t = Transaction::new(2);
        for id in 1..=200 {
            t.create(id, NodeKind::View)
                .layout(id, &sized(24.0, 24.0))
                .place(0, id, NIL);
        }
        t
    });
    let (apply, frame) = mount_cost(|| {
        let mut t = Transaction::new(2);
        mount(&mut t, 1..201, 24.0, 24.0, |k| drawings[k].clone());
        t
    });
    println!(
        "200 icons at 24 px, mount: apply {apply:.0} µs, first frame {frame:.0} µs \
         (200 plain views: {base_apply:.0} + {base_frame:.0} µs)"
    );
    println!(
        "  vector share: {:.0} µs, {:.2} µs per icon",
        apply + frame - base_apply - base_frame,
        (apply + frame - base_apply - base_frame) / 200.0
    );

    // Hits: 200 more nodes drawing the same icons, then the same
    // drawings again on those nodes.
    let (mut hit_apply, mut hit_frame, mut same_apply, mut same_frame) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..RUNS {
        let mut ui = root();
        let mut t = Transaction::new(2);
        mount(&mut t, 1..201, 24.0, 24.0, |k| drawings[k].clone());
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
        let mut t = Transaction::new(3);
        mount(&mut t, 201..401, 24.0, 24.0, |k| drawings[k].clone());
        let t0 = Instant::now();
        ui.apply_txn(&t).unwrap();
        hit_apply.push(us(t0));
        let t1 = Instant::now();
        ui.render(VIEW);
        hit_frame.push(us(t1));
        let mut t = Transaction::new(4);
        for id in 1..201 {
            t.drawing(id, drawings[id as usize - 1].clone());
        }
        let t0 = Instant::now();
        ui.apply_txn(&t).unwrap();
        same_apply.push(us(t0));
        let t1 = Instant::now();
        ui.render(VIEW);
        same_frame.push(us(t1));
    }
    let (ha, hf) = (median(hit_apply), median(hit_frame));
    println!(
        "200 more nodes, icons already drawn: apply {ha:.0} µs, frame {hf:.0} µs \
         (vector share {:.2} µs per icon)",
        (ha + hf - base_apply - base_frame) / 200.0
    );
    println!(
        "same drawings resent to the same nodes (key built, compared): apply {:.0} µs, \
         frame {:.0} µs",
        median(same_apply),
        median(same_frame)
    );

    for (name, dashes) in [("solid", ""), ("dashed 6 3", "6 3")] {
        let line = sparkline(dashes);
        let parse = median(
            (0..RUNS)
                .map(|_| {
                    let t = Instant::now();
                    std::hint::black_box(line.build().unwrap());
                    us(t)
                })
                .collect(),
        );
        let (apply, frame) = mount_cost(|| {
            let mut t = Transaction::new(2);
            mount(&mut t, 1..2, 600.0, 100.0, |_| line.clone());
            t
        });
        println!(
            "sparkline 2,000 points, {name}: parse {parse:.0} µs; mount apply {apply:.0} µs, \
             first frame {frame:.0} µs"
        );
    }

    // Worst case within the bounds: one path of 1 KiB, referenced by
    // 4,096 shapes. It parses once; the key holds all 4 MiB.
    let path = "M2 2".to_string() + &"l.004.004".repeat(113);
    let worst = Drawing {
        view_box: "0 0 24 24".into(),
        shapes: vec![
            Shape {
                geometry: path.into(),
                ..Shape::default()
            };
            craie_vector::svg::MAX_SHAPES
        ],
    };
    worst.check().unwrap();
    let (apply, frame) = mount_cost(|| {
        let mut t = Transaction::new(2);
        mount(&mut t, 1..2, 24.0, 24.0, |_| worst.clone());
        t
    });
    println!(
        "4,096 shapes sharing one 1 KiB path (4 MiB of references): apply {apply:.0} µs, \
         first frame {frame:.0} µs"
    );
}
