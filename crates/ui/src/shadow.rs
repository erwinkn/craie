//! Box shadows (`boxShadow`; the kit's elevation and rings), as CSS
//! draws them: each shadow is the box's shape moved by its offset,
//! grown by its spread (shrunk, inset), and blurred by a Gaussian of σ
//! = blur / 2. An outer shadow shows outside the border box, an inset
//! one inside the padding box. The first in the list paints on top.
//!
//! ```text
//! card: ring (spread 1, the line color), then six soft drops
//! [ {spread: 1, color: line}, {y: 18, blur: 47, color: shadow 3 %}, ... ]
//! ```
//!
//! A node stores its effective list (`Host::shadows`); a variant table
//! holds one in its values (`Values::shadows`), so `_hover: { elevation:
//! raised }` swaps it. Shadows draw in the node's own chunk (rects in
//! shadow mode, `RectInstance::FLAG_SHADOW`): outer ones before its
//! fill, inset ones after, so its content stays above them.

use craie_core::Rect;
use craie_scene::BoxShadow;

/// Shadows one node may hold: the kit's largest stack (`card`) has 7.
pub const MAX_SHADOWS: usize = 8;

/// Offsets, blur and spread are bounded (logical points) so a shadow's
/// bounds stay finite and its quad sane.
pub const MAX_EXTENT: f32 = 4096.0;

/// One shadow, logical points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    /// The CSS blur radius: σ is half of it.
    pub blur: f32,
    pub spread: f32,
    /// 0xRRGGBBAA.
    pub color: u32,
    pub inset: bool,
}

impl Shadow {
    /// Finite and in range: blur in [0, `MAX_EXTENT`], offsets and
    /// spread within ±`MAX_EXTENT`.
    pub fn valid(&self) -> bool {
        (0.0..=MAX_EXTENT).contains(&self.blur)
            && [self.x, self.y, self.spread]
                .iter()
                .all(|v| (-MAX_EXTENT..=MAX_EXTENT).contains(v))
    }
}

/// A node's shadows, first on top; at most `MAX_SHADOWS`. Inline so
/// style values stay `Copy`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shadows {
    len: u8,
    items: [Shadow; MAX_SHADOWS],
}

impl Shadows {
    /// `None` past `MAX_SHADOWS`.
    pub fn new(list: &[Shadow]) -> Option<Shadows> {
        if list.len() > MAX_SHADOWS {
            return None;
        }
        let mut s = Shadows {
            len: list.len() as u8,
            ..Shadows::default()
        };
        s.items[..list.len()].copy_from_slice(list);
        Some(s)
    }

    pub fn as_slice(&self) -> &[Shadow] {
        &self.items[..self.len as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn valid(&self) -> bool {
        self.as_slice().iter().all(Shadow::valid)
    }
}

/// Wire flag bits of one shadow.
pub mod shadow_flag {
    pub const INSET: u8 = 1 << 0;
}

/// A corner radius grown by `spread` (CSS Backgrounds 3, "shadow
/// shape"): r + s, except that a radius smaller than the spread grows
/// less (by s × (1 + (r/s - 1)³)), so a square corner stays square.
/// Shrinking clamps at 0.
fn spread_radius(r: f32, s: f32) -> f32 {
    if r <= 0.0 {
        0.0
    } else if s <= 0.0 || r >= s {
        (r + s).max(0.0)
    } else {
        let k = r / s - 1.0;
        r + s * (1.0 + k * k * k)
    }
}

/// How `s` draws on a box: `border` is the border box and its radius,
/// `padding` the padding box and its (an inset shadow is cut to it).
/// `None` when it draws nothing (transparent).
pub(crate) fn box_shadow(
    s: &Shadow,
    border: (Rect, f32),
    padding: (Rect, f32),
) -> Option<BoxShadow> {
    if s.color & 0xFF == 0 {
        return None;
    }
    let ((b, r), grow) = if s.inset {
        (padding, -s.spread)
    } else {
        (border, s.spread)
    };
    let w = (b.size.width + 2.0 * grow).max(0.0);
    let h = (b.size.height + 2.0 * grow).max(0.0);
    let shape = Rect::new(
        b.origin.x + s.x + (b.size.width - w) / 2.0,
        b.origin.y + s.y + (b.size.height - h) / 2.0,
        w,
        h,
    );
    Some(BoxShadow {
        shape,
        radius: spread_radius(r, grow),
        sigma: s.blur / 2.0,
        box_rect: b,
        box_radius: r,
        color: s.color,
        inset: s.inset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_keeps_square_corners_square() {
        assert_eq!(spread_radius(0.0, 4.0), 0.0);
        assert_eq!(spread_radius(8.0, 2.0), 10.0);
        assert_eq!(spread_radius(8.0, -10.0), 0.0);
        // r < s: grows less than s (r/s = 1/2: s × 7/8).
        assert_eq!(spread_radius(2.0, 4.0), 2.0 + 4.0 * 0.875);
    }

    #[test]
    fn shapes_follow_offset_and_spread() {
        let border = (Rect::new(0.0, 0.0, 100.0, 40.0), 6.0);
        let padding = (Rect::new(1.0, 1.0, 98.0, 38.0), 5.0);
        let ring = Shadow {
            spread: 1.0,
            color: 0x0000_00FF,
            ..Shadow::default()
        };
        let s = box_shadow(&ring, border, padding).unwrap();
        assert_eq!(s.shape, Rect::new(-1.0, -1.0, 102.0, 42.0));
        assert_eq!((s.radius, s.sigma, s.box_rect), (7.0, 0.0, border.0));
        let drop = Shadow {
            y: 18.0,
            blur: 47.0,
            color: 0x0000_0008,
            ..Shadow::default()
        };
        let s = box_shadow(&drop, border, padding).unwrap();
        assert_eq!(
            (s.shape, s.sigma),
            (Rect::new(0.0, 18.0, 100.0, 40.0), 23.5)
        );
        // Inset: the padding box, shrunk by the spread.
        let inset = Shadow {
            y: 1.0,
            blur: 2.0,
            spread: 2.0,
            color: 0x0000_001F,
            inset: true,
            ..Shadow::default()
        };
        let s = box_shadow(&inset, border, padding).unwrap();
        assert_eq!(s.shape, Rect::new(3.0, 4.0, 94.0, 34.0));
        assert_eq!((s.radius, s.box_rect, s.box_radius), (3.0, padding.0, 5.0));
        let clear = Shadow::default();
        assert_eq!(box_shadow(&clear, border, padding), None);
    }
}
