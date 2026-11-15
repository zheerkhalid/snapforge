//! The mouse cursor as a premultiplied BGRA sprite (drawn into recorded frames).

use crate::common::{Image, Point};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetObjectW, SelectObject, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HCURSOR, HICON, ICONINFO,
};

pub struct CursorShape {
    pub image: Image,
    pub hotspot: Point,
}

/// Current cursor handle (as isize) and position, or `None` when hidden.
pub fn current() -> Option<(isize, Point)> {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    // SAFETY: cbSize is set; valid out pointer.
    unsafe { GetCursorInfo(&mut ci).ok()? };
    if ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
        return None;
    }
    let POINT { x, y } = ci.ptScreenPos;
    Some((ci.hCursor.0 as isize, Point::new(x, y)))
}

fn bitmap_size(h: HBITMAP) -> Option<(i32, i32)> {
    if h.is_invalid() {
        return None;
    }
    let mut bm = BITMAP::default();
    // SAFETY: valid out buffer of the right size.
    let n = unsafe { GetObjectW(h, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) };
    (n != 0).then_some((bm.bmWidth, bm.bmHeight))
}

/// Render `hcursor` on a solid background into a 32-bit top-down buffer.
fn render_on(hcursor: HCURSOR, w: i32, h: i32, bg: u8) -> Option<Vec<u8>> {
    // SAFETY: GDI objects are created, used and released within this function.
    unsafe {
        let dc = CreateCompatibleDC(HDC::default());
        let bi = BITMAPINFO {
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
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, bmp);
        let len = (w * h * 4) as usize;
        std::ptr::write_bytes(bits as *mut u8, bg, len);
        let ok = DrawIconEx(dc, 0, 0, HICON(hcursor.0), w, h, 0, None, DI_NORMAL).is_ok();
        let out = ok.then(|| std::slice::from_raw_parts(bits as *const u8, len).to_vec());
        SelectObject(dc, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        out
    }
}

/// Extract the sprite. Alpha comes from rendering on black and on white; pixels that
/// invert the screen (I-beam, some crosshairs) become black with a white outline.
pub fn shape(hcursor: isize) -> Option<CursorShape> {
    let hc = HCURSOR(hcursor as *mut _);
    let mut ii = ICONINFO::default();
    // SAFETY: valid out pointer; the returned bitmaps are deleted below.
    unsafe { GetIconInfo(HICON(hc.0), &mut ii).ok()? };
    let (w, h) = match bitmap_size(ii.hbmColor) {
        Some(s) => s,
        None => bitmap_size(ii.hbmMask).map(|(w, h)| (w, h / 2))?,
    };
    // SAFETY: we own the bitmaps returned by GetIconInfo.
    unsafe {
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor);
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask);
        }
    }
    if w <= 0 || h <= 0 || w > 256 || h > 256 {
        return None;
    }
    let black = render_on(hc, w, h, 0)?;
    let white = render_on(hc, w, h, 255)?;
    let mut img = Image::new(w as u32, h as u32);
    let mut invert = vec![false; (w * h) as usize];
    for (i, inv_px) in invert.iter_mut().enumerate() {
        let p = i * 4;
        let mut a_sum = 0i32;
        let mut inv = false;
        for c in 0..3 {
            let (b, wh) = (black[p + c] as i32, white[p + c] as i32);
            if wh < b {
                inv = true;
            }
            a_sum += 255 - (wh - b).clamp(0, 255);
        }
        if inv {
            *inv_px = true;
            img.data[p..p + 4].copy_from_slice(&[0, 0, 0, 255]);
            continue;
        }
        let a = (a_sum / 3).clamp(0, 255) as u8;
        // On black, the rendered colour is already premultiplied by alpha.
        img.data[p..p + 4].copy_from_slice(&[black[p].min(a), black[p + 1].min(a), black[p + 2].min(a), a]);
    }
    // White outline around inverting pixels so they stay visible on dark backgrounds.
    if invert.iter().any(|&b| b) {
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if invert[i] || img.data[i * 4 + 3] != 0 {
                    continue;
                }
                let near = [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx >= 0 && ny >= 0 && nx < w && ny < h && invert[(ny * w + nx) as usize]
                });
                if near {
                    img.data[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    Some(CursorShape { image: img, hotspot: Point::new(ii.xHotspot as i32, ii.yHotspot as i32) })
}
