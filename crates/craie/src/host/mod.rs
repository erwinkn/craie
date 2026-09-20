//! Compact retained host tree.
//!
//! React is authoritative for composition; this module stores only the
//! retained representation needed for native execution. Ordinary nodes are
//! a fixed-size header in a contiguous arena — no per-node heap objects.
//!
//! Sparse capabilities (text content, scroll state, input state, native
//! components) live in side tables addressed by `aux` indices or by `NodeId`
//! keyed maps that only exist for nodes that need them.

mod node;
mod style;

pub use node::{Host, NodeFlags, NodeHeader, NodeId, NodeKind, ROOT, Siblings, TextRow, ViewRow};
pub use style::StyleId;
