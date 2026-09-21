//! Compact paint representation: the flat display data produced from the
//! retained host and consumed by the wgpu renderer.
//!
//! One node must never imply one GPU resource or one draw call. Everything
//! here is a row in a `Vec` destined for a shared instance buffer.
//!
//! `Scene::items` is a single instance stream in strict document order, so
//! painter ordering falls out of the data and the frame draws in ONE call.
//! Quads and glyphs share the instance format; `FLAG_SOLID` marks a
//! (possibly rounded, bordered) rect, `FLAG_COLOR` a color bitmap glyph.

use bytemuck::{Pod, Zeroable};

/// sRGB color packed as 0xRRGGBBAA. Used as the Parley brush so the value
/// flowing through layout is already the GPU-ready pixel format. The
/// shader decodes to linear for premultiplied compositing; sRGB render
/// targets encode back on store.
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

    pub fn alpha(self) -> u8 {
        self.0 as u8
    }
}

/// One drawable instance, in physical pixels: a filled/bordered rect or a
/// glyph bitmap blit, depending on `flags`.
///
/// `position`/`size` is the top-left rect. For glyphs, `uv_min`/`uv_max`
/// address the atlas page in `page`. Every instance carries a clip rect —
/// `clip_min`/`clip_max` are physical pixels and `params.z` is the clip
/// corner radius; with `FLAG_ROUND_CLIP` the clip edge is rounded.
///
/// For `FLAG_SOLID`, `params` = (corner_radius, border_width, clip_radius,
/// unused); `aux_color` is the border color when `FLAG_BORDER` is set.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub clip_min: [f32; 2],
    pub clip_max: [f32; 2],
    pub color: u32,
    /// Border color for `FLAG_SOLID | FLAG_BORDER`.
    pub aux_color: u32,
    /// (radius, border_width, clip_radius, unused) — solid rects only.
    pub params: [f32; 4],
    pub page: u16,
    pub flags: u16,
}

impl Instance {
    pub const FLAG_COLOR: u16 = 1;
    pub const FLAG_SOLID: u16 = 2;
    /// The instance's clip rect has rounded corners (params.z).
    pub const FLAG_ROUND_CLIP: u16 = 4;
    /// Solid rect carries a border ring of `params.y` width in `aux_color`.
    pub const FLAG_BORDER: u16 = 8;

    /// A solid filled rectangle; clip and border fields are set by the
    /// painter (`Instance::rect` is the full form).
    pub fn quad(x: f32, y: f32, w: f32, h: f32, color: u32) -> Instance {
        Instance {
            position: [x, y],
            size: [w, h],
            uv_min: [0.0; 2],
            uv_max: [0.0; 2],
            clip_min: [f32::MIN, f32::MIN],
            clip_max: [f32::MAX, f32::MAX],
            color,
            aux_color: 0,
            params: [0.0; 4],
            page: 0,
            flags: Instance::FLAG_SOLID,
        }
    }

    /// A filled rect with optional corner radius and border, already in
    /// physical pixels.
    pub fn rect(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: u32,
        radius: f32,
        border_w: f32,
        border_color: u32,
    ) -> Instance {
        let mut inst = Instance::quad(x, y, w, h, fill);
        inst.params[0] = radius;
        inst.params[1] = border_w;
        inst.aux_color = border_color;
        if border_w > 0.0 && border_color & 0xFF != 0 {
            inst.flags |= Instance::FLAG_BORDER;
        }
        inst
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
