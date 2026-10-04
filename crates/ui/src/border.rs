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
//! mixed colors: a piece per side, left and right first, top and bottom
//!              over them owning the corners
//! ```

use craie_core::Rect;
use craie_scene::BoxShadow;

/// Widths in logical points and colors (0xRRGGBBAA), in the order top,
/// right, bottom, left. `fallback` bits 0-3: side i's width is the
/// uniform border's; bits 4-7: its color is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

/// How `s` draws on a border box `b` with corner radius `r`, under a
/// uniform border of `uniform_width`: hard inset shadows (their color
/// set from the paint) in paint order. Sides narrower than `min` (one
/// device pixel) paint at `min`.
pub(crate) fn draw(
    s: &BorderSides,
    uniform_width: f32,
    b: Rect,
    r: f32,
    min: f32,
) -> Vec<(BoxShadow, SidePaint)> {
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
    // Painted sides of one paint: one ring.
    if sides.iter().all(|&i| s.paint(i) == s.paint(first)) {
        let hole = Rect::new(
            b.origin.x + l,
            b.origin.y + t,
            (b.size.width - l - rt).max(0.0),
            (b.size.height - t - bt).max(0.0),
        );
        // CSS rounds each inner corner at the radius less its two
        // widths; one radius less the narrowest width is as round or
        // rounder at every corner, so the hole never leaves the box.
        let narrowest = w.iter().copied().fold(f32::INFINITY, f32::min);
        return vec![(piece(hole, (r - narrowest).max(0.0)), s.paint(first))];
    }
    // Mixed paints: each side is the box less a hole cut along it, far
    // past the box elsewhere. Left and right first, so the top and
    // bottom (opaque) own the corners.
    let (x, y, bw, bh) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
    let far = bw + bh + 1.0;
    let hole = |i: usize| match i {
        0 => Rect::new(x - far, y + t, bw + 2.0 * far, bh + far),
        1 => Rect::new(x - far, y - far, bw - rt + far, bh + 2.0 * far),
        2 => Rect::new(x - far, y - far, bw + 2.0 * far, bh - bt + far),
        _ => Rect::new(x + l, y - far, bw + far, bh + 2.0 * far),
    };
    [3, 1, 0, 2]
        .into_iter()
        .filter(|&i| w[i] > 0.0)
        .map(|i| (piece(hole(i), 0.0), s.paint(i)))
        .collect()
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
        let (ring, paint) = d[0];
        assert_eq!(paint, SidePaint::Own(LINE));
        assert_eq!(
            (ring.box_rect, ring.shape),
            (b, Rect::new(0.0, 0.0, 100.0, 39.0))
        );
        assert!(ring.inset && ring.sigma == 0.0);
        let card = own([1.0, 2.0, 3.0, 4.0], [0x1111_11FF; 4]);
        let (ring, _) = draw(&card, 0.0, b, 8.0, 0.5)[0];
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
        let (ring, _) = draw(&s, 0.0, b, 0.0, 0.5)[0];
        assert_eq!(ring.shape, Rect::new(0.0, 0.0, 100.0, 39.0));
        assert!(draw(&own([1.0; 4], [0; 4]), 0.0, b, 0.0, 0.5).is_empty());
        assert!(draw(&own([0.0; 4], [LINE; 4]), 0.0, b, 0.0, 0.5).is_empty());
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

    #[test]
    fn mixed_paints_draw_a_piece_per_side() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let s = own(
            [2.0, 0.0, 1.0, 3.0],
            [0xFF00_00FF, 0x00FF_00FF, 0x0000_FFFF, 0x0000_00FF],
        );
        let d = draw(&s, 0.0, b, 0.0, 0.5);
        let paints: Vec<SidePaint> = d.iter().map(|p| p.1).collect();
        assert_eq!(
            paints,
            [0x0000_00FF, 0xFF00_00FF, 0x0000_FFFF].map(SidePaint::Own),
            "left, then top and bottom"
        );
        // Each hole leaves exactly its side of the box.
        let far = 141.0;
        assert_eq!(
            d[0].0.shape,
            Rect::new(3.0, -far, 100.0 + far, 40.0 + 2.0 * far)
        );
        assert_eq!(
            d[1].0.shape,
            Rect::new(-far, 2.0, 100.0 + 2.0 * far, 40.0 + far)
        );
        assert_eq!(
            d[2].0.shape,
            Rect::new(-far, -far, 100.0 + 2.0 * far, 39.0 + far)
        );
        assert!(d.iter().all(|p| p.0.box_rect == b && p.0.inset));
    }
}
