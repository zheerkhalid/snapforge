//! Pixel editing: clone stamp, remove and insert strips.

use super::{lerp_px, smoothstep, Color, Orientation};
use crate::common::{Image, Point, Rect};
use serde::{Deserialize, Serialize};

/// Clone-tool primitive: paint a round brush of `radius` centred at `dst_pt` with pixels
/// copied from around `src_pt`. `hardness` 0..=1: 1 = hard edge, 0 = soft from the centre.
/// The source is read before anything is written, so overlapping strokes are safe.
pub fn clone_stamp(img: &mut Image, src_pt: Point, dst_pt: Point, radius: u32, hardness: f32) {
    let r = radius.max(1) as i32;
    let src_rect = Rect::new(src_pt.x - r, src_pt.y - r, 2 * r + 1, 2 * r + 1);
    let patch_rect = src_rect.intersect(&img.bounds());
    if patch_rect.is_empty() {
        return;
    }
    let patch = img.crop(patch_rect);
    let hard = hardness.clamp(0.0, 1.0);
    let rf = r as f32;
    let inner = rf * hard;
    for dy in -r..=r {
        for dx in -r..=r {
            let (tx, ty) = (dst_pt.x + dx, dst_pt.y + dy);
            let (sx, sy) = (src_pt.x + dx, src_pt.y + dy);
            if !img.bounds().contains(Point::new(tx, ty)) || !patch_rect.contains(Point::new(sx, sy)) {
                continue;
            }
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            let wgt = if d <= inner {
                1.0
            } else if d >= rf + 0.5 {
                0.0
            } else {
                1.0 - smoothstep((d - inner) / (rf + 0.5 - inner).max(0.5))
            };
            if wgt <= 0.0 {
                continue;
            }
            let s = patch.pixel((sx - patch_rect.x) as u32, (sy - patch_rect.y) as u32);
            let t = img.pixel(tx as u32, ty as u32);
            img.set_pixel(tx as u32, ty as u32, lerp_px(t, s, (wgt * 255.0).round() as u32));
        }
    }
}

/// A horizontal band of rows (`Horizontal`) or a vertical band of columns (`Vertical`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StripParams {
    pub orientation: Orientation,
    /// First row (horizontal) or column (vertical) of the band.
    pub position: u32,
    pub size: u32,
    /// Fill for [`insert_strip`].
    pub color: Color,
}

impl Default for StripParams {
    fn default() -> Self {
        Self { orientation: Orientation::Horizontal, position: 0, size: 20, color: Color::WHITE }
    }
}

/// Cut a band out and join the remaining parts.
pub fn remove_strip(img: &Image, p: &StripParams) -> Image {
    let (w, h) = (img.width, img.height);
    match p.orientation {
        Orientation::Horizontal => {
            let start = p.position.min(h);
            let end = start.saturating_add(p.size).min(h);
            let mut out = Image::new(w, h - (end - start));
            let rl = w as usize * 4;
            out.data[..start as usize * rl].copy_from_slice(&img.data[..start as usize * rl]);
            out.data[start as usize * rl..].copy_from_slice(&img.data[end as usize * rl..]);
            out
        }
        Orientation::Vertical => {
            let start = p.position.min(w);
            let end = start.saturating_add(p.size).min(w);
            let nw = w - (end - start);
            let mut out = Image::new(nw, h);
            for y in 0..h as usize {
                let src = &img.data[y * w as usize * 4..(y + 1) * w as usize * 4];
                let dst = &mut out.data[y * nw as usize * 4..(y + 1) * nw as usize * 4];
                dst[..start as usize * 4].copy_from_slice(&src[..start as usize * 4]);
                dst[start as usize * 4..].copy_from_slice(&src[end as usize * 4..]);
            }
            out
        }
    }
}

/// Insert a band of `size` filled with `color` before row/column `position`.
pub fn insert_strip(img: &Image, p: &StripParams) -> Image {
    let fill = p.color.premul_bgra();
    let size = p.size.min(super::geometry::MAX_DIM);
    match p.orientation {
        Orientation::Horizontal => {
            let pos = p.position.min(img.height);
            let mut out = Image::filled(img.width, img.height + size, fill);
            out.blit(&img.crop(Rect::new(0, 0, img.width as i32, pos as i32)), 0, 0);
            let below = img.crop(Rect::new(0, pos as i32, img.width as i32, (img.height - pos) as i32));
            out.blit(&below, 0, (pos + size) as i32);
            out
        }
        Orientation::Vertical => {
            let pos = p.position.min(img.width);
            let mut out = Image::filled(img.width + size, img.height, fill);
            out.blit(&img.crop(Rect::new(0, 0, pos as i32, img.height as i32)), 0, 0);
            let right = img.crop(Rect::new(pos as i32, 0, (img.width - pos) as i32, img.height as i32));
            out.blit(&right, (pos + size) as i32, 0);
            out
        }
    }
}
