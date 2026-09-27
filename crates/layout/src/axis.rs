//! Flex-relative access to Taffy's geometry types. Taffy keeps these
//! helpers private to its crate; these are the same definitions, so the
//! owned engine reads and writes the same fields in the same order.

use taffy::geometry::AbsoluteAxis;
use taffy::{FlexDirection, FlexWrap, Point, Rect, Size};

pub(crate) trait Dir: Copy {
    fn is_row(self) -> bool;
    fn is_column(self) -> bool;
    fn is_reverse(self) -> bool;
    fn main_axis(self) -> AbsoluteAxis;
    fn cross_axis(self) -> AbsoluteAxis;
}

impl Dir for FlexDirection {
    #[inline(always)]
    fn is_row(self) -> bool {
        matches!(self, FlexDirection::Row | FlexDirection::RowReverse)
    }

    #[inline(always)]
    fn is_column(self) -> bool {
        matches!(self, FlexDirection::Column | FlexDirection::ColumnReverse)
    }

    #[inline(always)]
    fn is_reverse(self) -> bool {
        matches!(
            self,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        )
    }

    #[inline(always)]
    fn main_axis(self) -> AbsoluteAxis {
        if self.is_row() {
            AbsoluteAxis::Horizontal
        } else {
            AbsoluteAxis::Vertical
        }
    }

    #[inline(always)]
    fn cross_axis(self) -> AbsoluteAxis {
        if self.is_row() {
            AbsoluteAxis::Vertical
        } else {
            AbsoluteAxis::Horizontal
        }
    }
}

pub(crate) fn is_multi_line(wrap: FlexWrap) -> bool {
    wrap != FlexWrap::NoWrap
}

pub(crate) fn is_wrap_reverse(wrap: FlexWrap) -> bool {
    wrap == FlexWrap::WrapReverse
}

pub(crate) trait RectDir<T> {
    fn main_start(&self, dir: FlexDirection) -> T;
    fn main_end(&self, dir: FlexDirection) -> T;
    fn cross_start(&self, dir: FlexDirection) -> T;
    fn cross_end(&self, dir: FlexDirection) -> T;
}

impl<T: Copy> RectDir<T> for Rect<T> {
    #[inline(always)]
    fn main_start(&self, dir: FlexDirection) -> T {
        if dir.is_row() { self.left } else { self.top }
    }

    #[inline(always)]
    fn main_end(&self, dir: FlexDirection) -> T {
        if dir.is_row() {
            self.right
        } else {
            self.bottom
        }
    }

    #[inline(always)]
    fn cross_start(&self, dir: FlexDirection) -> T {
        if dir.is_row() { self.top } else { self.left }
    }

    #[inline(always)]
    fn cross_end(&self, dir: FlexDirection) -> T {
        if dir.is_row() {
            self.bottom
        } else {
            self.right
        }
    }
}

pub(crate) trait RectSum {
    fn main_axis_sum(&self, dir: FlexDirection) -> f32;
    fn cross_axis_sum(&self, dir: FlexDirection) -> f32;
}

impl RectSum for Rect<f32> {
    #[inline(always)]
    fn main_axis_sum(&self, dir: FlexDirection) -> f32 {
        if dir.is_row() {
            self.horizontal_axis_sum()
        } else {
            self.vertical_axis_sum()
        }
    }

    #[inline(always)]
    fn cross_axis_sum(&self, dir: FlexDirection) -> f32 {
        if dir.is_row() {
            self.vertical_axis_sum()
        } else {
            self.horizontal_axis_sum()
        }
    }
}

pub(crate) trait SizeDir<T> {
    fn main(self, dir: FlexDirection) -> T;
    fn cross(self, dir: FlexDirection) -> T;
    fn set_main(&mut self, dir: FlexDirection, value: T);
    fn set_cross(&mut self, dir: FlexDirection, value: T);
    fn with_main(self, dir: FlexDirection, value: T) -> Self;
    fn with_cross(self, dir: FlexDirection, value: T) -> Self;
}

impl<T: Copy> SizeDir<T> for Size<T> {
    #[inline(always)]
    fn main(self, dir: FlexDirection) -> T {
        if dir.is_row() {
            self.width
        } else {
            self.height
        }
    }

    #[inline(always)]
    fn cross(self, dir: FlexDirection) -> T {
        if dir.is_row() {
            self.height
        } else {
            self.width
        }
    }

    #[inline(always)]
    fn set_main(&mut self, dir: FlexDirection, value: T) {
        if dir.is_row() {
            self.width = value
        } else {
            self.height = value
        }
    }

    #[inline(always)]
    fn set_cross(&mut self, dir: FlexDirection, value: T) {
        if dir.is_row() {
            self.height = value
        } else {
            self.width = value
        }
    }

    #[inline(always)]
    fn with_main(mut self, dir: FlexDirection, value: T) -> Self {
        self.set_main(dir, value);
        self
    }

    #[inline(always)]
    fn with_cross(mut self, dir: FlexDirection, value: T) -> Self {
        self.set_cross(dir, value);
        self
    }
}

pub(crate) trait PointDir<T> {
    fn main(self, dir: FlexDirection) -> T;
    fn cross(self, dir: FlexDirection) -> T;
}

impl<T: Copy> PointDir<T> for Point<T> {
    #[inline(always)]
    fn main(self, dir: FlexDirection) -> T {
        if dir.is_row() { self.x } else { self.y }
    }

    #[inline(always)]
    fn cross(self, dir: FlexDirection) -> T {
        if dir.is_row() { self.y } else { self.x }
    }
}
