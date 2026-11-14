//! Zoom mapping and pixel rendering for the magnifier and the loupes (pure functions).
//!
//! Coordinates: a pixel `i` covers `[i, i + 1)`, its centre is `i + 0.5`. The output
//! centre `(w / 2, h / 2)` shows the centre of the source pixel `center`.

use crate::common::{Image, Point, Rect};

/// Magnifier zoom range.
pub const MIN_ZOOM: u32 = 2;
pub const MAX_ZOOM: u32 = 16;

/// Loupe (color picker, crosshair) zoom steps.
pub const LOUPE_ZOOMS: [u32; 9] = [4, 6, 8, 10, 12, 16, 20, 24, 32];

/// Continuous source coordinate shown at output pixel `o` (pixel centre) along one axis.
#[inline]
pub fn source_coord(o: i32, out_len: i32, center: i32, zoom: f64) -> f64 {
    center as f64 + 0.5 + (o as f64 + 0.5 - out_len as f64 / 2.0) / zoom
}

/// Inverse of [`source_coord`]: output coordinate of a continuous source coordinate.
#[inline]
pub fn output_coord(s: f64, out_len: i32, center: i32, zoom: f64) -> f64 {
    (s - center as f64 - 0.5) * zoom + out_len as f64 / 2.0
}

/// Source pixel shown at output pixel `(ox, oy)` without interpolation.
pub fn source_pixel(ox: i32, oy: i32, out: (i32, i32), center: Point, zoom: f64) -> Point {
    Point::new(
        source_coord(ox, out.0, center.x, zoom).floor() as i32,
        source_coord(oy, out.1, center.y, zoom).floor() as i32,
    )
}

/// The screen rectangle to grab so an `out` sized view at `zoom` around `center` can be
/// rendered with bilinear filtering (one pixel of margin on each side).
pub fn grab_rect(center: Point, out: (i32, i32), zoom: f64) -> Rect {
    let x0 = source_coord(0, out.0, center.x, zoom).floor() as i32 - 1;
    let y0 = source_coord(0, out.1, center.y, zoom).floor() as i32 - 1;
    let x1 = source_coord(out.0 - 1, out.0, center.x, zoom).floor() as i32 + 2;
    let y1 = source_coord(out.1 - 1, out.1, center.y, zoom).floor() as i32 + 2;
    Rect::from_ltrb(x0, y0, x1, y1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LensOptions {
    pub smooth: bool,
    pub negative: bool,
}

/// Render a zoomed view into `out` (BGRA, `w * h`, top-down, opaque).
/// `src` holds the screen pixels of `src_rect` (screen coordinates). Source pixels
/// outside `src` are clamped to its edge.
#[allow(clippy::too_many_arguments)]
pub fn magnify(
    src: &Image,
    src_rect: Rect,
    center: Point,
    zoom: f64,
    opts: LensOptions,
    out: &mut [u8],
    w: i32,
    h: i32,
) {
    if w <= 0 || h <= 0 || out.len() < (w * h * 4) as usize || src.is_empty() || zoom <= 0.0 {
        return;
    }
    let (sw, sh) = (src.width as i32, src.height as i32);
    let px = |x: i32, y: i32| -> [u8; 4] {
        let x = x.clamp(0, sw - 1) as u32;
        let y = y.clamp(0, sh - 1) as u32;
        src.pixel(x, y)
    };
    // Per-column source positions are the same for every row: precompute.
    let cols: Vec<(i32, i32, u32)> = (0..w)
        .map(|ox| {
            let s = source_coord(ox, w, center.x, zoom) - src_rect.x as f64;
            if opts.smooth {
                let f = s - 0.5;
                let x0 = f.floor();
                (x0 as i32, x0 as i32 + 1, ((f - x0) * 256.0) as u32)
            } else {
                let x = s.floor() as i32;
                (x, x, 0)
            }
        })
        .collect();
    for oy in 0..h {
        let s = source_coord(oy, h, center.y, zoom) - src_rect.y as f64;
        let (y0, y1, fy) = if opts.smooth {
            let f = s - 0.5;
            let y0 = f.floor();
            (y0 as i32, y0 as i32 + 1, ((f - y0) * 256.0) as u32)
        } else {
            let y = s.floor() as i32;
            (y, y, 0)
        };
        let row = &mut out[(oy * w * 4) as usize..((oy + 1) * w * 4) as usize];
        for (o, &(x0, x1, fx)) in row.chunks_exact_mut(4).zip(&cols) {
            let mut c = if fx == 0 && fy == 0 {
                px(x0, y0)
            } else {
                let (a, b, cc, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
                let mut r = [0u8; 4];
                for i in 0..3 {
                    let top = a[i] as u32 * (256 - fx) + b[i] as u32 * fx;
                    let bot = cc[i] as u32 * (256 - fx) + d[i] as u32 * fx;
                    r[i] = ((top * (256 - fy) + bot * fy + (1 << 15)) >> 16) as u8;
                }
                r
            };
            if opts.negative {
                c = [255 - c[0], 255 - c[1], 255 - c[2], 255];
            }
            c[3] = 255;
            o.copy_from_slice(&c);
        }
    }
}

/// Loupe grid geometry: an odd number of cells of `cell` pixels each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cells: i32,
    pub cell: i32,
}

impl Grid {
    /// Fit a grid of roughly `side` pixels for `zoom` (logical) at `dpi`.
    pub fn fit(side: i32, zoom: u32, dpi: u32) -> Grid {
        let cell = ((zoom as i64 * dpi as i64 + 48) / 96).max(2) as i32;
        let mut cells = (side / cell).max(3);
        if cells % 2 == 0 {
            cells -= 1;
        }
        Grid { cells, cell }
    }

    pub fn side(&self) -> i32 {
        self.cells * self.cell
    }

    pub fn half(&self) -> i32 {
        self.cells / 2
    }

    /// The screen rect the grid shows around `center`.
    pub fn source_rect(&self, center: Point) -> Rect {
        Rect::new(center.x - self.half(), center.y - self.half(), self.cells, self.cells)
    }

    /// Source pixel under grid-local point `(lx, ly)`, if inside the grid.
    pub fn source_at(&self, center: Point, lx: i32, ly: i32) -> Option<Point> {
        if lx < 0 || ly < 0 || lx >= self.side() || ly >= self.side() {
            return None;
        }
        Some(Point::new(center.x - self.half() + lx / self.cell, center.y - self.half() + ly / self.cell))
    }
}

/// Next/previous zoom step in [`LOUPE_ZOOMS`].
pub fn step_loupe_zoom(current: u32, up: bool) -> u32 {
    let i = LOUPE_ZOOMS.iter().position(|&z| z >= current).unwrap_or(LOUPE_ZOOMS.len() - 1);
    let j = if up { (i + 1).min(LOUPE_ZOOMS.len() - 1) } else { i.saturating_sub(1) };
    LOUPE_ZOOMS[j]
}

/// Next/previous magnifier zoom (2, 3, 4, 6, 8, 12, 16).
pub fn step_magnifier_zoom(current: u32, up: bool) -> u32 {
    const STEPS: [u32; 7] = [2, 3, 4, 6, 8, 12, 16];
    let i = STEPS.iter().position(|&z| z >= current).unwrap_or(STEPS.len() - 1);
    let j = if up { (i + 1).min(STEPS.len() - 1) } else { i.saturating_sub(1) };
    STEPS[j]
}

/// Render a loupe grid into `out` (BGRA, `side * side`, top-down). `src` holds the
/// pixels of `src_rect` (screen coordinates); pixels outside it draw a dark checkerboard.
/// The centre cell gets a two-tone frame.
pub fn render_grid(src: &Image, src_rect: Rect, center: Point, grid: Grid, out: &mut [u8]) {
    let side = grid.side();
    if side <= 0 || out.len() < (side * side * 4) as usize {
        return;
    }
    let half = grid.half();
    let cell = grid.cell;
    for cy in 0..grid.cells {
        for cx in 0..grid.cells {
            let sx = center.x - half + cx - src_rect.x;
            let sy = center.y - half + cy - src_rect.y;
            let base = if sx >= 0 && sy >= 0 && (sx as u32) < src.width && (sy as u32) < src.height {
                let p = src.pixel(sx as u32, sy as u32);
                [p[0], p[1], p[2], 255]
            } else if (cx + cy) % 2 == 0 {
                [40, 40, 40, 255]
            } else {
                [56, 56, 56, 255]
            };
            // Subtle grid: the last row and column of each cell are darkened a little.
            let k = 200u32;
            let grid_c = [
                (base[0] as u32 * k / 255) as u8,
                (base[1] as u32 * k / 255) as u8,
                (base[2] as u32 * k / 255) as u8,
                255,
            ];
            for py in 0..cell {
                let row = (cy * cell + py) as usize;
                for px in 0..cell {
                    let col = (cx * cell + px) as usize;
                    let c = if cell >= 6 && (px == cell - 1 || py == cell - 1) { grid_c } else { base };
                    let i = (row * side as usize + col) * 4;
                    out[i..i + 4].copy_from_slice(&c);
                }
            }
        }
    }
    let a = half * cell - 1;
    let b = (half + 1) * cell - 1;
    frame(out, side, a - 1, b + 1, [0, 0, 0, 255]);
    frame(out, side, a, b, [255, 255, 255, 255]);
}

fn frame(out: &mut [u8], side: i32, a: i32, b: i32, c: [u8; 4]) {
    let mut put = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && x < side && y < side {
            let i = ((y * side + x) * 4) as usize;
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
    fn centre_maps_to_centre_pixel() {
        for zoom in [2.0, 3.0, 4.0, 7.5, 16.0] {
            for (w, h) in [(200, 200), (201, 151), (64, 300)] {
                let c = Point::new(500, -300);
                assert_eq!(source_pixel(w / 2, h / 2, (w, h), c, zoom), c, "zoom {zoom} {w}x{h}");
                // Output -> source -> output is the identity on pixel centres.
                let s = source_coord(17, w, c.x, zoom);
                assert!((output_coord(s, w, c.x, zoom) - 17.5).abs() < 1e-9);
            }
        }
        // At 4x on a 200 px view, the centre pixel covers output 98..=101.
        let c = Point::new(10, 10);
        let sx = |ox: i32| source_pixel(ox, 100, (200, 200), c, 4.0).x;
        assert_eq!([sx(97), sx(98), sx(101), sx(102)], [9, 10, 10, 11]);
    }

    #[test]
    fn grab_rect_covers_every_sample() {
        for zoom in [2.0, 3.0, 5.0, 16.0] {
            let c = Point::new(-20, 33);
            let out = (123, 77);
            let r = grab_rect(c, out, zoom);
            for ox in [0, out.0 / 2, out.0 - 1] {
                for oy in [0, out.1 / 2, out.1 - 1] {
                    let s = (source_coord(ox, out.0, c.x, zoom), source_coord(oy, out.1, c.y, zoom));
                    // Bilinear reads floor(s - 0.5) and the next pixel.
                    let p = Point::new((s.0 - 0.5).floor() as i32, (s.1 - 0.5).floor() as i32);
                    assert!(r.contains(p) && r.contains(Point::new(p.x + 1, p.y + 1)), "{p:?} outside {r:?}");
                }
            }
        }
    }

    fn checker() -> (Image, Rect) {
        // 8x8 source at screen (100, 200): black/white checkerboard of single pixels.
        let mut img = Image::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        (img, Rect::new(100, 200, 8, 8))
    }

    #[test]
    fn pixelated_magnify_replicates_pixels() {
        let (img, r) = checker();
        let (w, h) = (16, 16);
        let mut out = vec![0u8; (w * h * 4) as usize];
        let c = Point::new(104, 204);
        magnify(&img, r, c, 4.0, LensOptions { smooth: false, negative: false }, &mut out, w, h);
        let at = |x: i32, y: i32| out[((y * w + x) * 4) as usize];
        // Centre pixel (104,204) is white ((4+4) even); the view centre (8.0) is its
        // centre, so it covers outputs 6..=9.
        for x in 6..10 {
            for y in 6..10 {
                assert_eq!(at(x, y), 255);
            }
        }
        assert_eq!(at(5, 6), 0);
        assert_eq!(at(10, 6), 0);
        // Negative inverts.
        magnify(&img, r, c, 4.0, LensOptions { smooth: false, negative: true }, &mut out, w, h);
        assert_eq!(out[((7 * w + 7) * 4) as usize], 0);
        assert_eq!(out[((7 * w + 7) * 4 + 3) as usize], 255, "opaque");
    }

    #[test]
    fn smooth_magnify_interpolates() {
        let (img, r) = checker();
        let (w, h) = (16, 16);
        let mut out = vec![0u8; (w * h * 4) as usize];
        magnify(&img, r, Point::new(104, 204), 4.0, LensOptions { smooth: true, negative: false }, &mut out, w, h);
        let at = |x: i32, y: i32| out[((y * w + x) * 4) as usize];
        // Between pixel centres the value is a blend, not pure black or white.
        let edge = at(8, 9);
        assert!(edge > 20 && edge < 235, "{edge}");
        // Uniform source stays uniform.
        let flat = Image::filled(8, 8, [10, 20, 30, 255]);
        magnify(&flat, r, Point::new(104, 204), 3.0, LensOptions { smooth: true, negative: false }, &mut out, w, h);
        assert!(out.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    }

    #[test]
    fn grid_fit_and_lookup() {
        let g = Grid::fit(160, 10, 96);
        assert_eq!(g, Grid { cells: 15, cell: 10 });
        assert_eq!(g.side(), 150);
        let g2 = Grid::fit(160, 8, 96);
        assert_eq!(g2.cells % 2, 1);
        assert_eq!(Grid::fit(160, 10, 192).cell, 20);
        let c = Point::new(50, 60);
        assert_eq!(g.source_at(c, 75, 75), Some(c));
        assert_eq!(g.source_at(c, 0, 0), Some(Point::new(43, 53)));
        assert_eq!(g.source_at(c, 149, 149), Some(Point::new(57, 67)));
        assert_eq!(g.source_at(c, 150, 0), None);
        assert_eq!(g.source_rect(c), Rect::new(43, 53, 15, 15));
    }

    #[test]
    fn grid_render_shows_centre_pixel() {
        let mut img = Image::filled(15, 15, [1, 2, 3, 255]);
        img.set_pixel(7, 7, [200, 100, 50, 255]);
        let g = Grid { cells: 15, cell: 8 };
        let mut out = vec![0u8; (g.side() * g.side() * 4) as usize];
        let c = Point::new(1007, 2007);
        render_grid(&img, Rect::new(1000, 2000, 15, 15), c, g, &mut out);
        let mid = (g.side() / 2) as usize;
        let i = (mid * g.side() as usize + mid) * 4;
        assert_eq!(&out[i..i + 4], &[200, 100, 50, 255]);
        assert_eq!(&out[(2 * g.side() as usize + 2) * 4..][..4], &[1, 2, 3, 255]);
    }

    #[test]
    fn zoom_steps() {
        assert_eq!(step_loupe_zoom(10, true), 12);
        assert_eq!(step_loupe_zoom(10, false), 8);
        assert_eq!(step_loupe_zoom(32, true), 32);
        assert_eq!(step_loupe_zoom(4, false), 4);
        assert_eq!(step_loupe_zoom(9, true), 12);
        assert_eq!(step_magnifier_zoom(2, false), 2);
        assert_eq!(step_magnifier_zoom(4, true), 6);
        assert_eq!(step_magnifier_zoom(16, true), 16);
        assert_eq!(step_magnifier_zoom(5, false), 4);
    }
}
