//! Mouse cursor compositing for backends that do not capture it (GDI, DXGI).
//!
//! The cursor is drawn with `DrawIconEx` onto the real captured pixels, so inverting
//! cursors (the I-beam, XOR masks) look exactly as they do on screen.

use super::gdi::{DibDc, ScreenDc};
use crate::common::{Image, Rect};
use std::ffi::c_void;
use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, BITMAP, HDC};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HICON, ICONINFO,
};

/// The visible cursor: handle, top-left drawing position (screen) and size.
struct CursorShot {
    icon: HICON,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn current() -> Option<CursorShot> {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    // SAFETY: valid out pointers; the bitmaps returned by GetIconInfo are owned by us and deleted.
    unsafe {
        GetCursorInfo(&mut ci).ok()?;
        if ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
            return None;
        }
        let icon = HICON(ci.hCursor.0);
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let mut bm = BITMAP::default();
        let (w, h) = if !ii.hbmColor.is_invalid() {
            GetObjectW(ii.hbmColor, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
            (bm.bmWidth, bm.bmHeight)
        } else {
            // Monochrome cursors stack the AND and XOR masks vertically.
            GetObjectW(ii.hbmMask, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
            (bm.bmWidth, bm.bmHeight / 2)
        };
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor);
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask);
        }
        let (w, h) = if w > 0 && h > 0 { (w, h) } else { (32, 32) };
        Some(CursorShot {
            icon,
            x: ci.ptScreenPos.x - ii.xHotspot as i32,
            y: ci.ptScreenPos.y - ii.yHotspot as i32,
            w,
            h,
        })
    }
}

/// Draw the cursor onto `dc`, whose (0, 0) is screen point (`origin_x`, `origin_y`).
pub fn draw_on_dc(dc: HDC, origin_x: i32, origin_y: i32) {
    let Some(c) = current() else { return };
    // SAFETY: valid DC and cursor handle; size 0 draws at the cursor's own size.
    unsafe {
        let _ = DrawIconEx(dc, c.x - origin_x, c.y - origin_y, c.icon, 0, 0, 0, None, DI_NORMAL);
    }
}

/// Composite the cursor onto `img`, whose top-left pixel is at screen point `origin`.
pub fn composite(img: &mut Image, origin_x: i32, origin_y: i32) {
    let Some(c) = current() else { return };
    let cur = Rect::new(c.x - origin_x, c.y - origin_y, c.w, c.h);
    let vis = cur.intersect(&img.bounds());
    if vis.is_empty() {
        return;
    }
    let Ok(screen) = ScreenDc::get() else { return };
    let Ok(mut dib) = DibDc::new(screen.0, c.w, c.h) else { return };
    // Copy the background under the cursor into the small DIB, draw, copy back.
    let bg = img.crop(vis);
    let (ox, oy) = (vis.x - cur.x, vis.y - cur.y);
    copy_into(dib.pixels_mut(), c.w, &bg, ox, oy);
    // SAFETY: valid DC and cursor handle.
    unsafe {
        let _ = DrawIconEx(dib.dc, 0, 0, c.icon, 0, 0, 0, None, DI_NORMAL);
    }
    let px = dib.pixels();
    let mut patch = Image::new(vis.w as u32, vis.h as u32);
    for row in 0..vis.h as usize {
        let s = ((oy as usize + row) * c.w as usize + ox as usize) * 4;
        let d = row * vis.w as usize * 4;
        patch.data[d..d + vis.w as usize * 4].copy_from_slice(&px[s..s + vis.w as usize * 4]);
    }
    patch.set_opaque();
    img.blit(&patch, vis.x, vis.y);
}

fn copy_into(dst: &mut [u8], dst_w: i32, src: &Image, x: i32, y: i32) {
    let rs = src.width as usize * 4;
    for row in 0..src.height as usize {
        let d = ((y as usize + row) * dst_w as usize + x as usize) * 4;
        dst[d..d + rs].copy_from_slice(src.row(row as u32));
    }
}
