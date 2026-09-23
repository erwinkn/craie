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

/// wgpu's own allocations in `Renderer::encode_frame` on this machine
/// (Metal, wgpu 27): per frame, and per render pass after the first.
/// Craie code in that phase holds no containers. A change here is a
/// visible cost change: re-measure and update with the reason.
const WGPU_FRAME: usize = 56;
const WGPU_PASS: usize = 23;
/// wgpu's staging allocations for one small buffer write in
/// `Renderer::upload`, and the extra tracking cost at the submission that
/// follows it. Craie's share of prepare (`collect`) is measured apart.
const WGPU_WRITE: usize = 8;
const WGPU_WRITE_SUBMIT: usize = 5;

/// The whole frame on a real device: UI render, renderer prepare
/// (collect + upload), and draw (plan + encode). Separate budgets per
/// phase: Craie phases (render, collect, plan) allocate nothing once
/// warm, changed or not; wgpu phases (upload, encode) cost a fixed
/// amount per write and per pass, the same every frame.
#[test]
fn whole_frame_budgets() {
    let Some(gpu) = craie_render::Gpu::try_headless() else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
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
    let mut renderer = craie_render::Renderer::new(&gpu, format);
    let mut ui = ui();
    #[derive(Debug, PartialEq)]
    struct Frame {
        render: usize,
        collect: usize,
        upload: usize,
        plan: usize,
        encode: usize,
        passes: u32,
    }
    let mut frame = |ui: &mut Ui, t: Option<&Transaction>| {
        let render = allocs(|| {
            if let Some(t) = t {
                ui.apply_txn(t).unwrap();
            }
            ui.render(VIEW);
        });
        let collect = allocs(|| renderer.collect(ui.scene_mut()));
        let upload = allocs(|| renderer.upload(&gpu, ui.scene_mut()));
        let plan = allocs(|| renderer.plan_frame(&gpu, w, h, ui.scene_mut()));
        let encode = allocs(|| renderer.encode_frame(&gpu, &view, ui.scene()));
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
            passes: renderer.stats.passes,
        }
    };
    let budget = |passes: u32| WGPU_FRAME + WGPU_PASS * (passes as usize - 1);

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
            first.encode <= budget(first.passes),
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
        assert!(f.encode <= budget(f.passes) + WGPU_WRITE_SUBMIT, "{f:?}");
        assert_eq!(
            (idle.render, idle.collect, idle.upload, idle.plan),
            (0, 0, 0, 0),
            "{idle:?}"
        );
    }
}
