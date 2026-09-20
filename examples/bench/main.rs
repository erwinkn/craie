//! Retained-state benchmark: 5,000-row list through the full pipeline.
//!
//!   cargo run --release --example bench
//!
//! Measures, per phase: wire decode+apply, Taffy layout (with Parley text
//! measure on leaves), and scene paint (glyph emit). Then a partial update
//! of 500 rows to show incremental cost. A counting allocator reports live
//! heap bytes attributed to each phase.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie::geom::Size;
use craie::ui::Ui;
use craie::wire::Encoder;
use taffy::{Dimension, FlexDirection, LengthPercentage, Rect, Style};

const ROWS: u32 = 5000;
const UPDATE_ROWS: u32 = 500;
const WIDTH: f32 = 1600.0;
const HEIGHT: f32 = 1000.0;
const KIND_VIEW: u8 = 0;
const KIND_TEXT: u8 = 1;
const NIL: u32 = u32::MAX;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn counters() -> (usize, usize) {
    (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

fn build_txn() -> Vec<u8> {
    let mut enc = Encoder::new();
    let column = Style {
        display: taffy::Display::Flex,
        flex_direction: FlexDirection::Column,
        size: taffy::Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        },
        ..Style::default()
    };
    let row_style = Style {
        display: taffy::Display::Flex,
        padding: Rect {
            left: LengthPercentage::length(8.0),
            right: LengthPercentage::length(8.0),
            top: LengthPercentage::length(4.0),
            bottom: LengthPercentage::length(4.0),
        },
        ..Style::default()
    };
    enc.style(0, &column);
    enc.style(1, &row_style);

    enc.create(0, KIND_VIEW);
    enc.set_style(0, 0);
    enc.view_paint(0, 0x1415_18FF);
    enc.place(NIL, 0, NIL);

    for i in 0..ROWS {
        let row = 1 + i * 2;
        let text = row + 1;
        enc.create(row, KIND_VIEW);
        enc.set_style(row, 1);
        enc.view_paint(row, if i % 2 == 0 { 0x1B1D_24FF } else { 0x2022_2BFF });
        enc.place(0, row, NIL);
        enc.create(text, KIND_TEXT);
        enc.set_text(
            text,
            &format!("row {i}: the quick brown fox jumps over {i} lazy dogs"),
        );
        enc.text_props(text, 14.0, 0xECEC_F0FF);
        enc.place(row, text, NIL);
    }
    enc.finish(1)
}

fn update_txn(seq: u64) -> Vec<u8> {
    let mut enc = Encoder::new();
    for i in (0..ROWS).step_by((ROWS / UPDATE_ROWS) as usize) {
        let text = 1 + i * 2 + 1;
        enc.set_text(text, &format!("row {i}: UPDATED at seq {seq}"));
    }
    enc.finish(seq)
}

fn report(name: &str, t: Instant, allocs0: usize, _bytes0: usize) {
    let (a1, b1) = counters();
    eprintln!(
        "{name:>22}: {:>8.2} ms, {:>7} allocs, live {:>8} KiB",
        t.elapsed().as_secs_f64() * 1000.0,
        a1 - allocs0,
        b1 / 1024
    );
}

fn main() {
    let mut ui = Ui::new(2.0);

    // --- encode (JS-side cost, reported for completeness) -------------
    let t = Instant::now();
    let (a0, b0) = counters();
    let buf = build_txn();
    report("encode 5000 rows", t, a0, b0);
    eprintln!("{:>22}: {} bytes on the wire", "txn size", buf.len());

    // --- apply ---------------------------------------------------------
    let t = Instant::now();
    let (a0, b0) = counters();
    ui.apply(&buf).unwrap();
    report("decode + apply", t, a0, b0);

    // --- layout + paint (cold) -----------------------------------------
    let t = Instant::now();
    let (a0, b0) = counters();
    let scene = ui.render(Size::new(WIDTH, HEIGHT)).clone();
    report("layout+paint cold", t, a0, b0);
    eprintln!(
        "{:>22}: {} quads, {} glyphs, {} rasters",
        "scene",
        scene.quads.len(),
        scene.glyphs.len(),
        ui.text.cache.stats.rasters
    );

    // --- steady-state repaint (no mutations) ---------------------------
    let t = Instant::now();
    let (a0, b0) = counters();
    let clean = !ui.needs_paint();
    let scene2 = ui.render(Size::new(WIDTH, HEIGHT)).clone();
    report("layout+paint warm", t, a0, b0);
    eprintln!(
        "{:>22}: clean={clean}, {} glyphs, {} rasters",
        "warm render",
        scene2.glyphs.len(),
        ui.text.cache.stats.rasters
    );

    // --- incremental update: 500 rows -----------------------------------
    let buf = update_txn(2);
    eprintln!("{:>22}: {} bytes on the wire", "update txn", buf.len());
    let t = Instant::now();
    let (a0, b0) = counters();
    ui.apply(&buf).unwrap();
    report("apply 500 updates", t, a0, b0);

    let t = Instant::now();
    let (a0, b0) = counters();
    let scene3 = ui.render(Size::new(WIDTH, HEIGHT)).clone();
    let rasters = ui.text.cache.stats.rasters;
    report("layout+paint 500", t, a0, b0);
    eprintln!(
        "{:>22}: {} glyphs, {} total rasters, {} glyph cache entries",
        "after update",
        scene3.glyphs.len(),
        rasters,
        ui.text.cache.len()
    );

    // --- structural ------------------------------------------------------
    let header = std::mem::size_of::<craie::host::NodeHeader>();
    let (_, bytes) = counters();
    eprintln!(
        "{:>22}: {} nodes x {header} B = {} KiB headers; live heap {} KiB",
        "retained state",
        ui.host.len(),
        ui.host.len() * header / 1024,
        bytes / 1024
    );
}
