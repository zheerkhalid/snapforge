//! Clipboard: CF_DIBV5 (straight alpha) + CF_DIB (24-bit fallback) + registered "PNG".

use super::{dib, save};
use crate::common::win::{pcwstr, wide};
use crate::common::Image;
use crate::settings::ImageFormat;
use anyhow::{anyhow, Context, Result};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;

/// How long to keep retrying when another process holds the clipboard open.
const OPEN_TIMEOUT: Duration = Duration::from_millis(1000);

/// Closes the clipboard on drop.
struct OpenGuard;

impl OpenGuard {
    fn open() -> Result<Self> {
        let start = Instant::now();
        loop {
            // SAFETY: plain call; a null owner associates the clipboard with this task.
            match unsafe { OpenClipboard(HWND::default()) } {
                Ok(()) => return Ok(OpenGuard),
                Err(e) if start.elapsed() >= OPEN_TIMEOUT => {
                    return Err(anyhow!("the clipboard is in use by another program ({e})"));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        // SAFETY: we opened the clipboard in `open`.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn png_format() -> u32 {
    let name = wide("PNG");
    // SAFETY: valid NUL-terminated string.
    unsafe { RegisterClipboardFormatW(pcwstr(&name)) }
}

/// Copy `bytes` into a movable global block and hand it to the clipboard.
/// The clipboard must be open and owned (after EmptyClipboard).
fn set_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    // SAFETY: we allocate, fill and unlock the block; on success the system owns it,
    // on failure we free it.
    unsafe {
        let h: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).context("GlobalAlloc failed")?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(h);
            return Err(anyhow!("GlobalLock failed"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        if let Err(e) = SetClipboardData(format, HANDLE(h.0)) {
            let _ = GlobalFree(h);
            return Err(anyhow!("SetClipboardData({format}) failed: {e}"));
        }
    }
    Ok(())
}

/// Read a clipboard format's bytes. The clipboard must be open.
fn get_bytes(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the handle belongs to the clipboard; we only lock/read/unlock it.
    unsafe {
        IsClipboardFormatAvailable(format).ok()?;
        let h = GetClipboardData(format).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() || size == 0 {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(v)
    }
}

/// Put the image on the clipboard as CF_DIBV5 (with alpha), CF_DIB and the "PNG" format.
pub fn copy_image(img: &Image) -> Result<()> {
    anyhow::ensure!(!img.is_empty(), "nothing to copy (empty image)");
    // Encode before opening so the clipboard is held as briefly as possible.
    let v5 = dib::build_dibv5(img);
    let dib24 = dib::build_dib24(img);
    let png = save::encode(img, ImageFormat::Png, 100).context("PNG encoding for the clipboard failed")?;
    let png_fmt = png_format();

    let _g = OpenGuard::open()?;
    // SAFETY: the clipboard is open.
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
    set_bytes(CF_DIBV5, &v5)?;
    set_bytes(CF_DIB, &dib24)?;
    if png_fmt != 0 {
        if let Err(e) = set_bytes(png_fmt, &png) {
            log::warn!("clipboard: PNG format not set: {e:#}");
        }
    }
    log::debug!("clipboard: copied {}x{} image", img.width, img.height);
    Ok(())
}

/// Put text on the clipboard (upload URLs, color values…).
pub fn copy_text(text: &str) -> Result<()> {
    let w = wide(text);
    // SAFETY: reinterpreting a u16 slice as bytes of the same memory.
    let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
    let _g = OpenGuard::open()?;
    // SAFETY: the clipboard is open.
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
    set_bytes(CF_UNICODETEXT, bytes)
}

/// Text currently on the clipboard, if any.
pub fn paste_text() -> Option<String> {
    let _g = OpenGuard::open().ok()?;
    let b = get_bytes(CF_UNICODETEXT)?;
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(crate::common::win::from_wide(&units))
}

/// Image currently on the clipboard, if any (PNG, then DIBV5, then DIB).
pub fn paste_image() -> Option<Image> {
    let png_fmt = png_format();
    let (png, v5, dib) = {
        let _g = match OpenGuard::open() {
            Ok(g) => g,
            Err(e) => {
                log::warn!("clipboard: {e:#}");
                return None;
            }
        };
        let png = if png_fmt != 0 { get_bytes(png_fmt) } else { None };
        // Only fetch the DIBs if needed (synthesised formats cost a conversion).
        let v5 = if png.is_none() { get_bytes(CF_DIBV5) } else { None };
        let dib = if png.is_none() && v5.is_none() { get_bytes(CF_DIB) } else { None };
        (png, v5, dib)
    };
    if let Some(p) = png {
        match save::decode(&p, "png") {
            Ok(img) => return Some(img),
            Err(e) => log::warn!("clipboard: PNG data unreadable: {e:#}"),
        }
    }
    for (name, data) in [("DIBV5", v5), ("DIB", dib)] {
        if let Some(d) = data {
            match dib::parse_dib(&d) {
                Ok(img) => return Some(img),
                Err(e) => log::warn!("clipboard: {name} data unreadable: {e:#}"),
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests share the system clipboard; serialise them.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn image_roundtrip() {
        let _l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut img = Image::filled(13, 9, [10, 20, 30, 255]);
        img.set_pixel(12, 8, [0, 0, 0, 0]);
        img.set_pixel(0, 0, [0, 0, 64, 128]);
        if let Err(e) = copy_image(&img) {
            eprintln!("clipboard unavailable, skipping: {e:#}");
            return;
        }
        let back = paste_image().expect("image on clipboard");
        assert_eq!((back.width, back.height), (13, 9));
        assert_eq!(back.pixel(12, 8)[3], 0);
        assert_eq!(back.pixel(5, 5), [10, 20, 30, 255]);
        assert!(back.pixel(0, 0)[3].abs_diff(128) <= 1);
    }

    #[test]
    fn text_roundtrip() {
        let _l = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = copy_text("https://example.com/ä b.png") {
            eprintln!("clipboard unavailable, skipping: {e:#}");
            return;
        }
        assert_eq!(paste_text().as_deref(), Some("https://example.com/ä b.png"));
    }
}
