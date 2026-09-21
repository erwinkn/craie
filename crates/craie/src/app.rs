//! The standard host application: one `Session`, one `Ui`, one window.
//!
//! This is the shared event-loop behavior for every Craie host — the
//! N-API runtime (`craie-node`) and the windowed examples drive exactly
//! this. Owns the GPU objects and the retained `Ui`; commits arrive
//! through the session and become at most one repaint per wake.

use std::sync::Arc;

use crate::a11y::A11yShared;
use crate::bridge::Session;
use crate::events::{self, Event};
use crate::geom::Size;
use crate::gpu::{Gpu, Renderer, WindowSurface};
use crate::platform::{App, Wake, Window};
use crate::ui::Ui;

/// A `platform::App` that renders a `Session`-fed `Ui` into one window.
pub struct HostApp {
    session: Arc<Session>,
    /// Custom painters registered before `ready` — moved into the `Ui`
    /// when it exists.
    painters: Vec<(u32, crate::custom::Painter)>,
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
            painters: Vec::new(),
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

    /// Registers a custom-element painter. Safe before or after `ready`.
    pub fn register_painter(&mut self, tag: u32, painter: crate::custom::Painter) {
        match &mut self.inner {
            Some(inner) => inner.ui.register_painter(tag, painter),
            None => self.painters.push((tag, painter)),
        }
    }

    /// Borrows the retained UI (for boot content applied before `run`).
    pub fn ui_mut(&mut self) -> Option<&mut Ui> {
        self.inner.as_mut().map(|i| &mut i.ui)
    }
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
        if !self.ui.needs_paint() {
            return;
        }
        let (w, h) = window.size();
        if w == 0 || h == 0 {
            return; // minimized; the resize/occluded path repaints
        }
        let scale = window.scale_factor() as f32;
        self.ui.scale = scale;
        // Layout and the scene viewport are logical; the surface is physical.
        self.ui
            .render(Size::new(w as f32 / scale, h as f32 / scale));
        self.renderer.sync_atlas(&self.gpu, &mut self.ui.text.atlas);
        window.request_redraw();
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
        for (tag, painter) in self.painters.drain(..) {
            ui.register_painter(tag, painter);
        }
        self.session.install_wake(wake.clone());
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
            .draw(&inner.gpu, &view, w, h, inner.ui.scene());
        window.pre_present_notify();
        inner.gpu.queue.present(frame);
    }
}
