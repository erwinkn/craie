//! Borders per side (React Native's `borderTopWidth` ... `borderLeftColor`;
//! the kit's dividers): widths and colors for each side, painted inside
//! the border box over the fill. The uniform border (`BoxPaint`) stays
//! the fast path; a node with sides paints these instead.
//!
//! ```text
//! a divider: sides { widths: [0, 0, 1, 0], colors: line }
//! one color:   one ring, the border box less the box inside the widths,
//!              rounded like the box (a hard shadow-mode rect)
//! mixed colors: one rect per side, top and bottom owning the corners
//! ```

use craie_core::Rect;
use craie_scene::BoxShadow;

/// Widths in logical points and colors (0xRRGGBBAA), in the order top,
/// right, bottom, left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BorderSides {
    pub widths: [f32; 4],
    pub colors: [u32; 4],
}

impl BorderSides {
    /// Widths finite, in [0, 4096].
    pub fn valid(&self) -> bool {
        self.widths.iter().all(|w| (0.0..=4096.0).contains(w))
    }

    /// Paints nothing: every side zero-wide or transparent.
    pub fn is_empty(&self) -> bool {
        (0..4).all(|i| self.widths[i] <= 0.0 || self.colors[i] & 0xFF == 0)
    }
}

/// What a box's sides draw: one ring of one color, or a rect per side.
pub(crate) enum SidesDraw {
    Ring(BoxShadow),
    Rects(Vec<(Rect, u32)>),
}

/// How `s` draws on a border box `b` with corner radius `r`.
pub(crate) fn draw(s: &BorderSides, b: Rect, r: f32) -> SidesDraw {
    let [t, rt, bt, l] = s.widths.map(|w| w.max(0.0));
    let r = r.max(0.0).min(b.size.width.min(b.size.height) / 2.0);
    // Sides that paint share one color: the ring.
    let painted: Vec<u32> = (0..4)
        .filter(|&i| s.widths[i] > 0.0 && s.colors[i] & 0xFF != 0)
        .map(|i| s.colors[i])
        .collect();
    if painted.windows(2).all(|w| w[0] == w[1])
        && let Some(&color) = painted.first()
    {
        let inner = Rect::new(
            b.origin.x + l,
            b.origin.y + t,
            (b.size.width - l - rt).max(0.0),
            (b.size.height - t - bt).max(0.0),
        );
        let widest = t.max(rt).max(bt).max(l);
        return SidesDraw::Ring(BoxShadow {
            shape: b,
            radius: r,
            sigma: 0.0,
            box_rect: inner,
            box_radius: (r - widest).max(0.0),
            color,
            inset: false,
        });
    }
    // Mixed colors: square edges, top and bottom over the corners.
    let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
    let side = [
        Rect::new(x, y, w, t),
        Rect::new(x + w - rt, y + t, rt, (h - t - bt).max(0.0)),
        Rect::new(x, y + h - bt, w, bt),
        Rect::new(x, y + t, l, (h - t - bt).max(0.0)),
    ];
    SidesDraw::Rects(
        (0..4)
            .filter(|&i| s.widths[i] > 0.0 && s.colors[i] & 0xFF != 0)
            .map(|i| (side[i], s.colors[i]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_color_draws_a_ring() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let divider = BorderSides {
            widths: [0.0, 0.0, 1.0, 0.0],
            colors: [0x2020_20FF; 4],
        };
        let SidesDraw::Ring(ring) = draw(&divider, b, 0.0) else {
            panic!("one color is a ring")
        };
        assert_eq!(ring.shape, b);
        assert_eq!(ring.box_rect, Rect::new(0.0, 0.0, 100.0, 39.0));
        assert_eq!((ring.sigma, ring.inset), (0.0, false));
        let card = BorderSides {
            widths: [1.0, 2.0, 3.0, 4.0],
            colors: [0x1111_11FF; 4],
        };
        let SidesDraw::Ring(ring) = draw(&card, b, 8.0) else {
            panic!("one color is a ring")
        };
        assert_eq!(ring.box_rect, Rect::new(4.0, 1.0, 94.0, 36.0));
        assert_eq!((ring.radius, ring.box_radius), (8.0, 4.0));
    }

    #[test]
    fn mixed_colors_draw_a_rect_per_side() {
        let b = Rect::new(0.0, 0.0, 100.0, 40.0);
        let s = BorderSides {
            widths: [2.0, 0.0, 1.0, 3.0],
            colors: [0xFF00_00FF, 0x00FF_00FF, 0x0000_FFFF, 0x0000_00FF],
        };
        let SidesDraw::Rects(rects) = draw(&s, b, 0.0) else {
            panic!("mixed colors are rects")
        };
        assert_eq!(
            rects,
            vec![
                (Rect::new(0.0, 0.0, 100.0, 2.0), 0xFF00_00FF),
                (Rect::new(0.0, 39.0, 100.0, 1.0), 0x0000_FFFF),
                (Rect::new(0.0, 2.0, 3.0, 37.0), 0x0000_00FF),
            ]
        );
        assert!(BorderSides::default().is_empty());
    }
}
