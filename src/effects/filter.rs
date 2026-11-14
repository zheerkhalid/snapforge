//! Convolution filters: Gaussian blur (three box passes), unsharp mask, pixelate.

use super::par_rows;
use crate::common::Image;
use serde::{Deserialize, Serialize};

/// Gaussian blur. `radius` is the visible extent in pixels, like CSS `blur()`:
/// the Gaussian's sigma is `radius / 2`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlurParams {
    pub radius: f32,
}

impl Default for BlurParams {
    fn default() -> Self {
        Self { radius: 4.0 }
    }
}

pub fn blur(img: &Image, p: &BlurParams) -> Image {
    let data = gaussian_blur_raw(&img.data, img.width as usize, img.height as usize, 4, p.radius / 2.0);
    Image::from_bgra(img.width, img.height, data)
}

/// Unsharp mask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharpenParams {
    /// Strength in percent (100 = add the full detail difference once).
    pub amount: f32,
    /// Blur radius of the mask (see [`BlurParams::radius`]).
    pub radius: f32,
    /// Differences smaller than this (0–255) are left alone (keeps flat areas clean).
    pub threshold: u8,
}

impl Default for SharpenParams {
    fn default() -> Self {
        Self { amount: 100.0, radius: 2.0, threshold: 0 }
    }
}

pub fn sharpen(img: &Image, p: &SharpenParams) -> Image {
    let blurred = gaussian_blur_raw(&img.data, img.width as usize, img.height as usize, 4, p.radius / 2.0);
    let k = (p.amount.max(0.0) / 100.0 * 256.0).round() as i32;
    let th = p.threshold as i32;
    let mut out = img.clone();
    let rl = img.width as usize * 4;
    par_rows(&mut out.data, rl, |y0, band| {
        let off = y0 * rl;
        let bl = &blurred[off..off + band.len()];
        for (px, b) in band.chunks_exact_mut(4).zip(bl.chunks_exact(4)) {
            let a = px[3] as i32;
            for c in 0..3 {
                let v = px[c] as i32;
                let d = v - b[c] as i32;
                if d.abs() < th {
                    continue;
                }
                px[c] = (v + ((d * k + 128) >> 8)).clamp(0, a) as u8;
            }
        }
    });
    out
}

/// Mosaic with square blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PixelateParams {
    pub block: u32,
}

impl Default for PixelateParams {
    fn default() -> Self {
        Self { block: 10 }
    }
}

pub fn pixelate(img: &Image, p: &PixelateParams) -> Image {
    let b = p.block.max(1) as usize;
    if img.is_empty() || b == 1 {
        return img.clone();
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (bw, bh) = (w.div_ceil(b), h.div_ceil(b));
    let mut avg = vec![0u8; bw * bh * 4];
    par_rows(&mut avg, bw * 4, |by0, band| {
        for (r, brow) in band.chunks_exact_mut(bw * 4).enumerate() {
            let y0 = (by0 + r) * b;
            let y1 = (y0 + b).min(h);
            for bx in 0..bw {
                let x0 = bx * b;
                let x1 = (x0 + b).min(w);
                let mut s = [0u64; 4];
                for y in y0..y1 {
                    let row = &img.data[(y * w + x0) * 4..(y * w + x1) * 4];
                    for px in row.chunks_exact(4) {
                        for c in 0..4 {
                            s[c] += px[c] as u64;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u64;
                for c in 0..4 {
                    brow[bx * 4 + c] = ((s[c] + n / 2) / n) as u8;
                }
            }
        }
    });
    let mut out = Image::new(img.width, img.height);
    par_rows(&mut out.data, w * 4, |y0, band| {
        for (r, row) in band.chunks_exact_mut(w * 4).enumerate() {
            let by = (y0 + r) / b;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let i = (by * bw + x / b) * 4;
                px.copy_from_slice(&avg[i..i + 4]);
            }
        }
    });
    out
}

// ------------------------------------------------------------------ box-blur engine

/// Box widths (odd) whose three successive passes approximate a Gaussian of `sigma`
/// (Kovesi, "Fast almost-Gaussian filtering").
pub(crate) fn box_radii(sigma: f32) -> [usize; 3] {
    if sigma < 0.3 {
        return [0; 3];
    }
    let n = 3.0f64;
    let s = sigma as f64;
    let w_ideal = (12.0 * s * s / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let wlf = wl as f64;
    let m = ((12.0 * s * s - n * wlf * wlf - 4.0 * n * wlf - 3.0 * n) / (-4.0 * wlf - 4.0)).round() as i64;
    let mut out = [0usize; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let size = if (i as i64) < m { wl } else { wu };
        *o = ((size.max(1) - 1) / 2) as usize;
    }
    out
}

/// Gaussian-like blur of interleaved `ch`-channel 8-bit data (edges clamp).
pub(crate) fn gaussian_blur_raw(src: &[u8], w: usize, h: usize, ch: usize, sigma: f32) -> Vec<u8> {
    let radii = box_radii(sigma);
    if w == 0 || h == 0 || radii.iter().all(|&r| r == 0) {
        return src.to_vec();
    }
    let rl = w * ch;
    // Horizontal: all three passes per row (rows are independent).
    let mut hbuf = vec![0u8; src.len()];
    par_rows(&mut hbuf, rl, |y0, band| {
        let mut a = vec![0u8; rl];
        let mut b = vec![0u8; rl];
        for (r, out) in band.chunks_exact_mut(rl).enumerate() {
            let y = y0 + r;
            a.copy_from_slice(&src[y * rl..(y + 1) * rl]);
            for &rad in &radii {
                if rad > 0 {
                    box_row(&a, &mut b, w, ch, rad);
                    std::mem::swap(&mut a, &mut b);
                }
            }
            out.copy_from_slice(&a);
        }
    });
    // Vertical: each pass needs the whole previous pass.
    let mut cur = hbuf;
    let mut next = vec![0u8; src.len()];
    for &rad in &radii {
        if rad == 0 {
            continue;
        }
        let input = &cur;
        par_rows(&mut next, rl, |y0, band| box_cols(input, band, y0, w, h, ch, rad));
        std::mem::swap(&mut cur, &mut next);
    }
    cur
}

#[inline]
fn recip(rad: usize) -> u64 {
    (1u64 << 32) / (2 * rad as u64 + 1)
}

/// One horizontal box pass over a row.
fn box_row(src: &[u8], dst: &mut [u8], w: usize, ch: usize, rad: usize) {
    let inv = recip(rad);
    let last = w - 1;
    // All channels of a pixel together (ch <= 4), so memory is walked once, in order.
    let mut sum = [0u64; 4];
    for (c, s) in sum.iter_mut().enumerate().take(ch) {
        *s = src[c] as u64 * (rad as u64 + 1);
        for i in 1..=rad {
            *s += src[i.min(last) * ch + c] as u64;
        }
    }
    for x in 0..w {
        let o = x * ch;
        let add = (x + rad + 1).min(last) * ch;
        let sub = x.saturating_sub(rad) * ch;
        for c in 0..ch {
            dst[o + c] = ((sum[c] * inv + (1 << 31)) >> 32) as u8;
            sum[c] = sum[c] + src[add + c] as u64 - src[sub + c] as u64;
        }
    }
}

/// One vertical box pass producing output rows `y0..` into `band`.
fn box_cols(src: &[u8], band: &mut [u8], y0: usize, w: usize, h: usize, ch: usize, rad: usize) {
    let rl = w * ch;
    let inv = recip(rad);
    let row = |y: i64| -> &[u8] {
        let y = y.clamp(0, h as i64 - 1) as usize;
        &src[y * rl..(y + 1) * rl]
    };
    let mut acc = vec![0u32; rl];
    for j in -(rad as i64)..=(rad as i64) {
        for (a, &s) in acc.iter_mut().zip(row(y0 as i64 + j)) {
            *a += s as u32;
        }
    }
    for (r, out) in band.chunks_exact_mut(rl).enumerate() {
        let y = (y0 + r) as i64;
        for (o, &a) in out.iter_mut().zip(&acc) {
            *o = ((a as u64 * inv + (1 << 31)) >> 32) as u8;
        }
        let (add, sub) = (row(y + rad as i64 + 1), row(y - rad as i64));
        for ((a, &p), &m) in acc.iter_mut().zip(add).zip(sub) {
            *a = *a + p as u32 - m as u32;
        }
    }
}
