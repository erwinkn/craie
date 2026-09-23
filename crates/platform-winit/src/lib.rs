//! Desktop platform adapter: winit window and event loop, input
//! normalization, system clipboard, IME, and the AccessKit adapter.
//!
//! Platform boundary.
//!
//! Everything above this crate (host, text, scene, render) consumes
//! Craie-level events only; winit is confined here. `Window` implements
//! `craie_ui::platform::PlatformWindow`, the contract the runtime sees.

pub mod a11y;
pub mod app;
pub mod capture;
pub mod clipboard;
mod winit;

use std::sync::{Arc, Mutex};

use craie_core::{Rect, Size};
use craie_ui::a11y::A11yShared;
use craie_ui::events::Event;
use craie_ui::platform::{PlatformWindow, WindowId};

/// Cross-thread wake handle for the event loop. Clone it freely; `wake`
/// pokes a `Wait`-state loop so the main thread can drain work queues
/// (e.g. transactions submitted to the bridge `Session`).
#[derive(Clone)]
pub struct Wake {
    pub(crate) proxy: ::winit::event_loop::EventLoopProxy<()>,
}

impl Wake {
    pub fn wake(&self) {
        // Fails only once the loop has exited; nothing is left to wake.
        let _ = self.proxy.send_event(());
    }
}

/// A platform window. Wraps the winit window but exposes only what Craie
/// needs; the GPU layer receives the raw handle through `surface_target`
/// without naming winit itself.
pub struct Window {
    inner: Arc<::winit::window::Window>,
    id: WindowId,
    /// AccessKit adapter, present when the app exposes `a11y_shared`.
    /// Mutex because both the driver (`process_event`) and the app
    /// (`update_a11y`) touch it through `&Window`.
    a11y: Mutex<Option<accesskit_winit::Adapter>>,
}

impl Window {
    pub(crate) fn new(
        inner: ::winit::window::Window,
        a11y: Option<accesskit_winit::Adapter>,
    ) -> Window {
        Window {
            id: WindowId(u64::from(inner.id())),
            inner: Arc::new(inner),
            a11y: Mutex::new(a11y),
        }
    }

    /// Forwards a window event to the AccessKit adapter (no-op without one).
    pub(crate) fn process_a11y_event(&self, event: &::winit::event::WindowEvent) {
        if let Some(a) = self.a11y.lock().unwrap().as_mut() {
            a.process_event(&self.inner, event);
        }
    }

    /// Pushes a tree update to assistive technology, if attached.
    pub fn update_a11y(&self, update: accesskit::TreeUpdate) {
        if let Some(a) = self.a11y.lock().unwrap().as_mut() {
            a.update_if_active(|| update);
        }
    }

    /// Handle for `Gpu::for_window`. `Arc<winit Window>` satisfies wgpu's
    /// `WindowHandle` bound without exposing the type generically.
    pub fn surface_target(&self) -> Arc<::winit::window::Window> {
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
        let s = self.inner.inner_size();
        (s.width, s.height)
    }

    /// Surface size in logical points.
    pub fn logical_size(&self) -> Size {
        PlatformWindow::logical_size(self)
    }

    /// Enables or disables the platform input method. While enabled the
    /// window delivers `Ime` events instead of plain key presses for
    /// composing text. `caret_area` (logical points, window-relative)
    /// anchors the candidate window near the text being composed.
    pub fn set_ime(&self, active: bool, caret_area: Option<Rect>) {
        self.inner.set_ime_allowed(active);
        if !active {
            return;
        }
        if let Some(r) = caret_area {
            self.set_ime_area(r);
        }
    }

    /// Points the IME candidate window at the text being composed
    /// (logical points; the request is issued in physical pixels).
    pub fn set_ime_area(&self, rect: Rect) {
        let scale = self.scale_factor();
        self.inner.set_ime_cursor_area(
            ::winit::dpi::PhysicalPosition::new(
                (rect.origin.x as f64 * scale) as i32,
                (rect.origin.y as f64 * scale) as i32,
            ),
            ::winit::dpi::PhysicalSize::new(
                (rect.size.width as f64 * scale) as u32,
                (rect.size.height as f64 * scale).max(1.0) as u32,
            ),
        );
    }
}

impl PlatformWindow for Window {
    fn id(&self) -> WindowId {
        self.id
    }

    fn surface_size(&self) -> (u32, u32) {
        self.size()
    }

    fn scale_factor(&self) -> f64 {
        self.inner.scale_factor()
    }

    fn request_frame(&self) {
        self.inner.request_redraw();
    }

    fn set_text_input(&self, active: bool, caret: Option<Rect>) {
        self.set_ime(active, caret);
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

    /// A `Wake::wake` fired — typically a `Session` submit. Drain your
    /// queues here. Return true to exit the event loop.
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

    /// A normalized input event. Positions are logical points. The
    /// implementation consumes what it handles natively (editing,
    /// scrolling) and forwards what it exposes to the JS side.
    fn event(&mut self, _window: &Window, _event: &Event) {}

    /// Return false to veto closing.
    fn close_requested(&mut self) -> bool {
        true
    }

    /// Shared accessibility state. `Some` opts the app into the platform
    /// AccessKit adapter (created before the window is shown).
    fn a11y_shared(&self) -> Option<Arc<A11yShared>> {
        None
    }
}

/// Opens one window and runs the event loop. Returns when the loop
/// exits (window closed, or `App::woke`/`close_requested` requested it).
pub fn run(title: &str, logical_size: Size, app: impl App) {
    winit::run(title, logical_size, app)
}
