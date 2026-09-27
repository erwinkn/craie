//! Craie UI runtime: the retained host tree, the mutation executor,
//! layout, text measurement, interaction, and the accessibility
//! projection. Platform-free: the platform adapter feeds normalized
//! events in and presents the scene out.
//!
//! React owns composition, state, and reconciliation. Craie owns only
//! what React does not: retained host state, layout, text, painting, and
//! presentation.

pub mod a11y;
#[cfg(test)]
mod a11y_tests;
pub mod animation;
#[cfg(test)]
mod animation_tests;
pub mod bridge;
pub mod claims;
#[cfg(test)]
mod claims_tests;
pub mod clipboard;
mod dispatch;
pub mod events;
mod executor;
mod group;
#[cfg(test)]
mod group_tests;
pub mod host;
pub mod image;
pub mod input;
pub mod layout;
pub mod list;
pub mod mutation;
mod order;
#[cfg(test)]
mod order_tests;
#[cfg(test)]
mod parts_tests;
pub mod platform;
mod press;
#[cfg(test)]
mod press_tests;
mod reach;
mod scene_sync;
pub mod selection;
pub mod states;
#[cfg(test)]
mod states_tests;
pub mod surface;
#[cfg(test)]
mod tests;
mod trap;
#[cfg(test)]
mod trap_tests;
pub mod ui;
pub mod vector;
pub mod wire;

pub use executor::validate;

pub use craie_core::geom;
pub use craie_core::{Point, Rect, Size};
pub use craie_scene as scene;
pub use craie_text as text;
