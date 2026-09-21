//! The standard host application: one `Session`, one `Ui`, one window.
//!
//! This is the shared event-loop behavior for every Craie host — the
//! N-API runtime (`craie-node`) and the windowed examples drive exactly
//! this. Owns the GPU objects and the retained `Ui`; commits arrive
//! through the session and become at most one repaint per wake.

use std::sync::Arc;

use crate::bridge::Session;
use crate::geom::Size;
use crate::gpu::{Gpu, Renderer, WindowSurface};
use crate::platform::{App, Wake, Window};
use crate::ui::Ui;

/// A `platform::App` that renders a `Session`-fed `Ui` into one window.
pub struct HostApp {
    session: Arc<Session>,
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
            inner: None,
        }
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
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
        self.ui.scale = window.scale_factor() as f32;
        self.ui.render(Size::new(w as f32, h as f32));
        self.renderer.sync_atlas(&self.gpu, &mut self.ui.text.atlas);
        window.request_redraw();
    }
}

impl App for HostApp {
    fn ready(&mut self, window: &Window, wake: &Wake) {
        let (w, h) = window.size();
        let (gpu, surface) = Gpu::for_window(window.surface_target());
        let surface = WindowSurface::new(&gpu, surface, w, h);
        let renderer = Renderer::new(&gpu, surface.config.format);
        let ui = Ui::new(window.scale_factor() as f32);
        self.session.install_wake(wake.clone());
        let mut inner = Inner {
            gpu,
            surface,
            renderer,
            ui,
        };
        inner.sync(window, &self.session, true);
        self.inner = Some(inner);
    }

    fn woke(&mut self, window: &Window) -> bool {
        if self.session.is_closed() {
            return true;
        }
        if let Some(inner) = &mut self.inner {
            inner.sync(window, &self.session, true);
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
