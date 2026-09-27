//! The platform contract: what the UI runtime needs from a host
//! platform. `craie-platform-winit` is the desktop implementation; the
//! harness drives a headless one.
//!
//! The runtime never owns the event loop, the window, the device, or the
//! render target. The contract carries a window id from the start so
//! events and roots can be keyed per window, although this milestone
//! runs one window. Services not listed here (timers, assets, font
//! discovery, image decoding) join the contract with the features that
//! need them. The clipboard seam is `crate::clipboard::Clipboard`; the
//! cross-thread wake is `crate::bridge::WakeFn`.

use craie_core::{Rect, Size};

/// Platform-assigned window identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

/// One platform window as the runtime sees it.
pub trait PlatformWindow {
    fn id(&self) -> WindowId;

    /// Drawable surface size in physical pixels.
    fn surface_size(&self) -> (u32, u32);

    /// Physical pixels per logical point.
    fn scale_factor(&self) -> f64;

    /// Asks for one frame. The platform calls back when it is time to
    /// draw; an app that never asks never draws.
    fn request_frame(&self);

    /// Enables or disables text input (IME). `caret` (logical points,
    /// window-relative) anchors the candidate window.
    fn set_text_input(&self, active: bool, caret: Option<Rect>);

    /// Drawable surface size in logical points.
    fn logical_size(&self) -> Size {
        let (w, h) = self.surface_size();
        let scale = self.scale_factor() as f32;
        Size::new(w as f32 / scale, h as f32 / scale)
    }
}
