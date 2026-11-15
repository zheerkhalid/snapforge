//! Post-processing for scanned pages: deskew (projection-profile angle search), automatic
//! contrast / gamma, and cropping of white borders. Pure functions on [`Image`].

use crate::common::{Image, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PostOptions {
    pub deskew: bool,
    pub auto_contrast: bool,
    pub auto_crop: bool,
}

/// Luminance with partially transparent pixels composited over white.
#[inline]
fn lum(px: &[u8]) -> u8 {
    let w = 255 - px[3] as u32;
    let b = (px[0] as u32 + w).min(255);
    let g = (px[1] as u32 + w).min(255);
    let r = (px[2] as u32 + w).min(255);
    ((r * 54 + g * 183 + b * 19) >> 8) as u8
}

/// Longest side of the image used for the skew search.
const SEARCH_MAX_SIDE: u32 = 1000;
/// Pixels darker than this count as ink for the skew search.
const INK_THRESHOLD: u8 = 128;
/// Cap on the number of ink points scored per angle.
const MAX_POINTS: usize = 200_000;

/// Ink points (centred coordinates) of a downsampled copy: a block is ink if its darkest
/// pixel is dark, so thin strokes survive the downsampling.
fn ink_points(img: &Image) -> Vec<(f32, f32)> {
    let (w, h) = (img.width, img.height);
    let f = w.max(h).div_ceil(SEARCH_MAX_SIDE).max(1);
    let (bw, bh) = (w.div_ceil(f), h.div_ceil(f));
    let mut mins = vec![255u8; bw as usize * bh as usize];
    for y in 0..h {
        let row = img.row(y);
        let base = (y / f) as usize * bw as usize;
        for (x, px) in row.chunks_exact(4).enumerate() {
            let l = lum(px);
            let m = &mut mins[base + x / f as usize];
            if l < *m {
                *m = l;
            }
        }
    }
    let (cx, cy) = (bw as f32 / 2.0, bh as f32 / 2.0);
    let mut pts = Vec::new();
    for (i, &m) in mins.iter().enumerate() {
        if m < INK_THRESHOLD {
            let x = (i % bw as usize) as f32;
            let y = (i / bw as usize) as f32;
            pts.push((x - cx, y - cy));
        }
    }
    if pts.len() > MAX_POINTS {
        let stride = pts.len().div_ceil(MAX_POINTS);
        pts = pts.into_iter().step_by(stride).collect();
    }
    pts
}

/// Sum of squared bin counts of the horizontal projection after undoing a rotation of
/// `deg`. Peaks when text lines become horizontal.
fn profile_score(pts: &[(f32, f32)], deg: f32, bins: &mut [u32], half: f32) -> f64 {
    let (s, c) = deg.to_radians().sin_cos();
    bins.iter_mut().for_each(|b| *b = 0);
    for &(x, y) in pts {
        let yr = y * c - x * s + half;
        let i = yr as isize;
        if i >= 0 && (i as usize) < bins.len() {
            bins[i as usize] += 1;
        }
    }
    bins.iter().map(|&b| (b as f64) * (b as f64)).sum()
}

/// Skew angle in degrees (positive = content rotated clockwise), searched in
/// `[-max_deg, max_deg]` via projection-profile variance on dark pixels. 0.0 if no content.
pub fn detect_skew_degrees(img: &Image, max_deg: f32) -> f32 {
    if img.is_empty() || !max_deg.is_finite() || max_deg <= 0.0 {
        return 0.0;
    }
    let pts = ink_points(img);
    if pts.len() < 8 {
        return 0.0;
    }
    let extent = pts.iter().fold(0.0f32, |m, &(x, y)| m.max(x.abs()).max(y.abs()));
    let half = extent * std::f32::consts::SQRT_2 + 2.0;
    let mut bins = vec![0u32; (half * 2.0) as usize + 2];
    let search = |from: f32, to: f32, step: f32, bins: &mut [u32]| -> f32 {
        let n = ((to - from) / step).round() as i32;
        let mut best = (f64::MIN, 0.0f32);
        for k in 0..=n {
            let a = from + k as f32 * step;
            let s = profile_score(&pts, a, bins, half);
            // Prefer the angle closest to 0 on ties (flat profiles, e.g. no lines at all).
            if s > best.0 || (s == best.0 && a.abs() < best.1.abs()) {
                best = (s, a);
            }
        }
        best.1
    };
    let coarse = search(-max_deg, max_deg, 0.5, &mut bins);
    let lo = (coarse - 0.5).max(-max_deg);
    let hi = (coarse + 0.5).min(max_deg);
    let fine = search(lo, hi, 0.05, &mut bins);
    if fine.abs() < 1e-4 {
        0.0
    } else {
        fine
    }
}

/// Rotate by `deg` (positive = clockwise) around the centre, bilinear, same output size;
/// the uncovered area is filled with `fill` (premultiplied BGRA).
pub fn rotate_bilinear(img: &Image, deg: f32, fill: [u8; 4]) -> Image {
    if img.is_empty() || !deg.is_finite() || deg.rem_euclid(360.0) == 0.0 {
        return img.clone();
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let fetch = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            fill.map(|v| v as f32)
        } else {
            let i = (y as usize * w + x as usize) * 4;
            [img.data[i] as f32, img.data[i + 1] as f32, img.data[i + 2] as f32, img.data[i + 3] as f32]
        }
    };
    let render = |rows: &mut [u8], first_row: usize| {
        for (r, out_row) in rows.chunks_exact_mut(w * 4).enumerate() {
            let dy = (first_row + r) as f32 - cy;
            for (dx, out) in out_row.chunks_exact_mut(4).enumerate() {
                let dx = dx as f32 - cx;
                // Inverse mapping: rotate the destination point back by -deg.
                let sx = dx * c + dy * s + cx;
                let sy = -dx * s + dy * c + cy;
                if sx <= -1.0 || sy <= -1.0 || sx >= w as f32 || sy >= h as f32 {
                    out.copy_from_slice(&fill);
                    continue;
                }
                let (x0, y0) = (sx.floor(), sy.floor());
                let (fx, fy) = (sx - x0, sy - y0);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let p00 = fetch(x0, y0);
                let p10 = fetch(x0 + 1, y0);
                let p01 = fetch(x0, y0 + 1);
                let p11 = fetch(x0 + 1, y0 + 1);
                for ch in 0..4 {
                    let top = p00[ch] + (p10[ch] - p00[ch]) * fx;
                    let bot = p01[ch] + (p11[ch] - p01[ch]) * fx;
                    out[ch] = (top + (bot - top) * fy + 0.5).clamp(0.0, 255.0) as u8;
                }
                // Premultiplied invariant: colour never exceeds alpha.
                let a = out[3];
                for v in &mut out[..3] {
                    *v = (*v).min(a);
                }
            }
        }
    };
    let mut data = vec![0u8; w * h * 4];
    let threads =
        if w * h >= 1 << 18 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8) } else { 1 };
    if threads <= 1 || h < threads {
        render(&mut data, 0);
    } else {
        let rows_per = h.div_ceil(threads);
        std::thread::scope(|sc| {
            for (n, chunk) in data.chunks_mut(rows_per * w * 4).enumerate() {
                let render = &render;
                sc.spawn(move || render(chunk, n * rows_per));
            }
        });
    }
    Image::from_bgra(img.width, img.height, data)
}

/// Detect the skew (up to 15°) and rotate it back with a white fill. Returns a clone when
/// the page is already straight (|angle| < 0.05°).
pub fn deskew(img: &Image) -> Image {
    let a = detect_skew_degrees(img, 15.0);
    if a.abs() < 0.05 {
        return img.clone();
    }
    rotate_bilinear(img, -a, [255, 255, 255, 255])
}

/// Levels stretch from the 0.5% / 99.5% luminance percentiles, then a gamma (0.5..2.0)
/// that moves the median luminance toward mid-grey. Alpha is preserved.
pub fn auto_contrast(img: &Image) -> Image {
    let mut hist = [0u64; 256];
    let mut n = 0u64;
    for px in img.data.chunks_exact(4) {
        let a = px[3] as u32;
        if a == 0 {
            continue;
        }
        let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255);
        let l = (un(px[2]) * 54 + un(px[1]) * 183 + un(px[0]) * 19) >> 8;
        hist[l as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return img.clone();
    }
    let percentile = |p: f64| -> usize {
        let target = ((n as f64) * p).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (i, &c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i;
            }
        }
        255
    };
    let lo = percentile(0.005) as f32;
    let hi = percentile(0.995) as f32;
    if hi - lo < 1.0 {
        return img.clone();
    }
    let median = percentile(0.5) as f32;
    let m = ((median - lo) / (hi - lo)).clamp(0.0, 1.0);
    let gamma = if m > 0.0 && m < 1.0 { (0.5f32.ln() / m.ln()).clamp(0.5, 2.0) } else { 1.0 };
    let mut lut = [0u8; 256];
    for (v, out) in lut.iter_mut().enumerate() {
        let t = ((v as f32 - lo) / (hi - lo)).clamp(0.0, 1.0);
        *out = (t.powf(gamma) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    let mut out = img.clone();
    for px in out.data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        match a {
            0 => {}
            255 => {
                for c in &mut px[..3] {
                    *c = lut[*c as usize];
                }
            }
            _ => {
                for c in &mut px[..3] {
                    let un = ((*c as u32 * 255 + a / 2) / a).min(255) as usize;
                    *c = ((lut[un] as u32 * a + 127) / 255) as u8;
                }
            }
        }
    }
    out
}

/// Bounding box of content that is not near-white (luminance < `threshold`). Isolated
/// noise is ignored: a row / column is content only if more than max(1, 0.2% of its
/// length) of its pixels are dark. The full bounds if nothing is found.
pub fn content_bounds(img: &Image, threshold: u8) -> Rect {
    let full = img.bounds();
    if img.is_empty() {
        return full;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let mut rows = vec![0u32; h];
    let mut cols = vec![0u32; w];
    for (y, row) in rows.iter_mut().enumerate() {
        for (x, px) in img.row(y as u32).chunks_exact(4).enumerate() {
            if lum(px) < threshold {
                *row += 1;
                cols[x] += 1;
            }
        }
    }
    let row_min = (w as f64 * 0.002).max(1.0);
    let col_min = (h as f64 * 0.002).max(1.0);
    let first_last = |v: &[u32], min: f64| {
        let first = v.iter().position(|&c| c as f64 > min)?;
        let last = v.iter().rposition(|&c| c as f64 > min)?;
        Some((first as i32, last as i32))
    };
    match (first_last(&rows, row_min), first_last(&cols, col_min)) {
        (Some((t, b)), Some((l, r))) => Rect::from_ltrb(l, t, r + 1, b + 1),
        _ => full,
    }
}

/// Crop to [`content_bounds`] (threshold 235) plus a 4 px margin, clipped to the image.
pub fn auto_crop_white(img: &Image) -> Image {
    let r = content_bounds(img, 235).inflate(4).intersect(&img.bounds());
    if r == img.bounds() || r.is_empty() {
        return img.clone();
    }
    img.crop(r)
}

/// Deskew, then auto-crop, then auto-contrast, as enabled.
pub fn post_process(img: Image, o: &PostOptions) -> Image {
    let mut img = img;
    if o.deskew {
        img = deskew(&img);
    }
    if o.auto_crop {
        img = auto_crop_white(&img);
    }
    if o.auto_contrast {
        img = auto_contrast(&img);
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    fn fill_rect(img: &mut Image, r: Rect, c: [u8; 4]) {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                img.set_pixel(x as u32, y as u32, c);
            }
        }
    }

    /// A white page with text-like horizontal bars.
    fn page(w: u32, h: u32) -> Image {
        let mut img = Image::filled(w, h, WHITE);
        let mut y = h as i32 / 8;
        while y < h as i32 * 7 / 8 {
            fill_rect(&mut img, Rect::new(w as i32 / 8, y, w as i32 * 3 / 4, (h / 60).max(3) as i32), BLACK);
            y += (h / 20).max(8) as i32;
        }
        img
    }

    #[test]
    fn detects_skew_both_directions() {
        let p = page(900, 1200);
        assert!(detect_skew_degrees(&p, 15.0).abs() < 0.1);
        for angle in [3.0f32, -2.0] {
            let rotated = rotate_bilinear(&p, angle, WHITE);
            let found = detect_skew_degrees(&rotated, 15.0);
            assert!((found - angle).abs() <= 0.3, "rotated {angle}, found {found}");
            let fixed = deskew(&rotated);
            let residual = detect_skew_degrees(&fixed, 15.0);
            assert!(residual.abs() <= 0.3, "residual {residual} after deskewing {angle}");
        }
    }

    #[test]
    fn detects_skew_on_large_page() {
        let p = page(2500, 3500);
        let rotated = rotate_bilinear(&p, 1.5, WHITE);
        let found = detect_skew_degrees(&rotated, 15.0);
        assert!((found - 1.5).abs() <= 0.3, "found {found}");
    }

    #[test]
    fn rotate_zero_is_identity() {
        let p = page(64, 48);
        assert_eq!(rotate_bilinear(&p, 0.0, WHITE), p);
        assert_eq!(rotate_bilinear(&p, 360.0, WHITE), p);
    }

    #[test]
    fn rotate_keeps_premultiplied_invariant_and_fills() {
        let img = Image::filled(40, 30, [10, 20, 30, 128]);
        let r = rotate_bilinear(&img, 30.0, [0, 0, 0, 0]);
        assert!(r.data.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        // A corner is outside the rotated source.
        assert_eq!(r.pixel(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn auto_crop_ignores_noise() {
        let mut img = Image::filled(300, 200, WHITE);
        fill_rect(&mut img, Rect::new(80, 50, 100, 60), BLACK);
        // Isolated noise specks in the border.
        img.set_pixel(5, 5, BLACK);
        img.set_pixel(290, 190, BLACK);
        img.set_pixel(20, 150, [40, 40, 40, 255]);
        assert_eq!(content_bounds(&img, 235), Rect::new(80, 50, 100, 60));
        let c = auto_crop_white(&img);
        assert_eq!((c.width, c.height), (108, 68));
        assert_eq!(c.pixel(4, 4), BLACK);
        assert_eq!(c.pixel(0, 0), WHITE);
    }

    #[test]
    fn auto_contrast_stretches_low_contrast() {
        let (w, h) = (256u32, 16u32);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = (100 + x * 50 / (w - 1)) as u8;
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let out = auto_contrast(&img);
        let (mut lo, mut hi) = (255u8, 0u8);
        for px in out.data.chunks_exact(4) {
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
            assert_eq!(px[3], 255);
        }
        assert!(lo <= 5, "lo {lo}");
        assert!(hi >= 250, "hi {hi}");
        // Monotonic along the gradient.
        let row: Vec<u8> = (0..w).map(|x| out.pixel(x, 0)[0]).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn degenerate_images_do_not_panic() {
        for img in [Image::new(0, 0), Image::new(0, 5), Image::filled(1, 1, BLACK), Image::filled(1, 1, WHITE)] {
            let _ = detect_skew_degrees(&img, 15.0);
            let _ = rotate_bilinear(&img, 7.0, WHITE);
            let _ = auto_contrast(&img);
            let _ = auto_crop_white(&img);
            let _ = post_process(img.clone(), &PostOptions { deskew: true, auto_contrast: true, auto_crop: true });
        }
        let white = Image::filled(50, 40, WHITE);
        assert_eq!(detect_skew_degrees(&white, 15.0), 0.0);
        assert_eq!(content_bounds(&white, 235), white.bounds());
        assert_eq!(auto_crop_white(&white), white);
        assert_eq!(auto_contrast(&white), white);
        let transparent = Image::new(10, 10);
        assert_eq!(auto_contrast(&transparent), transparent);
    }
}
