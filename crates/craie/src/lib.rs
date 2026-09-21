//! Craie — experimental native desktop renderer/runtime for React, in Rust.
//!
//! React owns composition, state and reconciliation. Craie owns only what
//! React does not: retained host state, layout, text, painting, and
//! presentation.
//!
//! The hot path is deliberately data-oriented: compact IDs, arenas, side
//! tables, and bit flags instead of per-node heap objects.

pub mod a11y;
pub mod app;
pub mod bridge;
pub mod clipboard;
pub mod custom;
pub mod events;
pub mod geom;
pub mod gpu;
pub mod host;
pub mod input;
pub mod layout;
pub mod platform;
pub mod scene;
pub mod text;
pub mod ui;
pub mod wire;

pub use geom::{Point, Rect, Size};
