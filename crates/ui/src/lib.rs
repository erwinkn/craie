//! Craie UI runtime: the retained host tree, the mutation executor,
//! layout, text measurement, interaction, and the accessibility
//! projection. Platform-free: the platform adapter feeds normalized
//! events in and presents the scene out.
//!
//! React owns composition, state, and reconciliation. Craie owns only
//! what React does not: retained host state, layout, text, painting, and
//! presentation.

pub mod a11y;
pub mod bridge;
pub mod clipboard;
mod dispatch;
pub mod events;
mod executor;
pub mod host;
pub mod input;
pub mod layout;
pub mod mutation;
pub mod platform;
mod scene_sync;
pub mod surface;
#[cfg(test)]
mod tests;
pub mod ui;
pub mod wire;

pub use executor::validate;

pub use craie_core::geom;
pub use craie_core::{Point, Rect, Size};
pub use craie_scene as scene;
pub use craie_text as text;
