//! Frame-cost benchmark — the craie counterpart of gpui-react's
//! `fixtures/performance` ("native frame and heap comparison").
//!
//! Same scene and protocol: an 800x600 document, a 32px status line,
//! `rows` identical 20px text rows (the `flow` retained shape). Mount,
//! first draw, then 10 warmup + 100 measured iterations of a one-op
//! status update plus draw, then the same for scroll steps, then
//! removal plus an empty draw. Live bytes are attributed per phase.
//!
//! Transactions are real React commits, dumped by:
//!
//!   bun examples/js/dump-framebench.tsx <dir> <rows>
//!
//! then:
//!
//!   cargo run --release --example framebench -- <dir> <rows> [--reps 3]
//!
//! Deliberate differences from the gpui-react fixture, matching what
//! Craie can do today:
//! - Only `flow` exists. List virtualization (gpui-react's `list`
//!   scene) is planned work.
//! - `scrollAndDraw` replaces `wheelAndDraw`: Craie has no input events
//!   or native scroller yet (both planned), so a scroll step applies a
//!   React-driven margin change on the content view — the mechanism a
//!   craie app ships today.
//! - `draw` = layout + paint (+ GPU submit unless `--no-gpu`); GPU
//!   *completion* and presentation are excluded, as in the fixture.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use craie::geom::Size;
use craie::gpu::{Gpu, Renderer};
use craie::ui::Ui;

const SCALE: f32 = 2.0;
const VIEW: Size = Size {
    width: 800.0,
    height: 600.0,
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
    BYTES.load(Ordering::Relaxed)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

// ------------------------------------------------------------------ frames

fn load_frame(dir: &str, name: &str, rows: usize) -> Vec<u8> {
    let path = format!("{dir}/{name}-flow-{rows}.bin");
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!("{path}: {e}\nrun: bun examples/js/dump-framebench.tsx {dir} {rows}")
    })
}

/// Reads a framed sequence: u32 count, then count x (u32 len + frame).
fn load_frames(dir: &str, name: &str, rows: usize) -> Vec<Vec<u8>> {
    let buf = load_frame(dir, name, rows);
    let mut at = 0;
    let get = |at: &mut usize| -> u32 {
        let v = u32::from_le_bytes(buf[*at..*at + 4].try_into().unwrap());
        *at += 4;
        v
    };
    let count = get(&mut at) as usize;
    (0..count)
        .map(|_| {
            let len = get(&mut at) as usize;
            let f = buf[at..at + len].to_vec();
            at += len;
            f
        })
        .collect()
}

// ------------------------------------------------------------------- stats

struct Stats {
    median: f64,
    mean: f64,
    min: f64,
}

fn stats(samples: &[f64]) -> Stats {
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    Stats {
        median: s[s.len() / 2],
        mean: s.iter().sum::<f64>() / s.len() as f64,
        min: s[0],
    }
}

// -------------------------------------------------------------------- gpu

struct GpuSide {
    gpu: Gpu,
    renderer: Renderer,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

impl GpuSide {
    fn new() -> GpuSide {
        let gpu = Gpu::headless();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let renderer = Renderer::new(&gpu, format);
        let (w, h) = ((VIEW.width * SCALE) as u32, (VIEW.height * SCALE) as u32);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("framebench"),
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
        GpuSide {
            view: target.create_view(&Default::default()),
            gpu,
            renderer,
            w,
            h,
        }
    }

    /// Scene upload + one draw submit — the GPU work inside a frame.
    fn draw(&mut self, ui: &mut Ui) {
        self.renderer.sync_atlas(&self.gpu, &mut ui.text.atlas);
        self.renderer
            .draw(&self.gpu, &self.view, self.w, self.h, ui.scene());
    }
}

/// One frame: layout + paint (+ GPU submit). Returns (total, layout, paint).
fn draw(ui: &mut Ui, gpu: &mut Option<GpuSide>) -> (f64, f64, f64) {
    let t = Instant::now();
    ui.layout(VIEW);
    let layout_ms = ms(t);
    let t = Instant::now();
    ui.paint(VIEW);
    let paint_ms = ms(t);
    let t = Instant::now();
    if let Some(g) = gpu {
        g.draw(ui);
    }
    (layout_ms + paint_ms + ms(t), layout_ms, paint_ms)
}

// -------------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).map(String::as_str).unwrap_or("/tmp/craie-framebench/wire");
    let rows: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .expect("usage: framebench <dir> <rows> [--ops N] [--warmup N] [--reps N] [--json path] [--no-gpu]");
    let opt = |name: &str, default: usize| -> usize {
        args.windows(2)
            .find(|w| w[0] == name)
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(default)
    };
    let ops = opt("--ops", 110);
    let warmup = opt("--warmup", 10);
    let reps = opt("--reps", 3);
    let gpu_enabled = !args.iter().any(|a| a == "--no-gpu");
    let json_path = args
        .windows(2)
        .find(|w| w[0] == "--json")
        .map(|w| w[1].clone());

    let mount = load_frame(dir, "mount", rows);
    let updates = load_frames(dir, "updates", rows);
    let scrolls = load_frames(dir, "scroll", rows);
    let remove = load_frame(dir, "remove", rows);
    assert_eq!(updates.len(), ops, "updates frame count != --ops");
    assert_eq!(scrolls.len(), ops, "scroll frame count != --ops");

    eprintln!("framebench flow rows={rows} ops={ops} warmup={warmup} reps={reps} gpu={gpu_enabled}");
    let mut json = String::from("[\n");

    for rep in 0..reps {
        let mut ui = Ui::new(SCALE);
        let mut gpu = gpu_enabled.then(GpuSide::new);
        let baseline = live();

        let t = Instant::now();
        ui.apply(&mount).unwrap();
        let mount_ms = ms(t);
        let after_mount = live();

        let (first_draw, ..) = draw(&mut ui, &mut gpu);
        let after_first_draw = live();

        let mut upd_apply = Vec::with_capacity(ops - warmup);
        let mut upd_draw = Vec::with_capacity(ops - warmup);
        for (i, f) in updates.iter().enumerate() {
            let t = Instant::now();
            ui.apply(f).unwrap();
            let apply_ms = ms(t);
            let (d, ..) = draw(&mut ui, &mut gpu);
            if i >= warmup {
                upd_apply.push(apply_ms);
                upd_draw.push(d);
            }
        }
        let after_updates = live();

        let mut scr_apply = Vec::with_capacity(ops - warmup);
        let mut scr_draw = Vec::with_capacity(ops - warmup);
        for (i, f) in scrolls.iter().enumerate() {
            let t = Instant::now();
            ui.apply(f).unwrap();
            let apply_ms = ms(t);
            let (d, ..) = draw(&mut ui, &mut gpu);
            if i >= warmup {
                scr_apply.push(apply_ms);
                scr_draw.push(d);
            }
        }
        let after_scrolls = live();

        let t = Instant::now();
        ui.apply(&remove).unwrap();
        let remove_ms = ms(t);
        let (empty_draw, ..) = draw(&mut ui, &mut gpu);
        let after_removal = live();

        let r = [
            ("mount", mount_ms),
            ("firstDraw", first_draw),
            ("update.apply", stats(&upd_apply).median),
            ("update.draw", stats(&upd_draw).median),
            ("scroll.apply", stats(&scr_apply).median),
            ("scroll.draw", stats(&scr_draw).median),
            ("remove", remove_ms),
            ("emptyDraw", empty_draw),
        ];
        eprintln!("-- rep {rep} (medians over {} ops)", ops - warmup);
        for (name, v) in r {
            eprintln!("  {name:>14}: {v:8.3} ms");
        }
        let kib = |a: usize, b: usize| (a as isize - b as isize) / 1024;
        eprintln!(
            "  {:>14}: mount {:+} KiB, firstDraw {:+} KiB, updates {:+} KiB, scrolls {:+} KiB, removal {:+} KiB",
            "liveKiB",
            kib(after_mount, baseline),
            kib(after_first_draw, after_mount),
            kib(after_updates, after_first_draw),
            kib(after_scrolls, after_updates),
            kib(after_removal, after_scrolls),
        );

        let st = |s: &Stats| format!("{{\"median\":{:.4},\"mean\":{:.4},\"min\":{:.4}}}", s.median, s.mean, s.min);
        json += &format!(
            "  {{\"rows\":{rows},\"rep\":{rep},\"mountMs\":{mount_ms:.4},\"firstDrawMs\":{first_draw:.4},\
             \"updateApplyMs\":{},\"updateDrawMs\":{},\"scrollApplyMs\":{},\"scrollDrawMs\":{},\
             \"removeMs\":{remove_ms:.4},\"emptyDrawMs\":{empty_draw:.4},\
             \"liveBytes\":{{\"baseline\":{baseline},\"afterMount\":{after_mount},\"afterFirstDraw\":{after_first_draw},\
             \"afterUpdates\":{after_updates},\"afterScrolls\":{after_scrolls},\"afterRemoval\":{after_removal}}}}},\n",
            st(&stats(&upd_apply)),
            st(&stats(&upd_draw)),
            st(&stats(&scr_apply)),
            st(&stats(&scr_draw)),
        );
    }

    if let Some(path) = json_path {
        json.truncate(json.trim_end().len() - 1); // drop trailing comma
        json += "\n]\n";
        std::fs::write(&path, &json).unwrap();
        eprintln!("wrote {path}");
    }
}
