//! Integer geometry in physical (device) pixels.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Rectangle with origin and size. `w`/`h` are never negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Normalized rectangle spanning two corner points (inclusive of both pixels).
    pub fn from_corners(a: Point, b: Point) -> Self {
        let x0 = a.x.min(b.x);
        let y0 = a.y.min(b.y);
        let x1 = a.x.max(b.x);
        let y1 = a.y.max(b.y);
        Self::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }

    pub fn from_ltrb(l: i32, t: i32, r: i32, b: i32) -> Self {
        Self::new(l, t, (r - l).max(0), (b - t).max(0))
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn area(&self) -> i64 {
        self.w.max(0) as i64 * self.h.max(0) as i64
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }
    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let l = self.x.max(o.x);
        let t = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::from_ltrb(l, t, r.max(l), b.max(t))
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect::from_ltrb(self.x.min(o.x), self.y.min(o.y), self.right().max(o.right()), self.bottom().max(o.bottom()))
    }

    pub fn inflate(&self, d: i32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2 * d, self.h + 2 * d)
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Clamp this rect so it lies fully inside `bounds` (moving, then shrinking).
    pub fn clamp_inside(&self, bounds: &Rect) -> Rect {
        let w = self.w.min(bounds.w);
        let h = self.h.min(bounds.h);
        let x = self.x.clamp(bounds.x, bounds.right() - w);
        let y = self.y.clamp(bounds.y, bounds.bottom() - h);
        Rect::new(x, y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_normalize() {
        let r = Rect::from_corners(Point::new(10, 20), Point::new(5, 2));
        assert_eq!(r, Rect::new(5, 2, 6, 19));
        let one = Rect::from_corners(Point::new(3, 3), Point::new(3, 3));
        assert_eq!(one, Rect::new(3, 3, 1, 1));
    }

    #[test]
    fn intersect_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
    }

    #[test]
    fn clamp() {
        let b = Rect::new(0, 0, 100, 100);
        assert_eq!(Rect::new(90, -5, 20, 20).clamp_inside(&b), Rect::new(80, 0, 20, 20));
    }
}
