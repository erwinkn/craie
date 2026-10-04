//! Retained drawing: chunks of specialized primitives, transform, clip,
//! and paint tables, raster residency, and draw-order derivation.
//!
//! No knowledge of widgets, layout, fonts, or GPUs: the UI layer writes
//! chunks and records; the renderer reads the pools and tables and the
//! derived draw list.

pub mod atlas;
pub mod chunk;
pub mod prim;
pub mod scene;
pub mod space;

pub use atlas::{ATLAS_PAGE_SIZE, AtlasStats, RasterAtlas, RasterGpu, RasterId, Residency};
pub use chunk::{BoxShadow, Chunk, ChunkWriter, GradientPaint, MAX_SEGMENTS, PaintSlot, Placement};
pub use prim::{
    Color, GlyphInstance, NO_PAINT, PathVertex, RectInstance, SegKind, Segment, gradient,
};
pub use scene::{DrawCmd, DrawList, OrderItem, Resolved, Scene};
pub use space::{ClipGpu, ClipRecord, Clips, NONE, TransformRecord, Transforms, WorldGpu};
