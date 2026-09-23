//! E01 (owned paragraph versus Parley, ARCHITECTURE.md §5): cost per
//! case on the pinned fonts. Correctness is `tests/e01_text.rs`.
//!
//! Per case, at the case's first bounded width: retained heap bytes of
//! one laid-out paragraph (what dropping it frees; ours also by
//! `Paragraph::heap_bytes`), allocations and
//! median time of a cold layout (warm engine, new paragraph), a width
//! change (rewrap), a one-character edit, and a span color change. A
//! fresh engine's first layout (font loading) is reported once.
//!
//!   cargo run --release -p craie-harness --example e01_text

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::Instant;

use craie_harness::e01::{self, Oracle};

static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new as isize - l.size() as isize, Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const RUNS: usize = 1001;

/// Allocations of one call.
fn allocs<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let a = ALLOCS.load(Ordering::Relaxed);
    let out = f();
    (out, ALLOCS.load(Ordering::Relaxed) - a)
}

/// Heap bytes `x` holds: what dropping it frees.
fn held<T>(x: T) -> isize {
    let a = LIVE.load(Ordering::Relaxed);
    drop(x);
    a - LIVE.load(Ordering::Relaxed)
}

/// Median microseconds of `f` and of `g` over `RUNS` calls each,
/// interleaved so both see the same core and clock conditions.
fn medians(mut f: impl FnMut(), mut g: impl FnMut()) -> (f64, f64) {
    let time = |h: &mut dyn FnMut()| {
        let s = Instant::now();
        h();
        s.elapsed().as_secs_f64() * 1e6
    };
    let (mut a, mut b) = (Vec::with_capacity(RUNS), Vec::with_capacity(RUNS));
    for _ in 0..RUNS {
        a.push(time(&mut f));
        b.push(time(&mut g));
    }
    a.sort_by(f64::total_cmp);
    b.sort_by(f64::total_cmp);
    (a[RUNS / 2], b[RUNS / 2])
}

/// Asks macOS for a performance core (user-interactive QoS).
#[cfg(target_os = "macos")]
fn performance_core() {
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(class: u32, priority: i32) -> i32;
    }
    // QOS_CLASS_USER_INTERACTIVE
    unsafe { pthread_set_qos_class_self_np(0x21, 0) };
}

#[cfg(not(target_os = "macos"))]
fn performance_core() {}

fn main() {
    performance_core();
    // Fresh engine: font loading, fallback resolution, first shaping.
    let cases = e01::cases();
    let multi = cases.iter().find(|c| c.name == "multilingual").unwrap();
    let s = Instant::now();
    let mut engine = e01::engine();
    let _ = e01::layout(&mut engine, &multi.text, &multi.spans, None);
    let ours_first = s.elapsed().as_secs_f64() * 1e6;
    let s = Instant::now();
    let mut oracle = Oracle::new();
    let _ = oracle.layout(&multi.text, &multi.spans, None);
    let parley_first = s.elapsed().as_secs_f64() * 1e6;
    println!(
        "fresh engine, first multilingual layout: ours {ours_first:.0} us, Parley {parley_first:.0} us\n"
    );

    // Warm-up: every case on both sides for half a second, so the first
    // case does not pay for clock ramp-up and cold caches.
    let s = Instant::now();
    while s.elapsed().as_secs_f64() < 0.5 {
        for case in &cases {
            for &w in &case.widths {
                std::hint::black_box(e01::layout(&mut engine, &case.text, &case.spans, w));
                std::hint::black_box(oracle.layout(&case.text, &case.spans, w));
            }
        }
    }

    println!(
        "{:14} {:12} {:>5} {:>6} | {:>15} {:>13} | {:>13} {:>13} | {:>13} {:>9} | {:>13} {:>9} | {:>13}",
        "case",
        "class",
        "bytes",
        "width",
        "retained B o/P",
        "heap_bytes",
        "cold us o/P",
        "cold alloc o/P",
        "rewrap us o/P",
        "alloc o/P",
        "edit us o/P",
        "alloc o/P",
        "color us o/P"
    );
    for case in &cases {
        let w = case.widths.iter().copied().find(Option::is_some).flatten();
        let text = &case.text;
        let spans = &case.spans;
        let ours_bytes = held(e01::layout(&mut engine, text, spans, w));
        let parley_bytes = held(oracle.layout(text, spans, w));
        let mut ours = e01::layout(&mut engine, text, spans, w);
        let mut theirs = oracle.layout(text, spans, w);
        let ours_heap = ours.heap_bytes();
        let (_, ours_cold_allocs) = allocs(|| e01::layout(&mut engine, text, spans, w));
        let (_, parley_cold_allocs) = allocs(|| oracle.layout(text, spans, w));
        let (ours_cold, parley_cold) = medians(
            || {
                std::hint::black_box(e01::layout(&mut engine, text, spans, w));
            },
            || {
                std::hint::black_box(oracle.layout(text, spans, w));
            },
        );

        // Width change: alternate between the width and 3/4 of it (or 200
        // for unbounded cases).
        let other = Some(w.map_or(200.0, |w| w * 0.75));
        let (mut a, mut b) = (false, false);
        let (_, ours_rewrap_allocs) = allocs(|| engine.rewrap(&mut ours, other));
        let (_, parley_rewrap_allocs) = allocs(|| oracle.rewrap(&mut theirs, other));
        let (ours_rewrap, parley_rewrap) = medians(
            || {
                a = !a;
                engine.rewrap(&mut ours, if a { w } else { other });
            },
            || {
                b = !b;
                oracle.rewrap(&mut theirs, if b { w } else { other });
            },
        );

        // One-character edit mid-text: both lay the paragraph out again
        // (incremental reflow is E04).
        let mid = text.char_indices().nth(text.chars().count() / 2).unwrap().0;
        let edited = format!("{}x{}", &text[..mid], &text[mid..]);
        let shifted: Vec<_> = spans
            .iter()
            .map(|s| {
                let mut s = *s;
                if s.start as usize > mid {
                    s.start += 1;
                }
                s
            })
            .collect();
        let (_, ours_edit_allocs) = allocs(|| e01::layout(&mut engine, &edited, &shifted, w));
        let (_, parley_edit_allocs) = allocs(|| oracle.layout(&edited, &shifted, w));
        let (ours_edit, parley_edit) = medians(
            || {
                std::hint::black_box(e01::layout(&mut engine, &edited, &shifted, w));
            },
            || {
                std::hint::black_box(oracle.layout(&edited, &shifted, w));
            },
        );

        // Span color: ours stores a paint slot index per glyph, so a color
        // is a paint record write and no layout work (costs.rs
        // `color_change_does_no_shapes_and_no_layouts`). Parley bakes the
        // brush into the layout's styles: a new layout.
        let parley_color = parley_cold;

        println!(
            "{:14} {:12} {:>5} {:>6} | {:>7}/{:<7} {:>13} | {:>6.1}/{:<6.1} {:>6}/{:<6} | {:>6.2}/{:<6.2} {:>4}/{:<4} | {:>6.1}/{:<6.1} {:>4}/{:<4} | {:>6}/{:<6.1}",
            case.name,
            case.class,
            text.len(),
            w.map_or("none".to_string(), |w| format!("{w}")),
            ours_bytes,
            parley_bytes,
            ours_heap,
            ours_cold,
            parley_cold,
            ours_cold_allocs,
            parley_cold_allocs,
            ours_rewrap,
            parley_rewrap,
            ours_rewrap_allocs,
            parley_rewrap_allocs,
            ours_edit,
            parley_edit,
            ours_edit_allocs,
            parley_edit_allocs,
            "0",
            parley_color,
        );
    }
}
