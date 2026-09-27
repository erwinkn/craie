//! Allocation invariants (principle 4): steady-state frames and warm
//! patches allocate nothing in Craie code, over the whole frame (UI
//! render, renderer prepare, plan, and encode); wgpu's own allocations
//! have separate, fixed budgets. A counting global allocator measures
//! the calling thread of this test binary only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use craie_core::geom::{Affine, Size};
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::ui::Ui;

thread_local! {
    /// Allocations made on this thread. Per thread, so tests in parallel
    /// and driver threads do not add to a measurement.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    // `try_with`: the slot is gone during thread teardown.
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
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
    let a = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - a
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

#[test]
fn steady_frames_do_not_allocate() {
    let mut ui = ui();
    // Unchanged frame: nothing at all.
    let n = allocs(|| {
        ui.render(VIEW);
    });
    assert_eq!(n, 0, "unchanged frame");

    // Copies are counted: a color-only paragraph change copies its span
    // list (one 16-byte span) and no text; a text change copies the text.
    let span = std::mem::size_of::<craie_ui::mutation::TextSpan>() as u64;
    let before = ui.counters();
    let mut t = Transaction::new(2);
    t.text(3, "row 3 retained text", 14.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    assert_eq!(ui.counters().since(&before).copied_bytes, span);
    let before = ui.counters();
    let mut t = Transaction::new(3);
    t.text(3, "changed", 14.0, 0xFF00_00FF);
    ui.apply_txn(&t).unwrap();
    // A text change also rewrites the span list: 7 text bytes + 1 span.
    assert_eq!(ui.counters().since(&before).copied_bytes, 7 + span);
    ui.render(VIEW);

    // Warm patches allocate nothing: queues and scratch are reused (the
    // transaction itself is built outside the measurement).
    let mut t = Transaction::new(4);
    t.fill(0, 0x2233_44FF);
    let n = allocs(|| {
        ui.apply_txn(&t).unwrap();
        ui.render(VIEW);
    });
    assert_eq!(n, 0, "color patch allocated {n} times");
    let mut t = Transaction::new(5);
    t.transform(3, Affine::translate(4.0, 0.0));
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    for k in 6..9u64 {
        let mut t = Transaction::new(k);
        t.transform(3, Affine::translate(4.0 * k as f32, 0.0));
        let n = allocs(|| {
            ui.apply_txn(&t).unwrap();
            ui.render(VIEW);
        });
        // The first patch through this path sizes its reused buffer once.
        let bound = if k == 6 { 1 } else { 0 };
        assert!(n <= bound, "transform patch {k} allocated {n} times");
    }
}

/// wgpu's own allocations in `Renderer::encode_frame`, measured on Metal
/// (M5 Max, wgpu 30) with 8 draws in one pass: per frame, and per render
/// pass after the first. Craie code in that phase holds no containers. A
/// change here is a visible cost change: re-measure and update with the
/// reason.
const WGPU_FRAME: usize = 56;
const WGPU_PASS: usize = 23;
/// wgpu's staging allocations for one small buffer write in
/// `Renderer::upload`, and the extra tracking cost at the submission that
/// follows it. Craie's share of prepare (`collect`) is measured apart.
const WGPU_WRITE: usize = 8;
const WGPU_WRITE_SUBMIT: usize = 5;

/// Allocations per phase of one frame.
#[derive(Debug, PartialEq)]
struct Frame {
    render: usize,
    collect: usize,
    upload: usize,
    plan: usize,
    encode: usize,
    passes: u32,
    draws: u32,
}

/// A real device, an offscreen target, and a renderer: runs whole frames
/// and counts allocations per phase.
struct GpuFrames {
    gpu: craie_render::Gpu,
    view: wgpu::TextureView,
    renderer: craie_render::Renderer,
    size: (u32, u32),
    /// Held until the device is gone (fields drop in order), so the
    /// tests' devices never overlap: the Vulkan validation layer can
    /// crash when threads create and destroy devices concurrently, and
    /// a shared device's queue could retire the other test's work inside
    /// a measured phase.
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl GpuFrames {
    fn new() -> Option<GpuFrames> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let gpu = craie_render::Gpu::try_headless()?;
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let (w, h) = (800u32, 600u32);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
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
        let renderer = craie_render::Renderer::new(&gpu, format);
        Some(GpuFrames {
            gpu,
            view,
            renderer,
            size: (w, h),
            _serial: serial,
        })
    }

    /// One frame: `change` (a transaction, a scroll) and UI render, then
    /// renderer collect, upload, plan, and encode.
    fn frame(&mut self, ui: &mut Ui, change: impl FnOnce(&mut Ui)) -> Frame {
        let (w, h) = self.size;
        let render = allocs(|| {
            change(ui);
            ui.render(VIEW);
        });
        let r = &mut self.renderer;
        let gpu = &self.gpu;
        let collect = allocs(|| r.collect(ui.scene_mut()));
        let upload = allocs(|| r.upload(gpu, ui.scene_mut()));
        let plan = allocs(|| r.plan_frame(gpu, w, h, ui.scene_mut()));
        let encode = allocs(|| r.encode_frame(gpu, &self.view, ui.scene()));
        // Retire the frame outside the measurement.
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .unwrap();
        Frame {
            render,
            collect,
            upload,
            plan,
            encode,
            passes: r.stats.passes,
            draws: r.stats.draw_calls,
        }
    }
}

/// Plus wgpu-core's command recording: each pass records its commands
/// into a fresh `Vec` that doubles as it fills, so one more reallocation
/// per doubling of the pass's commands. The base above covers 8 draws; a
/// pass has at most the frame's draws, so each pass may double once more
/// per doubling of the frame's draws past 8 (16 or 17 list draws on
/// Metal: 57, one over the base). Logarithmic in the draws, never linear.
fn budget(f: &Frame) -> usize {
    let growth = f.draws.div_ceil(8).next_power_of_two().ilog2() as usize;
    let passes = f.passes as usize;
    WGPU_FRAME + WGPU_PASS * (passes - 1) + growth * passes
}

/// The whole frame on a real device: UI render, renderer prepare
/// (collect + upload), and draw (plan + encode). Separate budgets per
/// phase: Craie phases (render, collect, plan) allocate nothing once
/// warm, changed or not; wgpu phases (upload, encode) cost a fixed
/// amount per write and per pass, the same every frame.
#[test]
fn whole_frame_budgets() {
    let Some(mut g) = GpuFrames::new() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut ui = ui();
    let mut frame = |ui: &mut Ui, t: Option<&Transaction>| {
        g.frame(ui, |ui| {
            if let Some(t) = t {
                ui.apply_txn(t).unwrap();
            }
        })
    };

    // Plain and with an opacity group (a layer: 4 passes).
    for opacity in [1.0, 0.5] {
        let mut t = Transaction::new(2);
        t.opacity(3, opacity);
        frame(&mut ui, Some(&t));
        frame(&mut ui, None);
        let first = frame(&mut ui, None);
        assert_eq!(
            (first.render, first.collect, first.upload, first.plan),
            (0, 0, 0, 0),
            "opacity {opacity}: {first:?}"
        );
        assert!(
            first.encode <= budget(&first),
            "opacity {opacity}: {first:?}"
        );
        for _ in 0..8 {
            assert_eq!(frame(&mut ui, None), first, "opacity {opacity}: steady");
        }
    }

    // Color patches, after two to warm the queues, alternating with idle
    // frames: the Craie phases allocate nothing either way; wgpu stages
    // the one paint write and tracks it at submission.
    for k in 0..6u32 {
        let mut t = Transaction::new(3 + k as u64);
        t.fill(0, 0x2233_44FF + (k << 8));
        let f = frame(&mut ui, Some(&t));
        let idle = frame(&mut ui, None);
        if k < 2 {
            continue;
        }
        assert_eq!((f.render, f.collect, f.plan), (0, 0, 0), "{f:?}");
        assert!(f.upload <= WGPU_WRITE, "{f:?}");
        assert!(f.encode <= budget(&f) + WGPU_WRITE_SUBMIT, "{f:?}");
        assert_eq!(
            (idle.render, idle.collect, idle.upload, idle.plan),
            (0, 0, 0, 0),
            "{idle:?}"
        );
    }
}

/// A window-sized scroller with a 10,000-item list, its first range
/// rendered.
fn list_ui() -> Ui {
    use craie_harness::ListDriver;
    use craie_ui::mutation::ItemDesc;
    let mut ui = Ui::new(2.0);
    let mut d = ListDriver::new(1, 100, 14.0);
    let mut s = craie_ui::host::default_style().to_taffy();
    s.size = taffy::Size {
        width: taffy::Dimension::percent(1.0),
        height: taffy::Dimension::percent(1.0),
    };
    s.overflow.y = taffy::Overflow::Scroll;
    let mut t = Transaction::new(1);
    t.create(0, NodeKind::View).layout(0, &s).append(NIL, 0);
    let items: Vec<ItemDesc> = (0..10_000)
        .map(|i| ItemDesc {
            template: 0,
            text_len: list_text(i).len() as u32,
            id: i,
            unchanged: false,
        })
        .collect();
    t.create(1, NodeKind::List)
        .list_config(1, 300.0, 20.0, &[d.template()])
        .list_splice(1, 0, 0, &items)
        .append(0, 1);
    ui.apply_txn(&t).unwrap();
    d.settle(&mut ui, VIEW, &list_text, 8);
    ui
}

fn list_text(i: u32) -> String {
    format!("item {i} with some words in it")
}

/// The scroll positions the list tests measure: all inside the range
/// rendered at mount (overscan 300).
fn list_scrolls() -> impl Iterator<Item = f32> {
    (1..8).map(|k| 10.0 * k as f32)
}

/// A virtualized list, UI phase (no GPU needed, so it always runs):
/// after one warm pass over the same scroll positions, unchanged frames
/// and scrolls inside the rendered range allocate nothing. The warm pass
/// builds the row chunks that come near the viewport once.
#[test]
fn list_ui_frames_do_not_allocate() {
    use craie_ui::host::NodeId;
    let mut ui = list_ui();
    for y in list_scrolls() {
        ui.scroll_to(NodeId(0), 0.0, y);
        ui.render(VIEW);
        ui.render(VIEW);
    }
    ui.scroll_to(NodeId(0), 0.0, 0.0);
    ui.render(VIEW);
    let n = allocs(|| {
        ui.render(VIEW);
    });
    assert_eq!(n, 0, "unchanged frame");
    for y in list_scrolls() {
        let n = allocs(|| {
            ui.scroll_to(NodeId(0), 0.0, y);
            ui.render(VIEW);
        });
        assert_eq!(n, 0, "scroll to {y}");
        let n = allocs(|| {
            ui.render(VIEW);
        });
        assert_eq!(n, 0, "idle after scroll to {y}");
    }
    assert!(
        !ui.take_events()
            .iter()
            .any(|e| e.kind == craie_ui::events::out_kind::LIST_RANGE),
        "the measured frames stayed inside the rendered range"
    );
}

/// The same list over the whole frame on a real device: warm scrolls
/// inside the rendered range, idle frames, and the settle frame that
/// snaps the content at rest. Craie phases allocate nothing; wgpu's
/// upload and encode stay within their budgets. Frames that change the
/// range (rows mount) and the mount itself are excluded: they create
/// nodes. Skips without a GPU adapter (the UI phase is covered above).
#[test]
fn list_frames_do_not_allocate() {
    use craie_ui::host::NodeId;
    let Some(mut g) = GpuFrames::new() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let mut ui = list_ui();
    // Warm: the first frames upload everything and grow buffers; one pass
    // over the scroll positions builds the row chunks that come near the
    // viewport (a first build uploads glyphs: content, not scroll cost).
    g.frame(&mut ui, |_| {});
    for y in list_scrolls() {
        g.frame(&mut ui, |ui| {
            ui.scroll_to(NodeId(0), 0.0, y);
        });
        g.frame(&mut ui, |_| {});
    }
    g.frame(&mut ui, |ui| {
        ui.scroll_to(NodeId(0), 0.0, 0.0);
    });
    for y in list_scrolls() {
        let f = g.frame(&mut ui, |ui| {
            ui.scroll_to(NodeId(0), 0.0, y);
        });
        assert_eq!(
            (f.render, f.collect, f.plan),
            (0, 0, 0),
            "scroll to {y}: {f:?}"
        );
        assert!(f.upload <= WGPU_WRITE, "scroll to {y}: {f:?}");
        assert!(
            f.encode <= budget(&f) + WGPU_WRITE_SUBMIT,
            "scroll to {y}: {f:?}"
        );
        let idle = g.frame(&mut ui, |_| {});
        assert_eq!(
            (idle.render, idle.collect, idle.upload, idle.plan),
            (0, 0, 0, 0),
            "idle after scroll to {y}: {idle:?}"
        );
        assert!(idle.encode <= budget(&idle), "{idle:?}");
    }
    assert!(
        !ui.take_events()
            .iter()
            .any(|e| e.kind == craie_ui::events::out_kind::LIST_RANGE),
        "the measured frames stayed inside the rendered range"
    );
    // The settle frame: the content snaps at rest (one world row).
    ui.set_time(1.0);
    assert!(ui.settle());
    let f = g.frame(&mut ui, |_| {});
    assert_eq!((f.render, f.collect, f.plan), (0, 0, 0), "settle: {f:?}");
    assert!(f.upload <= WGPU_WRITE, "settle: {f:?}");
    assert!(f.encode <= budget(&f) + WGPU_WRITE_SUBMIT, "settle: {f:?}");
}
