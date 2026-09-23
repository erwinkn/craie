//! Allocation invariants (principle 4): steady-state frames allocate
//! nothing, and patches allocate a bounded amount. A counting global
//! allocator measures this test binary only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use craie_core::geom::{Affine, Size};
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
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const VIEW: Size = Size {
    width: 400.0,
    height: 300.0,
};

fn allocs(f: impl FnOnce()) -> usize {
    let a = ALLOCS.load(Ordering::Relaxed);
    f();
    ALLOCS.load(Ordering::Relaxed) - a
}

fn ui() -> Ui {
    let mut ui = Ui::new(2.0);
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View)
        .fill(0, 0x1415_18FF)
        .append(NIL, 0);
    for i in 1..=20u32 {
        t.create(i, NodeKind::Text)
            .text(i, format!("row {i} retained text"), 14.0, 0xFFFF_FFFF)
            .append(0, i);
    }
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    ui.render(VIEW);
    ui
}

/// One test function: the allocator counter is process-wide, so the
/// cases run in sequence.
#[test]
fn steady_frames_do_not_allocate() {
    let mut ui = ui();
    // Unchanged frame: nothing at all.
    let n = allocs(|| {
        ui.render(VIEW);
    });
    assert_eq!(n, 0, "unchanged frame");

    // Copies are counted: a color-only paragraph change copies no text.
    let before = ui.counters();
    let mut t = Transaction::new(2);
    t.text(3, "row 3 retained text", 14.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    assert_eq!(ui.counters().since(&before).copied_bytes, 0);
    let before = ui.counters();
    let mut t = Transaction::new(3);
    t.text(3, "changed", 14.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    assert_eq!(ui.counters().since(&before).copied_bytes, 7);
    ui.render(VIEW);

    // Warm patches allocate at most once (queues and scratch are reused;
    // the transaction itself is built outside the measurement).
    let mut t = Transaction::new(4);
    t.fill(0, 0x2233_44FF);
    let n = allocs(|| {
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
    });
    assert!(n <= 1, "color patch allocated {n} times");
    let mut t = Transaction::new(5);
    t.transform(3, Affine::translate(4.0, 0.0));
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let mut t = Transaction::new(6);
    t.transform(3, Affine::translate(8.0, 0.0));
    let n = allocs(|| {
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
    });
    assert!(n <= 1, "transform patch allocated {n} times");
}
