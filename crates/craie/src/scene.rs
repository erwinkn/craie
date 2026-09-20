//! Compact paint representation: the flat display data produced from the
//! retained host (or, in milestone 0, directly from a laid-out text scene)
//! and consumed by the wgpu renderer.
//!
//! One node must never imply one GPU resource or one draw call. Everything
//! here is a row in a `Vec` destined for a shared instance buffer.

use bytemuck::{Pod, Zeroable};

/// sRGB color packed as 0xRRGGBBAA. Used as the Parley brush so the value
/// flowing through layout is already the GPU-ready pixel format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
}

/// One filled axis-aligned rectangle, in physical pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct QuadInstance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: u32,
}

/// One glyph bitmap blit, in physical pixels.
///
/// `position` is the top-left of the glyph bitmap (origin + placement).
/// `page` indexes into the alpha or color atlas texture array; `flags` bit 0
/// selects the color atlas (32-bit RGBA bitmap glyphs) instead of the R8
/// alpha atlas.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GlyphInstance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub color: u32,
    pub page: u16,
    pub flags: u16,
}

impl GlyphInstance {
    pub const FLAG_COLOR: u16 = 1;
}

/// Flat display data for one frame's worth of retained content.
///
/// Rebuilt only when paint data is dirty; the GPU upload of each buffer is
/// tracked separately so an unchanged scene uploads nothing.
#[derive(Default)]
pub struct Scene {
    pub clear: Option<Color>,
    pub quads: Vec<QuadInstance>,
    pub glyphs: Vec<GlyphInstance>,
}
