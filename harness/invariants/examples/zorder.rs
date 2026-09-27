//! Sibling z (work item 4): what a z costs.
//!
//! - One z change in a parent of 5,000 children, in a tree of 5k and
//!   of 100k nodes (20 such parents, as E15's wide tree): the re-sort
//!   alone (`refresh_orders`), and the whole next frame (the sort plus
//!   the draw-order rebuild, which walks the whole tree), against a
//!   frame after one transform change. From z all 0 (the first sort)
//!   and with 1 in 10 children already raised or lowered.
//! - Hit tests at 100k nodes along E15's pointer path, walking and with
//!   the reach index, with no z and with 1 in 10 cells given a z.
//!
//! Times are means; allocs are allocation calls per call.
//!
//!   cargo run --release -p craie-harness --example zorder

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::geom::{Affine, Size};
use craie_core::rng::Rng;
use craie_ui::events::mask;
use craie_ui::host::NodeId;
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

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

const VIEW: Size = Size {
    width: 1440.0,
    height: 900.0,
};
const POINTER: u32 =
    mask::POINTER_MOVE | mask::POINTER_DOWN | mask::POINTER_UP | mask::POINTER_ENTER_LEAVE;

fn style(f: impl FnOnce(&mut taffy::Style)) -> taffy::Style {
    let mut s = craie_ui::host::default_style().to_taffy();
    f(&mut s);
    s
}

fn wrap_row(s: &mut taffy::Style) {
    s.flex_direction = taffy::FlexDirection::Row;
    s.flex_wrap = taffy::FlexWrap::Wrap;
    s.align_content = Some(taffy::AlignContent::FLEX_START);
}

/// Root 1 holding groups of up to 5,000 filled 6 pt cells, `n` nodes
/// in all, every 50th cell listening. Returns the ui and the groups.
fn wide(n: u32) -> (Ui, Vec<u32>) {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(1, NodeKind::View)
        .layout(
            1,
            &style(|s| {
                s.size = taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                };
                wrap_row(s);
            }),
        )
        .append(NIL, 1);
    let group = style(|s| {
        s.size.width = taffy::Dimension::percent(1.0);
        wrap_row(s);
    });
    let cell = style(|s| {
        s.size = taffy::Size {
            width: taffy::Dimension::length(6.0),
            height: taffy::Dimension::length(6.0),
        };
    });
    let (mut next, mut left, mut groups) = (2, n - 1, Vec::new());
    while left > 0 {
        let g = next;
        next += 1;
        t.create(g, NodeKind::View).layout(g, &group).append(1, g);
        groups.push(g);
        for k in 0..left.min(5000) - 1 {
            let c = next;
            next += 1;
            t.create(c, NodeKind::View)
                .layout(c, &cell)
                .fill(c, 0x3366_99FF)
                .append(g, c);
            if k % 50 == 0 {
                t.interaction(c, POINTER, false);
            }
        }
        left -= left.min(5000);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    (ui, groups)
}

/// Gives 1 in 10 of `group`'s children a z of ±1.
fn scatter(ui: &mut Ui, rng: &mut Rng, group: u32, seq: u64) {
    let kids: Vec<u32> = ui
        .host
        .children(NodeId(group))
        .iter()
        .map(|c| c.0)
        .collect();
    let mut t = Transaction::new(seq);
    for c in kids {
        if rng.chance(0.1) {
            t.z(c, if rng.chance(0.5) { 1 } else { -1 });
        }
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
}

/// E15's pointer path: a Lissajous curve, 1,000 points.
fn path() -> Vec<(f32, f32)> {
    (0..1000)
        .map(|i| {
            let t = i as f32 / 1000.0 * std::f32::consts::TAU;
            let x = (0.5 + 0.49 * (3.0 * t).sin()) * VIEW.width;
            let y = (0.5 + 0.49 * (4.0 * t + 0.7).sin()) * VIEW.height;
            (x, y)
        })
        .collect()
}

/// Mean µs and allocation calls of `f` over `runs` calls.
fn cost(runs: usize, mut f: impl FnMut(usize)) -> (f64, f64) {
    f(0);
    let a = ALLOCS.load(Ordering::Relaxed);
    let t = Instant::now();
    for i in 0..runs {
        f(i);
    }
    let us = t.elapsed().as_secs_f64() * 1e6 / runs as f64;
    let allocs = (ALLOCS.load(Ordering::Relaxed) - a) as f64 / runs as f64;
    (us, allocs)
}

/// Mean µs of the re-sort and of the next frame after each of 50 edits.
fn after(ui: &mut Ui, mut edit: impl FnMut(&mut Transaction, usize)) -> (f64, f64) {
    let (mut sort, mut frame) = (0.0, 0.0);
    for i in 0..50 {
        let mut t = Transaction::new(10 + i as u64);
        edit(&mut t, i);
        ui.apply_txn(&t).unwrap();
        let t0 = Instant::now();
        ui.host.refresh_orders();
        let t1 = Instant::now();
        ui.render(VIEW);
        sort += (t1 - t0).as_secs_f64() * 1e6;
        frame += t1.elapsed().as_secs_f64() * 1e6;
    }
    (sort / 50.0, frame / 50.0)
}

fn main() {
    println!("Sibling z, {}x{} logical @2x", VIEW.width, VIEW.height);
    let mut rng = Rng::new(7);

    for n in [5_001, 100_000] {
        println!("\none z change in a parent of 5,000 children, {n} nodes (mean of 50)");
        let (mut ui, groups) = wide(n);
        let g = groups[0];
        let target = ui.host.children(NodeId(g))[2_500].0;
        for scattered in [false, true] {
            if scattered {
                scatter(&mut ui, &mut rng, g, 2);
            }
            let label = if scattered {
                "1 in 10 with z"
            } else {
                "all z 0       "
            };
            let (sort, frame) = after(&mut ui, |t, i| {
                t.z(target, if i % 2 == 0 { 2 } else { 0 });
            });
            let (_, spin) = after(&mut ui, |t, i| {
                t.transform(target, Affine::rotate(i as f32 * 0.1));
            });
            println!(
                "  {label}  re-sort {sort:>8.1} µs  frame {frame:>8.1} µs  (frame after a transform: {spin:.1} µs)"
            );
        }
    }

    println!("\nhit test at 100k nodes, along 1,000 points (mean per test)");
    let moves = path();
    let (mut ui, groups) = wide(100_000);
    for with_z in [false, true] {
        if with_z {
            for (k, &g) in groups.iter().enumerate() {
                scatter(&mut ui, &mut rng, g, 4 + k as u64);
            }
        }
        ui.refresh_reach();
        ui.host.refresh_orders();
        let mut hits = 0;
        for &(x, y) in &moves {
            let h = ui.hit_test(x, y);
            assert_eq!(
                h,
                ui.hit_test_walk(x, y),
                "index and walk disagree at ({x}, {y})"
            );
            hits += h.is_some() as usize;
        }
        let walk = cost(moves.len(), |i| {
            std::hint::black_box(ui.hit_test_walk(moves[i].0, moves[i].1));
        });
        let index = cost(moves.len(), |i| {
            std::hint::black_box(ui.hit_test(moves[i].0, moves[i].1));
        });
        let label = if with_z {
            "z on 1 in 10 cells"
        } else {
            "no z              "
        };
        println!(
            "  {label}  walk {:>8.2} µs {:>4.1} allocs  index {:>6.2} µs {:>4.1} allocs  ({hits} hit)",
            walk.0, walk.1, index.0, index.1
        );
    }
}
