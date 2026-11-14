//! Text rendering for captions and watermarks: GDI draws white-on-black text into a 32-bit
//! DIB, which becomes a coverage mask, which is tinted with the requested colour.

use super::Color;
use crate::common::win::{pcwstr, wide};
use crate::common::Image;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::Win32::Foundation::{COLORREF, HANDLE, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush, SelectObject,
    SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_EDITCONTROL, DT_EXPANDTABS, DT_LEFT, DT_NOPREFIX,
    DT_RIGHT, DT_WORDBREAK, FF_DONTCARE, FW_BOLD, FW_NORMAL, HDC, HFONT, OUT_DEFAULT_PRECIS, TRANSPARENT,
};

/// Font description. `size` is the character (em) height in pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontSpec {
    pub face: String,
    pub size: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self { face: "Segoe UI".into(), size: 18, bold: false, italic: false, underline: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Render `text` in `font` at `size` px. `max_width` > 0 wraps long lines (words, then
/// characters for words that don't fit). Returns a tightly sized premultiplied image, or an
/// empty (0x0) image for empty text or if GDI fails (logged).
pub fn render_text(text: &str, font: &str, size: u32, color: Color, max_width: u32) -> Image {
    let spec = FontSpec { face: font.to_string(), size, ..Default::default() };
    render_text_styled(text, &spec, color, max_width, TextAlign::Left)
}

/// [`render_text`] with bold/italic/underline and line alignment.
pub fn render_text_styled(text: &str, font: &FontSpec, color: Color, max_width: u32, align: TextAlign) -> Image {
    match coverage(text, font, max_width, align) {
        Some((w, h, cov)) => {
            let c = color.premul_bgra();
            let mut data = Vec::with_capacity(cov.len() * 4);
            for &a in &cov {
                let a = a as u32;
                data.extend_from_slice(&c.map(|v| ((v as u32 * a + 127) / 255) as u8));
            }
            Image::from_bgra(w, h, data)
        }
        None => Image::new(0, 0),
    }
}

/// Size the text would occupy (same rules as [`render_text_styled`]).
pub fn measure_text(text: &str, font: &FontSpec, max_width: u32) -> (u32, u32) {
    if text.is_empty() {
        return (0, 0);
    }
    // SAFETY: the DC and font are released before returning.
    unsafe {
        let dc = CreateCompatibleDC(HDC::default());
        if dc.is_invalid() {
            return (0, 0);
        }
        let hfont = create_font(font, font.size as i32);
        let old = SelectObject(dc, hfont);
        let r = calc_rect(dc, text, max_width, format_flags(max_width, TextAlign::Left));
        SelectObject(dc, old);
        let _ = DeleteObject(hfont);
        let _ = DeleteDC(dc);
        let pad = italic_pad(font);
        ((r.right - r.left).max(0) as u32 + pad, (r.bottom - r.top).max(0) as u32)
    }
}

/// Create a GDI font (caller deletes it). `px` is the em height in device pixels.
pub(crate) fn create_font(font: &FontSpec, px: i32) -> HFONT {
    let face = wide(if font.face.trim().is_empty() { "Segoe UI" } else { font.face.as_str() });
    let weight = if font.bold { FW_BOLD.0 } else { FW_NORMAL.0 };
    // SAFETY: `face` outlives the call.
    unsafe {
        CreateFontW(
            -px.max(1),
            0,
            0,
            0,
            weight as i32,
            font.italic as u32,
            font.underline as u32,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            FF_DONTCARE.0 as u32,
            pcwstr(&face),
        )
    }
}

fn italic_pad(font: &FontSpec) -> u32 {
    if font.italic {
        (font.size / 5).max(1)
    } else {
        0
    }
}

pub(crate) fn format_flags(max_width: u32, align: TextAlign) -> DRAW_TEXT_FORMAT {
    let mut f = DT_NOPREFIX | DT_EXPANDTABS;
    if max_width > 0 {
        f |= DT_WORDBREAK | DT_EDITCONTROL;
    }
    f | match align {
        TextAlign::Left => DT_LEFT,
        TextAlign::Center => DT_CENTER,
        TextAlign::Right => DT_RIGHT,
    }
}

/// # Safety
/// `dc` must be a valid DC with the font selected.
unsafe fn calc_rect(dc: HDC, text: &str, max_width: u32, flags: DRAW_TEXT_FORMAT) -> RECT {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT { left: 0, top: 0, right: max_width.min(i32::MAX as u32) as i32, bottom: 0 };
    DrawTextW(dc, &mut buf, &mut r, flags | DT_CALCRECT);
    r
}

/// Coverage mask of the rendered text: (width, height, 0..=255 per pixel).
fn coverage(text: &str, font: &FontSpec, max_width: u32, align: TextAlign) -> Option<(u32, u32, Vec<u8>)> {
    if text.is_empty() {
        return None;
    }
    let flags = format_flags(max_width, align);
    let pad = italic_pad(font) as i32;
    // SAFETY: every GDI object created here is selected out and deleted before returning; the
    // DIB bits are read only while the bitmap is alive and after GdiFlush.
    unsafe {
        let dc = CreateCompatibleDC(HDC::default());
        if dc.is_invalid() {
            log::warn!("render_text: CreateCompatibleDC failed");
            return None;
        }
        let hfont = create_font(font, font.size as i32);
        if hfont.is_invalid() {
            log::warn!("render_text: CreateFontW failed for {:?}", font.face);
            let _ = DeleteDC(dc);
            return None;
        }
        let old_font = SelectObject(dc, hfont);
        let r = calc_rect(dc, text, max_width, flags);
        let mut tw = (r.right - r.left).max(0);
        if max_width > 0 {
            // Alignment within the wrap width; never wider than asked.
            tw = if align == TextAlign::Left { tw.min(max_width as i32) } else { max_width as i32 };
        }
        tw += pad;
        let th = (r.bottom - r.top).max(0);
        let mut result = None;
        if tw > 0 && th > 0 && (tw as i64 * th as i64) < 400_000_000 {
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
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00FF_FFFF));
                    let mut buf: Vec<u16> = text.encode_utf16().collect();
                    let mut dr = RECT { left: 0, top: 0, right: tw - pad, bottom: th };
                    DrawTextW(dc, &mut buf, &mut dr, flags);
                    let _ = GdiFlush();
                    let px = std::slice::from_raw_parts(bits as *const u8, (tw * th * 4) as usize);
                    let cov: Vec<u8> = px.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).collect();
                    SelectObject(dc, old_bmp);
                    let _ = DeleteObject(bmp);
                    result = Some((tw as u32, th as u32, cov));
                }
                Ok(bmp) => {
                    let _ = DeleteObject(bmp);
                }
                Err(e) => log::warn!("render_text: CreateDIBSection failed: {e}"),
            }
        }
        SelectObject(dc, old_font);
        let _ = DeleteObject(hfont);
        let _ = DeleteDC(dc);
        result
    }
}
