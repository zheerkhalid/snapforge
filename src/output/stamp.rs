//! Date/time stamp: white text on a semi-transparent dark rounded badge, bottom-right.

use crate::common::win::{pcwstr, wide};
use crate::common::Image;
use std::ffi::c_void;
use windows::Win32::Foundation::{COLORREF, HANDLE, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GdiFlush, GetTextExtentPoint32W,
    SelectObject, SetBkMode, SetTextColor, TextOutW, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, FF_SWISS, FW_SEMIBOLD, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

/// Badge opacity (0–255).
const BADGE_ALPHA: u32 = 150;
const MIN_DIM: u32 = 40;

/// Text coverage mask: (width, height, coverage per pixel).
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

pub fn font_height(w: u32, h: u32) -> i32 {
    (w.min(h) / 40).max(11) as i32
}

/// Draw `text` (date/time) into the bottom-right corner, legible on any background.
/// Images smaller than 40 px in either dimension are left unchanged.
pub fn stamp(img: &mut Image, text: &str) {
    if img.width < MIN_DIM || img.height < MIN_DIM || text.trim().is_empty() {
        return;
    }
    let fh = font_height(img.width, img.height);
    match render_text(text, fh) {
        Some(mask) => composite(img, &mask, fh),
        None => log::warn!("stamp: text rendering failed"),
    }
}

/// Render white-on-black text with GDI into a 32-bit DIB and return its coverage.
pub fn render_text(text: &str, font_h: i32) -> Option<Mask> {
    let wtext: Vec<u16> = text.encode_utf16().collect();
    let face = wide("Segoe UI");
    // SAFETY: every GDI object created here is selected out and deleted before returning;
    // the DIB bits are only read while the bitmap is alive.
    unsafe {
        let dc: HDC = CreateCompatibleDC(HDC::default());
        if dc.is_invalid() {
            return None;
        }
        let font = CreateFontW(
            -font_h,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            FF_SWISS.0 as u32,
            pcwstr(&face),
        );
        if font.is_invalid() {
            let _ = DeleteDC(dc);
            return None;
        }
        let old_font = SelectObject(dc, font);
        let mut size = SIZE::default();
        let ok = GetTextExtentPoint32W(dc, &wtext, &mut size).as_bool();
        let result = if ok && size.cx > 0 && size.cy > 0 {
            let (tw, th) = (size.cx, size.cy);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: tw,
                    biHeight: -th,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
                Ok(bmp) if !bits.is_null() => {
                    let old_bmp = SelectObject(dc, bmp);
                    // A fresh DIB section is zero-filled (black).
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00FF_FFFF));
                    let _ = TextOutW(dc, 0, 0, &wtext);
                    let _ = GdiFlush();
                    let px = std::slice::from_raw_parts(bits as *const u8, (tw * th * 4) as usize);
                    let data = px.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).collect();
                    SelectObject(dc, old_bmp);
                    let _ = DeleteObject(bmp);
                    Some(Mask { w: tw as u32, h: th as u32, data })
                }
                Ok(bmp) => {
                    let _ = DeleteObject(bmp);
                    None
                }
                Err(_) => None,
            }
        } else {
            None
        };
        SelectObject(dc, old_font);
        let _ = DeleteObject(font);
        let _ = DeleteDC(dc);
        result
    }
}

/// Coverage (0..=1) of a rounded rectangle [0,w)x[0,h) with radius r at pixel centre (x, y).
fn rounded_coverage(x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let cx = x.clamp(r, w - r);
    let cy = y.clamp(r, h - r);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

/// Source-over one premultiplied colour with coverage onto the image.
#[inline]
fn blend(img: &mut Image, x: u32, y: u32, src: [u32; 4]) {
    let i = (y as usize * img.width as usize + x as usize) * 4;
    let ia = 255 - src[3];
    for c in 0..4 {
        let d = img.data[i + c] as u32;
        img.data[i + c] = (src[c] + (d * ia + 127) / 255).min(255) as u8;
    }
}

/// Composite the badge and the text mask into the bottom-right corner.
pub fn composite(img: &mut Image, mask: &Mask, font_h: i32) {
    let pad_x = (font_h as u32 / 2).max(4);
    let pad_y = (font_h as u32 / 5).max(2);
    let margin = (font_h as u32 / 2).max(4);
    let bw = mask.w + 2 * pad_x;
    let bh = mask.h + 2 * pad_y;
    // Badge origin; may go negative for tiny images, so clip while drawing.
    let bx = img.width as i64 - margin as i64 - bw as i64;
    let by = img.height as i64 - margin as i64 - bh as i64;
    let radius = (bh as f32 / 3.0).min(bw as f32 / 2.0);

    for yy in 0..bh {
        let iy = by + yy as i64;
        if iy < 0 || iy >= img.height as i64 {
            continue;
        }
        for xx in 0..bw {
            let ix = bx + xx as i64;
            if ix < 0 || ix >= img.width as i64 {
                continue;
            }
            let cov = rounded_coverage(xx as f32 + 0.5, yy as f32 + 0.5, bw as f32, bh as f32, radius);
            if cov <= 0.0 {
                continue;
            }
            let a = (BADGE_ALPHA as f32 * cov).round() as u32;
            blend(img, ix as u32, iy as u32, [0, 0, 0, a]);
            // Text (white, premultiplied: all channels = coverage).
            if xx >= pad_x && yy >= pad_y && xx - pad_x < mask.w && yy - pad_y < mask.h {
                let t = mask.data[((yy - pad_y) * mask.w + (xx - pad_x)) as usize] as u32;
                if t > 0 {
                    blend(img, ix as u32, iy as u32, [t, t, t, t]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bottom_right_changes() {
        for bg in [[255u8, 255, 255, 255], [0, 0, 0, 255], [30, 200, 90, 255]] {
            let orig = Image::filled(640, 400, bg);
            let mut img = orig.clone();
            stamp(&mut img, "2026-11-14 12:34:56");
            let mut changed = 0;
            let mut white_text = false;
            for y in 0..img.height {
                for x in 0..img.width {
                    if img.pixel(x, y) != orig.pixel(x, y) {
                        changed += 1;
                        assert!(x >= img.width / 2 && y >= img.height * 3 / 4, "pixel ({x},{y}) changed");
                    }
                    let p = img.pixel(x, y);
                    if p[0] > 230 && p[1] > 230 && p[2] > 230 && orig.pixel(x, y) != p {
                        white_text = true;
                    }
                }
            }
            assert!(changed > 100, "stamp drew nothing ({changed})");
            if bg != [255, 255, 255, 255] {
                assert!(white_text, "no white text pixels");
            }
            // Stays opaque.
            assert!(!img.has_transparency());
        }
    }

    #[test]
    fn small_images_untouched() {
        let orig = Image::filled(39, 300, [1, 2, 3, 255]);
        let mut img = orig.clone();
        stamp(&mut img, "2026-01-01 00:00:00");
        assert_eq!(img, orig);
    }

    #[test]
    fn font_height_rule() {
        assert_eq!(font_height(100, 100), 11);
        assert_eq!(font_height(1920, 1080), 27);
    }

    #[test]
    fn composite_clips_wide_text() {
        let mut img = Image::filled(50, 50, [0, 0, 0, 255]);
        let mask = Mask { w: 200, h: 12, data: vec![255; 2400] };
        composite(&mut img, &mask, 11);
        // Did not panic and the bottom-right is now white-ish text.
        assert!(img.pixel(30, 40)[0] > 200);
    }
}
