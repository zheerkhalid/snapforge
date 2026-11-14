//! Draw Mode annotations: pen strokes, GDI text and numbered step stamps, rendered with
//! tiny-skia, plus a byte-capped undo history of image snapshots.
//!
//! Buffers are premultiplied BGRA. tiny-skia thinks in RGBA, so colors are passed with
//! red and blue swapped ([`sk_color`]); blending is per-channel, so the result is exact.

use super::gdi;
use crate::common::Image;
use tiny_skia::{
    Color, FillRule, FilterQuality, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, PixmapRef,
    Stroke, Transform,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Pen,
    Text,
    Step,
}

/// Preset colors offered by the palette (0xRRGGBB).
pub const COLORS: [u32; 6] = [0xE5_39_35, 0xFB_8C_00, 0xFD_D8_35, 0x43_A0_47, 0x1E_88_E5, 0x21_21_21];

/// Color + size preset (0 = S, 1 = M, 2 = L).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: u32,
    pub size: u8,
}

impl Style {
    fn idx(self) -> usize {
        (self.size as usize).min(2)
    }
    /// Pen width in image pixels.
    pub fn pen_width(self) -> f32 {
        [2.0, 4.0, 8.0][self.idx()]
    }
    /// Text height in image pixels.
    pub fn text_px(self) -> i32 {
        [16, 24, 36][self.idx()]
    }
    /// Step stamp radius in image pixels.
    pub fn stamp_radius(self) -> f32 {
        [11.0, 15.0, 22.0][self.idx()]
    }
}

/// tiny-skia color for a BGRA buffer from 0xRRGGBB.
pub fn sk_color(rgb: u32, a: u8) -> Color {
    let r = (rgb >> 16) as u8;
    let g = (rgb >> 8) as u8;
    let b = rgb as u8;
    Color::from_rgba8(b, g, r, a)
}

pub fn bgr(rgb: u32) -> [u8; 3] {
    [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8]
}

/// A mutable tiny-skia view of an image (BGRA bytes treated as RGBA).
pub fn pixmap_mut(img: &mut Image) -> Option<PixmapMut<'_>> {
    PixmapMut::from_bytes(&mut img.data, img.width, img.height)
}

pub fn pixmap_ref(img: &Image) -> Option<PixmapRef<'_>> {
    PixmapRef::from_bytes(&img.data, img.width, img.height)
}

fn paint(rgb: u32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(sk_color(rgb, 255));
    p.anti_alias = true;
    p
}

/// Freehand stroke through `pts` (image coordinates), drawn with transform `ts`.
pub fn stroke(pm: &mut PixmapMut, pts: &[(f32, f32)], rgb: u32, width: f32, ts: Transform) {
    let Some(&(x0, y0)) = pts.first() else { return };
    if pts.len() == 1 {
        // A click draws a dot.
        if let Some(path) = PathBuilder::from_circle(x0, y0, width / 2.0) {
            pm.fill_path(&path, &paint(rgb), FillRule::Winding, ts, None);
        }
        return;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    // Smooth with quadratic segments through midpoints.
    for w in pts.windows(2).skip(1) {
        let (a, b) = (w[0], w[1]);
        pb.quad_to(a.0, a.1, (a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    }
    if let Some(&(xl, yl)) = pts.last() {
        pb.line_to(xl, yl);
    }
    if let Some(path) = pb.finish() {
        let st = Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
        pm.stroke_path(&path, &paint(rgb), &st, ts, None);
    }
}

/// Draw `src` with its top-left at (x, y) in image coordinates.
pub fn draw_image(pm: &mut PixmapMut, src: &Image, x: f32, y: f32, ts: Transform, quality: FilterQuality) {
    if let Some(p) = pixmap_ref(src) {
        let pp = PixmapPaint { quality, ..Default::default() };
        pm.draw_pixmap(0, 0, p, &pp, ts.pre_translate(x, y), None);
    }
}

/// Rendered text (colorized), or `None` for empty text.
pub fn text_image(text: &str, rgb: u32, px: i32) -> Option<Image> {
    gdi::text_mask(text, px, false).map(|m| gdi::colorize(&m, bgr(rgb), 255))
}

/// Draw text with its top-left at (x, y). The text box's top-left is where the user clicked.
pub fn text(pm: &mut PixmapMut, x: f32, y: f32, s: &str, rgb: u32, px: i32, ts: Transform) {
    if let Some(img) = text_image(s, rgb, px) {
        // `text_mask` pads by max(px/8, 2) on the left.
        let pad = (px / 8).max(2) as f32;
        draw_image(pm, &img, x - pad, y, ts, FilterQuality::Bilinear);
    }
}

/// Width of `s` in pixels as rendered by [`text`] (for the caret).
pub fn text_width(s: &str, px: i32) -> f32 {
    match gdi::text_mask(s, px, false) {
        Some(m) => m.w as f32 - 2.0 * (px / 8).max(2) as f32,
        None => 0.0,
    }
}

/// Numbered step stamp centered at (cx, cy): filled circle, thin white ring, white number.
pub fn step(pm: &mut PixmapMut, cx: f32, cy: f32, n: u32, rgb: u32, radius: f32, ts: Transform) {
    if let Some(c) = PathBuilder::from_circle(cx, cy, radius) {
        pm.fill_path(&c, &paint(rgb), FillRule::Winding, ts, None);
        let st = Stroke { width: (radius / 8.0).max(1.5), ..Default::default() };
        pm.stroke_path(&c, &paint(0xFF_FF_FF), &st, ts, None);
    }
    let label = n.to_string();
    let px = if label.len() > 2 { radius * 0.9 } else { radius * 1.2 } as i32;
    if let Some(m) = gdi::text_mask(&label, px.max(6), true) {
        let img = gdi::colorize(&m, [255, 255, 255], 255);
        // Centre the ink box, not the padded box.
        let (ix, iy) = ink_center(&m);
        draw_image(pm, &img, cx - ix, cy - iy, ts, FilterQuality::Bilinear);
    }
}

/// Centre of the inked area of a mask (falls back to the mask centre).
fn ink_center(m: &gdi::Mask) -> (f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..m.h {
        for x in 0..m.w {
            if m.data[(y * m.w + x) as usize] > 96 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 > x1 {
        return (m.w as f32 / 2.0, m.h as f32 / 2.0);
    }
    ((x0 + x1 + 1) as f32 / 2.0, (y0 + y1 + 1) as f32 / 2.0)
}

/// A pixmap the size of `img` holding a copy (for previews in tests).
pub fn to_pixmap(img: &Image) -> Option<Pixmap> {
    let mut p = Pixmap::new(img.width, img.height)?;
    p.data_mut().copy_from_slice(&img.data);
    Some(p)
}

/// One undo step: the image before an edit, the step number then, and how far the canvas
/// origin moved (image pixels) so undo can move the pin back.
pub struct Snap {
    pub img: Image,
    pub next_step: u32,
    pub shift: (i32, i32),
}

/// Undo history: whole-image snapshots, capped by bytes.
#[derive(Default)]
pub struct History {
    snaps: Vec<Snap>,
    bytes: usize,
}

/// Snapshots beyond this total are dropped oldest first (the newest one is always kept).
const HISTORY_BYTES: usize = 256 * 1024 * 1024;

impl History {
    pub fn push(&mut self, snap: Snap) {
        self.bytes += snap.img.data.len();
        self.snaps.push(snap);
        while self.bytes > HISTORY_BYTES && self.snaps.len() > 1 {
            let old = self.snaps.remove(0);
            self.bytes -= old.img.data.len();
        }
    }

    pub fn pop(&mut self) -> Option<Snap> {
        let s = self.snaps.pop()?;
        self.bytes -= s.img.data.len();
        Some(s)
    }

    pub fn len(&self) -> usize {
        self.snaps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snaps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_draws_in_bgra_order() {
        let mut img = Image::filled(40, 40, [255, 255, 255, 255]);
        {
            let mut pm = pixmap_mut(&mut img).expect("pixmap");
            stroke(&mut pm, &[(5.0, 20.0), (20.0, 20.0), (35.0, 20.0)], 0xFF_00_00, 6.0, Transform::identity());
        }
        // Pure red in BGRA = [0, 0, 255, 255].
        assert_eq!(img.pixel(20, 20), [0, 0, 255, 255]);
        assert_eq!(img.pixel(20, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn step_stamp_is_filled() {
        let mut img = Image::filled(60, 60, [255, 255, 255, 255]);
        {
            let mut pm = pixmap_mut(&mut img).expect("pixmap");
            step(&mut pm, 30.0, 30.0, 7, 0x00_00_FF, 20.0, Transform::identity());
        }
        // Inside the circle away from the digit: blue. Outside: white.
        assert_eq!(img.pixel(30, 14), [255, 0, 0, 255]);
        assert_eq!(img.pixel(2, 2), [255, 255, 255, 255]);
        // The digit puts white pixels near the centre.
        assert!((24..36).any(|x| img.pixel(x, 30)[1] > 200));
    }

    #[test]
    fn history_caps_and_restores() {
        let mut h = History::default();
        h.push(Snap { img: Image::new(2, 2), next_step: 1, shift: (0, 0) });
        h.push(Snap { img: Image::new(3, 3), next_step: 2, shift: (-4, 0) });
        assert_eq!(h.len(), 2);
        let s = h.pop().expect("snap");
        assert_eq!((s.img.width, s.next_step, s.shift), (3, 2, (-4, 0)));
        assert_eq!(h.pop().map(|s| s.next_step), Some(1));
        assert!(h.pop().is_none());
    }
}
