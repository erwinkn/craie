//! Allocation invariants (principle 4): steady-state frames and warm
//! patches allocate nothing in Craie code, over the whole frame (UI
//! render, renderer prepare, plan, and encode); wgpu's own allocations
//! have separate, fixed budgets. A counting global allocator measures
//! the calling thread of this test binary only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use craie_core::geom::{Affine, Size};
use std::f32::consts::TAU;
use std::sync::Arc;

use craie_ui::animation::{Prop, Timing, Value};
use craie_ui::keyframes::{Animation, Easing, Frame as KeyFrame, Keyframes, Sample, Trigger};
use craie_ui::mutation::{NIL, NodeKind, Transaction};
use craie_ui::states::value_field;
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

    // Transform parts: patches, a running rotate tween's frames and hit
    // tests through the turned node allocate nothing (each composes once
    // per change, in place).
    for k in 9..12u64 {
        let mut t = Transaction::new(k);
        t.rotate(3, 0.1 * k as f32)
            .scale(3, 1.5, 1.0)
            .translate(3, [0.0, 0.0, 0.5, 0.0]);
        let n = allocs(|| {
            ui.apply_txn(&t).unwrap();
            ui.render(VIEW);
        });
        assert_eq!(n, 0, "parts patch {k} allocated {n} times");
    }
    let mut t = Transaction::new(12);
    t.animate(
        3,
        Prop::Rotate,
        Value::Rotate(6.0),
        Timing::curve(1.0, [0.0, 0.0, 1.0, 1.0]),
    );
    ui.apply_txn(&t).unwrap();
    ui.render(VIEW);
    let n = allocs(|| {
        for f in 1..10 {
            ui.set_time(f as f64 * 0.05);
            ui.render(VIEW);
            for x in 0..10 {
                ui.hit_test(x as f32 * 20.0, 60.0);
            }
        }
    });
    assert_eq!(n, 0, "rotate tween frames allocated {n} times");

    // Keyframe loops: a spin and a pulse on two nodes, sampled each frame
    // on the native clock, allocate nothing past their start, iteration
    // boundaries included.
    let spin = Arc::new(Keyframes::new(vec![
        frame(0.0, value_field::ROTATE, |s| s.rotate = 0.0),
        frame(1.0, value_field::ROTATE, |s| s.rotate = TAU),
    ]));
    // Opacity 1 at the ends: the loop holds the node's layer through it.
    let pulse = Arc::new(Keyframes::new(vec![frame(
        0.5,
        value_field::OPACITY | value_field::SCALE_X,
        |s| {
            s.opacity = 0.4;
            s.scale[0] = 1.1;
        },
    )]));
    let forever = |k: &Arc<Keyframes>, secs: f32| Animation {
        iterations: f32::INFINITY,
        ..Animation::new(k.clone(), secs, Easing::LINEAR)
    };
    let mut t = Transaction::new(13);
    t.animation(4, Trigger::Base, false, &[forever(&spin, 1.0)])
        .animation(
            5,
            Trigger::Base,
            false,
            &[forever(&spin, 2.0), forever(&pulse, 0.5)],
        );
    // They start at 0.5: every quarter second after is exact in binary.
    ui.set_time(0.5);
    ui.apply_txn(&t).unwrap();
    // One cycle sizes the reused buffers (the pulse's opacity layer comes
    // and goes); the rotate tween above ends in it.
    for f in 0..=40 {
        ui.set_time(0.5 + f as f64 * 0.05);
        ui.render(VIEW);
    }
    ui.take_events();
    // Every other frame lands on a pulse boundary (opacity 1), every
    // fourth on a spin's (the identity transform), every eighth on both
    // of node 5's: their pins keep the layer and the transform records.
    let n = allocs(|| {
        for f in 1..40 {
            ui.set_time(2.5 + f as f64 * 0.25);
            ui.render(VIEW);
            ui.hit_test(20.0, 60.0);
        }
    });
    assert_eq!(n, 0, "keyframe loop frames allocated {n} times");
    assert_eq!(ui.motion().live(), 3);
}

fn frame(at: f32, mask: u16, f: impl FnOnce(&mut Sample)) -> KeyFrame {
    let mut values = Sample::default();
    f(&mut values);
    KeyFrame {
        at,
        easing: None,
        mask,
        values,
    }
}

/// wgpu's own allocations in `Renderer::encode_frame` (wgpu 30), besides
/// the command lists (`recorded` below): per frame with one pass that
/// draws, and per opacity layer (its own pass, the composite, and the
/// parent's pass resumed after it). They differ by backend:
///
/// | backend | frame | first layer | each further (1 to 5 layers) |
/// |---|---|---|---|
/// | Metal (M5 Max) | 53 | 67 | 62 to 70 |
/// | Vulkan (llvmpipe, exe1) | 49 | 50 | 47 to 54 |
///
/// Layers vary as wgpu's resource trackers grow; the budget takes the
/// most. Craie code in that phase holds no containers. A change here is a
/// visible cost change: re-measure and update with the reason.
#[cfg(target_vendor = "apple")]
const WGPU_FRAME: usize = 53;
#[cfg(target_vendor = "apple")]
const WGPU_LAYER: usize = 70;
#[cfg(not(target_vendor = "apple"))]
const WGPU_FRAME: usize = 49;
#[cfg(not(target_vendor = "apple"))]
const WGPU_LAYER: usize = 54;
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
    layers: u32,
    /// wgpu-core's allocations for the frame's command lists.
    recorded: usize,
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
        let (passes, draws) = pass_commands(&ui.scene().draw_list().cmds);
        assert_eq!(
            (passes.len() as u32, draws),
            (r.stats.passes, r.stats.draw_calls),
            "the passes Renderer::encode records"
        );
        Frame {
            render,
            collect,
            upload,
            plan,
            encode,
            passes: r.stats.passes,
            layers: r.stats.layers,
            recorded: passes.iter().map(|&c| command_list_allocs(c)).sum(),
        }
    }
}

/// The commands each render pass of `Renderer::encode` records, and the
/// draws: a pass binds its pipeline and two bind groups before its first
/// draw and switches pipelines between rects and glyphs and paths; a
/// layer draws in passes of its own, then composites (pipeline, bind
/// group, draw), and its parent resumes in a new pass.
fn pass_commands(cmds: &[craie_scene::DrawCmd]) -> (Vec<u32>, u32) {
    use craie_scene::DrawCmd;
    fn walk(cmds: &[DrawCmd], i: &mut usize, passes: &mut Vec<u32>, draws: &mut u32) {
        loop {
            let p = passes.len();
            passes.push(0);
            let mut bound = 0;
            loop {
                let Some(&c) = cmds.get(*i) else { return };
                *i += 1;
                let want = match c {
                    DrawCmd::EndLayer => return,
                    DrawCmd::BeginLayer { .. } => {
                        walk(cmds, i, passes, draws);
                        passes.push(3);
                        *draws += 1;
                        break;
                    }
                    DrawCmd::Rects { .. } | DrawCmd::Glyphs { .. } => 1,
                    DrawCmd::Paths { .. } => 2,
                };
                if bound != want {
                    passes[p] += if bound == 0 { 3 } else { 1 };
                    bound = want;
                }
                passes[p] += 1;
                *draws += 1;
            }
        }
    }
    let (mut passes, mut draws) = (Vec::new(), 0);
    walk(cmds, &mut 0, &mut passes, &mut draws);
    (passes, draws)
}

/// wgpu-core records each pass's commands into a fresh `Vec` (capacity
/// 4, then doubling): one allocation per capacity it reaches. Measured on
/// Metal, one pass of 2 to 62 draws (5 to 65 commands) costs exactly
/// `WGPU_FRAME` plus this; on Vulkan, so do the tests' frames (8, 16 and
/// 17 draws, and 1 to 5 layers). The frame's encode grows with the log of
/// each pass's draws, never linearly.
fn command_list_allocs(commands: u32) -> usize {
    match commands {
        0 => 0,
        c => c.div_ceil(4).next_power_of_two().ilog2() as usize + 1,
    }
}

fn budget(f: &Frame) -> usize {
    WGPU_FRAME + WGPU_LAYER * f.layers as usize + f.recorded
}

/// The whole frame on a real device: UI render, renderer prepare
/// (collect + upload), and draw (plan + encode). Separate budgets per
/// phase: Craie phases (render, collect, plan) allocate nothing once
/// warm, changed or not; wgpu phases (upload, encode) cost a fixed
/// amount per write and per layer, plus the log of each pass's commands,
/// the same every frame.
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
