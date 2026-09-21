//! Compact paint representation: the flat display data produced from the
//! retained host and consumed by the wgpu renderer.
//!
//! One node must never imply one GPU resource or one draw call. Everything
//! here is a row in a `Vec` destined for a shared instance buffer.
//!
//! `Scene::items` is a single instance stream in strict document order, so
//! painter ordering falls out of the data and the frame draws in ONE call.
//! Quads and glyphs share the 40-byte instance format; `FLAG_SOLID` marks a
//! plain rect (no atlas fetch), `FLAG_COLOR` a color bitmap glyph.

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

/// One drawable instance, in physical pixels: a filled rect or a glyph
/// bitmap blit, depending on `flags`.
///
/// `position` is the top-left corner. For glyphs, `uv_min`/`uv_max` address
/// the atlas page in `page`; `flags` bit 0 selects the 32-bit RGBA color
/// atlas over the R8 alpha atlas, bit 1 marks a solid rect (color only).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub color: u32,
    pub page: u16,
    pub flags: u16,
}

impl Instance {
    pub const FLAG_COLOR: u16 = 1;
    pub const FLAG_SOLID: u16 = 2;

    /// A solid filled rectangle.
    pub fn quad(x: f32, y: f32, w: f32, h: f32, color: u32) -> Instance {
        Instance {
            position: [x, y],
            size: [w, h],
            uv_min: [0.0; 2],
            uv_max: [0.0; 2],
            color,
            page: 0,
            flags: Instance::FLAG_SOLID,
        }
    }
}

/// Flat display data for one frame's worth of retained content.
///
/// Rebuilt only when paint data is dirty; the GPU upload of the buffer is
/// tracked separately so an unchanged scene uploads nothing.
#[derive(Clone, Default)]
pub struct Scene {
    pub clear: Option<Color>,
    /// Instances in document order — paint order IS vector order.
    pub items: Vec<Instance>,
}
