//! Image operations and effects. Pure functions on [`Image`] (premultiplied BGRA) with no UI.
//!
//! Every operation takes an `&Image` plus a parameter struct and returns a new `Image`
//! (a few pixel tools mutate in place). Parameter structs derive `Default`, `Clone`, `Debug`,
//! `PartialEq`, `Serialize` and `Deserialize`, so the editor can keep presets.
//!
//! Heavy filters split the image into row bands and run them on scoped threads.

pub mod beautify;
pub mod color;
pub mod filter;
pub mod fx;
pub mod geometry;
pub mod pixel;
pub mod selection;
pub mod text;

#[cfg(test)]
pub(crate) mod tests;

use crate::common::Image;
use serde::{Deserialize, Serialize};

// The flat `effects::name` API.
pub use beautify::*;
pub use color::*;
pub use filter::*;
pub use fx::*;
pub use geometry::*;
pub use pixel::*;
pub use selection::*;

/// A straight-alpha (not premultiplied) RGBA colour. `Default` is fully transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub fn is_transparent(self) -> bool {
        self.a == 0
    }
    /// Premultiplied BGRA, the layout of [`Image::data`].
    pub fn premul_bgra(self) -> [u8; 4] {
        let a = self.a as u32;
        let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        [pm(self.b), pm(self.g), pm(self.r), self.a]
    }
    /// Same colour with its alpha multiplied by `opacity` (0..=255).
    pub fn with_opacity(self, opacity: u8) -> Self {
        Self { a: ((self.a as u32 * opacity as u32 + 127) / 255) as u8, ..self }
    }
    /// Straight colour from a premultiplied BGRA pixel.
    pub fn from_premul(p: [u8; 4]) -> Self {
        let a = p[3] as u32;
        if a == 255 {
            return Self { r: p[2], g: p[1], b: p[0], a: 255 };
        }
        if a == 0 {
            return Self::TRANSPARENT;
        }
        let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
        Self { r: un(p[2]), g: un(p[1]), b: un(p[0]), a: p[3] }
    }
}

/// Which sides of an image an effect applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sides {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

impl Default for Sides {
    fn default() -> Self {
        Self::ALL
    }
}

impl Sides {
    pub const ALL: Sides = Sides { top: true, right: true, bottom: true, left: true };
    pub const BOTTOM: Sides = Sides { top: false, right: false, bottom: true, left: false };
}

/// Per-edge amounts in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Edges {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// One of nine anchor positions (canvas size, watermark placement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    /// Horizontal and vertical position factors: 0 = start, 1 = centre, 2 = end.
    pub fn factors(self) -> (i64, i64) {
        let i = Anchor::ALL.iter().position(|a| *a == self).unwrap_or(4) as i64;
        (i % 3, i / 3)
    }

    /// Offset of an `inner` box placed inside `outer` at this anchor (may be negative).
    pub fn offset(self, outer_w: u32, outer_h: u32, inner_w: u32, inner_h: u32) -> (i32, i32) {
        let (fx, fy) = self.factors();
        let dx = outer_w as i64 - inner_w as i64;
        let dy = outer_h as i64 - inner_h as i64;
        // Floor division keeps odd differences biased to the start, also when negative.
        ((dx * fx).div_euclid(2) as i32, (dy * fy).div_euclid(2) as i32)
    }

    /// Fluent message id for the label.
    pub fn label_key(self) -> &'static str {
        match self {
            Anchor::TopLeft => "fx-anchor-top-left",
            Anchor::Top => "fx-anchor-top",
            Anchor::TopRight => "fx-anchor-top-right",
            Anchor::Left => "fx-anchor-left",
            Anchor::Center => "fx-anchor-center",
            Anchor::Right => "fx-anchor-right",
            Anchor::BottomLeft => "fx-anchor-bottom-left",
            Anchor::Bottom => "fx-anchor-bottom",
            Anchor::BottomRight => "fx-anchor-bottom-right",
        }
    }
}

/// Horizontal or vertical (strips, combine direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    #[default]
    Horizontal,
    Vertical,
}

/// An 8-bit coverage mask the size of an image (255 = fully selected).
#[derive(Clone, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Mask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mask({}x{})", self.width, self.height)
    }
}

impl Mask {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize] }
    }
    #[cfg(test)]
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[y as usize * self.width as usize + x as usize]
    }
}

// ------------------------------------------------------------------ shared helpers

/// Threads to use for an image of `pixels` pixels (1 for small images).
pub(crate) fn thread_count(pixels: usize) -> usize {
    if pixels < 64 * 1024 {
        return 1;
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16)
}

/// Run `f(first_row, band)` over horizontal bands of `data` (rows of `row_len` elements) in
/// parallel. Bands are disjoint, so `f` can write freely.
pub(crate) fn par_rows<T: Send>(data: &mut [T], row_len: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    if row_len == 0 || data.is_empty() {
        return;
    }
    let rows = data.len() / row_len;
    // `data.len() / 4` approximates the pixel count for byte buffers; good enough to decide.
    let threads = thread_count(data.len() / 4).min(rows.max(1));
    if threads <= 1 {
        f(0, data);
        return;
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|s| {
        for (i, band) in data.chunks_mut(per * row_len).enumerate() {
            let f = &f;
            s.spawn(move || f(i * per, band));
        }
    });
}

/// Map every pixel independently (`f` gets and returns premultiplied BGRA), in parallel.
pub(crate) fn map_pixels(img: &Image, f: impl Fn([u8; 4]) -> [u8; 4] + Sync) -> Image {
    let mut out = img.clone();
    par_rows(&mut out.data, img.width as usize * 4, |_, band| {
        for px in band.chunks_exact_mut(4) {
            let o = f([px[0], px[1], px[2], px[3]]);
            px.copy_from_slice(&o);
        }
    });
    out
}

/// Map every pixel through a function of straight (un-premultiplied) colour; alpha is kept.
pub(crate) fn map_straight(img: &Image, f: impl Fn([u8; 3]) -> [u8; 3] + Sync) -> Image {
    map_pixels(img, |p| {
        let a = p[3];
        if a == 0 {
            return p;
        }
        if a == 255 {
            let o = f([p[0], p[1], p[2]]);
            return [o[0], o[1], o[2], 255];
        }
        let a32 = a as u32;
        let un = |c: u8| ((c as u32 * 255 + a32 / 2) / a32).min(255) as u8;
        let o = f([un(p[0]), un(p[1]), un(p[2])]);
        let pm = |c: u8| ((c as u32 * a32 + 127) / 255) as u8;
        [pm(o[0]), pm(o[1]), pm(o[2]), a]
    })
}

/// Source-over of a premultiplied pixel `s` onto `d`.
#[inline]
pub(crate) fn over(s: [u8; 4], d: [u8; 4]) -> [u8; 4] {
    let ia = 255 - s[3] as u32;
    if ia == 0 {
        return s;
    }
    let mut o = [0u8; 4];
    for c in 0..4 {
        o[c] = (s[c] as u32 + (d[c] as u32 * ia + 127) / 255).min(255) as u8;
    }
    o
}

/// Scale a premultiplied pixel by `k` (0..=255).
#[inline]
pub(crate) fn scale_px(p: [u8; 4], k: u32) -> [u8; 4] {
    if k >= 255 {
        return p;
    }
    let s = |c: u8| ((c as u32 * k + 127) / 255) as u8;
    [s(p[0]), s(p[1]), s(p[2]), s(p[3])]
}

/// Linear interpolation of premultiplied pixels: `a` at t = 0, `b` at t = 255.
#[inline]
pub(crate) fn lerp_px(a: [u8; 4], b: [u8; 4], t: u32) -> [u8; 4] {
    let mut o = [0u8; 4];
    for c in 0..4 {
        o[c] = ((a[c] as u32 * (255 - t) + b[c] as u32 * t + 127) / 255) as u8;
    }
    o
}

/// Composite `img` over a solid colour (no-op for a transparent colour).
pub(crate) fn flatten_onto(img: &Image, bg: Color) -> Image {
    if bg.is_transparent() {
        return img.clone();
    }
    let b = bg.premul_bgra();
    map_pixels(img, |p| over(p, b))
}

/// Source-over composite of premultiplied `src` onto `dst` at (x, y) with an extra opacity
/// (0..=255). Clipped; row-wise and parallel for large areas.
pub(crate) fn composite(dst: &mut Image, src: &Image, x: i32, y: i32, opacity: u8) {
    use crate::common::Rect;
    let r = Rect::new(x, y, src.width as i32, src.height as i32).intersect(&dst.bounds());
    if r.is_empty() || opacity == 0 {
        return;
    }
    let dw = dst.width as usize;
    let sw = src.width as usize;
    let k = opacity as u32;
    let start = r.y as usize * dw * 4;
    let end = r.bottom() as usize * dw * 4;
    par_rows(&mut dst.data[start..end], dw * 4, |row0, band| {
        for (i, row) in band.chunks_exact_mut(dw * 4).enumerate() {
            let sy = (r.y as usize + row0 + i) as i64 - y as i64;
            let sx0 = (r.x - x) as usize;
            let srow = &src.data[(sy as usize * sw + sx0) * 4..(sy as usize * sw + sx0 + r.w as usize) * 4];
            let drow = &mut row[r.x as usize * 4..r.right() as usize * 4];
            for (d, s) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                let s = scale_px([s[0], s[1], s[2], s[3]], k);
                if s[3] == 0 && s[0] == 0 && s[1] == 0 && s[2] == 0 {
                    continue;
                }
                let o = over(s, [d[0], d[1], d[2], d[3]]);
                d.copy_from_slice(&o);
            }
        }
    });
}

/// Rec. 601 luma of a BGR triple, 0..=255.
#[inline]
pub(crate) fn luma(b: u8, g: u8, r: u8) -> u8 {
    ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29 + 128) >> 8) as u8
}

/// Smoothstep from 0 to 1 over t in [0, 1].
#[inline]
pub(crate) fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
