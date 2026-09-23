//! Chunks: the retained unit of drawing.
//!
//! A chunk is one owner's prepared primitives (a view box, a paragraph,
//! an input, a surface): ranges in the rect, glyph, and paint pools plus
//! up to `MAX_SEGMENTS` same-kind runs in paint order. Its placement —
//! offset in a transform record's space, the record, the clip — lives in
//! a separate table the GPU reads, so moving, scrolling, or re-clipping
//! a chunk never rewrites its primitives.

use bytemuck::{Pod, Zeroable};
use craie_core::geom::Rect;
use craie_core::span::Span;

use crate::atlas::RasterId;
use crate::prim::{GlyphInstance, NO_PAINT, RectInstance, SegKind, Segment};
use crate::space::NONE;

pub const MAX_SEGMENTS: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
pub struct Chunk {
    pub rects: Span,
    pub glyphs: Span,
    pub paints: Span,
    pub segments: [Option<Segment>; MAX_SEGMENTS],
    /// Local bounds of every primitive (logical units).
    pub bounds: Rect,
    pub live: bool,
}

impl Chunk {
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        self.segments.iter().map_while(|s| *s)
    }
}

/// Chunk placement as the GPU reads it: 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Placement {
    /// Chunk origin in its transform record's space (logical units).
    pub offset: [f32; 2],
    pub transform: u32,
    /// Clip record, `NONE` when unclipped.
    pub clip: u32,
}

impl Default for Placement {
    fn default() -> Placement {
        Placement {
            offset: [0.0; 2],
            transform: 0,
            clip: NONE,
        }
    }
}

/// A paint slot local to the chunk being written. Also the text brush:
/// a shaped run carries its slot, so a color change patches the paint
/// record and touches no glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PaintSlot(pub u32);

/// Scratch buffers for building one chunk. Reused across chunks; the
/// scene copies the result into its pools on commit.
#[derive(Default)]
pub struct ChunkWriter {
    pub(crate) rects: Vec<RectInstance>,
    pub(crate) glyphs: Vec<GlyphInstance>,
    pub(crate) paints: Vec<u32>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) bounds: Option<Rect>,
}

impl ChunkWriter {
    pub fn new() -> ChunkWriter {
        ChunkWriter::default()
    }

    pub fn clear(&mut self) {
        self.rects.clear();
        self.glyphs.clear();
        self.paints.clear();
        self.segments.clear();
        self.bounds = None;
    }

    /// The rect instances written so far (chunk-local).
    pub fn rects(&self) -> &[RectInstance] {
        &self.rects
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty() && self.glyphs.is_empty()
    }

    /// Adds a paint record (0xRRGGBBAA) and returns its slot.
    pub fn paint(&mut self, color: u32) -> PaintSlot {
        self.paints.push(color);
        PaintSlot(self.paints.len() as u32 - 1)
    }

    fn extend(&mut self, kind: SegKind, bounds: Rect) {
        match self.segments.last_mut() {
            Some(s) if s.kind == kind => s.len += 1,
            _ => {
                let start = match kind {
                    SegKind::Rects => self.rects.len() as u32 - 1,
                    SegKind::Glyphs => self.glyphs.len() as u32 - 1,
                };
                assert!(
                    self.segments.len() < MAX_SEGMENTS,
                    "too many chunk segments"
                );
                self.segments.push(Segment {
                    kind,
                    start,
                    len: 1,
                });
            }
        }
        self.bounds = Some(match self.bounds {
            Some(b) => b.union(&bounds),
            None => bounds,
        });
    }

    /// A filled rect in chunk-local logical units, edges snapped.
    pub fn rect(&mut self, r: Rect, radius: f32, fill: PaintSlot) {
        self.rect_bordered(r, radius, fill, None, 0.0);
    }

    pub fn rect_bordered(
        &mut self,
        r: Rect,
        radius: f32,
        fill: PaintSlot,
        border: Option<PaintSlot>,
        border_width: f32,
    ) {
        self.rects.push(RectInstance {
            rect: [r.origin.x, r.origin.y, r.size.width, r.size.height],
            radius,
            border_width,
            fill: fill.0,
            border: border.map_or(NO_PAINT, |b| b.0),
            chunk: 0,
            flags: RectInstance::FLAG_SNAP,
        });
        self.extend(SegKind::Rects, r);
    }

    /// A glyph bitmap whose top-left sits at (`x`, `y`), logical units.
    /// `w`/`h` is its logical size (pixels / scale), for bounds only.
    pub fn glyph(&mut self, x: f32, y: f32, w: f32, h: f32, raster: RasterId, paint: PaintSlot) {
        self.glyphs.push(GlyphInstance {
            pos: [x, y],
            raster: raster.0,
            paint: paint.0,
            chunk: 0,
        });
        self.extend(SegKind::Glyphs, Rect::new(x, y, w, h));
    }
}
