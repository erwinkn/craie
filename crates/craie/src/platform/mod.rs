//! Platform boundary.
//!
//! Everything above this module (host, text, scene, gpu) consumes
//! Craie-level events only; winit is confined to `platform::winit`. The
//! trait surface is intentionally minimal — a window that can report its
//! size/scale and ask for a redraw.

mod winit;

use std::sync::Arc;

use crate::geom::Size;

/// Cross-thread wake handle for the event loop. Clone it freely; `wake`
/// pokes a `Wait`-state loop so the main thread can drain work queues
/// (e.g. wire transactions arriving over the bridge socket).
#[derive(Clone)]
pub struct Wake {
    pub(crate) proxy: ::winit::event_loop::EventLoopProxy,
}

impl Wake {
    pub fn wake(&self) {
        self.proxy.wake_up();
    }
}

/// A platform window. Wraps the winit window but exposes only what Craie
/// needs; the GPU layer receives the raw handle through `surface_target`
/// without naming winit itself.
pub struct Window {
    inner: Arc<dyn ::winit::window::Window>,
}

impl Window {
    pub(crate) fn new(inner: Box<dyn ::winit::window::Window>) -> Window {
        Window {
            inner: Arc::from(inner),
        }
    }

    /// Handle for `Gpu::for_window`. `Arc<dyn winit Window>` satisfies
    /// wgpu's `WindowHandle` bound without exposing the type generically.
    pub fn surface_target(&self) -> Arc<dyn ::winit::window::Window> {
        self.inner.clone()
    }

    pub fn request_redraw(&self) {
        self.inner.request_redraw();
    }

    /// Notify the windowing system that a frame is about to be presented.
    /// Call after recording GPU work, before `Queue::present`. On Wayland
    /// this schedules the frame callback that throttles `RedrawRequested`;
    /// elsewhere it is a no-op.
    pub fn pre_present_notify(&self) {
        self.inner.pre_present_notify();
    }

    pub fn scale_factor(&self) -> f64 {
        self.inner.scale_factor()
    }

    /// Surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        let s = self.inner.surface_size();
        (s.width, s.height)
    }

    /// Surface size in logical points.
    pub fn logical_size(&self) -> Size {
        let (w, h) = self.size();
        let scale = self.scale_factor() as f32;
        Size::new(w as f32 / scale, h as f32 / scale)
    }
}

/// Application callbacks driven by the platform event loop.
///
/// Rendering happens only in `redraw`, in response to an explicit
/// `request_redraw` or an OS expose event — an unchanged app does nothing.
pub trait App: 'static {
    /// The window exists and a surface can be created. `wake` is how
    /// background threads (bridge listener, timers) interrupt `Wait`.
    fn ready(&mut self, window: &Window, wake: &Wake);

    /// A background thread called `Wake::wake`. Drain your queues here.
    /// Return true to exit the event loop.
    fn woke(&mut self, _window: &Window) -> bool {
        false
    }

    /// Physical size or scale factor changed.
    fn resized(&mut self, window: &Window);

    /// The window became fully hidden (`true`) or visible again
    /// (`false`) per macOS occlusion state. While occluded, surface
    /// acquisition fails and presents are wasted; on becoming visible a
    /// repaint should be requested.
    fn occluded(&mut self, _window: &Window, _occluded: bool) {}

    /// The OS or the app requested a frame.
    fn redraw(&mut self, window: &Window);

    /// Return false to veto closing.
    fn close_requested(&mut self) -> bool {
        true
    }
}

/// Opens one window and runs the event loop. Returns when the loop
/// exits (window closed, or `App::woke`/`close_requested` requested it).
pub fn run(title: &str, logical_size: Size, app: impl App) {
    winit::run(title, logical_size, app)
}
