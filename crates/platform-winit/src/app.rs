//! The standard host application: one `Session`, one `Ui`, one window.
//!
//! This is the shared event-loop behavior for every Craie host — the
//! N-API runtime (`craie-node`) and the windowed examples drive exactly
//! this. Owns the GPU objects and the retained `Ui`; commits arrive
//! through the session and become at most one repaint per wake.

use std::sync::Arc;

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
}

struct Inner {
    gpu: Gpu,
    surface: WindowSurface,
    renderer: Renderer,
    ui: Ui,
}

impl HostApp {
    pub fn new(session: Arc<Session>) -> HostApp {
        HostApp {
            session,
            surfaces: Vec::new(),
            a11y: A11yShared::new(),
            inner: None,
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
        if prepare_frame(&mut self.ui, &mut self.renderer, &self.gpu, (w, h), scale) {
            window.request_redraw();
        }
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

    /// Pushes queued UI events to the JS side, and keeps the platform
    /// IME pointed at the focused input's caret. Associated fn so the
    /// caller can pass `&mut inner.ui` while `inner` is borrowed.
    fn flush_out(ui: &mut Ui, window: &Window, session: &Session) {
        let events = ui.take_events();
        if !events.is_empty() {
            session.post_events(events::encode_events(&events));
        }
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
        };
        inner.sync(window, &self.session, true);
        Inner::publish_a11y(&mut inner.ui, window, &self.a11y);
        self.inner = Some(inner);
    }

    fn woke(&mut self, window: &Window) -> bool {
        if self.session.is_closed() {
            return true;
        }
        if let Some(inner) = &mut self.inner {
            inner.sync(window, &self.session, true);
            // Assistive-tech action requests arrive through the shared
            // queue; drain them on the UI thread like input events.
            let actions: Vec<accesskit::ActionRequest> =
                std::mem::take(&mut *self.a11y.actions.lock().unwrap());
            for req in &actions {
                inner.ui.a11y_action(req);
            }
            Inner::flush_out(&mut inner.ui, window, &self.session);
            Inner::publish_a11y(&mut inner.ui, window, &self.a11y);
            if inner.ui.needs_paint() {
                window.request_redraw();
            }
        }
        false
    }

    fn resized(&mut self, window: &Window) {
        let Some(inner) = &mut self.inner else { return };
        let (w, h) = window.size();
        inner.surface.resize(&inner.gpu, w, h);
        // Size change invalidates wrap widths: every cache goes.
        inner.ui.invalidate_layout();
        inner.sync(window, &self.session, false);
    }

    fn occluded(&mut self, window: &Window, occluded: bool) {
        // Becoming visible again needs a repaint: acquires while
        // occluded were skipped, so the last presented frame is stale.
        if !occluded {
            window.request_redraw();
        }
    }

    fn event(&mut self, window: &Window, event: &Event) {
        let Some(inner) = &mut self.inner else { return };
        inner.ui.dispatch(event);
        Inner::flush_out(&mut inner.ui, window, &self.session);
        Inner::publish_a11y(&mut inner.ui, window, &self.a11y);
        if inner.ui.needs_paint() {
            window.request_redraw();
        }
    }

    fn a11y_shared(&self) -> Option<Arc<A11yShared>> {
        Some(self.a11y.clone())
    }

    fn redraw(&mut self, window: &Window) {
        let Some(inner) = &mut self.inner else { return };
        let (w, h) = window.size();
        if w == 0 || h == 0 {
            return; // minimized / zero-sized surface
        }
        // Native interaction (wheel, editing, focus, assistive actions)
        // changes state without a commit: prepare it here, on the one
        // frame path, before drawing.
        let scale = window.scale_factor() as f32;
        prepare_frame(
            &mut inner.ui,
            &mut inner.renderer,
            &inner.gpu,
            (w, h),
            scale,
        );
        let frame = match inner.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                inner.surface.resize(&inner.gpu, w, h);
                window.request_redraw();
                return;
            }
            // Occluded/Timeout: skip; the Occluded(false) event repaints
            // once the window is visible again.
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        inner
            .renderer
            .draw(&inner.gpu, &view, w, h, inner.ui.scene_mut());
        window.pre_present_notify();
        inner.gpu.queue.present(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
