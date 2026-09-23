//! Craie core: the vocabulary every other crate shares. Geometry,
//! revisions, dirty queues, the span pool, cost counters, and a
//! deterministic PRNG for model tests.
//!
//! No knowledge of React, winit, wgpu, fonts, or Taffy.

pub mod counters;
pub mod dirty;
pub mod geom;
pub mod rev;
pub mod rng;
pub mod span;

pub use geom::{Affine, Point, Rect, RectPx, Size};
