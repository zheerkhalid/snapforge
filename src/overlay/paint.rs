//! GDI renderer for overlay scenes: the frozen image and its dimmed copy live in DIB
//! sections selected into memory DCs; invalid rects are composed in a small reusable
//! backbuffer and blitted to the window.

use super::loupe;
use super::scene::{Env, Font, Prim, Scene};
use crate::common::win::{pcwstr, wide};
use crate::common::{Image, Point, Rect};
use std::collections::HashMap;
use std::ffi::c_void;
use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreatePen, DeleteDC, DeleteObject, DrawTextW, FillRect,
    GetStockObject, GetTextExtentPoint32W, IntersectClipRect, LineTo, MoveToEx, RestoreDC, RoundRect, SaveDC,
    SelectObject, SetBkMode, SetDCBrushColor, SetDCPenColor, SetTextColor, SetViewportOrgEx, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DC_BRUSH, DC_PEN, DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, HPEN, PS_SOLID, SRCCOPY, TRANSPARENT,
};

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

    pub fn to_image(&self) -> Image {
        Image::from_bgra(self.w as u32, self.h as u32, self.data().to_vec())
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

/// Copy `src` into `bright` and its dimmed copy (same formula as [`Image::dimmed`])
/// into `dim`, split across threads: this is the bulk of the time-to-first-paint.
fn load_pixels(src: &[u8], bright: &mut [u8], dim: &mut [u8], width: usize) {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let rows = src.len() / (width * 4).max(1);
    let chunk = rows.div_ceil(threads).max(1) * width * 4;
    let job = |s: &[u8], b: &mut [u8], d: &mut [u8]| {
        b.copy_from_slice(s);
        for (o, &c) in d.iter_mut().zip(s) {
            *o = ((c as u32 * super::scene::DIM) >> 8) as u8;
        }
    };
    if threads == 1 || src.len() < 1 << 20 {
        job(src, bright, dim);
        return;
    }
    std::thread::scope(|sc| {
        for ((s, b), d) in src.chunks(chunk).zip(bright.chunks_mut(chunk)).zip(dim.chunks_mut(chunk)) {
            sc.spawn(move || job(s, b, d));
        }
    });
}

pub struct Canvas {
    /// Virtual-screen rect the images cover.
    pub origin: Rect,
    bright: Dib,
    dim: Dib,
    back: Option<Dib>,
    loupe: Option<(Dib, Point, i32)>,
    scratch: HDC,
    fonts: HashMap<Font, HFONT>,
    pens: HashMap<(u32, i32), HPEN>,
}

impl Canvas {
    /// Prepare the canvas for `img`, reusing `cache`'s bitmaps when the size matches.
    pub fn load(cache: Option<Canvas>, img: &Image, origin: Rect) -> Option<Canvas> {
        let (w, h) = (img.width as i32, img.height as i32);
        let mut c = match cache {
            Some(c) if c.bright.w == w && c.bright.h == h => c,
            _ => {
                // SAFETY: plain memory DC creation; released in Drop.
                let scratch = unsafe { CreateCompatibleDC(None) };
                Canvas {
                    origin,
                    bright: Dib::new(w, h)?,
                    dim: Dib::new(w, h)?,
                    back: None,
                    loupe: None,
                    scratch,
                    fonts: HashMap::new(),
                    pens: HashMap::new(),
                }
            }
        };
        c.origin = origin;
        c.loupe = None;
        if img.data.len() != c.bright.len() {
            return None;
        }
        let (bright, dim) = (&mut c.bright, &mut c.dim);
        load_pixels(&img.data, bright.data_mut(), dim.data_mut(), img.width as usize);
        Some(c)
    }

    fn font(&mut self, f: Font) -> HFONT {
        if let Some(h) = self.fonts.get(&f) {
            return *h;
        }
        let face = wide("Segoe UI");
        // SAFETY: valid NUL-terminated face name; the font is deleted in Drop.
        let h = unsafe {
            CreateFontW(
                -f.px.max(1),
                0,
                0,
                0,
                if f.bold { 600 } else { 400 },
                0,
                0,
                0,
                1, // DEFAULT_CHARSET
                0, // OUT_DEFAULT_PRECIS
                0, // CLIP_DEFAULT_PRECIS
                5, // CLEARTYPE_QUALITY
                0, // DEFAULT_PITCH
                pcwstr(&face),
            )
        };
        self.fonts.insert(f, h);
        h
    }

    fn pen(&mut self, color: u32, width: i32) -> HPEN {
        // SAFETY: plain pen creation; deleted in Drop.
        *self.pens.entry((color, width)).or_insert_with(|| unsafe { CreatePen(PS_SOLID, width, COLORREF(color)) })
    }

    fn render_loupe(&mut self, center: Point, cell: i32) -> Option<HDC> {
        if let Some((d, c, k)) = &self.loupe {
            if *c == center && *k == cell {
                return Some(d.dc);
            }
        }
        let side = loupe::CELLS * cell;
        let mut dib = match self.loupe.take() {
            Some((d, _, _)) if d.w == side => d,
            _ => Dib::new(side, side)?,
        };
        let src = loupe::Src { data: self.bright.data(), width: self.bright.w as u32, height: self.bright.h as u32 };
        let c = Point::new(center.x - self.origin.x, center.y - self.origin.y);
        loupe::render(src, c, cell, dib.data_mut());
        let dc = dib.dc;
        self.loupe = Some((dib, center, cell));
        Some(dc)
    }

    /// Draw `scene` restricted to `clip` (virtual coords) onto `hdc`, where device
    /// pixel (0, 0) corresponds to virtual point `vp`.
    pub fn draw(&mut self, hdc: HDC, vp: Point, clip: Rect, scene: &Scene) {
        let clip = clip.intersect(&self.origin);
        if clip.is_empty() {
            return;
        }
        let o = self.origin;
        // SAFETY: all handles are valid GDI objects owned by this canvas or the caller;
        // DC state is saved and restored around the drawing.
        unsafe {
            let saved = SaveDC(hdc);
            let _ = SetViewportOrgEx(hdc, -vp.x, -vp.y, None);
            IntersectClipRect(hdc, clip.x, clip.y, clip.right(), clip.bottom());
            let _ = BitBlt(hdc, clip.x, clip.y, clip.w, clip.h, self.dim.dc, clip.x - o.x, clip.y - o.y, SRCCOPY);
            for b in &scene.bright {
                let r = b.intersect(&clip);
                if !r.is_empty() {
                    let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, self.bright.dc, r.x - o.x, r.y - o.y, SRCCOPY);
                }
            }
            SetBkMode(hdc, TRANSPARENT);
            let dc_brush = HBRUSH(GetStockObject(DC_BRUSH).0);
            for p in &scene.prims {
                if p.bounds().intersect(&clip).is_empty() {
                    continue;
                }
                match p {
                    Prim::Fill { r, color } => {
                        SetDCBrushColor(hdc, COLORREF(*color));
                        FillRect(hdc, &rect(r), dc_brush);
                    }
                    Prim::Panel { r, color, border, radius } => {
                        SelectObject(hdc, GetStockObject(DC_BRUSH));
                        SelectObject(hdc, GetStockObject(DC_PEN));
                        SetDCBrushColor(hdc, COLORREF(*color));
                        SetDCPenColor(hdc, COLORREF(*border));
                        let _ = RoundRect(hdc, r.x, r.y, r.right(), r.bottom(), *radius, *radius);
                    }
                    Prim::Text { r, text, color, font } => {
                        let f = self.font(*font);
                        SelectObject(hdc, f);
                        SetTextColor(hdc, COLORREF(*color));
                        let mut buf: Vec<u16> = text.encode_utf16().collect();
                        let mut rc = rect(r);
                        DrawTextW(
                            hdc,
                            &mut buf,
                            &mut rc,
                            DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX | DT_END_ELLIPSIS,
                        );
                    }
                    Prim::Line { a, b, color, width } => {
                        let pen = self.pen(*color, *width);
                        SelectObject(hdc, pen);
                        let _ = MoveToEx(hdc, a.x, a.y, None);
                        let _ = LineTo(hdc, b.x, b.y);
                    }
                    Prim::Loupe { r, center, cell } => {
                        if let Some(src) = self.render_loupe(*center, *cell) {
                            let _ = BitBlt(hdc, r.x, r.y, r.w, r.h, src, 0, 0, SRCCOPY);
                        }
                    }
                }
            }
            let _ = RestoreDC(hdc, saved);
        }
    }

    /// Paint `r` (virtual coords) to the window DC through the backbuffer.
    pub fn paint_buffered(&mut self, window_dc: HDC, r: Rect, scene: &Scene) {
        let r = r.intersect(&self.origin);
        if r.is_empty() {
            return;
        }
        let fits = self.back.as_ref().is_some_and(|b| b.w >= r.w && b.h >= r.h);
        if !fits {
            let (w, h) = self.back.as_ref().map_or((0, 0), |b| (b.w, b.h));
            self.back = Dib::new(w.max(r.w), h.max(r.h));
        }
        let Some(back_dc) = self.back.as_ref().map(|b| b.dc) else {
            // Out of memory for a backbuffer: draw directly.
            self.draw(window_dc, Point::new(self.origin.x, self.origin.y), r, scene);
            return;
        };
        self.draw(back_dc, Point::new(r.x, r.y), r, scene);
        // SAFETY: both DCs are valid; the window DC is in client coordinates.
        unsafe {
            let _ = BitBlt(window_dc, r.x - self.origin.x, r.y - self.origin.y, r.w, r.h, back_dc, 0, 0, SRCCOPY);
        }
    }

    /// Render the whole scene into a new image (visual checks, tests).
    pub fn render_image(&mut self, scene: &Scene) -> Option<Image> {
        let dib = Dib::new(self.origin.w, self.origin.h)?;
        self.draw(dib.dc, Point::new(self.origin.x, self.origin.y), self.origin, scene);
        let mut img = dib.to_image();
        img.set_opaque();
        Some(img)
    }

    /// Bytes held by the cached bitmaps.
    pub fn memory_bytes(&self) -> usize {
        self.bright.len() + self.dim.len() + self.back.as_ref().map_or(0, |b| b.len())
    }
}

impl Env for Canvas {
    fn text_size(&mut self, text: &str, font: Font) -> (i32, i32) {
        let f = self.font(font);
        let buf: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        // SAFETY: scratch DC and font are valid; buffer length is passed via the slice.
        unsafe {
            SelectObject(self.scratch, f);
            let _ = GetTextExtentPoint32W(self.scratch, &buf, &mut sz);
        }
        (sz.cx, sz.cy)
    }

    fn pixel(&self, p: Point) -> Option<[u8; 4]> {
        let src = loupe::Src { data: self.bright.data(), width: self.bright.w as u32, height: self.bright.h as u32 };
        src.pixel(p.x - self.origin.x, p.y - self.origin.y)
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: fonts/pens/scratch DC were created by this canvas and are not selected
        // into any DC that outlives it (draw() restores the DC state).
        unsafe {
            let _ = DeleteDC(self.scratch);
            for f in self.fonts.values() {
                let _ = DeleteObject(*f);
            }
            for p in self.pens.values() {
                let _ = DeleteObject(*p);
            }
        }
    }
}

fn rect(r: &Rect) -> RECT {
    crate::common::win::rect_to_win(r)
}
