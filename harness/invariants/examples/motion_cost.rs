//! Keyframe animations (ARCHITECTURE-update topic 7): what `enter` adds
//! to a mount, and what a frame of running animations costs.
//!
//! - mount: 1,000 rows (a 36 pt View with a fill and a label), decoded
//!   from wire bytes, applied and rendered once; plain, and with every
//!   row carrying `enter` (opacity 0 and translate y 8 at 0, 200 ms, one
//!   keyframes list shared on the wire). Median of 15 fresh `Ui`s.
//! - frames: the mean frame (sample, patch, render) while the 1,000
//!   enters run; with one row looping a rotation and 999 still; with
//!   every row looping; and with nothing running. Allocs are allocation
//!   calls per frame.
//!
//!   cargo run --release -p craie-harness --example motion_cost

use std::alloc::{GlobalAlloc, Layout, System};
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::geom::Size;
use craie_ui::keyframes::{Animation, Easing, Fill, Frame, Keyframes, Sample, Trigger};
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::states::value_field;
use craie_ui::ui::Ui;
use craie_ui::wire;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

const VIEW: Size = Size {
    width: 1440.0,
    height: 900.0,
};
const ROWS: u32 = 1000;

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

fn enter() -> Animation {
    let mut a = Animation::new(
        Arc::new(Keyframes::new(vec![frame(
            0.0,
            value_field::OPACITY | value_field::TRANSLATE_Y,
            |s| {
                s.opacity = 0.0;
                s.translate[1] = 8.0;
            },
        )])),
        0.2,
        Easing::Bezier([0.23, 1.0, 0.32, 1.0]),
    );
    a.fill = Fill::Backwards;
    a
}

fn spin() -> Animation {
    let turn = |at: f32, r: f32| frame(at, value_field::ROTATE, |s| s.rotate = r);
    let mut a = Animation::new(
        Arc::new(Keyframes::new(vec![turn(0.0, 0.0), turn(1.0, TAU)])),
        1.0,
        Easing::Bezier([0.0, 0.0, 1.0, 1.0]),
    );
    a.iterations = f32::INFINITY;
    a
}

/// The mount transaction: a scrolling column of `ROWS` rows.
fn rows(with_enter: bool) -> Vec<u8> {
    let mut t = Transaction::new(1);
    let mut col = craie_ui::host::default_style().to_taffy();
    col.flex_direction = taffy::FlexDirection::Column;
    col.size.height = taffy::Dimension::percent(1.0);
    col.overflow.y = taffy::Overflow::Scroll;
    t.create(0, NodeKind::View).layout(0, &col).append(NIL, 0);
    let mut row = craie_ui::host::default_style().to_taffy();
    row.size.height = taffy::Dimension::length(36.0);
    row.flex_shrink = 0.0;
    let anim = [enter()];
    let labels: Vec<String> = (0..ROWS).map(|i| format!("Row {i}")).collect();
    for (i, text) in (0..ROWS).zip(&labels) {
        let (id, label) = (1 + 2 * i, 2 + 2 * i);
        t.create(id, NodeKind::View)
            .layout(id, &row)
            .fill(id, 0x1b1d_22ff)
            .append(0, id)
            .create(label, NodeKind::Text)
            .text(label, text, 14.0, 0xffff_ffff)
            .append(id, label);
        if with_enter {
            t.animation(id, Trigger::Enter, false, &anim);
        }
    }
    wire::encode(&t)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn mount(buf: &[u8]) -> (f64, Ui) {
    let mut ui = Ui::new(2.0);
    let t0 = Instant::now();
    ui.apply(buf).unwrap();
    ui.render(VIEW);
    (t0.elapsed().as_secs_f64() * 1e3, ui)
}

/// Mean ms and allocs per frame over `n` frames from `from`, 1/120 s
/// apart.
fn frames(ui: &mut Ui, from: f64, n: usize) -> (f64, f64) {
    let a0 = allocs();
    let t0 = Instant::now();
    for i in 0..n {
        ui.set_time(from + i as f64 / 120.0);
        ui.render(VIEW);
    }
    let ms = t0.elapsed().as_secs_f64() * 1e3 / n as f64;
    (ms, (allocs() - a0) as f64 / n as f64)
}

fn main() {
    println!("mount, {ROWS} rows (median of 15, ms):");
    for with_enter in [false, true] {
        let buf = rows(with_enter);
        let times = (0..15).map(|_| mount(&buf).0).collect();
        let name = if with_enter { "with enter" } else { "plain" };
        println!(
            "  {name:<12} {:.2}  ({} wire bytes)",
            median(times),
            buf.len()
        );
    }

    println!("frames (mean ms, allocs per frame):");
    // The enters run over [0, 0.2]: frames from 0.01 to 0.19.
    let (_, mut ui) = mount(&rows(true));
    let (ms, al) = frames(&mut ui, 0.01, 22);
    println!(
        "  1,000 enters   {ms:.3}  {al:.1}  (live {})",
        ui.motion().live()
    );
    ui.set_time(1.0);
    ui.render(VIEW);
    let (ms, al) = frames(&mut ui, 1.0, 120);
    println!("  still          {ms:.3}  {al:.1}");

    let mut t = Transaction::new(ui.seq + 1);
    t.animation(1, Trigger::Base, false, &[spin()]);
    ui.apply_txn(&t).unwrap();
    // Past the first frames (tweens, topology for the new record).
    frames(&mut ui, 2.0, 30);
    let (ms, al) = frames(&mut ui, 2.3, 120);
    println!(
        "  1 loop         {ms:.3}  {al:.1}  (live {})",
        ui.motion().live()
    );

    let mut t = Transaction::new(ui.seq + 1);
    let list = [spin()];
    for i in 0..ROWS {
        t.animation(1 + 2 * i, Trigger::Base, false, &list);
    }
    ui.apply_txn(&t).unwrap();
    frames(&mut ui, 4.0, 30);
    let (ms, al) = frames(&mut ui, 4.3, 120);
    println!(
        "  1,000 loops    {ms:.3}  {al:.1}  (live {})",
        ui.motion().live()
    );
}
