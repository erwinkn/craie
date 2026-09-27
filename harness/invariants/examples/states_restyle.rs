//! State styles (ARCHITECTURE-update topic 5): what a native restyle
//! costs, with no JS round trip.
//!
//! - hover: a 200 × 40 scope (the row) and N = 1, 100 or 1,000
//!   dependents elsewhere, each with `_row: { _hover: fill }`. One
//!   hover change is a pointer move in or out of the row, then a frame:
//!   dispatch (hit test, input bits, restyle) and render (paint patch).
//!   Against the same moves over a row that only listens for enter and
//!   leave, with no scope or table: the difference is the state work.
//! - breakpoint: 1,000 rows 36 pt tall with `_narrow: { height: 44 }`;
//!   the window crosses 1,023 pt back and forth. Against the same
//!   resize with no variant tables, and with no tables but a
//!   transaction setting the 1,000 heights directly (what an app
//!   without variants sends; JS time not included).
//!
//! Times are the mean per change; allocs are allocation calls per
//! change; the frame counters say what the change touched. Events go
//! to a buffer kept across changes, so allocs are the UI's own.
//!
//!   cargo run --release -p craie-harness --example states_restyle

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie_core::geom::Size;
use craie_layout::LayoutRow;
use craie_ui::events::{Event, UiEvent, mask};
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::states::{
    TermDecl, Values, VariantDecl, env_bit, layout_key, state_bit, value_field,
};
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

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

const WIDE: Size = Size {
    width: 1440.0,
    height: 900.0,
};
const NARROW: Size = Size {
    width: 900.0,
    height: 900.0,
};
const RUNS: usize = 200;

fn style(f: impl FnOnce(&mut taffy::Style)) -> taffy::Style {
    let mut s = craie_ui::host::default_style().to_taffy();
    f(&mut s);
    s
}

fn sized(w: f32, h: f32) -> taffy::Style {
    style(|s| {
        s.size = taffy::Size {
            width: taffy::Dimension::length(w),
            height: taffy::Dimension::length(h),
        }
    })
}

/// A root filling the window: a column, or wrapped rows.
fn root(t: &mut Transaction, wrap: bool) {
    t.create(0, NodeKind::View)
        .layout(
            0,
            &style(|s| {
                s.size = taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                };
                if wrap {
                    s.flex_direction = taffy::FlexDirection::Row;
                    s.flex_wrap = taffy::FlexWrap::Wrap;
                    s.align_content = Some(taffy::AlignContent::FLEX_START);
                }
            }),
        )
        .place(NIL, 0, NIL);
}

struct Cost {
    us: f64,
    allocs: f64,
    patched: f64,
    layout_passes: f64,
}

/// Mean time, allocation calls and counters of `f` over `RUNS` calls,
/// events drained between calls (off the clock).
fn cost(ui: &mut Ui, mut f: impl FnMut(&mut Ui, usize)) -> Cost {
    let mut events: Vec<UiEvent> = Vec::with_capacity(64);
    // Warm the buffers once each way.
    f(ui, 0);
    f(ui, 1);
    ui.drain_events(&mut events);
    events.clear();
    let before = ui.counters();
    let mut ns = 0u128;
    let mut count = 0;
    for i in 0..RUNS {
        let a = allocs();
        let t = Instant::now();
        f(ui, i);
        ns += t.elapsed().as_nanos();
        count += allocs() - a;
        ui.drain_events(&mut events);
        events.clear();
    }
    let spent = ui.counters().since(&before);
    let per = |v: u64| v as f64 / RUNS as f64;
    Cost {
        us: ns as f64 / RUNS as f64 / 1e3,
        allocs: count as f64 / RUNS as f64,
        patched: per(spent.paints_patched),
        layout_passes: per(spent.layout_passes),
    }
}

fn print(label: &str, c: &Cost) {
    println!(
        "{label:<42} {:>9.2} µs {:>7.1} allocs {:>7.1} paints patched {:>4.1} layouts",
        c.us, c.allocs, c.patched, c.layout_passes
    );
}

/// The row (id 1, a scope) at the top left, then `n` 6 pt cells
/// wrapped after it, each filled while the row is hovered. Without
/// `scopes`, the row only listens and the cells have no table.
fn hover_tree(n: u32, scopes: bool) -> Ui {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    root(&mut t, true);
    t.create(1, NodeKind::View)
        .layout(1, &sized(200.0, 40.0))
        .interaction(1, mask::POINTER_ENTER_LEAVE, false)
        .place(0, 1, NIL);
    if scopes {
        t.states(1, 0);
    }
    let hovered = [VariantDecl {
        terms: vec![TermDecl {
            scope: 1,
            mask: state_bit::HOVER,
        }],
        env: 0,
        values: Values {
            mask: value_field::FILL,
            fill: 0x3434_4AFF,
            ..Values::default()
        },
    }];
    let cell = sized(6.0, 6.0);
    for id in 2..2 + n {
        t.create(id, NodeKind::View)
            .layout(id, &cell)
            .fill(id, 0x1B1D_22FF)
            .place(0, id, NIL);
        if scopes {
            t.variants(id, &hovered);
        }
    }
    ui.apply_txn(&t).unwrap();
    ui.render(WIDE);
    ui
}

fn row_style(height: f32) -> taffy::Style {
    style(|s| {
        s.size.height = taffy::Dimension::length(height);
        s.flex_shrink = 0.0;
    })
}

/// 1,000 rows 36 pt tall in a column, each 44 pt when narrow (with
/// `variants`).
fn breakpoint_tree(variants: bool) -> Ui {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    root(&mut t, false);
    let row = row_style(36.0);
    let tall = row_style(44.0);
    let narrow = [VariantDecl {
        terms: Vec::new(),
        env: env_bit::NARROW,
        values: Values {
            mask: value_field::LAYOUT,
            layout_keys: layout_key::HEIGHT,
            layout: LayoutRow::from(&tall),
            ..Values::default()
        },
    }];
    for id in 1..=1000 {
        t.create(id, NodeKind::View)
            .layout(id, &row)
            .place(0, id, NIL);
        if variants {
            t.variants(id, &narrow);
        }
    }
    ui.apply_txn(&t).unwrap();
    ui.render(WIDE);
    ui
}

fn main() {
    println!(
        "State styles, {}x{} logical @2x; mean per change over {RUNS}",
        WIDE.width, WIDE.height
    );
    let hover = |ui: &mut Ui, i: usize| {
        let x = if i.is_multiple_of(2) { 100.0 } else { 1300.0 };
        let y = if i.is_multiple_of(2) { 20.0 } else { 880.0 };
        ui.dispatch(&Event::PointerMove { x, y });
        ui.render(WIDE);
    };
    let mut ui = hover_tree(1_000, false);
    let c = cost(&mut ui, hover);
    print("hover, no scopes (1,000 cells)", &c);
    assert_eq!(c.patched, 0.0, "nothing reads hover");
    for n in [1, 100, 1_000] {
        let mut ui = hover_tree(n, true);
        let c = cost(&mut ui, hover);
        print(&format!("hover, {n} dependents"), &c);
        assert!(c.patched >= n as f64, "every dependent repaints");
        assert_eq!(c.layout_passes, 0.0, "a fill never relayouts");
    }
    for variants in [false, true] {
        let mut ui = breakpoint_tree(variants);
        let c = cost(&mut ui, |ui, i| {
            ui.render(if i % 2 == 0 { NARROW } else { WIDE });
        });
        let label = if variants {
            "breakpoint, 1,000 _narrow rows"
        } else {
            "breakpoint, no tables (resize only)"
        };
        print(label, &c);
    }
    // The heights sent directly: one transaction per crossing.
    let mut ui = breakpoint_tree(false);
    let heights = |seq: u64, h: f32| {
        let mut t = Transaction::new(seq);
        let s = row_style(h);
        for id in 1..=1000 {
            t.layout(id, &s);
        }
        t
    };
    let (tall, short) = (heights(2, 44.0), heights(3, 36.0));
    let c = cost(&mut ui, |ui, i| {
        let (size, t) = if i % 2 == 0 {
            (NARROW, &tall)
        } else {
            (WIDE, &short)
        };
        ui.apply_txn(t).unwrap();
        ui.render(size);
    });
    print("breakpoint, no tables, 1,000 heights sent", &c);
}
