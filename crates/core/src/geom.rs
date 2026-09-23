//! Small geometry types shared across the crate.
//!
//! These are deliberately unit-agnostic. Each module documents whether it
//! works in logical points or physical pixels.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Size {
    pub const ZERO: Size = Size {
        width: 0.0,
        height: 0.0,
    };

    pub fn new(width: f32, height: f32) -> Size {
        Size { width, height }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        origin: Point::ZERO,
        size: Size::ZERO,
    };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }
}

/// Axis-aligned rectangle in integer pixels, used for atlas dirty tracking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectPx {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

impl RectPx {
    pub fn new(min_x: u32, min_y: u32, max_x: u32, max_y: u32) -> RectPx {
        RectPx {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn union(&mut self, other: RectPx) {
        self.min_x = self.min_x.min(other.min_x);
        self.min_y = self.min_y.min(other.min_y);
        self.max_x = self.max_x.max(other.max_x);
        self.max_y = self.max_y.max(other.max_y);
    }
}

impl Rect {
    pub fn max_x(&self) -> f32 {
        self.origin.x + self.size.width
    }

    pub fn max_y(&self) -> f32 {
        self.origin.y + self.size.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.origin.x && p.y >= self.origin.y && p.x < self.max_x() && p.y < self.max_y()
    }

    /// True when the rects share interior area.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.origin.x < other.max_x()
            && other.origin.x < self.max_x()
            && self.origin.y < other.max_y()
            && other.origin.y < self.max_y()
    }

    /// Intersection, empty (zero size) when disjoint.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x0 = self.origin.x.max(other.origin.x);
        let y0 = self.origin.y.max(other.origin.y);
        let x1 = self.max_x().min(other.max_x());
        let y1 = self.max_y().min(other.max_y());
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    /// Smallest rect covering both. An empty rect contributes nothing.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.size.width <= 0.0 || self.size.height <= 0.0 {
            return *other;
        }
        if other.size.width <= 0.0 || other.size.height <= 0.0 {
            return *self;
        }
        let x0 = self.origin.x.min(other.origin.x);
        let y0 = self.origin.y.min(other.origin.y);
        let x1 = self.max_x().max(other.max_x());
        let y1 = self.max_y().max(other.max_y());
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// A 2D affine transform, CSS `matrix(a, b, c, d, e, f)` order:
///
/// ```text
/// x' = a·x + c·y + e
/// y' = b·x + d·y + f
/// ```
///
/// `Affine::mul(p, c)` is `p · c`: apply `c` first, then `p`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct Affine(pub [f32; 6]);

impl Default for Affine {
    fn default() -> Affine {
        Affine::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn translate(x: f32, y: f32) -> Affine {
        Affine([1.0, 0.0, 0.0, 1.0, x, y])
    }

    pub fn scale(sx: f32, sy: f32) -> Affine {
        Affine([sx, 0.0, 0.0, sy, 0.0, 0.0])
    }

    /// Counter-clockwise in a y-up frame; clockwise on screen (y down),
    /// matching CSS `rotate()`.
    pub fn rotate(radians: f32) -> Affine {
        let (s, c) = radians.sin_cos();
        Affine([c, s, -s, c, 0.0, 0.0])
    }

    /// `self · other`: `other` applies first.
    pub fn mul(&self, other: &Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = other.0;
        Affine([
            a * a2 + c * b2,
            b * a2 + d * b2,
            a * c2 + c * d2,
            b * c2 + d * d2,
            a * e2 + c * f2 + e,
            b * e2 + d * f2 + f,
        ])
    }

    /// `self` applied about `origin` instead of (0, 0).
    pub fn about(&self, origin: Point) -> Affine {
        Affine::translate(origin.x, origin.y)
            .mul(self)
            .mul(&Affine::translate(-origin.x, -origin.y))
    }

    pub fn apply(&self, p: Point) -> Point {
        let [a, b, c, d, e, f] = self.0;
        Point::new(a * p.x + c * p.y + e, b * p.x + d * p.y + f)
    }

    pub fn determinant(&self) -> f32 {
        self.0[0] * self.0[3] - self.0[1] * self.0[2]
    }

    /// Inverse, or `None` for a degenerate (zero-area) transform.
    pub fn invert(&self) -> Option<Affine> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let [a, b, c, d, e, f] = self.0;
        let inv = 1.0 / det;
        Some(Affine([
            d * inv,
            -b * inv,
            -c * inv,
            a * inv,
            (c * f - d * e) * inv,
            (b * e - a * f) * inv,
        ]))
    }

    /// Pure translation.
    pub fn is_translation(&self) -> bool {
        let [a, b, c, d, _, _] = self.0;
        a == 1.0 && b == 0.0 && c == 0.0 && d == 1.0
    }

    /// Maps axis-aligned rects to axis-aligned rects (no rotation or
    /// skew; scale and translation allowed).
    pub fn is_axis_aligned(&self) -> bool {
        self.0[1] == 0.0 && self.0[2] == 0.0
    }

    pub fn translation(&self) -> Point {
        Point::new(self.0[4], self.0[5])
    }

    /// Bounding box of `r` after the transform.
    pub fn map_rect(&self, r: &Rect) -> Rect {
        let pts = [
            self.apply(r.origin),
            self.apply(Point::new(r.max_x(), r.origin.y)),
            self.apply(Point::new(r.origin.x, r.max_y())),
            self.apply(Point::new(r.max_x(), r.max_y())),
        ];
        let mut x0 = f32::INFINITY;
        let mut y0 = f32::INFINITY;
        let mut x1 = f32::NEG_INFINITY;
        let mut y1 = f32::NEG_INFINITY;
        for p in pts {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4
    }

    #[test]
    fn compose_and_invert() {
        let t = Affine::translate(10.0, 5.0)
            .mul(&Affine::rotate(0.7))
            .mul(&Affine::scale(2.0, 3.0));
        let p = Point::new(3.0, -4.0);
        let q = t.apply(p);
        assert!(close(t.invert().unwrap().apply(q), p));
        // Order: scale first, then rotate, then translate.
        let manual = Affine::translate(10.0, 5.0)
            .apply(Affine::rotate(0.7).apply(Affine::scale(2.0, 3.0).apply(p)));
        assert!(close(q, manual));
    }

    #[test]
    fn about_keeps_origin_fixed() {
        let o = Point::new(50.0, 20.0);
        let t = Affine::rotate(1.0).about(o);
        assert!(close(t.apply(o), o));
        assert!(Affine::scale(2.0, 2.0).is_axis_aligned());
        assert!(!Affine::rotate(0.1).is_axis_aligned());
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert!(a.intersects(&b));
        assert_eq!(a.intersect(&b), Rect::new(5.0, 5.0, 5.0, 5.0));
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert!(!a.intersects(&Rect::new(10.0, 0.0, 5.0, 5.0)));
    }
}
