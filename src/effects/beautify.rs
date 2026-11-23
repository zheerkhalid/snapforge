//! Beautify: put a screenshot on a padded backdrop (gradient, solid colour or transparent)
//! with rounded corners and a soft shadow, optionally padded out to a fixed aspect ratio.
//! One step instead of round corners + drop shadow + expand canvas.

use super::{composite, gaussian_blur_raw, par_rows, round_corners, Color};
use crate::common::Image;
use serde::{Deserialize, Serialize};

/// Two-colour diagonal gradients (top-left to bottom-right), straight RGB.
pub const GRADIENTS: [([u8; 3], [u8; 3]); 8] = [
    ([255, 153, 102], [255, 94, 138]),  // sunset
    ([64, 156, 255], [46, 212, 191]),   // ocean
    ([142, 84, 233], [71, 118, 230]),   // violet
    ([52, 168, 110], [22, 120, 140]),   // forest
    ([240, 80, 60], [250, 176, 52]),    // ember
    ([60, 70, 88], [28, 32, 44]),       // slate
    ([232, 236, 242], [200, 208, 220]), // mist
    ([28, 40, 84], [8, 10, 24]),        // midnight
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backdrop {
    /// Index into [`GRADIENTS`].
    Gradient(usize),
    Solid(Color),
    Transparent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BeautifyParams {
    pub backdrop: Backdrop,
    /// Space around the picture on every side, in pixels.
    pub padding: u32,
    pub corner_radius: u32,
    /// Shadow strength in percent (0 = none).
    pub shadow: u8,
    /// Pad the result out to this width:height ratio (the picture stays centred).
    pub ratio: Option<(u32, u32)>,
}

impl Default for BeautifyParams {
    fn default() -> Self {
        Self { backdrop: Backdrop::Gradient(0), padding: 64, corner_radius: 12, shadow: 50, ratio: None }
    }
}

/// Padding is capped so a typo cannot allocate gigabytes.
const MAX_PADDING: u32 = 2000;

/// Size of the result for an image of `w` × `h`.
pub fn beautify_size(w: u32, h: u32, p: &BeautifyParams) -> (u32, u32) {
    let pad = p.padding.min(MAX_PADDING);
    let (mut ow, mut oh) = (w + 2 * pad, h + 2 * pad);
    if let Some((rw, rh)) = p.ratio.filter(|&(a, b)| a > 0 && b > 0) {
        // Grow the short side; never shrink.
        let want_w = (oh as u64 * rw as u64).div_ceil(rh as u64) as u32;
        let want_h = (ow as u64 * rh as u64).div_ceil(rw as u64) as u32;
        if want_w > ow {
            ow = want_w;
        } else if want_h > oh {
            oh = want_h;
        }
    }
    (ow, oh)
}

pub fn beautify(img: &Image, p: &BeautifyParams) -> Image {
    if img.is_empty() {
        return img.clone();
    }
    let src = if p.corner_radius > 0 { round_corners(img, p.corner_radius) } else { img.clone() };
    let (w, h) = beautify_size(src.width, src.height, p);
    let (x, y) = (((w - src.width) / 2) as i32, ((h - src.height) / 2) as i32);
    let mut out = backdrop(w, h, p.backdrop);
    let room = x.min(y).max(0) as f32;
    if p.shadow > 0 && room >= 2.0 {
        // The shadow scales with the room around the picture, so it never looks cut off.
        let blur = (room * 0.55).clamp(2.0, 90.0);
        let drop = (blur * 0.3).round() as i32;
        shadow(&mut out, &src, x, y + drop, blur, p.shadow.min(100));
    }
    composite(&mut out, &src, x, y, 255);
    out
}

fn backdrop(w: u32, h: u32, b: Backdrop) -> Image {
    match b {
        Backdrop::Transparent => Image::new(w, h),
        Backdrop::Solid(c) => Image::filled(w, h, c.premul_bgra()),
        Backdrop::Gradient(i) => {
            let (a, z) = GRADIENTS[i.min(GRADIENTS.len() - 1)];
            let mut out = Image::new(w, h);
            let wu = w as usize;
            let span = (w + h).saturating_sub(2).max(1) as f32;
            par_rows(&mut out.data, wu * 4, |y0, band| {
                for (i, px) in band.chunks_exact_mut(4).enumerate() {
                    let (x, y) = (i % wu, y0 + i / wu);
                    let t = (x + y) as f32 / span;
                    // A little ordered noise hides 8-bit banding on large canvases.
                    let n = (((x * 7 + y * 13) % 16) as f32 / 16.0) - 0.5;
                    let ch =
                        |c: usize| (a[c] as f32 + (z[c] as f32 - a[c] as f32) * t + n).round().clamp(0.0, 255.0) as u8;
                    px.copy_from_slice(&[ch(2), ch(1), ch(0), 255]);
                }
            });
            out
        }
    }
}

/// Blend a blurred black copy of `src`'s alpha into `dst` with its top-left at (x, y).
fn shadow(dst: &mut Image, src: &Image, x: i32, y: i32, blur: f32, strength: u8) {
    let (w, h) = (dst.width as usize, dst.height as usize);
    let mut alpha = vec![0u8; w * h];
    for sy in 0..src.height as i32 {
        let dy = y + sy;
        if dy < 0 || dy >= h as i32 {
            continue;
        }
        let srow = src.row(sy as u32);
        for sx in 0..src.width as i32 {
            let dx = x + sx;
            if dx >= 0 && dx < w as i32 {
                alpha[dy as usize * w + dx as usize] = srow[sx as usize * 4 + 3];
            }
        }
    }
    let alpha = gaussian_blur_raw(&alpha, w, h, 1, blur / 2.0);
    // 100 % strength is a 70 % black shadow at its darkest.
    let k = strength as u32 * 178 / 100;
    par_rows(&mut dst.data, w * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let a = alpha[y0 * w + i] as u32 * k / 255;
            if a == 0 {
                continue;
            }
            let ia = 255 - a;
            for c in px.iter_mut().take(3) {
                *c = ((*c as u32 * ia + 127) / 255) as u8;
            }
            px[3] = (a + (px[3] as u32 * ia + 127) / 255).min(255) as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot() -> Image {
        Image::filled(40, 20, [10, 200, 30, 255])
    }

    #[test]
    fn grows_by_the_padding_and_keeps_the_picture_centred() {
        let p = BeautifyParams { padding: 16, corner_radius: 0, shadow: 0, ..Default::default() };
        let out = beautify(&shot(), &p);
        assert_eq!((out.width, out.height), (72, 52));
        assert_eq!(out.pixel(16, 16), [10, 200, 30, 255]);
        assert_eq!(out.pixel(55, 35), [10, 200, 30, 255]);
        // Backdrop is opaque gradient: top-left is the first stop, bottom-right the second.
        let (a, z) = GRADIENTS[0];
        let tl = out.pixel(0, 0);
        let br = out.pixel(71, 51);
        assert!(tl[3] == 255 && br[3] == 255);
        for c in 0..3 {
            assert!((tl[2 - c] as i32 - a[c] as i32).abs() <= 1, "{tl:?}");
            assert!((br[2 - c] as i32 - z[c] as i32).abs() <= 1, "{br:?}");
        }
    }

    #[test]
    fn transparent_backdrop_with_shadow_and_round_corners() {
        let p =
            BeautifyParams { backdrop: Backdrop::Transparent, padding: 20, corner_radius: 6, shadow: 80, ratio: None };
        let out = beautify(&shot(), &p);
        // The far corner of the canvas stays clear, just below the picture is shadowed.
        assert_eq!(out.pixel(0, 0)[3], 0);
        let below = out.pixel(40, 42);
        assert!(below[3] > 20 && below[0] == 0 && below[1] == 0 && below[2] == 0, "{below:?}");
        // The picture's own corner pixel was rounded away (only shadow or nothing there).
        assert!(out.pixel(20, 20)[1] < 100, "{:?}", out.pixel(20, 20));
        // The centre is untouched.
        assert_eq!(out.pixel(40, 30), [10, 200, 30, 255]);
    }

    #[test]
    fn solid_backdrop_and_no_shadow_without_room() {
        let white = Backdrop::Solid(Color::WHITE);
        let p = BeautifyParams { backdrop: white, padding: 0, corner_radius: 0, shadow: 100, ratio: None };
        let out = beautify(&shot(), &p);
        assert_eq!(out, shot());
        let p = BeautifyParams { backdrop: white, padding: 8, corner_radius: 0, shadow: 0, ratio: None };
        assert_eq!(beautify(&shot(), &p).pixel(2, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn ratio_pads_the_short_side_only() {
        let mut p = BeautifyParams { padding: 10, ratio: Some((1, 1)), ..Default::default() };
        assert_eq!(beautify_size(40, 20, &p), (60, 60));
        p.ratio = Some((16, 9));
        let (w, h) = beautify_size(40, 20, &p);
        assert_eq!(h, 40);
        assert_eq!(w, 72); // ceil(40 * 16 / 9)
        p.ratio = Some((0, 5));
        assert_eq!(beautify_size(40, 20, &p), (60, 40));
        // A tall picture grows sideways for a wide ratio, and the picture stays centred.
        p.ratio = Some((4, 3));
        let tall = Image::filled(10, 50, [1, 2, 3, 255]);
        let out = beautify(&tall, &BeautifyParams { shadow: 0, corner_radius: 0, ..p.clone() });
        assert_eq!((out.width, out.height), (94, 70));
        assert_eq!(out.pixel(42, 10), [1, 2, 3, 255]);
        assert_eq!(out.pixel(51, 59), [1, 2, 3, 255]);
    }

    #[test]
    fn huge_padding_is_capped_and_tiny_images_work() {
        let p = BeautifyParams { padding: u32::MAX, shadow: 0, ..Default::default() };
        assert_eq!(beautify_size(1, 1, &p), (4001, 4001));
        let out = beautify(&Image::filled(1, 1, [9, 9, 9, 255]), &BeautifyParams::default());
        assert_eq!((out.width, out.height), (129, 129));
        assert!(beautify(&Image::new(0, 0), &BeautifyParams::default()).is_empty());
    }
}
