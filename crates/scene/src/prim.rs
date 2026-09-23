//! Drawing primitives. Each kind lives in its own dense array (a span
//! pool per kind); chunks own ranges of them. Geometry is chunk-local
//! and in logical units; the chunk's placement and transform record map
//! it to device pixels.

use bytemuck::{Pod, Zeroable};

/// "No paint" marker for optional paint references (e.g. no border).
pub const NO_PAINT: u32 = u32::MAX;

/// sRGB color packed as 0xRRGGBBAA. The shader decodes to linear for
/// premultiplied blending; sRGB targets encode back on store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Color(pub u32);

impl Color {
    pub const WHITE: Color = Color(0xFFFF_FFFF);
    pub const BLACK: Color = Color(0x0000_00FF);
    pub const TRANSPARENT: Color = Color(0);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32))
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::rgba(r, g, b, 255)
    }

    pub fn alpha(self) -> u8 {
        self.0 as u8
    }
}

/// A filled, optionally rounded and bordered rectangle. 40 bytes.
///
/// `fill` and `border` index the paint pool (absolute indices, written
/// when the chunk is committed). `chunk` indexes the chunk placement
/// table.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct RectInstance {
    /// x, y, width, height in chunk-local logical units.
    pub rect: [f32; 4],
    /// Corner radius, logical units.
    pub radius: f32,
    /// Border ring width, logical units (0 = none).
    pub border_width: f32,
    pub fill: u32,
    /// Border paint, `NO_PAINT` when the rect has no border.
    pub border: u32,
    pub chunk: u32,
    pub flags: u32,
}

impl RectInstance {
    /// Edges snap to the device-pixel grid when the placement is
    /// axis-aligned. UI boxes set it; free-form surface content may not.
    pub const FLAG_SNAP: u32 = 1;
}

/// One glyph bitmap placed in a chunk. 20 bytes.
///
/// `pos` is the bitmap's top-left in chunk-local logical units, with the
/// raster's bearing already applied. The bitmap's pixel size comes from
/// the raster residency table, so an atlas relocation touches no glyph.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct GlyphInstance {
    pub pos: [f32; 2],
    pub raster: u32,
    pub paint: u32,
    pub chunk: u32,
}

/// Primitive kind of a chunk segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegKind {
    Rects,
    Glyphs,
}

/// A run of same-kind primitives inside a chunk, in paint order.
/// `start` is relative to the chunk's range of that kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegKind,
    pub start: u32,
    pub len: u32,
}
