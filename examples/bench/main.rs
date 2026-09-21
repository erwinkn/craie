//! Retained-state benchmarks with independently timed phases.
//!
//!   cargo run --release --example bench
//!
//! Two workloads:
//!   1. The synthetic 5,000-row list (uniform rows, worst case for
//!      retained-text memory).
//!   2. A Marbre-like transcript: mixed message rows — headers, wrapped
//!      paragraphs, code blocks — then a streamed response appended one
//!      transaction per token, which is what the product actually does.
//!
//! Every phase reports wall time and allocations; a counting allocator
//! tracks live heap. Layout and paint are timed separately via
//! `Ui::layout` / `Ui::paint`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie::geom::Size;
use craie::ui::Ui;
use craie::wire::Encoder;
use taffy::{Dimension, FlexDirection, LengthPercentage, Rect, Style};

const KIND_VIEW: u8 = 0;
const KIND_TEXT: u8 = 1;
const NIL: u32 = u32::MAX;
const SCALE: f32 = 2.0;
/// Logical viewport ~1600x1000 physical at 2x.
const VIEW: Size = Size {
    width: 800.0,
    height: 500.0,
};

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

fn live() -> usize {
    BYTES.load(Ordering::Relaxed) / 1024
}

struct Phase {
    name: &'static str,
    ms: f64,
    allocs: usize,
}

struct Timer {
    allocs0: usize,
    t: Instant,
}

impl Timer {
    fn start() -> Timer {
        Timer {
            allocs0: ALLOCS.load(Ordering::Relaxed),
            t: Instant::now(),
        }
    }
    fn stop(self, name: &'static str) -> Phase {
        Phase {
            name,
            ms: self.t.elapsed().as_secs_f64() * 1000.0,
            allocs: ALLOCS.load(Ordering::Relaxed) - self.allocs0,
        }
    }
}

fn report(p: &Phase) {
    eprintln!("  {:>26}: {:>8.2} ms, {:>7} allocs", p.name, p.ms, p.allocs);
}

fn style(f: impl FnOnce(&mut Style)) -> Style {
    let mut s = Style::default();
    f(&mut s);
    s
}

// ---------------------------------------------------------- workload 1

const ROWS: u32 = 5000;
const UPDATE_ROWS: u32 = 500;

fn build_rows_txn() -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.style(
        0,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.flex_direction = FlexDirection::Column;
            s.size = taffy::Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            };
        }),
    );
    enc.style(
        1,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.padding = Rect {
                left: LengthPercentage::length(8.0),
                right: LengthPercentage::length(8.0),
                top: LengthPercentage::length(4.0),
                bottom: LengthPercentage::length(4.0),
            };
        }),
    );

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

fn update_rows_txn(seq: u64) -> Vec<u8> {
    let mut enc = Encoder::new();
    for i in (0..ROWS).step_by((ROWS / UPDATE_ROWS) as usize) {
        let text = 1 + i * 2 + 1;
        enc.set_text(text, &format!("row {i}: UPDATED at seq {seq}"));
    }
    enc.finish(seq)
}

fn bench_rows() {
    eprintln!("\n== 5k uniform rows ==");
    let mut ui = Ui::new(SCALE);

    let t = Timer::start();
    let buf = build_rows_txn();
    report(&t.stop("encode 5000 rows"));
    eprintln!("  {:>26}: {} bytes on the wire", "txn", buf.len());

    let t = Timer::start();
    ui.apply(&buf).unwrap();
    report(&t.stop("decode + apply"));

    let t = Timer::start();
    let (compute_ms, round_ms) = ui.layout_timed(VIEW);
    report(&t.stop("layout (cold)"));
    eprintln!(
        "  {:>26}: compute {compute_ms:.2} ms, rounding {round_ms:.2} ms",
        "layout split"
    );

    let t = Timer::start();
    ui.paint(VIEW);
    report(&t.stop("paint+emit (cold)"));
    eprintln!(
        "  {:>26}: {} instances, {} rasters, {} cache entries",
        "scene",
        ui.scene().items.len(),
        ui.text.cache.stats.rasters,
        ui.text.cache.len()
    );

    // Warm repaint: nothing changed. Emitted batches replay verbatim.
    let t = Timer::start();
    ui.paint(VIEW);
    report(&t.stop("paint (warm, unchanged)"));

    // Warm layout: clean tree, all Taffy cache hits.
    let t = Timer::start();
    ui.layout(VIEW);
    report(&t.stop("layout (warm, unchanged)"));
    eprintln!(
        "  {:>26}: {} hits / {} misses total",
        "taffy cache",
        ui.layouts.cache_hits,
        ui.layouts.cache_misses
    );

    // Incremental: 500 text updates, the classic "some rows changed" case.
    let buf = update_rows_txn(2);
    let t = Timer::start();
    ui.apply(&buf).unwrap();
    report(&t.stop("apply 500 text updates"));

    let t = Timer::start();
    ui.layout(VIEW);
    report(&t.stop("layout (500 dirty)"));

    let t = Timer::start();
    ui.paint(VIEW);
    report(&t.stop("paint (500 dirty)"));

    let header = std::mem::size_of::<craie::host::NodeHeader>();
    eprintln!(
        "  {:>26}: {} nodes x {header} B = {} KiB headers; live heap {} KiB",
        "retained state",
        ui.host.len(),
        ui.host.len() * header / 1024,
        live()
    );

    // --- production overheads ------------------------------------------
    // Session commit copy: the napi boundary does Vec::from(&[u8]) — one
    // memcpy per commit — then the UI thread drains under one lock.
    let session = craie::bridge::Session::new();
    let txn = build_rows_txn();
    let t = Timer::start();
    session.submit(Vec::from(&txn[..])).unwrap();
    let drained = session.take_commits();
    report(&t.stop("commit copy + drain"));
    eprintln!(
        "  {:>26}: {} commits, {} bytes",
        "drained",
        drained.len(),
        drained.iter().map(Vec::len).sum::<usize>()
    );

    bench_gpu(&mut ui);
}

/// wgpu overhead on the built scene: atlas upload, instance-buffer write,
/// one draw call, GPU completion. Headless adapter.
fn bench_gpu(ui: &mut Ui) {
    use craie::gpu::{Gpu, Renderer};
    let gpu = Gpu::headless();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = Renderer::new(&gpu, format);
    let (w, h) = ((VIEW.width * SCALE) as u32, (VIEW.height * SCALE) as u32);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());

    let t = Timer::start();
    renderer.sync_atlas(&gpu, &mut ui.text.atlas);
    report(&t.stop("atlas upload"));
    eprintln!(
        "  {:>26}: {} bytes uploaded",
        "atlas",
        renderer.atlas_upload_bytes
    );

    let scene = ui.scene().clone();
    let t = Timer::start();
    renderer.draw(&gpu, &view, w, h, &scene);
    report(&t.stop("draw submit (upload+1 call)"));

    let t = Timer::start();
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    report(&t.stop("gpu completion (first)"));

    // Steady state: same scene again — warmup effects excluded.
    let t = Timer::start();
    renderer.draw(&gpu, &view, w, h, &scene);
    report(&t.stop("draw submit (warm)"));
    let t = Timer::start();
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .unwrap();
    report(&t.stop("gpu completion (warm)"));
}

// ---------------------------------------------------------- workload 2

const MESSAGES: u32 = 2000;
const STREAM_TOKENS: u32 = 300;

/// A message row: avatar gutter view + a column of (header, body, maybe
/// code block) texts. Mirrors a chat transcript's real shape — variable
/// text length and a mix of leaf kinds per row.
fn message(enc: &mut Encoder, base: u32, i: u32) {
    let row = base;
    let avatar = base + 1;
    let col = base + 2;
    let header = base + 3;
    let body = base + 4;
    let code = base + 5;

    enc.create(row, KIND_VIEW);
    enc.set_style(row, 1); // row style
    enc.place(0, row, NIL);

    enc.create(avatar, KIND_VIEW);
    enc.set_style(avatar, 2); // 28x28 avatar
    enc.view_paint(avatar, 0x3A3D_4AFF);
    enc.place(row, avatar, NIL);

    enc.create(col, KIND_VIEW);
    enc.set_style(col, 3); // column
    enc.place(row, col, NIL);

    enc.create(header, KIND_TEXT);
    enc.set_text(header, &format!("user-{}", i % 17));
    enc.text_props(header, 13.0, 0x9AA0_AEFF);
    enc.place(col, header, NIL);

    let body_text = match i % 5 {
        0 => "Short reply.".to_string(),
        1 => "A medium-length answer with a couple of clauses, the kind of \
              text that wraps to two lines on a normal window."
            .to_string(),
        2 => "A longer explanation that runs for several lines once wrapped — \
              the sort of response an assistant produces when it explains a \
              code change, covering motivation, the approach taken, and what \
              to watch out for when adapting it."
            .to_string(),
        3 => "Here's the diff you asked for.".to_string(),
        _ => "ok".to_string(),
    };
    enc.create(body, KIND_TEXT);
    enc.set_text(body, &body_text);
    enc.text_props(body, 14.0, 0xECEC_F0FF);
    enc.place(col, body, NIL);

    // Every fourth message carries a code block.
    if i % 4 == 3 {
        enc.create(code, KIND_TEXT);
        enc.set_text(
            code,
            "fn render(&mut self) {\n    self.scene.items.clear();\n    self.paint(viewport);\n}",
        );
        enc.text_props(code, 12.0, 0xB1E1_8AFF);
        enc.place(col, code, NIL);
    }
}

fn build_transcript_txn() -> Vec<u8> {
    let mut enc = Encoder::new();
    enc.style(
        0,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.flex_direction = FlexDirection::Column;
            s.size = taffy::Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            };
        }),
    );
    enc.style(
        1,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.gap = taffy::Size {
                width: LengthPercentage::length(10.0),
                height: LengthPercentage::length(2.0),
            };
            s.padding = Rect {
                left: LengthPercentage::length(12.0),
                right: LengthPercentage::length(12.0),
                top: LengthPercentage::length(6.0),
                bottom: LengthPercentage::length(6.0),
            };
        }),
    );
    enc.style(
        2,
        &style(|s| {
            s.size = taffy::Size {
                width: Dimension::length(28.0),
                height: Dimension::length(28.0),
            };
            s.flex_shrink = 0.0;
        }),
    );
    enc.style(
        3,
        &style(|s| {
            s.display = taffy::Display::Flex;
            s.flex_direction = FlexDirection::Column;
            s.flex_grow = 1.0;
        }),
    );

    enc.create(0, KIND_VIEW);
    enc.set_style(0, 0);
    enc.view_paint(0, 0x1415_18FF);
    enc.place(NIL, 0, NIL);

    for i in 0..MESSAGES {
        message(&mut enc, 1 + i * 8, i);
    }
    enc.finish(1)
}

/// One streaming transaction per token: the last message's body grows a
/// word at a time, which is how a response actually arrives.
fn stream_txn(text_id: u32, seq: u64, acc: &mut String) -> Vec<u8> {
    const WORDS: &[&str] = &[
        "the", "renderer", "keeps", "one", "ordered", "instance", "stream",
        "so", "paint", "order", "is", "vector", "order", "and", "a", "frame",
        "is", "a", "single", "draw", "call", "regardless", "of", "node",
        "count", "in", "the", "retained", "tree",
    ];
    if !acc.is_empty() {
        acc.push(' ');
    }
    acc.push_str(WORDS[(seq as usize) % WORDS.len()]);
    let mut enc = Encoder::new();
    enc.set_text(text_id, acc);
    enc.finish(seq)
}

fn bench_transcript() {
    eprintln!("\n== Marbre-like transcript ({MESSAGES} messages) ==");
    let mut ui = Ui::new(SCALE);

    let buf = build_transcript_txn();
    eprintln!("  {:>26}: {} bytes on the wire", "txn", buf.len());

    let t = Timer::start();
    ui.apply(&buf).unwrap();
    report(&t.stop("decode + apply"));

    let t = Timer::start();
    ui.layout(VIEW);
    report(&t.stop("layout (cold)"));

    let t = Timer::start();
    ui.paint(VIEW);
    report(&t.stop("paint+emit (cold)"));
    eprintln!(
        "  {:>26}: {} instances, {} rasters, {} cache entries",
        "scene",
        ui.scene().items.len(),
        ui.text.cache.stats.rasters,
        ui.text.cache.len()
    );

    let t = Timer::start();
    ui.paint(VIEW);
    report(&t.stop("paint (warm, unchanged)"));

    // Streaming: one transaction per token into a new trailing message.
    let last_body = 1 + (MESSAGES - 1) * 8 + 4;
    let mut acc = String::new();
    let mut apply_ms = 0.0;
    let mut layout_ms = 0.0;
    let mut paint_ms = 0.0;
    let allocs0 = ALLOCS.load(Ordering::Relaxed);
    let t_all = Instant::now();
    for seq in 2..2 + STREAM_TOKENS as u64 {
        let buf = stream_txn(last_body, seq, &mut acc);
        let t = Instant::now();
        ui.apply(&buf).unwrap();
        apply_ms += t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        ui.layout(VIEW);
        layout_ms += t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        ui.paint(VIEW);
        paint_ms += t.elapsed().as_secs_f64() * 1000.0;
    }
    let total = t_all.elapsed().as_secs_f64() * 1000.0;
    let allocs = ALLOCS.load(Ordering::Relaxed) - allocs0;
    eprintln!("  {:>26}: {STREAM_TOKENS} txns in {total:.2} ms ({allocs} allocs)", "stream totals");
    eprintln!(
        "  {:>26}: apply {apply_ms:.2} ms, layout {layout_ms:.2} ms, paint {paint_ms:.2} ms",
        "stream phase sums"
    );
    eprintln!(
        "  {:>26}: apply {:.3} ms, layout {:.3} ms, paint {:.3} ms",
        "stream per-txn avg",
        apply_ms / STREAM_TOKENS as f64,
        layout_ms / STREAM_TOKENS as f64,
        paint_ms / STREAM_TOKENS as f64,
    );

    eprintln!(
        "  {:>26}: {} nodes; live heap {} KiB",
        "retained state",
        ui.host.len(),
        live()
    );
}

fn main() {
    bench_rows();
    bench_transcript();
}
