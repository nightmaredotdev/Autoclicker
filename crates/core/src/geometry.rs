//! Screen geometry in physical pixels (virtual-screen coordinates).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A point in virtual-screen coordinates. Coordinates may be negative on
/// multi-monitor setups where a monitor is left of or above the primary one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Offsets the point, saturating instead of overflowing.
    pub fn offset(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x.saturating_add(dx), self.y.saturating_add(dy))
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// A rectangle with exclusive `right`/`bottom` edges, matching Win32 `RECT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    /// Clamps a point into the rectangle (the last valid pixel is
    /// `right - 1`/`bottom - 1`). An empty rectangle returns the point unchanged.
    pub fn clamp(&self, p: Point) -> Point {
        if self.is_empty() {
            return p;
        }
        Point::new(
            p.x.clamp(self.left, self.right - 1),
            p.y.clamp(self.top, self.bottom - 1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_points_inside_including_negative_origins() {
        let r = Rect::new(-1920, -200, 0, 880);
        assert_eq!(r.clamp(Point::new(-5000, 0)), Point::new(-1920, 0));
        assert_eq!(r.clamp(Point::new(10, 10)), Point::new(-1, 10));
        assert_eq!(r.clamp(Point::new(-100, 2000)), Point::new(-100, 879));
        assert_eq!(r.clamp(Point::new(-100, 100)), Point::new(-100, 100));
        assert!(r.contains(r.clamp(Point::new(i32::MAX, i32::MIN))));
    }

    #[test]
    fn offset_saturates() {
        assert_eq!(
            Point::new(i32::MAX, 0).offset(5, -1),
            Point::new(i32::MAX, -1)
        );
    }
}
