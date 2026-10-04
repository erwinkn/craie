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

/// An outer shadow's corner radius: the box's used radius `r` grown
/// by `spread`, adjusted (CSS Backgrounds 3, "outset-adjusted border
/// radius") so that a small rounded corner with a big spread does not
/// turn round: r + s × (1 − (1 − r/s)³ × (1 − coverage³)) where
/// coverage = 2 × min(r / width, r / height), so 2r over the box's
/// longer side for one radius. A circle stays a circle (coverage 1); a
/// square corner stays square.
fn outset_radius(size: (f32, f32), r: f32, s: f32) -> f32 {
    if r <= 0.0 {
        return 0.0;
    }
    if s <= 0.0 {
        return (r + s).max(0.0);
    }
    let coverage = 2.0 * r / size.0.max(size.1);
    if r > s || coverage > 1.0 {
        return r + s;
    }
    let k = 1.0 - r / s;
    r + s * (1.0 - k * k * k * (1.0 - coverage * coverage * coverage))
}

/// The radius a box of `size` draws with: at most half its shorter side.
fn used_radius(size: (f32, f32), r: f32) -> f32 {
    r.max(0.0).min(size.0.min(size.1) / 2.0)
}

/// How far `shadows` reach past the box on any side (logical points):
/// the farthest offset, spread and 3 σ of an outer shadow.
pub(crate) fn reach(shadows: &Shadows) -> f32 {
    shadows
        .as_slice()
        .iter()
        .filter(|s| !s.inset && s.color & 0xFF != 0)
        .map(|s| s.x.abs().max(s.y.abs()) + s.spread.max(0.0) + 1.5 * s.blur + 1.0)
        .fold(0.0, f32::max)
}

/// How `s` draws on a box (`border`: the border box and its radius)
/// whose painted border is `widths` wide (top, right, bottom, left): an
/// outer shadow is cut against the border box, an inset one against the
/// box inside the painted border. `None` when it draws nothing
/// (transparent).
pub(crate) fn box_shadow(s: &Shadow, border: (Rect, f32), widths: [f32; 4]) -> Option<BoxShadow> {
    if s.color & 0xFF == 0 {
        return None;
    }
    let (b, r) = border;
    let r = used_radius((b.size.width, b.size.height), r);
    let (b, r) = if s.inset {
        let [t, rt, bt, l] = widths.map(|w| w.max(0.0));
        // Opposite sides wider than the box meet in the middle.
        let fit = |a: f32, b: f32, size: f32| {
            let k = if a + b > size { size / (a + b) } else { 1.0 };
            (a * k, b * k)
        };
        let (l, rt) = fit(l, rt, b.size.width);
        let (t, bt) = fit(t, bt, b.size.height);
        let inner = Rect::new(
            b.origin.x + l,
            b.origin.y + t,
            b.size.width - l - rt,
            b.size.height - t - bt,
        );
        (inner, (r - t.max(rt).max(bt).max(l)).max(0.0))
    } else {
        (b, r)
    };
    let grow = if s.inset { -s.spread } else { s.spread };
    let w = (b.size.width + 2.0 * grow).max(0.0);
    let h = (b.size.height + 2.0 * grow).max(0.0);
    let shape = Rect::new(
        b.origin.x + s.x + (b.size.width - w) / 2.0,
        b.origin.y + s.y + (b.size.height - h) / 2.0,
        w,
        h,
    );
    let radius = if s.inset {
        (r + grow).max(0.0)
    } else {
        outset_radius((b.size.width, b.size.height), r, grow)
    };
    Some(BoxShadow {
        shape,
        radius,
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

    /// CSS's outset-adjusted radius: a circle stays one whatever the
    /// spread (and a radius past half the box acts as half of it); a
    /// square corner stays square; a small corner on a long box grows
    /// less than the spread.
    #[test]
    fn outset_radii_keep_the_shape() {
        let size = (32.0, 32.0);
        assert_eq!(outset_radius(size, 16.0, 40.0), 56.0);
        assert_eq!(outset_radius(size, used_radius(size, 100.0), 40.0), 56.0);
        assert_eq!(outset_radius(size, 0.0, 4.0), 0.0);
        assert_eq!(outset_radius(size, 8.0, 2.0), 10.0);
        assert_eq!(outset_radius(size, 8.0, -10.0), 0.0);
        // r = 2, s = 4 on a 100 × 40 box: coverage 2 × 2 / 100.
        let want = 2.0 + 4.0 * (1.0 - 0.125 * (1.0 - 0.04f32.powi(3)));
        assert!((outset_radius((100.0, 40.0), 2.0, 4.0) - want).abs() < 1e-5);
        // A 100 × 40 pill-ish box, radius 20, spread 40: coverage 0.4,
        // radius 55.32 (Chrome's).
        assert!((outset_radius((100.0, 40.0), 20.0, 40.0) - 55.32).abs() < 1e-3);
    }

    #[test]
    fn shapes_follow_offset_and_spread() {
        let border = (Rect::new(0.0, 0.0, 100.0, 40.0), 6.0);
        let ring = Shadow {
            spread: 1.0,
            color: 0x0000_00FF,
            ..Shadow::default()
        };
        let s = box_shadow(&ring, border, [1.0; 4]).unwrap();
        assert_eq!(s.shape, Rect::new(-1.0, -1.0, 102.0, 42.0));
        assert_eq!((s.radius, s.sigma, s.box_rect), (7.0, 0.0, border.0));
        let drop = Shadow {
            y: 18.0,
            blur: 47.0,
            color: 0x0000_0008,
            ..Shadow::default()
        };
        let s = box_shadow(&drop, border, [1.0; 4]).unwrap();
        assert_eq!(
            (s.shape, s.sigma),
            (Rect::new(0.0, 18.0, 100.0, 40.0), 23.5)
        );
        // Inset: inside the painted border (1), shrunk by the spread.
        let inset = Shadow {
            y: 1.0,
            blur: 2.0,
            spread: 2.0,
            color: 0x0000_001F,
            inset: true,
            ..Shadow::default()
        };
        let s = box_shadow(&inset, border, [1.0; 4]).unwrap();
        assert_eq!(s.shape, Rect::new(3.0, 4.0, 94.0, 34.0));
        assert_eq!(
            (s.radius, s.box_rect, s.box_radius),
            (3.0, Rect::new(1.0, 1.0, 98.0, 38.0), 5.0)
        );
        // Borders per side: inside each side's width.
        let s = box_shadow(&inset, border, [0.0, 2.0, 1.0, 4.0]).unwrap();
        assert_eq!(
            (s.box_rect, s.box_radius),
            (Rect::new(4.0, 0.0, 94.0, 39.0), 2.0)
        );
        let clear = Shadow::default();
        assert_eq!(box_shadow(&clear, border, [1.0; 4]), None);
        // Reach: offset, spread and 3 σ of the farthest outer shadow.
        let list = Shadows::new(&[ring, drop, inset]).unwrap();
        assert_eq!(reach(&list), 18.0 + 1.5 * 47.0 + 1.0);
    }
}
