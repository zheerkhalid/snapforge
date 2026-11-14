//! Magnifier loupe: placement next to the cursor and the zoomed pixel grid.

use crate::common::{Image, Point, Rect};

/// Source pixels shown per side (odd, so there is a centre pixel).
pub const CELLS: i32 = 15;
/// Zoom factor at 96 DPI.
pub const ZOOM: i32 = 8;

/// Zoom cell size in screen pixels for a monitor DPI.
pub fn cell_px(dpi: u32) -> i32 {
    ((ZOOM as i64 * dpi as i64 + 48) / 96).max(ZOOM as i64) as i32
}

/// Where to put a loupe of `size` for `cursor` on monitor `mon`: below-right of the
/// cursor by default, flipped left/up when it would leave the monitor, then clamped.
pub fn place(cursor: Point, size: (i32, i32), gap: i32, mon: &Rect) -> Rect {
    let (w, h) = size;
    let mut x = cursor.x + gap;
    let mut y = cursor.y + gap;
    if x + w > mon.right() {
        x = cursor.x - gap - w;
    }
    if y + h > mon.bottom() {
        y = cursor.y - gap - h;
    }
    Rect::new(x, y, w, h).clamp_inside(mon)
}

/// Borrowed BGRA source pixels (tightly packed, top-down).
#[derive(Clone, Copy)]
pub struct Src<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
}

impl<'a> Src<'a> {
    pub fn of(img: &'a Image) -> Self {
        Self { data: &img.data, width: img.width, height: img.height }
    }

    pub fn pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.data.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// Render the zoomed grid into `out` (BGRA, `side * side`, top-down) where
/// `side = CELLS * cell`. `center` is in image coordinates. Pixels outside the image
/// show a dark checkerboard.
pub fn render(img: Src, center: Point, cell: i32, out: &mut [u8]) {
    let side = (CELLS * cell).max(0) as usize;
    if out.len() < side * side * 4 || cell <= 0 {
        return;
    }
    let half = CELLS / 2;
    for cy in 0..CELLS {
        for cx in 0..CELLS {
            let sx = center.x - half + cx;
            let sy = center.y - half + cy;
            let base = if let Some(p) = img.pixel(sx, sy) {
                [p[0], p[1], p[2], 255]
            } else if (cx + cy) % 2 == 0 {
                [40, 40, 40, 255]
            } else {
                [56, 56, 56, 255]
            };
            // Grid: the last row/column of each cell is darkened a little.
            let grid = darken(base, 0.22);
            for py in 0..cell {
                let row = (cy * cell + py) as usize;
                for px in 0..cell {
                    let col = (cx * cell + px) as usize;
                    let c = if px == cell - 1 || py == cell - 1 { grid } else { base };
                    let i = (row * side + col) * 4;
                    out[i..i + 4].copy_from_slice(&c);
                }
            }
        }
    }
    // Centre marker: a white frame with a dark outline around the centre cell.
    let x0 = half * cell - 1;
    let x1 = (half + 1) * cell - 1;
    frame(out, side, x0 - 1, x1 + 1, [0, 0, 0, 255]);
    frame(out, side, x0, x1, [255, 255, 255, 255]);
}

fn darken(c: [u8; 4], amount: f32) -> [u8; 4] {
    let k = 1.0 - amount;
    [(c[0] as f32 * k) as u8, (c[1] as f32 * k) as u8, (c[2] as f32 * k) as u8, 255]
}

/// 1-px square outline from (a, a) to (b, b) inclusive.
fn frame(out: &mut [u8], side: usize, a: i32, b: i32, c: [u8; 4]) {
    let mut put = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && (x as usize) < side && (y as usize) < side {
            let i = (y as usize * side + x as usize) * 4;
            out[i..i + 4].copy_from_slice(&c);
        }
    };
    for t in a..=b {
        put(t, a);
        put(t, b);
        put(a, t);
        put(b, t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_stays_on_screen() {
        let mon = Rect::new(0, 0, 1920, 1080);
        let size = (140, 200);
        // Default: below-right.
        assert_eq!(place(Point::new(100, 100), size, 20, &mon), Rect::new(120, 120, 140, 200));
        // Near the right/bottom edge: flipped.
        assert_eq!(place(Point::new(1900, 1070), size, 20, &mon), Rect::new(1740, 850, 140, 200));
        // Every cursor position on a grid keeps the loupe fully inside the monitor.
        for m in [mon, Rect::new(-1280, -200, 1280, 1024), Rect::new(0, 0, 160, 150)] {
            for x in (m.x..m.right()).step_by(37) {
                for y in (m.y..m.bottom()).step_by(29) {
                    let r = place(Point::new(x, y), size, 20, &m);
                    assert_eq!(r.intersect(&m), r.clamp_inside(&m));
                    assert!(r.x >= m.x && r.y >= m.y && r.right() <= m.right() && r.bottom() <= m.bottom());
                }
            }
        }
    }

    #[test]
    fn zoom_scales_with_dpi() {
        assert_eq!(cell_px(96), 8);
        assert_eq!(cell_px(144), 12);
        assert_eq!(cell_px(192), 16);
        assert_eq!(cell_px(72), 8);
    }

    #[test]
    fn render_magnifies_centre_pixel() {
        let mut img = Image::filled(31, 31, [10, 20, 30, 255]);
        img.set_pixel(15, 15, [200, 100, 50, 255]);
        let cell = 8;
        let side = (CELLS * cell) as usize;
        let mut out = vec![0u8; side * side * 4];
        render(Src::of(&img), Point::new(15, 15), cell, &mut out);
        let at = |out: &[u8], x: usize, y: usize| {
            let i = (y * side + x) * 4;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        let mid = side / 2;
        assert_eq!(at(&out, mid, mid), [200, 100, 50, 255]);
        assert_eq!(at(&out, 2, 2), [10, 20, 30, 255]);
        // Off-image source pixels are drawn (checkerboard), not left transparent.
        render(Src::of(&img), Point::new(0, 0), cell, &mut out);
        assert_eq!(at(&out, 1, 1)[3], 255);
    }
}
