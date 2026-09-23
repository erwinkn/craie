//! Surfaces: native drawing nodes fed by payload bytes.
//!
//! A surface is a real host node: it lays out, clips, hit-tests,
//! scrolls, and carries semantics like any other. Its content comes from
//! a native painter selected by the surface kind, reading the payload
//! bytes the bridge copied in with the commit (a typed array on the JS
//! side). There is no JS callback at paint time.
//!
//! Painters emit logical-space quads inside the node's content box.
//! Rust hosts register more kinds with `Ui::register_surface`.

use crate::geom::Rect;
use crate::host::SurfaceData;

/// Built-in surface kinds.
pub mod kind {
    /// Bar chart. Payload: little-endian f32 values in [0, 1], one bar
    /// each. params[0] = bar color, params[1] = color of the maximum bar
    /// (0 = same as params[0]), params[2] = gap between bars as f32 bits
    /// (0 = 2 points).
    pub const BARS: u32 = 1;
}

/// A logical-space filled rect a painter emits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Fill, 0xRRGGBBAA.
    pub color: u32,
    /// Corner radius, logical points.
    pub radius: f32,
    /// Border ring width, logical points.
    pub border_w: f32,
    /// Border ring color, 0xRRGGBBAA.
    pub border_color: u32,
}

/// Paints one surface: data + logical content rect in, quads out.
pub type SurfacePainter = Box<dyn FnMut(&SurfaceData, Rect, &mut Vec<Quad>)>;

/// Payload bytes as little-endian f32 values (a trailing partial value
/// is ignored).
pub fn payload_f32(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}

/// The built-in bar chart painter (`kind::BARS`).
pub fn paint_bars(data: &SurfaceData, rect: Rect, out: &mut Vec<Quad>) {
    let n = data.payload.len() / 4;
    if n == 0 || rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return;
    }
    let color = data.params[0];
    let max_color = if data.params[1] != 0 {
        data.params[1]
    } else {
        color
    };
    let gap = match f32::from_bits(data.params[2]) {
        g if g.is_finite() && g > 0.0 => g,
        _ => 2.0,
    };
    let max = payload_f32(&data.payload).fold(f32::NEG_INFINITY, f32::max);
    let bw = rect.size.width / n as f32;
    for (i, v) in payload_f32(&data.payload).enumerate() {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let h = rect.size.height * v;
        out.push(Quad {
            x: rect.origin.x + i as f32 * bw + gap / 2.0,
            y: rect.origin.y + rect.size.height - h,
            w: (bw - gap).max(1.0),
            h,
            color: if v == max.clamp(0.0, 1.0) {
                max_color
            } else {
                color
            },
            radius: 1.5,
            ..Quad::default()
        });
    }
}
