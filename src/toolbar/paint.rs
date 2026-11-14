//! Toolbar rendering: tiny-skia into a 32-bit DIB back buffer, icons from cached
//! premultiplied BGRA DIBs via `AlphaBlend`, badge digits with GDI text.

use super::icons::{self, Icon, Palette};
use super::layout::{Button, Layout, Theme};
use crate::common::win::wide;
use crate::settings::Destination;
use std::collections::HashMap;
use std::ffi::c_void;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, PixmapMut, Rect as SkRect, Transform};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FW_BOLD,
    HBITMAP, HDC, HGDIOBJ, OUT_TT_PRECIS, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL, HICON};

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
        // SAFETY: valid BITMAPINFO; the section and DC are released in Drop.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let hbm = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(h) if !bits.is_null() => h,
                _ => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let old = SelectObject(dc, hbm);
            Some(Dib { dc, hbm, old, bits: bits as *mut u8, w, h })
        }
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the DIB section holds w*h*4 bytes for as long as `self` lives; GDI work
        // is flushed before CPU access.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize)
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: we own the DC and bitmap; the original object is restored first.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.hbm);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Icons rendered once per (icon, pixel size, theme).
#[derive(Default)]
pub struct IconCache {
    map: HashMap<(Icon, i32, bool), Dib>,
}

impl IconCache {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    fn get(&mut self, icon: Icon, size: i32, dark: bool) -> Option<&Dib> {
        use std::collections::hash_map::Entry;
        match self.map.entry((icon, size, dark)) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(e) => {
                let pm = icons::render(icon, size as u32, Palette::for_theme(dark))?;
                let mut dib = Dib::new(size, size)?;
                dib.bytes().copy_from_slice(&icons::to_bgra(&pm));
                Some(e.insert(dib))
            }
        }
    }
}

/// Everything the painter needs to know about the current state.
pub struct View<'a> {
    pub layout: &'a Layout,
    pub theme: Theme,
    pub hover: Option<usize>,
    pub pressed: Option<usize>,
    pub dest: Destination,
    pub delay: u32,
    pub app_icon: HICON,
}

/// The DIB stores BGRA; tiny-skia writes RGBA, so colours are passed with R and B swapped.
fn bgra(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[2], c[1], c[0], c[3])
}

fn fill_paint(c: [u8; 4]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(bgra(c));
    p.anti_alias = true;
    p
}

pub fn icon_for(button: Button, dest: Destination) -> Icon {
    match button {
        Button::Mode(m) => Icon::Mode(m),
        Button::Output => Icon::Dest(dest),
        Button::Delay => Icon::Delay,
        Button::Menu => Icon::Menu,
        Button::Minimize => Icon::Minimize,
    }
}

/// Icon box (x, y) inside an item rect.
fn icon_origin(l: &Layout, it: &super::layout::Item) -> (i32, i32) {
    let btn = it.rect.h;
    let x = it.rect.x + (btn.min(it.rect.w) - l.icon) / 2;
    let y = it.rect.y + (it.rect.h - l.icon) / 2;
    (x, y)
}

/// Delay badge circle/pill: (cx, cy, half width, radius).
fn badge_geom(l: &Layout, it: &super::layout::Item, digits: usize) -> (f32, f32, f32, f32) {
    let (ix, iy) = icon_origin(l, it);
    let r = 6.5 * l.scale * if it.rect.h > (34.0 * l.scale) as i32 { 1.15 } else { 1.0 };
    let half = if digits > 1 { r * 1.45 } else { r };
    let cx = ix as f32 + l.icon as f32 - half * 0.55;
    let cy = iy as f32 + r * 0.6;
    (cx, cy, half, r)
}

fn pill(cx: f32, cy: f32, half: f32, r: f32) -> Option<tiny_skia::Path> {
    if half <= r + 0.01 {
        return PathBuilder::from_circle(cx, cy, r);
    }
    icons::rrect(cx - half, cy - r, cx + half, cy + r, r)
}

/// Paint the whole toolbar into `dib` (which must be layout-sized).
pub fn render(dib: &mut Dib, v: &View, cache: &mut IconCache) {
    let l = v.layout;
    let th = v.theme;
    let (w, h) = (dib.w as u32, dib.h as u32);
    {
        let Some(mut pm) = PixmapMut::from_bytes(dib.bytes(), w, h) else { return };
        pm.fill(bgra(th.bg));
        // Hover / pressed highlights.
        for (i, it) in l.items.iter().enumerate() {
            let col = if v.pressed == Some(i) {
                th.pressed
            } else if v.hover == Some(i) {
                th.hover
            } else {
                continue;
            };
            let r = &it.rect;
            if let Some(p) = icons::rrect(r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32, l.corner) {
                pm.fill_path(&p, &fill_paint(col), FillRule::Winding, Transform::identity(), None);
            }
        }
        // Grip dots (2 × 3).
        let dot = fill_paint(th.grip);
        for row in 0..3 {
            for col in 0..2 {
                let cx = l.dots.x as f32 + l.dot_radius + col as f32 * l.dot_pitch;
                let cy = l.dots.y as f32 + row as f32 * l.dot_pitch;
                if let Some(p) = PathBuilder::from_circle(cx, cy, l.dot_radius) {
                    pm.fill_path(&p, &dot, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        // Separators.
        let sep = fill_paint(th.separator);
        for &x in &l.separators {
            if let Some(r) = SkRect::from_xywh(x as f32, l.sep_top as f32, 1.0, (l.sep_bottom - l.sep_top) as f32) {
                pm.fill_rect(r, &sep, Transform::identity(), None);
            }
        }
        // Output drop-down chevron.
        if let Some(it) = l.items.iter().find(|i| i.button == Button::Output) {
            let s = l.scale;
            let cx = it.rect.right() as f32 - 8.0 * s;
            let cy = it.rect.y as f32 + it.rect.h as f32 / 2.0 + 0.5 * s;
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 3.0 * s, cy - 1.5 * s);
            pb.line_to(cx + 3.0 * s, cy - 1.5 * s);
            pb.line_to(cx, cy + 1.75 * s);
            pb.close();
            if let Some(p) = pb.finish() {
                let mut c = th.fg;
                c[3] = 200;
                pm.fill_path(&p, &fill_paint(c), FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    // Icons.
    let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
    for it in &l.items {
        let icon = icon_for(it.button, v.dest);
        let (x, y) = icon_origin(l, it);
        if let Some(src) = cache.get(icon, l.icon, th.dark) {
            // SAFETY: both DCs are valid memory DCs with DIB sections selected.
            unsafe {
                let _ = AlphaBlend(dib.dc, x, y, l.icon, l.icon, src.dc, 0, 0, l.icon, l.icon, blend);
            }
        }
    }
    if !v.app_icon.is_invalid() {
        let r = &l.app_icon;
        // SAFETY: valid DC and icon handle.
        unsafe {
            let _ = DrawIconEx(dib.dc, r.x, r.y, v.app_icon, r.w, r.h, 0, None, DI_NORMAL);
        }
    }

    // Delay badge.
    if v.delay > 0 {
        if let Some(it) = l.items.iter().find(|i| i.button == Button::Delay) {
            let text = v.delay.to_string();
            let (cx, cy, half, r) = badge_geom(l, it, text.len());
            {
                let Some(mut pm) = PixmapMut::from_bytes(dib.bytes(), w, h) else { return };
                let base = if v.pressed.is_some_and(|i| l.items[i].button == Button::Delay) {
                    th.pressed
                } else if v.hover.is_some_and(|i| l.items[i].button == Button::Delay) {
                    th.hover
                } else {
                    [0, 0, 0, 0]
                };
                // Ring in the background colour so the badge reads as "on top" of the icon.
                let ring = 1.2 * l.scale;
                if let Some(p) = pill(cx, cy, half + ring, r + ring) {
                    pm.fill_path(&p, &fill_paint(th.bg), FillRule::Winding, Transform::identity(), None);
                    if base[3] > 0 {
                        pm.fill_path(&p, &fill_paint(base), FillRule::Winding, Transform::identity(), None);
                    }
                }
                if let Some(p) = pill(cx, cy, half, r) {
                    pm.fill_path(&p, &fill_paint(th.badge), FillRule::Winding, Transform::identity(), None);
                }
            }
            let face = wide("Segoe UI");
            let height = -((r * 1.5).round() as i32);
            // SAFETY: font created and deleted here; valid DC and buffers.
            unsafe {
                let font = CreateFontW(
                    height,
                    0,
                    0,
                    0,
                    FW_BOLD.0 as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET.0 as u32,
                    OUT_TT_PRECIS.0 as u32,
                    CLIP_DEFAULT_PRECIS.0 as u32,
                    ANTIALIASED_QUALITY.0 as u32,
                    0,
                    PCWSTR(face.as_ptr()),
                );
                let old = SelectObject(dib.dc, font);
                let c = th.badge_text;
                SetTextColor(dib.dc, COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16));
                SetBkMode(dib.dc, TRANSPARENT);
                let mut rc = RECT {
                    left: (cx - half - 2.0).floor() as i32,
                    top: (cy - r).floor() as i32,
                    right: (cx + half + 2.0).ceil() as i32,
                    bottom: (cy + r).ceil() as i32 + 1,
                };
                let mut t: Vec<u16> = text.encode_utf16().collect();
                DrawTextW(dib.dc, &mut t, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
                SelectObject(dib.dc, old);
                let _ = DeleteObject(font);
            }
        }
    }
    // GDI text leaves alpha at 0; the window ignores alpha, but keep the buffer opaque
    // for previews.
    for px in dib.bytes().chunks_exact_mut(4) {
        px[3] = 255;
    }
}
