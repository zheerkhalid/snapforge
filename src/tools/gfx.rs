//! Drawing helpers shared by the screen tools.
//!
//! Every tool renders into a top-down 32-bpp DIB section ([`Dib`]) through tiny-skia.
//! tiny-skia stores RGBA, the DIB wants BGRA: instead of converting each frame, colors
//! are given to tiny-skia with red and blue swapped ([`col`]), so its bytes are already
//! BGRA and the DIB can go straight to `UpdateLayeredWindow` / `BitBlt`. Text comes from
//! GDI as a grayscale coverage mask that is blended in with the wanted color.

use crate::common::win::{pcwstr, wide};
use crate::common::{Image, Point, Rect};
use std::collections::HashMap;
use std::ffi::c_void;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GetDC, GetTextExtentPoint32W,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HFONT, HGDIOBJ, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    UpdateLayeredWindow, UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO,
};

// ---------------------------------------------------------------- DIB

/// A top-down 32-bpp DIB section selected into its own memory DC.
pub struct Dib {
    pub dc: HDC,
    bmp: HBITMAP,
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
        // SAFETY: valid BITMAPINFO; the bitmap and DC are owned by the returned Dib and
        // released in Drop. `bits` is valid for w*h*4 bytes while the bitmap lives.
        unsafe {
            let bmp = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(bmp);
                return None;
            }
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                let _ = DeleteObject(bmp);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Dib { dc, bmp, old, bits: bits as *mut u8, w, h })
        }
    }

    /// Reuse `cache` when it already has the size, else allocate.
    pub fn reuse(cache: Option<Dib>, w: i32, h: i32) -> Option<Dib> {
        match cache {
            Some(d) if d.w == w && d.h == h => Some(d),
            _ => Dib::new(w, h),
        }
    }

    pub fn len(&self) -> usize {
        self.w as usize * self.h as usize * 4
    }

    pub fn data(&self) -> &[u8] {
        // SAFETY: `bits` points to the DIB's w*h*4 bytes, alive as long as `self`.
        unsafe { std::slice::from_raw_parts(self.bits, self.len()) }
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` guarantees exclusive access from Rust. GDI only
        // touches the bits during calls made through `self.dc`, which need `&self`.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len()) }
    }

    /// tiny-skia view of the pixels (colors must come from [`col`]).
    pub fn pixmap(&mut self) -> Option<PixmapMut<'_>> {
        let (w, h) = (self.w as u32, self.h as u32);
        PixmapMut::from_bytes(self.data_mut(), w, h)
    }

    pub fn clear(&mut self) {
        self.data_mut().fill(0);
    }

    /// Copy as an [`Image`] (premultiplied BGRA), e.g. for previews.
    pub fn to_image(&self) -> Image {
        Image::from_bgra(self.w as u32, self.h as u32, self.data().to_vec())
    }

    /// Copy `img` (BGRA) to (x, y), clipped.
    pub fn blit_image(&mut self, img: &Image, x: i32, y: i32) {
        let dst = Rect::new(x, y, img.width as i32, img.height as i32).intersect(&Rect::new(0, 0, self.w, self.h));
        if dst.is_empty() {
            return;
        }
        let stride = self.w as usize * 4;
        let rs = dst.w as usize * 4;
        let data = self.data_mut();
        for row in 0..dst.h {
            let s = (((dst.y - y + row) as usize) * img.width as usize + (dst.x - x) as usize) * 4;
            let d = (dst.y + row) as usize * stride + dst.x as usize * 4;
            data[d..d + rs].copy_from_slice(&img.data[s..s + rs]);
        }
    }

    /// Copy a raw BGRA block of `w` x `h` to (x, y), clipped.
    pub fn blit_raw(&mut self, src: &[u8], w: i32, h: i32, x: i32, y: i32) {
        if src.len() < (w.max(0) * h.max(0) * 4) as usize {
            return;
        }
        let dst = Rect::new(x, y, w, h).intersect(&Rect::new(0, 0, self.w, self.h));
        if dst.is_empty() {
            return;
        }
        let stride = self.w as usize * 4;
        let rs = dst.w as usize * 4;
        let data = self.data_mut();
        for row in 0..dst.h {
            let s = (((dst.y - y + row) * w + (dst.x - x)) * 4) as usize;
            let d = (dst.y + row) as usize * stride + dst.x as usize * 4;
            data[d..d + rs].copy_from_slice(&src[s..s + rs]);
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: we own the DC and the bitmap; restore the original object before deleting.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bmp);
        }
    }
}

// ---------------------------------------------------------------- colors and shapes

/// A color for drawing into a BGRA [`Dib`] through tiny-skia (red and blue swapped).
pub fn col(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(b, g, r, a)
}

pub fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint { anti_alias: true, ..Default::default() };
    p.set_color(c);
    p
}

pub fn rrect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    // Cubic approximation of quarter circles (kappa).
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

pub fn fill(pm: &mut PixmapMut, path: Option<Path>, c: Color) {
    if let Some(p) = path {
        pm.fill_path(&p, &paint(c), FillRule::Winding, Transform::identity(), None);
    }
}

pub fn stroke(pm: &mut PixmapMut, path: Option<Path>, c: Color, width: f32) {
    if let Some(p) = path {
        let s = Stroke { width, ..Default::default() };
        pm.stroke_path(&p, &paint(c), &s, Transform::identity(), None);
    }
}

pub fn line(pm: &mut PixmapMut, a: (f32, f32), b: (f32, f32), c: Color, width: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(a.0, a.1);
    pb.line_to(b.0, b.1);
    stroke(pm, pb.finish(), c, width);
}

pub fn circle(x: f32, y: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(x, y, r.max(0.1))
}

/// A soft drop shadow under a rounded rect: a few expanding translucent layers.
pub fn shadow(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, r: f32, spread: f32) {
    let layers = 6;
    for i in 0..layers {
        let t = (i + 1) as f32 / layers as f32;
        let d = spread * t;
        let a = (26.0 * (1.0 - t) + 4.0) as u8;
        fill(pm, rrect(x - d, y - d + spread * 0.35, w + 2.0 * d, h + 2.0 * d, r + d), col(0, 0, 0, a));
    }
}

/// Fill an axis-aligned rect of the DIB with a premultiplied BGRA value (no blending).
pub fn fill_rect_raw(dib: &mut Dib, r: Rect, bgra: [u8; 4]) {
    let r = r.intersect(&Rect::new(0, 0, dib.w, dib.h));
    if r.is_empty() {
        return;
    }
    let stride = dib.w as usize * 4;
    let data = dib.data_mut();
    for y in r.y..r.bottom() {
        let row = &mut data[y as usize * stride + r.x as usize * 4..y as usize * stride + r.right() as usize * 4];
        for px in row.chunks_exact_mut(4) {
            px.copy_from_slice(&bgra);
        }
    }
}

/// Premultiply a straight color into BGRA bytes.
pub fn premul(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    let m = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
    [m(b), m(g), m(r), a]
}

// ---------------------------------------------------------------- text

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Ui,
    UiSemibold,
    Mono,
}

/// GDI text rendered as a coverage mask and blended into a DIB.
#[derive(Default)]
pub struct Text {
    mask: Option<Dib>,
    fonts: HashMap<(i32, Face), HFONT>,
}

impl Text {
    fn font(&mut self, px: i32, face: Face) -> HFONT {
        *self.fonts.entry((px, face)).or_insert_with(|| {
            let (name, weight) = match face {
                Face::Ui => ("Segoe UI", 400),
                Face::UiSemibold => ("Segoe UI Semibold", 600),
                Face::Mono => ("Consolas", 400),
            };
            let n = wide(name);
            // SAFETY: valid NUL-terminated face name; the font is deleted in Drop.
            // ANTIALIASED_QUALITY (4): grayscale AA, so any channel is a clean mask.
            unsafe { CreateFontW(-px.max(1), 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 4, 0, pcwstr(&n)) }
        })
    }

    fn ensure_mask(&mut self, w: i32, h: i32) -> Option<&mut Dib> {
        let need = match &self.mask {
            Some(m) => m.w < w || m.h < h,
            None => true,
        };
        if need {
            let (ow, oh) = self.mask.as_ref().map_or((0, 0), |m| (m.w, m.h));
            self.mask = Dib::new(w.max(ow).max(64), h.max(oh).max(16));
        }
        self.mask.as_mut()
    }

    /// Size of `s` in pixels for a font `px` pixels high.
    pub fn measure(&mut self, s: &str, px: i32, face: Face) -> (i32, i32) {
        let font = self.font(px, face);
        let Some(mask) = self.ensure_mask(1, 1) else {
            return (0, 0);
        };
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut sz = SIZE::default();
        // SAFETY: the font is selected into our own DC only for the measurement.
        unsafe {
            let old = SelectObject(mask.dc, font);
            let _ = GetTextExtentPoint32W(mask.dc, &w, &mut sz);
            SelectObject(mask.dc, old);
        }
        (sz.cx, sz.cy)
    }

    /// Draw `s` with its box anchored at `x` (per `align`) and top `y`. `c` is straight RGBA.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        dst: &mut Dib,
        s: &str,
        x: i32,
        y: i32,
        px: i32,
        face: Face,
        c: [u8; 4],
        align: Align,
    ) -> (i32, i32) {
        if s.is_empty() {
            return (0, 0);
        }
        let (tw, th) = self.measure(s, px, face);
        if tw <= 0 || th <= 0 {
            return (0, 0);
        }
        let x0 = match align {
            Align::Left => x,
            Align::Center => x - tw / 2,
            Align::Right => x - tw,
        };
        let font = self.font(px, face);
        let Some(mask) = self.ensure_mask(tw, th) else {
            return (0, 0);
        };
        let mstride = mask.w as usize * 4;
        {
            let data = mask.data_mut();
            for row in 0..th as usize {
                data[row * mstride..row * mstride + tw as usize * 4].fill(0);
            }
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: drawing into our own mask DC with a font we own.
        unsafe {
            let old = SelectObject(mask.dc, font);
            SetBkMode(mask.dc, TRANSPARENT);
            SetTextColor(mask.dc, COLORREF(0x00FF_FFFF));
            let _ = TextOutW(mask.dc, 0, 0, &w);
            SelectObject(mask.dc, old);
        }
        blend_mask(dst, mask.data(), mstride, Rect::new(x0, y, tw, th), c);
        (tw, th)
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        for (_, f) in self.fonts.drain() {
            // SAFETY: fonts we created; no longer selected anywhere.
            unsafe {
                let _ = DeleteObject(f);
            }
        }
    }
}

/// Source-over blend straight color `c` through coverage (green channel of `mask`).
fn blend_mask(dst: &mut Dib, mask: &[u8], mstride: usize, r: Rect, c: [u8; 4]) {
    let clip = r.intersect(&Rect::new(0, 0, dst.w, dst.h));
    if clip.is_empty() {
        return;
    }
    let stride = dst.w as usize * 4;
    let data = dst.data_mut();
    for y in clip.y..clip.bottom() {
        for x in clip.x..clip.right() {
            let mi = (y - r.y) as usize * mstride + (x - r.x) as usize * 4 + 1;
            let cov = mask.get(mi).copied().unwrap_or(0) as u32 * c[3] as u32 / 255;
            if cov == 0 {
                continue;
            }
            let di = y as usize * stride + x as usize * 4;
            let inv = 255 - cov;
            let src = [c[2], c[1], c[0]];
            for k in 0..3 {
                data[di + k] = ((src[k] as u32 * cov + data[di + k] as u32 * inv + 127) / 255) as u8;
            }
            data[di + 3] = (cov + (data[di + 3] as u32 * inv + 127) / 255).min(255) as u8;
        }
    }
}

// ---------------------------------------------------------------- windows

/// Show a per-pixel-alpha layered window at `pos` with the whole DIB.
pub fn present(hwnd: HWND, dib: &Dib, pos: Point) {
    let dst = POINT { x: pos.x, y: pos.y };
    let sz = SIZE { cx: dib.w, cy: dib.h };
    let src = POINT { x: 0, y: 0 };
    let blend = blend_fn();
    // SAFETY: valid pointers to locals; the DIB DC holds a premultiplied 32-bpp bitmap.
    unsafe {
        if let Err(e) = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&dst),
            Some(&sz),
            dib.dc,
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        ) {
            log::debug!("tools: UpdateLayeredWindow failed: {e}");
        }
    }
}

/// Show the part of `dib` starting at `src` as a layered window covering `dst`.
pub fn present_sub(hwnd: HWND, dib: &Dib, src: Point, dst: Rect) {
    if dst.is_empty() {
        return;
    }
    let pd = POINT { x: dst.x, y: dst.y };
    let sz = SIZE { cx: dst.w, cy: dst.h };
    let ps = POINT { x: src.x, y: src.y };
    let blend = blend_fn();
    // SAFETY: valid pointers to locals; the source rect lies inside the DIB (caller).
    unsafe {
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&pd),
            Some(&sz),
            dib.dc,
            Some(&ps),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

/// Update only `dirty` (client coordinates) of a layered window whose size and position
/// are unchanged since the last full [`present`].
pub fn present_dirty(hwnd: HWND, dib: &Dib, pos: Point, dirty: Rect) {
    let dirty = dirty.intersect(&Rect::new(0, 0, dib.w, dib.h));
    if dirty.is_empty() {
        return;
    }
    let dst = POINT { x: pos.x, y: pos.y };
    let sz = SIZE { cx: dib.w, cy: dib.h };
    let src = POINT { x: 0, y: 0 };
    let blend = blend_fn();
    let rc = crate::common::win::rect_to_win(&dirty);
    let info = UPDATELAYEREDWINDOWINFO {
        cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
        hdcDst: HDC::default(),
        pptDst: &dst,
        psize: &sz,
        hdcSrc: dib.dc,
        pptSrc: &src,
        crKey: COLORREF(0),
        pblend: &blend,
        dwFlags: ULW_ALPHA,
        prcDirty: &rc,
    };
    // SAFETY: every pointer in `info` refers to a local that outlives the call.
    unsafe {
        if !UpdateLayeredWindowIndirect(hwnd, &info).as_bool() {
            present(hwnd, dib, pos);
        }
    }
}

fn blend_fn() -> BLENDFUNCTION {
    BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    }
}

// ---------------------------------------------------------------- live screen pixels

/// Live screen pixels via GDI `BitBlt` from the screen DC. Our own tool windows are
/// excluded from capture (`WDA_EXCLUDEFROMCAPTURE`), so they never show up here.
#[derive(Default)]
pub struct Grabber {
    dib: Option<Dib>,
}

impl Grabber {
    /// Grab `r` (virtual-screen coordinates) into an opaque image.
    pub fn grab(&mut self, r: Rect) -> Option<Image> {
        self.grab_dib(r)?;
        let dib = self.dib.as_ref()?;
        let mut img = Image::from_bgra(r.w as u32, r.h as u32, dib.data().to_vec());
        img.set_opaque();
        Some(img)
    }

    /// Grab `r` into the internal DIB and return it (valid until the next grab).
    pub fn grab_dib(&mut self, r: Rect) -> Option<&mut Dib> {
        if r.is_empty() {
            return None;
        }
        let dib = Dib::reuse(self.dib.take(), r.w, r.h)?;
        // SAFETY: the screen DC is released before returning; BitBlt writes into our DIB.
        let ok = unsafe {
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                false
            } else {
                let ok = BitBlt(dib.dc, 0, 0, r.w, r.h, screen, r.x, r.y, SRCCOPY | CAPTUREBLT).is_ok();
                ReleaseDC(HWND::default(), screen);
                ok
            }
        };
        self.dib = Some(dib);
        let dib = self.dib.as_mut()?;
        if !ok {
            // Off-screen or secure desktop: show black rather than stale pixels.
            dib.clear();
        }
        for px in dib.data_mut().chunks_exact_mut(4) {
            px[3] = 255;
        }
        Some(dib)
    }

    pub fn release(&mut self) {
        self.dib = None;
    }

    /// The DIB of the last grab.
    pub fn into_dib(self) -> Option<Dib> {
        self.dib
    }
}

/// The color of one screen pixel.
pub fn screen_pixel(p: Point) -> Option<[u8; 4]> {
    let mut g = Grabber::default();
    let img = g.grab(Rect::new(p.x, p.y, 1, 1))?;
    Some(img.pixel(0, 0))
}
