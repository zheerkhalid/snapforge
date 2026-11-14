//! Pure pin geometry: zoom steps, zooming around a point, client/image mapping and
//! canvas expand/shrink. No Win32 here, so it is unit-tested directly.

use crate::common::{Image, Point, Rect};

pub const MIN_ZOOM: f64 = 0.1;
pub const MAX_ZOOM: f64 = 10.0;
/// Per wheel notch.
const ZOOM_STEP: f64 = 1.1;

/// Zoom after `notches` wheel steps (positive = in). Snaps to exactly 100% when the step
/// crosses or lands within 1% of it, and clamps to 10%..=1000%.
pub fn next_zoom(z: f64, notches: i32) -> f64 {
    if notches == 0 {
        return z.clamp(MIN_ZOOM, MAX_ZOOM);
    }
    let mut n = z;
    for _ in 0..notches.unsigned_abs() {
        let prev = n;
        n = if notches > 0 { n * ZOOM_STEP } else { n / ZOOM_STEP };
        if (prev < 1.0 && n > 1.0) || (prev > 1.0 && n < 1.0) || (n - 1.0).abs() < 0.01 {
            n = 1.0;
        }
    }
    n.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Step opacity (percent) by `notches` × 10, clamped to 10..=100.
pub fn next_opacity(pct: u8, notches: i32) -> u8 {
    let v = (pct as i32 / 10 * 10 + notches * 10).clamp(10, 100);
    v as u8
}

/// The screen origin of the image so that the image point under `anchor` stays under it
/// when the zoom changes from `old` to `new`.
pub fn zoom_about(origin: (f64, f64), old: f64, new: f64, anchor: Point) -> (f64, f64) {
    let k = new / old;
    let ax = anchor.x as f64;
    let ay = anchor.y as f64;
    (ax - (ax - origin.0) * k, ay - (ay - origin.1) * k)
}

/// Displayed size of an image at `zoom` (at least 1×1).
pub fn view_size(w: u32, h: u32, zoom: f64) -> (i32, i32) {
    (((w as f64 * zoom).round() as i32).max(1), ((h as f64 * zoom).round() as i32).max(1))
}

/// Client point (relative to the image's top-left on screen) → image coordinates.
pub fn client_to_image(x: f64, y: f64, zoom: f64) -> (f32, f32) {
    ((x / zoom) as f32, (y / zoom) as f32)
}

/// Which borders a Ctrl+drag grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Edges {
    pub fn any(self) -> bool {
        self.left || self.top || self.right || self.bottom
    }
}

/// Border zone hit-test for a `w`×`h` view; `grip` is the zone thickness in pixels.
pub fn hit_edges(p: Point, w: i32, h: i32, grip: i32) -> Edges {
    if p.x < -grip || p.y < -grip || p.x >= w + grip || p.y >= h + grip {
        return Edges::default();
    }
    let g = grip.min(w / 3).min(h / 3).max(2);
    Edges { left: p.x < g, top: p.y < g, right: p.x >= w - g, bottom: p.y >= h - g }
}

/// The new canvas, in the old image's coordinates, after dragging `edges` by (dx, dy)
/// image pixels. Moving the left/top edge outwards (negative) expands, inwards crops;
/// likewise right/bottom with positive/negative. The result is at least 1×1.
pub fn canvas_after_drag(w: u32, h: u32, edges: Edges, dx: i32, dy: i32) -> Rect {
    let (w, h) = (w as i32, h as i32);
    let mut l = 0;
    let mut t = 0;
    let mut r = w;
    let mut b = h;
    if edges.left {
        l = (l + dx).min(r - 1);
    }
    if edges.right {
        r = (r + dx).max(l + 1);
    }
    if edges.top {
        t = (t + dy).min(b - 1);
    }
    if edges.bottom {
        b = (b + dy).max(t + 1);
    }
    Rect::from_ltrb(l, t, r, b)
}

/// Apply a canvas rect (old-image coordinates) to `img`: areas outside the old image are
/// filled with `fill` (premultiplied BGRA), areas outside the rect are cropped away.
pub fn apply_canvas(img: &Image, canvas: Rect, fill: [u8; 4]) -> Image {
    let mut out = Image::filled(canvas.w.max(1) as u32, canvas.h.max(1) as u32, fill);
    out.blit(img, -canvas.x, -canvas.y);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_snap_and_clamp() {
        assert_eq!(next_zoom(1.0, 0), 1.0);
        let z = next_zoom(1.0, 1);
        assert!((z - 1.1).abs() < 1e-9);
        assert_eq!(next_zoom(z, -1), 1.0);
        // 0.95 → 1.045 crosses 100% → snaps to 1.0.
        assert_eq!(next_zoom(0.95, 1), 1.0);
        assert_eq!(next_zoom(9.5, 5), MAX_ZOOM);
        assert_eq!(next_zoom(0.12, -5), MIN_ZOOM);
        // Many notches in then out return to 100%.
        assert_eq!(next_zoom(next_zoom(1.0, 7), -7), 1.0);
    }

    #[test]
    fn opacity_steps() {
        assert_eq!(next_opacity(100, 1), 100);
        assert_eq!(next_opacity(100, -1), 90);
        assert_eq!(next_opacity(15, -3), 10);
        assert_eq!(next_opacity(50, 2), 70);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let origin = (100.0, 200.0);
        let anchor = Point::new(150, 260); // image point (50, 60) at zoom 1
        let o = zoom_about(origin, 1.0, 2.0, anchor);
        assert_eq!(o, (50.0, 140.0));
        // The image point under the anchor is unchanged.
        let before = client_to_image(anchor.x as f64 - origin.0, anchor.y as f64 - origin.1, 1.0);
        let after = client_to_image(anchor.x as f64 - o.0, anchor.y as f64 - o.1, 2.0);
        assert_eq!(before, after);
        // Zooming back restores the origin.
        assert_eq!(zoom_about(o, 2.0, 1.0, anchor), origin);
        // Anchor at the origin: origin does not move.
        assert_eq!(zoom_about(origin, 1.0, 3.0, Point::new(100, 200)), origin);
    }

    #[test]
    fn view_sizes() {
        assert_eq!(view_size(200, 100, 1.0), (200, 100));
        assert_eq!(view_size(200, 100, 0.5), (100, 50));
        assert_eq!(view_size(3, 3, 0.1), (1, 1));
    }

    #[test]
    fn edge_hits() {
        let e = hit_edges(Point::new(2, 50), 200, 100, 8);
        assert!(e.left && !e.top && !e.right && !e.bottom);
        let e = hit_edges(Point::new(199, 99), 200, 100, 8);
        assert!(e.right && e.bottom);
        assert!(!hit_edges(Point::new(100, 50), 200, 100, 8).any());
        assert!(!hit_edges(Point::new(500, 50), 200, 100, 8).any());
    }

    #[test]
    fn canvas_expand_and_shrink() {
        let left = Edges { left: true, ..Default::default() };
        let bottom = Edges { bottom: true, ..Default::default() };
        // Expand left by 10.
        assert_eq!(canvas_after_drag(100, 50, left, -10, 0), Rect::new(-10, 0, 110, 50));
        // Crop left by 30.
        assert_eq!(canvas_after_drag(100, 50, left, 30, 0), Rect::new(30, 0, 70, 50));
        // Cannot crop past the opposite edge.
        assert_eq!(canvas_after_drag(100, 50, left, 500, 0), Rect::new(99, 0, 1, 50));
        // Bottom: grow and shrink; dx is ignored.
        assert_eq!(canvas_after_drag(100, 50, bottom, 7, 20), Rect::new(0, 0, 100, 70));
        assert_eq!(canvas_after_drag(100, 50, bottom, 0, -80), Rect::new(0, 0, 100, 1));
        // Corner.
        let tr = Edges { top: true, right: true, ..Default::default() };
        assert_eq!(canvas_after_drag(100, 50, tr, 5, -5), Rect::new(0, -5, 105, 55));
    }

    #[test]
    fn canvas_apply_fills_and_crops() {
        let img = Image::filled(4, 4, [10, 20, 30, 255]);
        let white = [255, 255, 255, 255];
        let out = apply_canvas(&img, Rect::new(-2, 0, 6, 4), white);
        assert_eq!((out.width, out.height), (6, 4));
        assert_eq!(out.pixel(0, 0), white);
        assert_eq!(out.pixel(2, 0), [10, 20, 30, 255]);
        let out = apply_canvas(&img, Rect::new(1, 1, 2, 2), [0; 4]);
        assert_eq!((out.width, out.height), (2, 2));
        assert_eq!(out.pixel(1, 1), [10, 20, 30, 255]);
        let out = apply_canvas(&img, Rect::new(0, 0, 4, 6), [0; 4]);
        assert_eq!(out.pixel(0, 5), [0, 0, 0, 0]);
    }
}
