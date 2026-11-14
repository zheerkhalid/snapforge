//! Selection helpers: rectangle, ellipse and polygon masks, statistics, move/copy, masked apply.

use super::{lerp_px, par_rows, Color, Mask};
use crate::common::{Image, Point, Rect};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Hard-edged rectangle mask (clipped to the image).
pub fn rect_mask(width: u32, height: u32, rect: Rect) -> Mask {
    let mut m = Mask::new(width, height);
    let r = rect.intersect(&Rect::new(0, 0, width as i32, height as i32));
    for y in r.y..r.bottom() {
        let row = y as usize * width as usize;
        m.data[row + r.x as usize..row + r.right() as usize].fill(255);
    }
    m
}

/// Anti-aliased ellipse inscribed in `rect`.
pub fn ellipse_mask(width: u32, height: u32, rect: Rect) -> Mask {
    let mut m = Mask::new(width, height);
    if rect.is_empty() {
        return m;
    }
    let (cx, cy) = (rect.x as f32 + rect.w as f32 / 2.0, rect.y as f32 + rect.h as f32 / 2.0);
    let (rx, ry) = (rect.w as f32 / 2.0, rect.h as f32 / 2.0);
    let w = width as usize;
    par_rows(&mut m.data, w.max(1), |y0, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let py = (y0 + r) as f32 + 0.5 - cy;
            if py.abs() > ry + 1.0 {
                continue;
            }
            for (x, v) in row.iter_mut().enumerate() {
                let px = x as f32 + 0.5 - cx;
                // Approximate signed distance to the ellipse edge in pixels.
                // g = k - 1 with k the normalised radius; distance ~ g / |grad g|.
                let k = ((px / rx).powi(2) + (py / ry).powi(2)).sqrt();
                if k < 0.5 {
                    *v = 255;
                    continue;
                }
                let grad = ((px / (rx * rx)).powi(2) + (py / (ry * ry)).powi(2)).sqrt().max(1e-9);
                let d = k * (k - 1.0) / grad;
                *v = ((0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    });
    m
}

/// Anti-aliased polygon (freehand lasso), even-odd rule. Points are pixel coordinates;
/// each point is taken as the centre of its pixel.
pub fn polygon_mask(width: u32, height: u32, points: &[Point]) -> Mask {
    let pts: Vec<(f32, f32)> = points.iter().map(|p| (p.x as f32 + 0.5, p.y as f32 + 0.5)).collect();
    polygon_mask_f(width, height, &pts)
}

/// [`polygon_mask`] with sub-pixel coordinates (pixel corners at integers).
pub fn polygon_mask_f(width: u32, height: u32, pts: &[(f32, f32)]) -> Mask {
    const SS: usize = 4;
    let mut m = Mask::new(width, height);
    if pts.len() < 3 || width == 0 {
        return m;
    }
    let w = width as usize;
    let edges: Vec<((f32, f32), (f32, f32))> =
        (0..pts.len()).map(|i| (pts[i], pts[(i + 1) % pts.len()])).filter(|(a, b)| a.1 != b.1).collect();
    let ymin = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let ymax = (pts.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(height as usize);
    par_rows(&mut m.data, w, |y0, band| {
        let mut acc = vec![0f32; w];
        let mut xs: Vec<f32> = Vec::new();
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let y = y0 + r;
            if y < ymin || y >= ymax {
                continue;
            }
            acc.fill(0.0);
            for s in 0..SS {
                let sy = y as f32 + (s as f32 + 0.5) / SS as f32;
                xs.clear();
                for &((x0, y0e), (x1, y1e)) in &edges {
                    if (sy >= y0e && sy < y1e) || (sy >= y1e && sy < y0e) {
                        xs.push(x0 + (sy - y0e) * (x1 - x0) / (y1e - y0e));
                    }
                }
                xs.sort_by(|a, b| a.total_cmp(b));
                for pair in xs.chunks_exact(2) {
                    add_span(&mut acc, pair[0], pair[1], 1.0 / SS as f32);
                }
            }
            for (v, a) in row.iter_mut().zip(&acc) {
                *v = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    });
    m
}

fn add_span(acc: &mut [f32], a: f32, b: f32, wt: f32) {
    let w = acc.len() as f32;
    let (a, b) = (a.clamp(0.0, w), b.clamp(0.0, w));
    if b <= a {
        return;
    }
    let (ia, ib) = (a.floor() as usize, b.floor() as usize);
    if ia == ib {
        acc[ia] += (b - a) * wt;
        return;
    }
    acc[ia] += (ia as f32 + 1.0 - a) * wt;
    for v in &mut acc[ia + 1..ib] {
        *v += wt;
    }
    if ib < acc.len() {
        acc[ib] += (b - ib as f32) * wt;
    }
}

/// Soften a mask's edge (Gaussian, `radius` as in [`super::BlurParams`]).
pub fn feather_mask(mask: &Mask, radius: f32) -> Mask {
    let data = super::filter::gaussian_blur_raw(&mask.data, mask.width as usize, mask.height as usize, 1, radius / 2.0);
    Mask { width: mask.width, height: mask.height, data }
}

/// Bounding box of the selected (non-zero) pixels; empty if nothing is selected.
pub fn mask_bounds(mask: &Mask) -> Rect {
    let w = mask.width as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, row) in mask.data.chunks_exact(w.max(1)).enumerate() {
        if let Some(first) = row.iter().position(|&v| v > 0) {
            let last = row.iter().rposition(|&v| v > 0).unwrap_or(first);
            x0 = x0.min(first);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y + 1;
        }
    }
    if x0 == usize::MAX {
        return Rect::default();
    }
    Rect::from_ltrb(x0 as i32, y0 as i32, x1 as i32, y1 as i32)
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SelectionStats {
    /// Pixels at least half selected.
    pub pixel_count: u64,
    pub unique_colors: usize,
    pub bounds: Rect,
}

pub fn selection_stats(img: &Image, mask: &Mask) -> SelectionStats {
    let mut count = 0u64;
    let mut colors: HashSet<u32> = HashSet::new();
    for (px, &m) in img.data.chunks_exact(4).zip(&mask.data) {
        if m >= 128 {
            count += 1;
            colors.insert(u32::from_le_bytes([px[0], px[1], px[2], px[3]]));
        }
    }
    SelectionStats { pixel_count: count, unique_colors: colors.len(), bounds: mask_bounds(mask) }
}

/// The selected pixels (multiplied by the mask), cropped to the selection's bounds,
/// and where they came from.
pub fn extract_selection(img: &Image, mask: &Mask) -> (Image, Rect) {
    let b = mask_bounds(mask);
    let mut piece = img.crop(b);
    for y in 0..b.h as u32 {
        for x in 0..b.w as u32 {
            let m = mask.get(b.x as u32 + x, b.y as u32 + y) as u32;
            let p = piece.pixel(x, y);
            piece.set_pixel(x, y, super::scale_px(p, m));
        }
    }
    (piece, b)
}

/// Composite `piece` over a copy of `img` at (x, y).
pub fn paste(img: &Image, piece: &Image, x: i32, y: i32) -> Image {
    let mut out = img.clone();
    out.draw_over(piece, x, y);
    out
}

/// Move or copy a selection.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MoveParams {
    pub dx: i32,
    pub dy: i32,
    /// Keep the original pixels (copy) instead of clearing them (move).
    pub copy: bool,
    /// What a move leaves behind; transparent by default.
    pub fill: Color,
}

pub fn move_selection(img: &Image, mask: &Mask, p: &MoveParams) -> Image {
    let (piece, b) = extract_selection(img, mask);
    let mut out = img.clone();
    if !p.copy {
        let fill = p.fill.premul_bgra();
        for (px, &m) in out.data.chunks_exact_mut(4).zip(&mask.data) {
            if m > 0 {
                let o = lerp_px([px[0], px[1], px[2], px[3]], fill, m as u32);
                px.copy_from_slice(&o);
            }
        }
    }
    out.draw_over(&piece, b.x + p.dx, b.y + p.dy);
    out
}

/// Blend `effected` into `base` where the mask is set (both the same size).
/// Use it to apply any whole-image effect to a selection only.
pub fn apply_masked(base: &Image, effected: &Image, mask: &Mask) -> Image {
    if base.width != effected.width || base.height != effected.height || mask.data.len() != base.len_px() {
        log::warn!("apply_masked: size mismatch, effect not applied");
        return base.clone();
    }
    let mut out = base.clone();
    for ((px, e), &m) in out.data.chunks_exact_mut(4).zip(effected.data.chunks_exact(4)).zip(&mask.data) {
        if m == 0 {
            continue;
        }
        let o = lerp_px([px[0], px[1], px[2], px[3]], [e[0], e[1], e[2], e[3]], m as u32);
        px.copy_from_slice(&o);
    }
    out
}
