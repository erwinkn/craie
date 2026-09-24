//! The standard host application: one `Session`, one `Ui`, one window.
//!
//! This is the shared event-loop behavior for every Craie host — the
//! N-API runtime (`craie-node`) and the windowed examples drive exactly
//! this. Owns the GPU objects and the retained `Ui`; commits arrive
//! through the session and become at most one repaint per wake.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use craie_core::Size;
use craie_render::{Gpu, Renderer, WindowSurface};
use craie_ui::a11y::A11yShared;
use craie_ui::bridge::Session;
use craie_ui::events::{self, Event};
use craie_ui::surface::SurfacePainter;
use craie_ui::ui::Ui;

use crate::clipboard::SystemClipboard;
use crate::{App, Wake, Window};

/// A `platform::App` that renders a `Session`-fed `Ui` into one window.
pub struct HostApp {
    session: Arc<Session>,
    /// Surface painters registered before `ready`, moved into the `Ui`
    /// when it exists.
    surfaces: Vec<(u32, SurfacePainter)>,
    /// Accessibility handoff shared with the platform adapter.
    a11y: Arc<A11yShared>,
    inner: Option<Inner>,
    /// `CRAIE_CAPTURE`: write one settled frame to this PNG, then exit.
    capture: Option<(PathBuf, Duration)>,
    capture_due: Option<Instant>,
    /// Zero of the UI clock (`Ui::set_time`).
    start: Instant,
}

struct Inner {
    gpu: Gpu,
    surface: WindowSurface,
    renderer: Renderer,
    ui: Ui,
    stats: FrameStats,
}

/// Frame costs over a window of about half a second, for
/// `out_kind::FRAME_STATS`. CPU time is all preparation the frame shows
/// (layout, scene, upload; also when it ran after a commit, before the
/// redraw) plus encoding and submitting the draw; the wait for a
/// drawable is not in it. A window counts one burst of frames: a gap of
/// more than `GAP` after a frame that left nothing owed starts a new
/// one, so sparse frames (a HUD that paints its last report, an idle
/// timeout) never make a report of their own, while slow frames of
/// continuous work still do.
pub(crate) struct FrameStats {
    since: Instant,
    last: Option<Instant>,
    frames: u32,
    cpu_sum: f64,
    cpu_max: f64,
    prepare_sum: f64,
    /// Preparation done since the last frame, outside one (ms).
    pending_ms: f64,
    /// The last frame left work owed (animations, a paint): a gap after
    /// it is slow rendering, not idle time.
    busy: bool,
}

impl FrameStats {
    const WINDOW: Duration = Duration::from_millis(500);
    const GAP: Duration = Duration::from_millis(250);

    pub(crate) fn new() -> FrameStats {
        FrameStats {
            since: Instant::now(),
            last: None,
            frames: 0,
            cpu_sum: 0.0,
            cpu_max: 0.0,
            prepare_sum: 0.0,
            pending_ms: 0.0,
            busy: false,
        }
    }

    /// Preparation that ran before the frame that will show it.
    pub(crate) fn prepared(&mut self, ms: f64) {
        self.pending_ms += ms;
    }

    /// Records one frame; at the end of a window returns its event.
    pub(crate) fn frame(
        &mut self,
        cpu_ms: f64,
        prepare_ms: f64,
        nodes: usize,
        tweens: usize,
        busy: bool,
    ) -> Option<events::UiEvent> {
        self.frame_at(Instant::now(), cpu_ms, prepare_ms, nodes, tweens, busy)
    }

    fn frame_at(
        &mut self,
        now: Instant,
        cpu_ms: f64,
        prepare_ms: f64,
        nodes: usize,
        tweens: usize,
        busy: bool,
    ) -> Option<events::UiEvent> {
        let cpu_ms = cpu_ms + self.pending_ms;
        let prepare_ms = prepare_ms + self.pending_ms;
        self.pending_ms = 0.0;
        // A gap after an idle frame starts a new burst; after a busy one
        // it is a slow frame of the same burst.
        let idle_gap = !self.busy && self.last.is_some_and(|l| now - l > Self::GAP);
        self.busy = busy;
        if self.frames == 0 || idle_gap {
            self.since = now;
            self.frames = 0;
            self.cpu_sum = 0.0;
            self.cpu_max = 0.0;
            self.prepare_sum = 0.0;
        }
        self.last = Some(now);
        self.frames += 1;
        self.cpu_sum += cpu_ms;
        self.cpu_max = self.cpu_max.max(cpu_ms);
        self.prepare_sum += prepare_ms;
        let elapsed = now - self.since;
        if elapsed < Self::WINDOW {
            return None;
        }
        let n = self.frames as f64;
        let mut e = events::UiEvent::new(events::out_kind::FRAME_STATS, u32::MAX);
        // Intervals over time: the window opens at its first frame.
        e.x = ((n - 1.0) / elapsed.as_secs_f64()) as f32;
        e.y = (self.cpu_sum / n) as f32;
        e.a = self.cpu_max as f32;
        e.b = (self.prepare_sum / n) as f32;
        e.key = nodes as u32;
        e.revision = tweens as u32;
        self.frames = 0;
        Some(e)
    }
}

impl HostApp {
    pub fn new(session: Arc<Session>) -> HostApp {
        // Text engines created from here on use the system's fonts.
        crate::fonts::install();
        HostApp {
            session,
            surfaces: Vec::new(),
            a11y: A11yShared::new(),
            inner: None,
            capture: std::env::var_os("CRAIE_CAPTURE").map(|p| {
                let ms = std::env::var("CRAIE_CAPTURE_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1500);
                (PathBuf::from(p), Duration::from_millis(ms))
            }),
            capture_due: None,
            start: Instant::now(),
        }
    }

    /// Sets the UI clock to now; called at the top of every callback.
    fn tick(&mut self) {
        let t = self.start.elapsed().as_secs_f64();
        if let Some(inner) = &mut self.inner {
            inner.ui.set_time(t);
        }
    }

    /// The shared accessibility state (latest tree + pending actions).
    pub fn a11y(&self) -> &Arc<A11yShared> {
        &self.a11y
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Registers a native surface painter for `kind`. Safe before or
    /// after `ready`.
    pub fn register_surface(&mut self, kind: u32, painter: SurfacePainter) {
        match &mut self.inner {
            Some(inner) => inner.ui.register_surface(kind, painter),
            None => self.surfaces.push((kind, painter)),
        }
    }

    /// Borrows the retained UI (for boot content applied before `run`).
    pub fn ui_mut(&mut self) -> Option<&mut Ui> {
        self.inner.as_mut().map(|i| &mut i.ui)
    }
}

/// The one frame path: when the UI owes a paint, lay out, update the
/// scene, and upload what changed (an unchanged frame uploads nothing).
/// `surface` is in physical pixels. Returns whether anything was
/// prepared.
pub fn prepare_frame(
    ui: &mut Ui,
    renderer: &mut Renderer,
    gpu: &Gpu,
    surface: (u32, u32),
    scale: f32,
) -> bool {
    let (w, h) = surface;
    if !ui.needs_paint() || w == 0 || h == 0 {
        return false; // idle, or minimized (resize repaints)
    }
    ui.scale = scale;
    // Layout and the scene viewport are logical; the surface is physical.
    ui.render(Size::new(w as f32 / scale, h as f32 / scale));
    renderer.prepare(gpu, ui.scene_mut());
    true
}

impl Inner {
    /// Drains the session into the retained UI, then lays out + repaints
    /// once if anything can change pixels.
    fn sync(&mut self, window: &Window, session: &Session, drain: bool) {
        if drain {
            for buf in session.take_commits() {
                match self.ui.apply(&buf) {
                    Ok(seq) => session.ack(seq),
                    Err(e) => {
                        eprintln!("[craie] undecodable transaction: {e:?}");
                        session.close("undecodable transaction");
                    }
                }
            }
        }
        let (w, h) = window.size();
        let scale = window.scale_factor() as f32;
        let t = Instant::now();
        if prepare_frame(&mut self.ui, &mut self.renderer, &self.gpu, (w, h), scale) {
            window.request_redraw();
        }
        self.stats.prepared(t.elapsed().as_secs_f64() * 1e3);
        // A frame can raise events (list ranges, anchoring scrolls).
        Inner::flush_out(&mut self.ui, session);
    }

    /// Publishes a fresh semantic tree when a11y-observable state
    /// changed — independent of paint (labels and focus don't dirty).
    fn publish_a11y(ui: &mut Ui, window: &Window, shared: &A11yShared) {
        if !ui.take_a11y_stale() {
            return;
        }
        let tree = ui.a11y_tree(window.logical_size());
        *shared.latest.lock().unwrap() = Some(tree.clone());
        window.update_a11y(tree);
    }

    /// Pushes queued UI events to the JS side. Associated fn so the
    /// caller can pass `&mut inner.ui` while `inner` is borrowed.
    fn flush_out(ui: &mut Ui, session: &Session) {
        let events = ui.take_events();
        if !events.is_empty() {
            // Animation ends resolve JS promises: those frames never drop.
            let reliable = events
                .iter()
                .any(|e| e.kind == events::out_kind::ANIMATION_END);
            session.post_events(events::encode_events(&events), reliable);
        }
    }

    /// Publishes geometry-dependent platform state (accessibility bounds,
    /// the IME caret area) only from a prepared frame: while the UI owes
    /// a paint, layout is stale, so this requests the frame instead and
    /// `redraw` publishes after `prepare_frame`.
    fn publish_frame_state(ui: &mut Ui, window: &Window, shared: &A11yShared) {
        if ui.needs_paint() {
            window.request_redraw();
            return;
        }
        Inner::publish_a11y(ui, window, shared);
        window.set_ime(ui.ime_wanted(), ui.ime_area());
    }
}

impl App for HostApp {
    fn ready(&mut self, window: &Window, wake: &Wake) {
        let (w, h) = window.size();
        let (gpu, surface) = Gpu::for_window(window.surface_target());
        let surface = WindowSurface::new(&gpu, surface, w, h);
        let renderer = Renderer::new(&gpu, surface.config.format);
        let mut ui = Ui::new(window.scale_factor() as f32);
        ui.set_time(self.start.elapsed().as_secs_f64());
        for (kind, painter) in self.surfaces.drain(..) {
            ui.register_surface(kind, painter);
        }
        ui.inputs.clipboard = Box::new(SystemClipboard);
        let poke = wake.clone();
        self.session.install_wake(Arc::new(move || poke.wake()));
        let mut inner = Inner {
            gpu,
            surface,
            renderer,
            ui,
            stats: FrameStats::new(),
        };
        inner.sync(window, &self.session, true);
        Inner::publish_frame_state(&mut inner.ui, window, &self.a11y);
        self.inner = Some(inner);
        if let Some((_, delay)) = &self.capture {
            let delay = *delay;
            self.capture_due = Some(Instant::now() + delay);
            let wake = wake.clone();
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                wake.wake();
            });
        }
    }

    fn woke(&mut self, window: &Window) -> bool {
        if self.session.is_closed() {
            return true;
        }
        self.tick();
        if let Some(inner) = &mut self.inner {
            inner.sync(window, &self.session, true);
            // Assistive-tech action requests arrive through the shared
            // queue; drain them on the UI thread like input events.
            let actions: Vec<accesskit::ActionRequest> =
                std::mem::take(&mut *self.a11y.actions.lock().unwrap());
            for req in &actions {
                inner.ui.a11y_action(req);
            }
            Inner::flush_out(&mut inner.ui, &self.session);
            Inner::publish_frame_state(&mut inner.ui, window, &self.a11y);
            if let (Some((path, _)), Some(due)) = (&self.capture, self.capture_due)
                && Instant::now() >= due
            {
                let format = inner.surface.config.format;
                let size = window.size();
                match crate::capture::capture_png(
                    &inner.gpu,
                    &mut inner.renderer,
                    format,
                    &mut inner.ui,
                    size,
                    path,
                ) {
                    Ok(()) => eprintln!("[craie] captured {}", path.display()),
                    Err(e) => eprintln!("[craie] capture failed: {e}"),
                }
                return true;
            }
        }
        false
    }

    fn resized(&mut self, window: &Window) {
        self.tick();
        let Some(inner) = &mut self.inner else { return };
        let (w, h) = window.size();
        inner.surface.resize(&inner.gpu, w, h);
        // Size change invalidates wrap widths: every cache goes.
        inner.ui.invalidate_layout();
        inner.sync(window, &self.session, false);
        Inner::publish_frame_state(&mut inner.ui, window, &self.a11y);
    }

    fn occluded(&mut self, window: &Window, occluded: bool) {
        // Becoming visible again needs a repaint: acquires while
        // occluded were skipped, so the last presented frame is stale.
        if !occluded {
            window.request_redraw();
        }
    }

    fn event(&mut self, window: &Window, event: &Event) {
        self.tick();
        let Some(inner) = &mut self.inner else { return };
        inner.ui.dispatch(event);
        Inner::flush_out(&mut inner.ui, &self.session);
        Inner::publish_frame_state(&mut inner.ui, window, &self.a11y);
    }

    fn a11y_shared(&self) -> Option<Arc<A11yShared>> {
        Some(self.a11y.clone())
    }

    fn next_timer(&self) -> Option<Instant> {
        let at = self.inner.as_ref()?.ui.next_settle()?;
        Some(self.start + Duration::from_secs_f64(at.max(0.0)))
    }

    /// A moving space may have come to rest: snap it and repaint.
    fn timer(&mut self, window: &Window) {
        self.tick();
        let Some(inner) = &mut self.inner else { return };
        if inner.ui.settle() {
            window.request_redraw();
        }
    }

    fn redraw(&mut self, window: &Window) {
        self.tick();
        let Some(inner) = &mut self.inner else { return };
        let (w, h) = window.size();
        if w == 0 || h == 0 {
            return; // minimized / zero-sized surface
        }
        // Native interaction (wheel, editing, focus, assistive actions)
        // changes state without a commit: prepare it here, on the one
        // frame path, before drawing.
        let scale = window.scale_factor() as f32;
        let t = Instant::now();
        prepare_frame(
            &mut inner.ui,
            &mut inner.renderer,
            &inner.gpu,
            (w, h),
            scale,
        );
        // Recorded now: a failed acquisition below must not lose it; the
        // next presented frame shows this work.
        inner.stats.prepared(t.elapsed().as_secs_f64() * 1e3);
        // Layout is current now: bounds and the caret area are too; the
        // frame's events (list ranges, anchoring scrolls) go out.
        Inner::flush_out(&mut inner.ui, &self.session);
        Inner::publish_frame_state(&mut inner.ui, window, &self.a11y);
        let frame = match inner.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                inner.surface.resize(&inner.gpu, w, h);
                window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                window.request_redraw();
                return;
            }
            // Occluded: no frame, and none requested (running animations
            // would spin the loop with nothing drawn); the Occluded(false)
            // event repaints once the window is visible again.
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let t = Instant::now();
        inner
            .renderer
            .draw(&inner.gpu, &view, w, h, inner.ui.scene_mut());
        let cpu_ms = t.elapsed().as_secs_f64() * 1e3;
        window.pre_present_notify();
        inner.gpu.queue.present(frame);
        // Running animations advance every frame: ask for the next one
        // (presentation paces it). Idle requests none.
        if inner.ui.animating() {
            window.request_redraw();
        }
        let tweens = inner.ui.animation_count();
        let busy = inner.ui.animating() || inner.ui.needs_paint();
        if let Some(e) = inner
            .stats
            .frame(cpu_ms, 0.0, inner.ui.host.len(), tweens, busy)
        {
            self.session.post_events(events::encode_events(&[e]), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPD-03: frames far apart (a HUD painting a report, then its idle
    /// timeout) never report; a burst does, with its rate, and counts
    /// preparation that ran before its frames (SPD-02).
    #[test]
    fn frame_stats_report_bursts_only() {
        let t0 = Instant::now();
        let ms = |v: u64| t0 + Duration::from_millis(v);
        let mut stats = FrameStats::new();
        for k in 0..8 {
            assert!(
                stats
                    .frame_at(ms(k * 1200), 1.0, 1.0, 10, 0, false)
                    .is_none(),
                "sparse frame {k}"
            );
        }
        let start = 8 * 1200 + 5000;
        let mut report = None;
        for k in 0..=60 {
            if k == 30 {
                stats.prepared(3.0);
            }
            if let Some(e) = stats.frame_at(ms(start + k * 10), 1.0, 0.5, 10, 7, true) {
                report = Some(e);
                break;
            }
        }
        let e = report.expect("a burst reports");
        assert!((e.x - 100.0).abs() < 1.0, "fps {}", e.x);
        assert_eq!(
            e.a, 4.0,
            "the frame after a commit's preparation carries it"
        );
        assert_eq!(e.revision, 7);
    }

    /// SPD-06: continuous slow frames (300 ms each, work still owed)
    /// report; they are not idle gaps.
    #[test]
    fn slow_busy_frames_still_report() {
        let t0 = Instant::now();
        let mut stats = FrameStats::new();
        let report = (0..6).find_map(|k| {
            stats.frame_at(
                t0 + Duration::from_millis(k * 300),
                300.0,
                290.0,
                10,
                3,
                true,
            )
        });
        let e = report.expect("slow frames report");
        assert!((e.x - 3.33).abs() < 0.1, "fps {}", e.x);
        assert_eq!(e.y, 300.0);
    }
    use craie_ui::events::Event;
    use craie_ui::host::NodeId;
    use craie_ui::mutation::{NIL, NodeKind, Transaction};

    /// Native interaction with no JS commit reaches the frame: a wheel
    /// scroll makes the UI owe a paint, and the frame path prepares it
    /// (one world matrix uploaded) before drawing.
    #[test]
    fn native_input_reaches_the_frame_path() {
        let Some(gpu) = Gpu::try_headless() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let mut renderer = Renderer::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut ui = Ui::new(1.0);
        let mut s = taffy::Style::default();
        s.size.width = taffy::Dimension::length(200.0);
        s.size.height = taffy::Dimension::length(100.0);
        s.overflow = taffy::Point {
            x: taffy::Overflow::Scroll,
            y: taffy::Overflow::Scroll,
        };
        let mut row = taffy::Style::default();
        row.size.width = taffy::Dimension::length(200.0);
        row.size.height = taffy::Dimension::length(300.0);
        row.flex_shrink = 0.0;
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View).layout(0, &s).append(NIL, 0);
        t.create(1, NodeKind::View)
            .layout(1, &row)
            .fill(1, 0x3344_55FF)
            .append(0, 1);
        ui.apply_txn(&t).unwrap();
        assert!(prepare_frame(&mut ui, &mut renderer, &gpu, (200, 200), 1.0));
        assert!(
            !prepare_frame(&mut ui, &mut renderer, &gpu, (200, 200), 1.0),
            "idle"
        );
        ui.dispatch(&Event::Wheel {
            x: 10.0,
            y: 10.0,
            dx: 0.0,
            dy: 40.0,
        });
        assert!(ui.needs_paint());
        assert!(prepare_frame(&mut ui, &mut renderer, &gpu, (200, 200), 1.0));
        assert_eq!(ui.scroll_offset(NodeId(0)), [0.0, 40.0]);
        assert_eq!(
            renderer.stats.upload_bytes,
            size_of::<craie_scene::WorldGpu>() as u64
        );
        // The settle signal: the content is due to rest SETTLE_SECS after
        // the wheel; the host timer settles it and the frame path uploads
        // the one world row whose snap flag changed.
        let due = ui.next_settle().expect("scrolled content is moving");
        ui.set_time(due);
        assert!(ui.settle());
        assert!(prepare_frame(&mut ui, &mut renderer, &gpu, (200, 200), 1.0));
        assert_eq!(
            renderer.stats.upload_bytes,
            size_of::<craie_scene::WorldGpu>() as u64
        );
        assert_eq!(ui.next_settle(), None);
        assert!(
            !prepare_frame(&mut ui, &mut renderer, &gpu, (200, 200), 1.0),
            "at rest: idle"
        );
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use craie_ui::events::{Button, Event, Key, KeyInput, Mods};
    use craie_ui::host::NodeId;
    use craie_ui::mutation::{NIL, NodeKind, Transaction};

    /// Typing a newline into an auto-height multiline input moves the
    /// node below it with no JS commit. The UI owes a paint until the
    /// frame is prepared (so `publish_frame_state` publishes nothing
    /// geometric early), and the tree built after `prepare_frame` carries
    /// the new bounds.
    #[test]
    fn native_reflow_publishes_after_the_frame() {
        let Some(gpu) = Gpu::try_headless() else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let mut renderer = Renderer::new(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut ui = Ui::new(1.0);
        // The input measures text: the pinned fonts, whatever the global
        // default source is (another test may install the system's).
        ui.text = craie_text::TextEngine::with_source(Box::new(craie_text::fonts::pinned()));
        let mut input = taffy::Style::default();
        input.size.width = taffy::Dimension::length(200.0);
        let mut button = taffy::Style::default();
        button.size = taffy::Size {
            width: taffy::Dimension::length(80.0),
            height: taffy::Dimension::length(30.0),
        };
        let mut t = Transaction::new(1);
        t.create(0, NodeKind::View).append(NIL, 0);
        t.create(1, NodeKind::Input)
            .layout(1, &input)
            .input_config(1, 16.0, 0xFFFF_FFFF, "", true)
            .append(0, 1);
        t.create(2, NodeKind::View)
            .layout(2, &button)
            .role(2, craie_ui::mutation::Role::Button)
            .append(0, 2);
        ui.apply_txn(&t).unwrap();
        prepare_frame(&mut ui, &mut renderer, &gpu, (400, 400), 1.0);
        let y0 = ui.abs_rect(NodeId(2)).origin.y;
        ui.dispatch(&Event::PointerDown {
            x: 5.0,
            y: 5.0,
            button: Button::Primary,
            mods: Mods::default(),
        });
        prepare_frame(&mut ui, &mut renderer, &gpu, (400, 400), 1.0);
        ui.dispatch(&Event::KeyDown(KeyInput {
            key: Key::Enter,
            text: None,
            char: None,
            mods: Mods::default(),
        }));
        assert!(ui.needs_paint(), "geometry is stale until the frame");
        prepare_frame(&mut ui, &mut renderer, &gpu, (400, 400), 1.0);
        assert!(!ui.needs_paint());
        let y1 = ui.abs_rect(NodeId(2)).origin.y;
        assert!(y1 > y0, "the newline must grow the input: {y0} -> {y1}");
        let tree = ui.a11y_tree(craie_core::Size::new(400.0, 400.0));
        let b = tree
            .nodes
            .iter()
            .find(|(n, _)| *n == craie_ui::a11y::aid(NodeId(2)))
            .unwrap()
            .1
            .bounds()
            .unwrap();
        assert_eq!(b.y0 as f32, y1);
    }
}
