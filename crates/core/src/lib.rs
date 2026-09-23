//! Craie core: the vocabulary every other crate shares. Geometry,
//! handles, revisions, dirty queues, the span pool, and cost counters.
//!
//! No knowledge of React, winit, wgpu, fonts, or Taffy.

pub mod geom;

pub use geom::{Point, Rect, RectPx, Size};
