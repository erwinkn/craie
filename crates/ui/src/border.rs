//! Borders per side (React Native's `borderTopWidth` ... `borderLeftColor`;
//! the kit's dividers): widths and colors for each side, painted inside
//! the border box over the fill. The uniform border (`BoxPaint`) stays
//! the fast path; a node with sides paints these instead.
//!
//! A side the app didn't set falls back to the uniform border, resolved
//! when drawn: its width from the node's border width, its color from
//! the node's border paint slot, so variants and animations of
//! `borderColor` and `borderWidth` reach it.
//!
//! Every piece is a hard inset shadow whose box is the border box (the
//! fill's box) and whose shape is the hole: the shader snaps the box like
//! the fill and the hole relative to it, so a side is round(width x
//! scale) device pixels from the fill's edge wherever the box lands.
//!
//! ```text
//! a divider: sides { widths: [0, 0, 1, 0] }
//! one color:   one ring, the border box less the box inside the widths,
//!              rounded like the box; its inner corners at the radius less
//!              the narrowest width (CSS's for equal widths; never a gap)
//! mixed colors: the ring's top zone (corners included) in the top's
//!              paint, its bottom zone in the bottom's, the left and right
//!              sides between them; no piece overlaps another
//! ```

use craie_core::Rect;
use craie_scene::BoxShadow;

/// Widths in logical points and colors (0xRRGGBBAA), in the order top,
/// right, bottom, left. `fallback` bits 0-3: side i's width is the
/// uniform border's; bits 4-7: its color is. The default falls back on
/// every side: no sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderSides {
    pub widths: [f32; 4],
    pub colors: [u32; 4],
    pub fallback: u8,
}

/// Where a piece's color comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidePaint {
    Own(u32),
    /// The node's border paint slot (patched in place by color changes).
    Uniform,
}

impl Default for BorderSides {
    fn default() -> BorderSides {
        BorderSides {
            widths: [0.0; 4],
            colors: [0; 4],
            fallback: BorderSides::ALL_FALLBACK,
        }
    }
}

impl BorderSides {
    /// Every side falls back: no sides (the uniform border paints).
    pub const ALL_FALLBACK: u8 = 0xFF;

    /// Widths finite, in [0, 4096].
    pub fn valid(&self) -> bool {
        self.widths.iter().all(|w| (0.0..=4096.0).contains(w))
    }

    /// Side `i`'s width, the uniform border's when it falls back.
    pub fn width(&self, i: usize, uniform: f32) -> f32 {
        if self.fallback & (1 << i) != 0 {
            uniform
        } else {
            self.widths[i]
        }
        .max(0.0)
    }

    pub(crate) fn paint(&self, i: usize) -> SidePaint {
        if self.fallback & (1 << (4 + i)) != 0 {
            SidePaint::Uniform
        } else {
            SidePaint::Own(self.colors[i])
        }
    }

    /// The width side `i` paints at, at least `min` when it paints at
    /// all: 0 for a transparent side of its own color. A side of the
    /// uniform color paints whatever that color is now, so it may turn
    /// visible without a rebuild.
    pub(crate) fn painted(&self, i: usize, uniform_width: f32, min: f32) -> f32 {
        let w = self.width(i, uniform_width);
        let visible = match self.paint(i) {
            SidePaint::Own(c) => c & 0xFF != 0,
            SidePaint::Uniform => true,
        };
        if w > 0.0 && visible { w.max(min) } else { 0.0 }
    }
}

/// One piece of a box's sides: a hard inset shadow (its color from the
/// paint), drawn only between two y values when banded.
pub(crate) type Piece = (BoxShadow, SidePaint, Option<(f32, f32)>);

/// How `s` draws on a border box `b` with corner radius `r`, under a
/// uniform border of `uniform_width`: pieces that don't overlap. Sides
/// narrower than `min` (one device pixel) paint at `min`.
pub(crate) fn draw(s: &BorderSides, uniform_width: f32, b: Rect, r: f32, min: f32) -> Vec<Piece> {
    let w: [f32; 4] = std::array::from_fn(|i| s.painted(i, uniform_width, min));
    let r = r.max(0.0).min(b.size.width.min(b.size.height) / 2.0);
    let piece = |hole: Rect, radius: f32| BoxShadow {
        shape: hole,
        radius,
        sigma: 0.0,
        box_rect: b,
        box_radius: r,
        color: 0,
        inset: true,
    };
    let sides: Vec<usize> = (0..4).filter(|&i| w[i] > 0.0).collect();
    let Some(&first) = sides.first() else {
        return Vec::new();
    };
    let [t, rt, bt, l] = w;
    let hole = Rect::new(
        b.origin.x + l,
        b.origin.y + t,
        (b.size.width - l - rt).max(0.0),
        (b.size.height - t - bt).max(0.0),
    );
    // CSS rounds each inner corner at the radius less its two widths; one
    // radius less the narrowest width is as round or rounder at every
    // corner, so the hole never leaves the box.
    let narrowest = w.iter().copied().fold(f32::INFINITY, f32::min);
    let ring = piece(hole, (r - narrowest).max(0.0));
    // Painted sides of one paint: one ring.
    if sides.iter().all(|&i| s.paint(i) == s.paint(first)) {
        return vec![(ring, s.paint(first), None)];
    }
    // Mixed paints: the ring's top zone (down to the radius or the top
    // width) in the top's paint, corners included, and its bottom zone in
    // the bottom's; the left and right sides between them. A side not
    // painted leaves its zone to the left and right, into the corners.
    let (y, h) = (b.origin.y, b.size.height);
    let (mut a, mut z) = (
        if t > 0.0 { r.max(t) } else { 0.0 },
        if bt > 0.0 { r.max(bt) } else { 0.0 },
    );
    if a + z > h {
        (a, z) = (h * a / (a + z), h * z / (a + z));
    }
    let mut out = Vec::new();
    if t > 0.0 {
        out.push((ring, s.paint(0), Some((y, y + a))));
    }
    if bt > 0.0 {
        out.push((ring, s.paint(2), Some((y + h - z, y + h))));
    }
    // A side's piece: the box less a hole cut along it, far past the box
    // elsewhere, in the band between the zones.
    let (x, bw) = (b.origin.x, b.size.width);
    let far = bw + h + 1.0;
    let band = (a + z < h).then_some((y + a, y + h - z));
    if let Some(band) = band {
        if l > 0.0 {
            let cut = Rect::new(x + l, y - far, bw + far, h + 2.0 * far);
            out.push((piece(cut, 0.0), s.paint(3), Some(band)));
        }
        if rt > 0.0 {
            let cut = Rect::new(x - far, y - far, bw - rt + far, h + 2.0 * far);
            out.push((piece(cut, 0.0), s.paint(1), Some(band)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: u32 = 0x2020_20FF;

    fn own(widths: [f32; 4], colors: [u32; 4]) -> BorderSides {
        BorderSides {
            widths,
            colors,
            fallback: 0,
        }
    }

    #[test]
    fn one_paint_draws_a_ring() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let divider = own([0.0, 0.0, 1.0, 0.0], [LINE; 4]);
        let d = draw(&divider, 0.0, b, 0.0, 0.5);
        assert_eq!(d.len(), 1);
        let (ring, paint, band) = d[0];
        assert_eq!((paint, band), (SidePaint::Own(LINE), None));
        assert_eq!(
            (ring.box_rect, ring.shape),
            (b, Rect::new(0.0, 0.0, 100.0, 39.0))
        );
        assert!(ring.inset && ring.sigma == 0.0);
        let card = own([1.0, 2.0, 3.0, 4.0], [0x1111_11FF; 4]);
        let (ring, _, _) = draw(&card, 0.0, b, 8.0, 0.5)[0];
        assert_eq!(ring.shape, Rect::new(4.0, 1.0, 94.0, 36.0));
        assert_eq!(
            (ring.box_radius, ring.radius),
            (8.0, 7.0),
            "radius less the narrowest"
        );
    }

    /// A transparent side of its own color paints nothing, even beside
    /// sides of one color (#34 review: it drew a full ring).
    #[test]
    fn transparent_sides_paint_nothing() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let s = own([1.0; 4], [0, 0, LINE, 0]);
        let (ring, _, _) = draw(&s, 0.0, b, 0.0, 0.5)[0];
        assert_eq!(ring.shape, Rect::new(0.0, 0.0, 100.0, 39.0));
        assert!(draw(&own([1.0; 4], [0; 4]), 0.0, b, 0.0, 0.5).is_empty());
        assert!(draw(&own([0.0; 4], [LINE; 4]), 0.0, b, 0.0, 0.5).is_empty());
        assert_eq!(BorderSides::default().fallback, BorderSides::ALL_FALLBACK);
    }

    /// Unset sides take the uniform border's width and paint slot;
    /// a hairline paints at least `min`.
    #[test]
    fn fallen_back_sides_resolve_from_the_uniform_border() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        // `borderBottomWidth: 1` over `borderColor`: width set, colors not.
        let s = BorderSides {
            widths: [0.0, 0.0, 1.0, 0.0],
            colors: [0; 4],
            fallback: 0b1111_1011,
        };
        let d = draw(&s, 2.0, b, 0.0, 0.5);
        assert_eq!(d.len(), 1, "one paint: a ring");
        assert_eq!(d[0].1, SidePaint::Uniform);
        assert_eq!(d[0].0.shape, Rect::new(2.0, 2.0, 96.0, 37.0));
        let hair = own([0.25, 0.0, 0.0, 0.0], [LINE; 4]);
        assert_eq!(draw(&hair, 0.0, b, 0.0, 0.5)[0].0.shape.origin.y, 0.5);
    }

    /// Mixed paints: the ring's top and bottom zones (corners included)
    /// and the sides between them, in bands that tile the box's height.
    #[test]
    fn mixed_paints_split_into_bands() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let (red, green, blue, black) = (0xFF00_00FF, 0x00FF_00FF, 0x0000_FFFF, 0x0000_00FF);
        let s = own([2.0, 1.0, 1.0, 3.0], [red, green, blue, black]);
        let d = draw(&s, 0.0, b, 8.0, 0.5);
        let bands: Vec<(SidePaint, Option<(f32, f32)>)> = d.iter().map(|p| (p.1, p.2)).collect();
        assert_eq!(
            bands,
            [
                (SidePaint::Own(red), Some((0.0, 8.0))),
                (SidePaint::Own(blue), Some((32.0, 40.0))),
                (SidePaint::Own(black), Some((8.0, 32.0))),
                (SidePaint::Own(green), Some((8.0, 32.0))),
            ]
        );
        // The zones cut the ring; the sides cut the box along their edge.
        assert_eq!(d[0].0.shape, Rect::new(3.0, 2.0, 96.0, 37.0));
        let far = 141.0;
        assert_eq!(
            d[2].0.shape,
            Rect::new(3.0, -far, 100.0 + far, 40.0 + 2.0 * far)
        );
        assert!(
            d.iter()
                .all(|p| p.0.box_rect == b && p.0.inset && p.0.box_radius == 8.0)
        );
        // Square: the zones are the widths. No top: the sides reach it.
        let d = draw(
            &own([0.0, 1.0, 2.0, 1.0], [0, green, red, green]),
            0.0,
            b,
            0.0,
            0.5,
        );
        let bands: Vec<Option<(f32, f32)>> = d.iter().map(|p| p.2).collect();
        assert_eq!(
            bands,
            [Some((38.0, 40.0)), Some((0.0, 38.0)), Some((0.0, 38.0))]
        );
        // Zones that meet split the height in proportion.
        let short = Rect::new(0.0, 0.0, 100.0, 12.0);
        let d = draw(
            &own([1.0, 1.0, 1.0, 1.0], [red, green, blue, green]),
            0.0,
            short,
            6.0,
            0.5,
        );
        let bands: Vec<Option<(f32, f32)>> = d.iter().map(|p| p.2).collect();
        assert_eq!(bands, [Some((0.0, 6.0)), Some((6.0, 12.0))]);
    }
}
