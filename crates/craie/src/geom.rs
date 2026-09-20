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
