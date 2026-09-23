//! Cost counters. Work that the architecture promises not to repeat is
//! counted here so tests can assert on it (principle 4: costs are
//! visible). Counters only grow; callers diff two snapshots.

/// Monotonic work counters, one per costed operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Layout passes that ran the layout engine (roots computed).
    pub layout_passes: u64,
    /// Nodes whose layout was computed rather than served from cache.
    pub layout_nodes: u64,
    /// Paragraphs shaped and broken into lines.
    pub shapes: u64,
    /// Glyph rasterizations.
    pub rasters: u64,
    /// Scene chunks whose primitives were regenerated.
    pub chunks_built: u64,
    /// Chunk paint records patched in place.
    pub paints_patched: u64,
    /// Local transform records written.
    pub transforms_written: u64,
    /// Draw-order rebuilds.
    pub draw_orders: u64,
    /// Bytes handed to the GPU for upload (instances, tables, atlas,
    /// uniforms).
    pub upload_bytes: u64,
    /// Bytes copied from transactions into retained host state (text,
    /// span lists, labels, payloads).
    pub copied_bytes: u64,
}

impl Counters {
    /// Per-field difference `self - earlier`.
    pub fn since(&self, earlier: &Counters) -> Counters {
        Counters {
            layout_passes: self.layout_passes - earlier.layout_passes,
            layout_nodes: self.layout_nodes - earlier.layout_nodes,
            shapes: self.shapes - earlier.shapes,
            rasters: self.rasters - earlier.rasters,
            chunks_built: self.chunks_built - earlier.chunks_built,
            paints_patched: self.paints_patched - earlier.paints_patched,
            transforms_written: self.transforms_written - earlier.transforms_written,
            draw_orders: self.draw_orders - earlier.draw_orders,
            upload_bytes: self.upload_bytes - earlier.upload_bytes,
            copied_bytes: self.copied_bytes - earlier.copied_bytes,
        }
    }
}
