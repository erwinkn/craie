//! Platform boundary.
//!
//! Everything above this module (host, text, scene, gpu) consumes
//! Craie-level events only; winit is confined to `platform::winit`. The
//! trait surface is intentionally minimal — a window that can report its
//! size/scale, ask for a redraw, and receive input-method requests.

mod winit;

use std::sync::{Arc, Mutex};

use crate::a11y::A11yShared;
use crate::events::Event;
use crate::geom::{Rect, Size};

/// Cross-thread wake handle for the event loop. Clone it freely; `wake`
/// pokes a `Wait`-state loop so the main thread can drain work queues
/// (e.g. transactions submitted to the bridge `Session`).
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
    /// AccessKit adapter, present when the app exposes `a11y_shared`.
    /// Mutex because both the driver (`process_event`) and the app
    /// (`update_a11y`) touch it through `&Window`.
    a11y: Mutex<Option<accesskit_winit::Adapter>>,
}

impl Window {
    pub(crate) fn new(
        inner: Box<dyn ::winit::window::Window>,
        a11y: Option<accesskit_winit::Adapter>,
    ) -> Window {
        Window {
            inner: Arc::from(inner),
            a11y: Mutex::new(a11y),
        }
    }

    /// Forwards a window event to the AccessKit adapter (no-op without one).
    pub(crate) fn process_a11y_event(&self, event: &::winit::event::WindowEvent) {
        if let Some(a) = self.a11y.lock().unwrap().as_mut() {
            a.process_event(&*self.inner, event);
        }
    }

    /// Pushes a tree update to assistive technology, if attached.
    pub fn update_a11y(&self, update: accesskit::TreeUpdate) {
        if let Some(a) = self.a11y.lock().unwrap().as_mut() {
            a.update_if_active(|| update);
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

    /// Enables or disables the platform input method. While enabled the
    /// window delivers `Ime` events instead of plain key presses for
    /// composing text. `caret_area` (logical points, window-relative)
    /// anchors the candidate window near the text being composed.
    pub fn set_ime(&self, active: bool, caret_area: Option<Rect>) {
        use ::winit::window::{
            ImeCapabilities, ImeEnableRequest, ImeHint, ImePurpose, ImeRequest, ImeRequestData,
        };
        let request = if active {
            let scale = self.scale_factor();
            let data = caret_area.map(|r| {
                ImeRequestData::default()
                    .with_hint_and_purpose(ImeHint::NONE, ImePurpose::Normal)
                    .with_cursor_area(
                        ::winit::dpi::Position::Physical(
                            ::winit::dpi::PhysicalPosition::new(
                                (r.origin.x as f64 * scale) as i32,
                                (r.origin.y as f64 * scale) as i32,
                            ),
                        ),
                        ::winit::dpi::Size::Physical(::winit::dpi::PhysicalSize::new(
                            (r.size.width as f64 * scale) as u32,
                            (r.size.height as f64 * scale).max(1.0) as u32,
                        )),
                    )
            });
            let Some(data) = data else {
                // Enable without an area only after the first caret
                // update arrives; nothing sensible to send yet.
                return;
            };
            let caps = ImeCapabilities::new().with_hint_and_purpose().with_cursor_area();
            let Some(req) = ImeEnableRequest::new(caps, data) else {
                return;
            };
            ImeRequest::Enable(req)
        } else {
            ImeRequest::Disable
        };
        let _ = self.inner.request_ime_update(request);
    }

    /// Points the IME candidate window at the text being composed
    /// (logical points; the request is issued in physical pixels).
    pub fn set_ime_area(&self, rect: Rect) {
        let scale = self.scale_factor();
        let data = ::winit::window::ImeRequestData::default().with_cursor_area(
            ::winit::dpi::Position::Physical(::winit::dpi::PhysicalPosition::new(
                (rect.origin.x as f64 * scale) as i32,
                (rect.origin.y as f64 * scale) as i32,
            )),
            ::winit::dpi::Size::Physical(::winit::dpi::PhysicalSize::new(
                (rect.size.width as f64 * scale) as u32,
                (rect.size.height as f64 * scale).max(1.0) as u32,
            )),
        );
        let _ = self
            .inner
            .request_ime_update(::winit::window::ImeRequest::Update(data));
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
