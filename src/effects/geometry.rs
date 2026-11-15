//! Geometry: crop, resize, canvas size, expand canvas, rotate, flip.

use super::{par_rows, Anchor, Color, Edges};
use crate::common::Image;
#[cfg(test)]
use crate::common::Rect;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ crop

/// Copy of `rect` (clipped to the image).
#[cfg(test)]
pub fn crop(img: &Image, rect: Rect) -> Image {
    img.crop(rect)
}

// ------------------------------------------------------------------ resize

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeUnit {
    Pixels,
    #[default]
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResampleFilter {
    /// Hard pixels (pixel art, zooming UI shots by whole factors).
    Nearest,
    Bilinear,
    /// Mitchell–Netravali (B = C = 1/3): soft, no ringing.
    Mitchell,
    /// Lanczos3: sharpest, best for downscaling screenshots.
    #[default]
    Lanczos3,
}

impl ResampleFilter {
    #[cfg(test)]
    pub const ALL: [ResampleFilter; 4] =
        [ResampleFilter::Nearest, ResampleFilter::Bilinear, ResampleFilter::Mitchell, ResampleFilter::Lanczos3];

    fn support(self) -> f64 {
        match self {
            ResampleFilter::Nearest => 0.5,
            ResampleFilter::Bilinear => 1.0,
            ResampleFilter::Mitchell => 2.0,
            ResampleFilter::Lanczos3 => 3.0,
        }
    }

    fn eval(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            ResampleFilter::Nearest => {
                if x < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            ResampleFilter::Bilinear => (1.0 - x).max(0.0),
            ResampleFilter::Mitchell => {
                let (b, c) = (1.0 / 3.0, 1.0 / 3.0);
                if x < 1.0 {
                    ((12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b))
                        / 6.0
                } else if x < 2.0 {
                    ((-b - 6.0 * c) * x * x * x
                        + (6.0 * b + 30.0 * c) * x * x
                        + (-12.0 * b - 48.0 * c) * x
                        + (8.0 * b + 24.0 * c))
                        / 6.0
                } else {
                    0.0
                }
            }
            ResampleFilter::Lanczos3 => {
                if x < 3.0 {
                    sinc(x) * sinc(x / 3.0)
                } else {
                    0.0
                }
            }
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            ResampleFilter::Nearest => "fx-filter-nearest",
            ResampleFilter::Bilinear => "fx-filter-bilinear",
            ResampleFilter::Mitchell => "fx-filter-mitchell",
            ResampleFilter::Lanczos3 => "fx-filter-lanczos",
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

/// Resize by pixels or percent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResizeParams {
    pub unit: ResizeUnit,
    /// Target width in pixels (`unit == Pixels`; 0 = derive from height).
    pub width: u32,
    /// Target height in pixels (`unit == Pixels`; 0 = derive from width).
    pub height: u32,
    /// Scale in percent (`unit == Percent`).
    pub percent: f32,
    /// Keep the aspect ratio. With both width and height set, the image fits inside that box.
    pub keep_aspect: bool,
    pub filter: ResampleFilter,
}

impl Default for ResizeParams {
    fn default() -> Self {
        Self {
            unit: ResizeUnit::Percent,
            width: 0,
            height: 0,
            percent: 100.0,
            keep_aspect: true,
            filter: Default::default(),
        }
    }
}

/// Largest dimension a resize may produce (keeps allocations sane).
pub const MAX_DIM: u32 = 32768;

/// Output size of [`resize`] for a `w`x`h` source (never 0 for a non-empty source).
pub fn resize_dims(w: u32, h: u32, p: &ResizeParams) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    let clamp = |v: f64| (v.round() as i64).clamp(1, MAX_DIM as i64) as u32;
    match p.unit {
        ResizeUnit::Percent => {
            let s = (p.percent.max(0.01) as f64) / 100.0;
            (clamp(w as f64 * s), clamp(h as f64 * s))
        }
        ResizeUnit::Pixels => {
            let (tw, th) = (p.width, p.height);
            if !p.keep_aspect {
                let nw = if tw == 0 { w } else { tw };
                let nh = if th == 0 { h } else { th };
                return (nw.min(MAX_DIM), nh.min(MAX_DIM));
            }
            let s = match (tw, th) {
                (0, 0) => 1.0,
                (tw, 0) => tw as f64 / w as f64,
                (0, th) => th as f64 / h as f64,
                (tw, th) => (tw as f64 / w as f64).min(th as f64 / h as f64),
            };
            (clamp(w as f64 * s), clamp(h as f64 * s))
        }
    }
}

/// Resize with the given parameters.
pub fn resize(img: &Image, p: &ResizeParams) -> Image {
    let (w, h) = resize_dims(img.width, img.height, p);
    resize_to(img, w, h, p.filter)
}

/// Percent for the quick-resize shortcuts: Ctrl+1/2/3/4 → 25/50/75/200 %.
pub fn quick_resize_percent(slot: u8) -> Option<f32> {
    match slot {
        1 => Some(25.0),
        2 => Some(50.0),
        3 => Some(75.0),
        4 => Some(200.0),
        _ => None,
    }
}

/// Quick resize by shortcut slot (1–4) with Lanczos3. Unknown slots return a copy.
pub fn quick_resize(img: &Image, slot: u8) -> Image {
    match quick_resize_percent(slot) {
        Some(percent) => resize(img, &ResizeParams { percent, ..Default::default() }),
        None => img.clone(),
    }
}

/// Resample to exactly `w`x`h` (separable, fixed-point, in premultiplied space).
pub fn resize_to(img: &Image, w: u32, h: u32, filter: ResampleFilter) -> Image {
    if img.is_empty() || w == 0 || h == 0 {
        return Image::new(w, h);
    }
    if w == img.width && h == img.height {
        return img.clone();
    }
    if filter == ResampleFilter::Nearest {
        return resize_nearest(img, w, h);
    }
    let horiz = if w != img.width { resample_h(img, w, filter) } else { img.clone() };
    let mut out = if h != img.height { resample_v(&horiz, h, filter) } else { horiz };
    fix_premul(&mut out);
    out
}

fn resize_nearest(img: &Image, w: u32, h: u32) -> Image {
    let mut out = Image::new(w, h);
    let xs: Vec<usize> = (0..w).map(|x| (((x as u64 * 2 + 1) * img.width as u64) / (2 * w as u64)) as usize).collect();
    let (sw, sh) = (img.width as usize, img.height as u64);
    par_rows(&mut out.data, w as usize * 4, |y0, band| {
        for (r, row) in band.chunks_exact_mut(w as usize * 4).enumerate() {
            let sy = (((y0 + r) as u64 * 2 + 1) * sh / (2 * h as u64)) as usize;
            let src = &img.data[sy * sw * 4..(sy + 1) * sw * 4];
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let s = xs[x] * 4;
                px.copy_from_slice(&src[s..s + 4]);
            }
        }
    });
    out
}

const PREC: u32 = 22;

/// Fixed-point filter weights for each output coordinate.
struct Weights {
    start: Vec<usize>,
    len: Vec<usize>,
    ksize: usize,
    w: Vec<i32>,
}

fn weights(in_size: u32, out_size: u32, filter: ResampleFilter) -> Weights {
    let scale = in_size as f64 / out_size as f64;
    let fscale = scale.max(1.0);
    let support = filter.support() * fscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut start = Vec::with_capacity(out_size as usize);
    let mut len = Vec::with_capacity(out_size as usize);
    let mut w = vec![0i32; ksize * out_size as usize];
    let mut tmp = vec![0f64; ksize];
    for i in 0..out_size as usize {
        let center = (i as f64 + 0.5) * scale;
        let xmin = ((center - support + 0.5).floor().max(0.0)) as usize;
        let xmax = ((center + support + 0.5).floor() as usize).min(in_size as usize);
        let n = xmax.saturating_sub(xmin).min(ksize);
        let mut total = 0.0;
        for (k, t) in tmp.iter_mut().enumerate().take(n) {
            let v = filter.eval((xmin as f64 + k as f64 - center + 0.5) / fscale);
            *t = v;
            total += v;
        }
        let row = &mut w[i * ksize..(i + 1) * ksize];
        if total != 0.0 {
            for k in 0..n {
                row[k] = (tmp[k] / total * (1u32 << PREC) as f64).round() as i32;
            }
        }
        start.push(xmin);
        len.push(n);
    }
    Weights { start, len, ksize, w }
}

#[inline]
fn clip8(v: i32) -> u8 {
    (v >> PREC).clamp(0, 255) as u8
}

fn resample_h(img: &Image, out_w: u32, filter: ResampleFilter) -> Image {
    let wt = weights(img.width, out_w, filter);
    let mut out = Image::new(out_w, img.height);
    let sw = img.width as usize;
    par_rows(&mut out.data, out_w as usize * 4, |y0, band| {
        for (r, row) in band.chunks_exact_mut(out_w as usize * 4).enumerate() {
            let src = &img.data[(y0 + r) * sw * 4..(y0 + r + 1) * sw * 4];
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let ws = &wt.w[x * wt.ksize..x * wt.ksize + wt.len[x]];
                let s0 = wt.start[x] * 4;
                let mut acc = [1i32 << (PREC - 1); 4];
                for (k, &wk) in ws.iter().enumerate() {
                    let s = &src[s0 + k * 4..s0 + k * 4 + 4];
                    acc[0] += s[0] as i32 * wk;
                    acc[1] += s[1] as i32 * wk;
                    acc[2] += s[2] as i32 * wk;
                    acc[3] += s[3] as i32 * wk;
                }
                px.copy_from_slice(&[clip8(acc[0]), clip8(acc[1]), clip8(acc[2]), clip8(acc[3])]);
            }
        }
    });
    out
}

fn resample_v(img: &Image, out_h: u32, filter: ResampleFilter) -> Image {
    let wt = weights(img.height, out_h, filter);
    let rl = img.width as usize * 4;
    let mut out = Image::new(img.width, out_h);
    par_rows(&mut out.data, rl, |y0, band| {
        let mut acc = vec![0i32; rl];
        for (r, row) in band.chunks_exact_mut(rl).enumerate() {
            let y = y0 + r;
            acc.fill(1 << (PREC - 1));
            let ws = &wt.w[y * wt.ksize..y * wt.ksize + wt.len[y]];
            for (k, &wk) in ws.iter().enumerate() {
                let sy = wt.start[y] + k;
                let src = &img.data[sy * rl..(sy + 1) * rl];
                for (a, &s) in acc.iter_mut().zip(src) {
                    *a += s as i32 * wk;
                }
            }
            for (o, &a) in row.iter_mut().zip(&acc) {
                *o = clip8(a);
            }
        }
    });
    out
}

/// Ringing filters can push a colour channel above alpha; clamp to keep valid premultiplied data.
fn fix_premul(img: &mut Image) {
    let rl = img.width as usize * 4;
    par_rows(&mut img.data, rl, |_, band| {
        for px in band.chunks_exact_mut(4) {
            let a = px[3];
            px[0] = px[0].min(a);
            px[1] = px[1].min(a);
            px[2] = px[2].min(a);
        }
    });
}

// ------------------------------------------------------------------ canvas

/// Change the canvas size; the image is placed at `anchor`, new area filled with `fill`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasParams {
    pub width: u32,
    pub height: u32,
    pub anchor: Anchor,
    /// Fill for new area; transparent by default.
    pub fill: Color,
}

pub fn canvas_size(img: &Image, p: &CanvasParams) -> Image {
    let (w, h) = (p.width.min(MAX_DIM), p.height.min(MAX_DIM));
    let mut out = Image::filled(w, h, p.fill.premul_bgra());
    let (x, y) = p.anchor.offset(w, h, img.width, img.height);
    out.blit(img, x, y);
    out
}

/// Add (or, never, remove) space on each edge.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExpandParams {
    pub edges: Edges,
    pub fill: Color,
}

pub fn expand_canvas(img: &Image, p: &ExpandParams) -> Image {
    let e = p.edges;
    let w = (img.width + e.left + e.right).min(MAX_DIM);
    let h = (img.height + e.top + e.bottom).min(MAX_DIM);
    let mut out = Image::filled(w, h, p.fill.premul_bgra());
    out.blit(img, e.left as i32, e.top as i32);
    out
}

// ------------------------------------------------------------------ flip / rotate

pub fn flip_horizontal(img: &Image) -> Image {
    let mut out = img.clone();
    let rl = img.width as usize * 4;
    par_rows(&mut out.data, rl, |_, band| {
        for row in band.chunks_exact_mut(rl) {
            let n = row.len() / 4;
            for i in 0..n / 2 {
                for c in 0..4 {
                    row.swap(i * 4 + c, (n - 1 - i) * 4 + c);
                }
            }
        }
    });
    out
}

pub fn flip_vertical(img: &Image) -> Image {
    let rl = img.width as usize * 4;
    let mut out = Image::new(img.width, img.height);
    for (dst, src) in out.data.chunks_exact_mut(rl.max(1)).zip(img.data.chunks_exact(rl.max(1)).rev()) {
        dst.copy_from_slice(src);
    }
    out
}

/// Clockwise rotation by a multiple of 90°.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarterTurn {
    #[default]
    Cw90,
    Cw180,
    Cw270,
}

pub fn rotate_quarter(img: &Image, turn: QuarterTurn) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    match turn {
        QuarterTurn::Cw180 => {
            let mut out = img.clone();
            let n = w * h;
            for i in 0..n / 2 {
                for c in 0..4 {
                    out.data.swap(i * 4 + c, (n - 1 - i) * 4 + c);
                }
            }
            out
        }
        QuarterTurn::Cw90 | QuarterTurn::Cw270 => {
            let mut out = Image::new(img.height, img.width);
            let cw = turn == QuarterTurn::Cw90;
            // Output is h wide and w tall. Out (ox, oy) <- src.
            par_rows(&mut out.data, h * 4, |y0, band| {
                for (r, row) in band.chunks_exact_mut(h * 4).enumerate() {
                    let oy = y0 + r;
                    for ox in 0..h {
                        let (sx, sy) = if cw { (oy, h - 1 - ox) } else { (w - 1 - oy, ox) };
                        let s = (sy * w + sx) * 4;
                        row[ox * 4..ox * 4 + 4].copy_from_slice(&img.data[s..s + 4]);
                    }
                }
            });
            out
        }
    }
}

/// Rotate by any angle (degrees, clockwise), bilinear, expanding the canvas to fit.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RotateParams {
    pub angle: f32,
    /// Fill for the uncovered corners; transparent by default.
    pub fill: Color,
}

/// Size of the canvas that holds a `w`x`h` image rotated by `angle` degrees.
pub fn rotated_size(w: u32, h: u32, angle: f32) -> (u32, u32) {
    let t = (angle as f64).to_radians();
    let (s, c) = (t.sin().abs(), t.cos().abs());
    let nw = (w as f64 * c + h as f64 * s - 1e-6).ceil().max(1.0);
    let nh = (w as f64 * s + h as f64 * c - 1e-6).ceil().max(1.0);
    ((nw as u32).min(MAX_DIM), (nh as u32).min(MAX_DIM))
}

pub fn rotate(img: &Image, p: &RotateParams) -> Image {
    if img.is_empty() {
        return img.clone();
    }
    let a = (p.angle as f64).rem_euclid(360.0);
    let exact = |deg: f64| (a - deg).abs() < 1e-6;
    let turned = if exact(0.0) || exact(360.0) {
        Some(img.clone())
    } else if exact(90.0) {
        Some(rotate_quarter(img, QuarterTurn::Cw90))
    } else if exact(180.0) {
        Some(rotate_quarter(img, QuarterTurn::Cw180))
    } else if exact(270.0) {
        Some(rotate_quarter(img, QuarterTurn::Cw270))
    } else {
        None
    };
    if let Some(t) = turned {
        return t;
    }
    let (nw, nh) = rotated_size(img.width, img.height, p.angle);
    let t = a.to_radians();
    let (sin, cos) = (t.sin(), t.cos());
    let (scx, scy) = (img.width as f64 / 2.0, img.height as f64 / 2.0);
    let (ocx, ocy) = (nw as f64 / 2.0, nh as f64 / 2.0);
    let fill = p.fill.premul_bgra();
    let (sw, sh) = (img.width as i64, img.height as i64);
    let mut out = Image::new(nw, nh);
    let sample = |x: i64, y: i64| -> [u32; 4] {
        if x < 0 || y < 0 || x >= sw || y >= sh {
            [0; 4]
        } else {
            let i = ((y * sw + x) * 4) as usize;
            let d = &img.data[i..i + 4];
            [d[0] as u32, d[1] as u32, d[2] as u32, d[3] as u32]
        }
    };
    par_rows(&mut out.data, nw as usize * 4, |y0, band| {
        for (r, row) in band.chunks_exact_mut(nw as usize * 4).enumerate() {
            let dy = (y0 + r) as f64 + 0.5 - ocy;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let dx = x as f64 + 0.5 - ocx;
                // Inverse rotation (clockwise on screen with y down).
                let sx = dx * cos + dy * sin + scx - 0.5;
                let sy = -dx * sin + dy * cos + scy - 0.5;
                let x0 = sx.floor();
                let y0f = sy.floor();
                let fx = ((sx - x0) * 256.0) as u32;
                let fy = ((sy - y0f) * 256.0) as u32;
                let (ix, iy) = (x0 as i64, y0f as i64);
                if ix < -1 || iy < -1 || ix >= sw || iy >= sh {
                    px.copy_from_slice(&fill);
                    continue;
                }
                let (p00, p10, p01, p11) =
                    (sample(ix, iy), sample(ix + 1, iy), sample(ix, iy + 1), sample(ix + 1, iy + 1));
                let mut o = [0u8; 4];
                for c in 0..4 {
                    let top = p00[c] * (256 - fx) + p10[c] * fx;
                    let bot = p01[c] * (256 - fx) + p11[c] * fx;
                    o[c] = ((top * (256 - fy) + bot * fy + 32768) >> 16) as u8;
                }
                px.copy_from_slice(&super::over(o, fill));
            }
        }
    });
    out
}
