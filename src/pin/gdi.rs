//! GDI helpers for pins: a top-down 32-bit DIB section and anti-aliased text rendered
//! to a coverage mask (used by the text tool, step stamps, overlays and the OCR tests).

use crate::common::win::wide;
use crate::common::Image;
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, SelectObject,
    SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_LEFT, DT_NOPREFIX, FW_NORMAL, FW_SEMIBOLD,
    HBITMAP, HDC, HFONT, HGDIOBJ, OUT_TT_PRECIS, TRANSPARENT,
};

/// A top-down 32-bit DIB section selected into its own memory DC.
pub struct Dib {
    pub dc: HDC,
    hbm: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

impl Dib {
    pub fn new(w: i32, h: i32) -> Option<Dib> {
        if w <= 0 || h <= 0 {
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid BITMAPINFO; the section and DC are released in Drop, and every
        // failure path releases what was created so far.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let hbm = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(h) if !bits.is_null() => h,
                Ok(h) => {
                    let _ = DeleteObject(h);
                    let _ = DeleteDC(dc);
                    return None;
                }
                Err(e) => {
                    log::warn!("pin: CreateDIBSection {w}x{h} failed: {e}");
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let old = SelectObject(dc, hbm);
            Some(Dib { dc, hbm, old, bits: bits as *mut u8, w, h })
        }
    }

    /// The pixels (BGRA, top-down, stride = w * 4).
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: GDI drawing into the section is flushed first; the section is
        // `w * h * 4` bytes and lives as long as `self`.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, self.w as usize * self.h as usize * 4)
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: we own the DC and bitmap; the original bitmap is selected back first.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.hbm);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// An 8-bit coverage mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// Segoe UI with a character height of `px` pixels. The caller deletes it.
pub fn font(px: i32, bold: bool) -> HFONT {
    let face = wide("Segoe UI");
    // SAFETY: valid NUL-terminated face name.
    unsafe {
        CreateFontW(
            -px.max(1),
            0,
            0,
            0,
            if bold { FW_SEMIBOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            PCWSTR(face.as_ptr()),
        )
    }
}

/// Render `text` (may contain '\n') in Segoe UI at `px` into a grayscale coverage mask.
/// Uses grayscale anti-aliasing (not ClearType), so it composites on any background.
/// Returns `None` for empty text or when GDI fails.
pub fn text_mask(text: &str, px: i32, bold: bool) -> Option<Mask> {
    if text.is_empty() {
        return None;
    }
    let mut w16: Vec<u16> = text.encode_utf16().collect();
    let fmt: DRAW_TEXT_FORMAT = DT_LEFT | DT_NOPREFIX;
    let f = font(px, bold);
    if f.is_invalid() {
        return None;
    }
    // Measure with a scratch DC.
    let mut rc = RECT::default();
    let mut probe = Dib::new(1, 1)?;
    // SAFETY: the font is selected into our own DC and restored before deletion.
    unsafe {
        let old = SelectObject(probe.dc, f);
        DrawTextW(probe.dc, &mut w16, &mut rc, fmt | DT_CALCRECT);
        SelectObject(probe.dc, old);
    }
    let _ = probe.bytes();
    // Italic overhang and anti-aliasing can spill a pixel or two: pad.
    let pad = (px / 8).max(2);
    let (w, h) = (rc.right - rc.left + 2 * pad, rc.bottom - rc.top + pad);
    let mut dib = match Dib::new(w, h) {
        Some(d) => d,
        None => {
            // SAFETY: we created the font.
            unsafe {
                let _ = DeleteObject(f);
            }
            return None;
        }
    };
    // SAFETY: drawing into our own DIB with our own font; both released below.
    unsafe {
        let old = SelectObject(dib.dc, f);
        SetBkMode(dib.dc, TRANSPARENT);
        SetTextColor(dib.dc, COLORREF(0x00FF_FFFF));
        let mut r = RECT { left: pad, top: 0, right: w, bottom: h };
        DrawTextW(dib.dc, &mut w16, &mut r, fmt);
        SelectObject(dib.dc, old);
        let _ = DeleteObject(f);
    }
    let data = dib.bytes().chunks_exact(4).map(|p| p[1]).collect();
    Some(Mask { w: w as u32, h: h as u32, data })
}

/// Colorize a mask: straight `bgr` color, alpha = coverage × `alpha` / 255, premultiplied.
pub fn colorize(mask: &Mask, bgr: [u8; 3], alpha: u8) -> Image {
    let mut img = Image::new(mask.w, mask.h);
    for (px, &c) in img.data.chunks_exact_mut(4).zip(&mask.data) {
        let a = (c as u32 * alpha as u32 + 127) / 255;
        if a == 0 {
            continue;
        }
        for i in 0..3 {
            px[i] = ((bgr[i] as u32 * a + 127) / 255) as u8;
        }
        px[3] = a as u8;
    }
    img
}

/// `text` in solid `bgr` on an opaque `bg` background, with `margin` pixels around it.
pub fn text_image(text: &str, px: i32, bgr: [u8; 3], bg: [u8; 3], margin: u32) -> Option<Image> {
    let m = text_mask(text, px, false)?;
    let mut img = Image::filled(m.w + 2 * margin, m.h + 2 * margin, [bg[0], bg[1], bg[2], 255]);
    img.draw_over(&colorize(&m, bgr, 255), margin as i32, margin as i32);
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_has_ink() {
        let m = text_mask("Hello", 32, false).expect("mask");
        assert!(m.w > 40 && m.h >= 32, "{}x{}", m.w, m.h);
        assert!(m.data.contains(&255));
        assert!(m.data.iter().filter(|&&c| c == 0).count() > m.data.len() / 3);
        assert!(text_mask("", 32, false).is_none());
    }

    #[test]
    fn colorize_premultiplies() {
        let m = Mask { w: 2, h: 1, data: vec![255, 128] };
        let img = colorize(&m, [0, 0, 200], 255);
        assert_eq!(img.pixel(0, 0), [0, 0, 200, 255]);
        let p = img.pixel(1, 0);
        assert_eq!(p[3], 128);
        assert_eq!(p[2], 100);
    }
}
