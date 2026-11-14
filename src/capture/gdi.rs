//! GDI backend: `BitBlt` from the screen DC into a top-down 32-bit DIB section.
//!
//! Always available and stateless, so it is the last resort of the engine chain. It sees
//! layered windows (`CAPTUREBLT`) but not hardware overlays or some DirectX content.

use super::{cursor, CaptureError};
use crate::common::{Image, Rect};
use std::ffi::c_void;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};

/// A memory DC with a selected top-down 32-bit DIB section; everything is freed on drop.
pub(crate) struct DibDc {
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub width: i32,
    pub height: i32,
}

impl DibDc {
    /// `reference` is the DC the memory DC must be compatible with (the screen DC).
    pub fn new(reference: HDC, width: i32, height: i32) -> Result<Self, CaptureError> {
        if width <= 0 || height <= 0 {
            return Err(CaptureError::Empty);
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: plain GDI object creation; every handle is owned by the returned guard
        // (or freed right here on failure). `bits` is only used while the DIB is alive.
        unsafe {
            let dc = CreateCompatibleDC(reference);
            if dc.is_invalid() {
                return Err(CaptureError::Failed("CreateCompatibleDC failed".into()));
            }
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(reference, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                Ok(b) => {
                    let _ = DeleteObject(b);
                    let _ = DeleteDC(dc);
                    return Err(CaptureError::Failed("CreateDIBSection returned no bits".into()));
                }
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(CaptureError::Failed(format!("CreateDIBSection {width}x{height}: {e}")));
                }
            };
            let old = SelectObject(dc, bitmap);
            Ok(Self { dc, bitmap, old, bits: bits as *mut u8, width, height })
        }
    }

    /// The DIB pixels (BGRA, top-down, stride = width * 4).
    pub fn pixels(&self) -> &[u8] {
        // SAFETY: flush pending GDI drawing, then view the DIB memory, which lives as long
        // as `self` and has exactly width * height * 4 bytes (32 bpp rows are DWORD-aligned).
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts(self.bits, self.width as usize * self.height as usize * 4)
        }
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `pixels`, and `&mut self` guarantees exclusive access.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, self.width as usize * self.height as usize * 4)
        }
    }
}

impl Drop for DibDc {
    fn drop(&mut self) {
        // SAFETY: restoring the original object before deleting what we created.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The screen DC, released on drop.
pub(crate) struct ScreenDc(pub HDC);

impl ScreenDc {
    pub fn get() -> Result<Self, CaptureError> {
        // SAFETY: GetDC(NULL) returns the screen DC; released in Drop.
        let dc = unsafe { GetDC(HWND::default()) };
        if dc.is_invalid() {
            return Err(CaptureError::Failed("GetDC(NULL) failed".into()));
        }
        Ok(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: releasing the DC obtained in `get`.
        unsafe {
            ReleaseDC(HWND::default(), self.0);
        }
    }
}

/// Capture `rect` (virtual-screen coordinates) with BitBlt.
pub fn grab(rect: Rect, include_cursor: bool) -> Result<Image, CaptureError> {
    if rect.is_empty() {
        return Err(CaptureError::Empty);
    }
    let screen = ScreenDc::get()?;
    let mut dib = DibDc::new(screen.0, rect.w, rect.h)?;
    // SAFETY: both DCs are valid for the duration of the call.
    unsafe {
        BitBlt(dib.dc, 0, 0, rect.w, rect.h, screen.0, rect.x, rect.y, SRCCOPY | CAPTUREBLT)
            .map_err(|e| CaptureError::Failed(format!("BitBlt: {e}")))?;
    }
    if include_cursor {
        cursor::draw_on_dc(dib.dc, rect.x, rect.y);
    }
    let px = dib.pixels_mut();
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    Ok(Image::from_bgra(rect.w as u32, rect.h as u32, px.to_vec()))
}
