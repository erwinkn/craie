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
use crate::prim::{GlyphInstance, NO_PAINT, PathVertex, RectInstance, SegKind, Segment, gradient};
use crate::space::NONE;

pub const MAX_SEGMENTS: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
pub struct Chunk {
    pub rects: Span,
    pub glyphs: Span,
    pub paints: Span,
    /// Path mesh vertices and indices (three per triangle).
    pub path_vertices: Span,
    pub path_indices: Span,
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

/// A linear or radial gradient paint (`prim::gradient` layout).
#[derive(Clone, Debug, PartialEq)]
pub struct GradientPaint {
    /// `gradient::LINEAR` or `gradient::RADIAL`.
    pub kind: u32,
    /// Linear: x0, y0, x1, y1; radial: cx, cy, r, 0 (gradient space).
    pub geometry: [f32; 4],
    /// Chunk-local space to gradient space.
    pub to_gradient: [f32; 6],
    /// (offset in [0, 1], 0xRRGGBBAA), offsets ascending.
    pub stops: Vec<(f32, u32)>,
}

/// A box shadow as the scene draws it (`ChunkWriter::set_shadow`), in
/// chunk-local logical units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxShadow {
    /// The shadow's shape before the blur: the box offset and spread.
    pub shape: Rect,
    pub radius: f32,
    /// The Gaussian's standard deviation (CSS: half the blur radius).
    pub sigma: f32,
    /// The box it is cut against: an outer shadow shows outside it, an
    /// inset one inside it.
    pub box_rect: Rect,
    pub box_radius: f32,
    pub color: u32,
    pub inset: bool,
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
    pub(crate) path_vertices: Vec<PathVertex>,
    /// Chunk-local vertex indices (rebased on commit).
    pub(crate) path_indices: Vec<u32>,
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
        self.path_vertices.clear();
        self.path_indices.clear();
        self.segments.clear();
        self.bounds = None;
    }

    /// The rect instances written so far (chunk-local).
    pub fn rects(&self) -> &[RectInstance] {
        &self.rects
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty() && self.glyphs.is_empty() && self.path_indices.is_empty()
    }

    /// Adds a paint record (0xRRGGBBAA) and returns its slot.
    pub fn paint(&mut self, color: u32) -> PaintSlot {
        self.paints.push(color);
        PaintSlot(self.paints.len() as u32 - 1)
    }

    /// Adds a gradient paint record and returns its slot (for
    /// `mesh`'s `gradient` flag). Stops beyond `gradient::MAX_STOPS`
    /// are dropped; offsets clamp to [0, 1] and never decrease (SVG);
    /// non-finite numbers become 0. No stops draws nothing.
    pub fn gradient(&mut self, g: &GradientPaint) -> PaintSlot {
        let slot = PaintSlot(self.paints.len() as u32);
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let stops = &g.stops[..g.stops.len().min(gradient::MAX_STOPS)];
        self.paints.push(g.kind | (stops.len() as u32) << 16);
        self.paints.extend(g.geometry.map(|v| finite(v).to_bits()));
        self.paints
            .extend(g.to_gradient.map(|v| finite(v).to_bits()));
        let mut last = 0.0f32;
        for &(offset, color) in stops {
            last = finite(offset).clamp(last, 1.0);
            self.paints.push(last.to_bits());
            self.paints.push(color);
        }
        slot
    }

    /// A triangle mesh in chunk-local logical units: `indices` (three
    /// per triangle) index `vertices`. `gradient` says the slot is a
    /// gradient record. Its bounds are the vertices' bounds. A mesh with
    /// a partial triangle, an index out of range, or a non-finite
    /// vertex is refused (returns false) and writes nothing.
    pub fn mesh(
        &mut self,
        vertices: &[[f32; 2]],
        indices: &[u32],
        paint: PaintSlot,
        gradient: bool,
    ) -> bool {
        if indices.is_empty()
            || !indices.len().is_multiple_of(3)
            || indices.iter().any(|&i| i as usize >= vertices.len())
            || vertices
                .iter()
                .any(|v| !(v[0].is_finite() && v[1].is_finite()))
        {
            return false;
        }
        let base = self.path_vertices.len() as u32;
        let info = if gradient { PathVertex::GRADIENT } else { 0 };
        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
        for &[x, y] in vertices {
            lo = [lo[0].min(x), lo[1].min(y)];
            hi = [hi[0].max(x), hi[1].max(y)];
            self.path_vertices.push(PathVertex {
                pos: [x, y],
                paint: paint.0,
                info,
            });
        }
        let start = self.path_indices.len() as u32;
        self.path_indices.extend(indices.iter().map(|&i| base + i));
        let bounds = Rect::new(lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]);
        match self.segments.last_mut() {
            Some(s) if s.kind == SegKind::Paths => s.len += indices.len() as u32,
            _ => {
                assert!(
                    self.segments.len() < MAX_SEGMENTS,
                    "too many chunk segments"
                );
                self.segments.push(Segment {
                    kind: SegKind::Paths,
                    start,
                    len: indices.len() as u32,
                });
            }
        }
        self.bounds = Some(match self.bounds {
            Some(b) => b.union(&bounds),
            None => bounds,
        });
        true
    }

    /// Reserves a rect at this point of the paint order, written later
    /// with `set_shadow` (its paint records may come after content that
    /// numbers its own slots, like an input's).
    pub fn reserve_rect(&mut self) -> usize {
        self.rects.push(RectInstance::default());
        self.extend_segment(SegKind::Rects);
        self.rects.len() - 1
    }

    /// Writes a box shadow into the reserved rect `at` (`reserve_rect`),
    /// with its paint records: the color, then the box it is cut
    /// against. Its bounds reach 3 σ past an outer shadow's shape (where
    /// the Gaussian has fallen under 0.3 %); an inset one stays in its
    /// box.
    pub fn set_shadow(&mut self, at: usize, s: &BoxShadow) {
        let color = self.paint(s.color);
        let params = PaintSlot(self.paints.len() as u32);
        let b = s.box_rect;
        self.paints.extend(
            [
                b.origin.x,
                b.origin.y,
                b.size.width,
                b.size.height,
                s.box_radius,
            ]
            .map(f32::to_bits),
        );
        let mut flags = RectInstance::FLAG_SNAP | RectInstance::FLAG_SHADOW;
        if s.inset {
            flags |= RectInstance::FLAG_INSET;
        }
        self.rects[at] = RectInstance {
            rect: [
                s.shape.origin.x,
                s.shape.origin.y,
                s.shape.size.width,
                s.shape.size.height,
            ],
            radius: s.radius,
            border_width: s.sigma,
            fill: color.0,
            border: params.0,
            chunk: 0,
            flags,
        };
        let bounds = if s.inset {
            b
        } else {
            let m = 3.0 * s.sigma + 1.0;
            Rect::new(
                s.shape.origin.x - m,
                s.shape.origin.y - m,
                s.shape.size.width + 2.0 * m,
                s.shape.size.height + 2.0 * m,
            )
        };
        self.grow(bounds);
    }

    /// Writes a filled rect (edges snapped) into the reserved rect `at`
    /// (`reserve_rect`), with its color's paint record.
    pub fn set_fill(&mut self, at: usize, r: Rect, color: u32) {
        let fill = self.paint(color);
        self.rects[at] = RectInstance {
            rect: [r.origin.x, r.origin.y, r.size.width, r.size.height],
            radius: 0.0,
            border_width: 0.0,
            fill: fill.0,
            border: NO_PAINT,
            chunk: 0,
            flags: RectInstance::FLAG_SNAP,
        };
        self.grow(r);
    }

    fn extend(&mut self, kind: SegKind, bounds: Rect) {
        self.extend_segment(kind);
        self.grow(bounds);
    }

    fn grow(&mut self, bounds: Rect) {
        self.bounds = Some(match self.bounds {
            Some(b) => b.union(&bounds),
            None => bounds,
        });
    }

    fn extend_segment(&mut self, kind: SegKind) {
        match self.segments.last_mut() {
            Some(s) if s.kind == kind => s.len += 1,
            _ => {
                let start = match kind {
                    SegKind::Rects => self.rects.len() as u32 - 1,
                    SegKind::Glyphs => self.glyphs.len() as u32 - 1,
                    SegKind::Paths => unreachable!("meshes extend in `mesh`"),
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
