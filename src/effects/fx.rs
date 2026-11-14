//! Presentation effects: spotlight, drop shadow, frame, torn edge, fade edge, reflection,
//! watermark, caption, oil paint, sketch.

use super::filter::gaussian_blur_raw;
use super::selection::{ellipse_mask, feather_mask, rect_mask};
use super::text::{render_text_styled, FontSpec, TextAlign};
use super::{composite, flatten_onto, lerp_px, over, par_rows, scale_px, smoothstep, Anchor, Color, Sides};
use crate::common::rng::Rng;
use crate::common::{Image, Rect};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------ spotlight

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotShape {
    Rectangle,
    #[default]
    Ellipse,
}

/// Dim everything outside a rectangle or ellipse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpotlightParams {
    pub rect: Rect,
    pub shape: SpotShape,
    /// How strongly the outside is dimmed, in percent.
    pub dim: u8,
    /// Width of the soft transition in pixels.
    pub feather: u32,
    /// Dimming colour (usually black).
    pub color: Color,
    /// Fade the outside to transparent instead of to `color`.
    pub transparent_outside: bool,
}

impl Default for SpotlightParams {
    fn default() -> Self {
        Self {
            rect: Rect::default(),
            shape: SpotShape::Ellipse,
            dim: 60,
            feather: 24,
            color: Color::BLACK,
            transparent_outside: false,
        }
    }
}

pub fn spotlight(img: &Image, p: &SpotlightParams) -> Image {
    let (w, h) = (img.width, img.height);
    let mut mask = match p.shape {
        SpotShape::Rectangle => rect_mask(w, h, p.rect),
        SpotShape::Ellipse => ellipse_mask(w, h, p.rect),
    };
    if p.feather > 0 {
        mask = feather_mask(&mask, p.feather as f32);
    }
    let k = p.dim.min(100) as u32 * 255 / 100;
    let dim_px = p.color.with_opacity(k as u8).premul_bgra();
    let mut out = img.clone();
    let wu = w as usize;
    par_rows(&mut out.data, wu * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let m = mask.data[y0 * wu + i] as u32;
            if m == 255 {
                continue;
            }
            let orig = [px[0], px[1], px[2], px[3]];
            let dimmed = if p.transparent_outside { scale_px(orig, 255 - k) } else { over(dim_px, orig) };
            px.copy_from_slice(&lerp_px(dimmed, orig, m));
        }
    });
    out
}

// ------------------------------------------------------------------ drop shadow

/// Soft drop shadow; the canvas grows to hold it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShadowParams {
    pub offset_x: i32,
    pub offset_y: i32,
    /// Blur radius (CSS-style: sigma = blur / 2).
    pub blur: f32,
    /// Shadow opacity in percent.
    pub opacity: u8,
    pub color: Color,
    /// Round the image's corners first (0 = square).
    pub corner_radius: u32,
    /// Canvas background; transparent by default.
    pub background: Color,
}

impl Default for ShadowParams {
    fn default() -> Self {
        Self {
            offset_x: 0,
            offset_y: 10,
            blur: 28.0,
            opacity: 45,
            color: Color::BLACK,
            corner_radius: 0,
            background: Color::TRANSPARENT,
        }
    }
}

/// Margins the shadow adds on each side: (left, top, right, bottom).
pub fn shadow_margins(p: &ShadowParams) -> (u32, u32, u32, u32) {
    let ext = (p.blur.max(0.0) * 1.5).ceil() as i32 + 1;
    let l = (ext - p.offset_x).max(0) as u32;
    let r = (ext + p.offset_x).max(0) as u32;
    let t = (ext - p.offset_y).max(0) as u32;
    let b = (ext + p.offset_y).max(0) as u32;
    (l, t, r, b)
}

pub fn drop_shadow(img: &Image, p: &ShadowParams) -> Image {
    let src = if p.corner_radius > 0 { round_corners(img, p.corner_radius) } else { img.clone() };
    let (l, t, r, b) = shadow_margins(p);
    let (w, h) = (src.width + l + r, src.height + t + b);
    // Alpha plane of the offset image, blurred.
    let mut alpha = vec![0u8; w as usize * h as usize];
    let (ax, ay) = (l as i32 + p.offset_x, t as i32 + p.offset_y);
    // Copy the alpha channel into the plane, clipped (the offset may push it off-canvas).
    let x0 = (-ax).max(0) as usize;
    let x1 = (src.width as i32).min(w as i32 - ax).max(0) as usize;
    for y in 0..src.height as i32 {
        let dy = ay + y;
        if dy < 0 || dy >= h as i32 || x1 <= x0 {
            continue;
        }
        let srow = src.row(y as u32);
        let drow = &mut alpha[dy as usize * w as usize..(dy as usize + 1) * w as usize];
        for x in x0..x1 {
            drow[(ax + x as i32) as usize] = srow[x * 4 + 3];
        }
    }
    let alpha = gaussian_blur_raw(&alpha, w as usize, h as usize, 1, p.blur / 2.0);
    let k = p.opacity.min(100) as u32 * 255 / 100;
    let c = p.color.premul_bgra();
    let mut out = Image::new(w, h);
    par_rows(&mut out.data, w as usize * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let a = alpha[y0 * w as usize + i] as u32 * k / 255;
            px.copy_from_slice(&scale_px(c, a));
        }
    });
    composite(&mut out, &src, l as i32, t as i32, 255);
    flatten_onto(&out, p.background)
}

/// Clip the image to a rounded rectangle (anti-aliased).
pub fn round_corners(img: &Image, radius: u32) -> Image {
    let mut out = img.clone();
    let (w, h) = (img.width as f32, img.height as f32);
    let r = (radius as f32).min(w / 2.0).min(h / 2.0);
    if r <= 0.0 {
        return out;
    }
    let wu = img.width as usize;
    par_rows(&mut out.data, wu * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % wu) as f32 + 0.5, ((y0 + i / wu) as f32) + 0.5);
            let d = rounded_rect_sd(x - w / 2.0, y - h / 2.0, w / 2.0, h / 2.0, r);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov < 1.0 {
                let o = scale_px([px[0], px[1], px[2], px[3]], (cov * 255.0).round() as u32);
                px.copy_from_slice(&o);
            }
        }
    });
    out
}

/// Signed distance from (px, py) (relative to the centre) to a rounded rectangle with half
/// extents (bx, by) and corner radius r. Negative inside.
fn rounded_rect_sd(px: f32, py: f32, bx: f32, by: f32, r: f32) -> f32 {
    let qx = px.abs() - bx + r;
    let qy = py.abs() - by + r;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

// ------------------------------------------------------------------ frame

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameStyle {
    #[default]
    Solid,
    /// Two lines (each a third of the width) with `gap_color` between them.
    Double,
}

/// Border around the image; the canvas grows by `width` on each side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameParams {
    pub style: FrameStyle,
    pub width: u32,
    pub color: Color,
    pub gap_color: Color,
    /// Outer corner radius (0 = square).
    pub corner_radius: u32,
    /// Outside the rounded corners; transparent by default.
    pub background: Color,
}

impl Default for FrameParams {
    fn default() -> Self {
        Self {
            style: FrameStyle::Solid,
            width: 8,
            color: Color::rgb(64, 64, 72),
            gap_color: Color::WHITE,
            corner_radius: 0,
            background: Color::TRANSPARENT,
        }
    }
}

pub fn frame(img: &Image, p: &FrameParams) -> Image {
    let bw = p.width.min(4096);
    let (w, h) = (img.width + 2 * bw, img.height + 2 * bw);
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    let r = (p.corner_radius as f32).min(hw).min(hh);
    let bwf = bw as f32;
    let col = p.color.premul_bgra();
    let gap = p.gap_color.premul_bgra();
    let bg = p.background.premul_bgra();
    let t = if p.style == FrameStyle::Double { (bwf / 3.0).max(1.0).round() } else { bwf };
    let mut out = Image::new(w, h);
    let wu = w as usize;
    par_rows(&mut out.data, wu * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % wu, y0 + i / wu);
            let depth = -rounded_rect_sd(x as f32 + 0.5 - hw, y as f32 + 0.5 - hh, hw, hh, r);
            let cov = |a: f32| (depth - a + 0.5).clamp(0.0, 1.0);
            // Partition of the pixel: outside | line | gap | line | image.
            let c_out = cov(0.0);
            let c_img = cov(bwf);
            let (c_line, c_gap) = if p.style == FrameStyle::Double {
                let line1 = c_out - cov(t);
                let gapc = cov(t) - cov(bwf - t);
                let line2 = cov(bwf - t) - c_img;
                (line1 + line2, gapc)
            } else {
                (c_out - c_img, 0.0)
            };
            let ip = if c_img > 0.0 {
                let (ix, iy) = (x as i64 - bw as i64, y as i64 - bw as i64);
                if ix >= 0 && iy >= 0 && ix < img.width as i64 && iy < img.height as i64 {
                    img.pixel(ix as u32, iy as u32)
                } else {
                    [0; 4]
                }
            } else {
                [0; 4]
            };
            let mut o = [0u8; 4];
            for c in 0..4 {
                let v = bg[c] as f32 * (1.0 - c_out)
                    + col[c] as f32 * c_line
                    + gap[c] as f32 * c_gap
                    + ip[c] as f32 * c_img;
                o[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            px.copy_from_slice(&o);
        }
    });
    out
}

// ------------------------------------------------------------------ torn edge

/// Torn-paper edge on the chosen sides, with an optional shadow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TornEdgeParams {
    pub sides: Sides,
    /// Maximum depth of the tears in pixels.
    pub depth: u32,
    /// Average tooth width in pixels.
    pub tooth: u32,
    /// Same seed, same edge.
    pub seed: u64,
    /// A thin light rim along the tear, like paper fibres.
    pub paper_rim: bool,
    pub shadow_enabled: bool,
    pub shadow: ShadowParams,
    /// Final background; transparent by default.
    pub background: Color,
}

impl Default for TornEdgeParams {
    fn default() -> Self {
        Self {
            sides: Sides::BOTTOM,
            depth: 14,
            tooth: 16,
            seed: 1,
            paper_rim: true,
            shadow_enabled: true,
            shadow: ShadowParams { offset_x: 2, offset_y: 4, blur: 10.0, opacity: 45, ..Default::default() },
            background: Color::TRANSPARENT,
        }
    }
}

/// Cut depth along one side: `len` samples (one per pixel), each in 0..=depth.
fn tear_profile(len: usize, depth: f32, tooth: f32, rng: &mut Rng) -> Vec<f32> {
    let mut rand = || (rng.next_u64() >> 11) as f32 / (1u64 << 53) as f32;
    // Control points alternating shallow and deep, at irregular spacing.
    let mut pts: Vec<(f32, f32)> = Vec::new();
    let mut x = -tooth * rand();
    let mut deep = rand() < 0.5;
    while x < len as f32 + tooth {
        let d = if deep { 0.55 + 0.45 * rand() } else { 0.05 + 0.3 * rand() };
        pts.push((x, d * depth));
        // Occasionally a small double tooth for irregularity.
        let step = tooth * (0.35 + 0.65 * rand()) * if rand() < 0.2 { 0.45 } else { 1.0 };
        x += step.max(2.0);
        deep = !deep;
    }
    // Fine fibre jitter every 2-3 px.
    let jitter: Vec<f32> = (0..len / 2 + 3).map(|_| (rand() - 0.5) * (depth * 0.12).clamp(0.6, 2.0)).collect();
    let mut out = Vec::with_capacity(len);
    let mut seg = 0;
    for i in 0..len {
        let xf = i as f32 + 0.5;
        while seg + 1 < pts.len() && pts[seg + 1].0 < xf {
            seg += 1;
        }
        let (a, b) = (pts[seg], pts[(seg + 1).min(pts.len() - 1)]);
        let t = if b.0 > a.0 { ((xf - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) } else { 0.0 };
        let base = a.1 + (b.1 - a.1) * t;
        let jf = xf / 2.0;
        let j0 = (jf.floor() as usize).min(jitter.len() - 1);
        let j1 = (j0 + 1).min(jitter.len() - 1);
        let jt = jf - jf.floor();
        let j = jitter[j0] * (1.0 - jt) + jitter[j1] * jt;
        out.push((base + j).clamp(0.0, depth));
    }
    out
}

/// Coverage of a pixel whose centre is `s` px inside the edge, for a tear of depth `d`
/// with local slope `slope`; and the distance inside the tear line.
#[inline]
fn tear_cov(s: f32, d: f32, slope: f32) -> (f32, f32) {
    let dist = (s - d) / (1.0 + slope * slope).sqrt();
    ((dist + 0.5).clamp(0.0, 1.0), dist)
}

pub fn torn_edge(img: &Image, p: &TornEdgeParams) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 {
        return img.clone();
    }
    let depth = (p.depth.max(1) as f32).min(w.min(h) as f32 / 3.0);
    let tooth = p.tooth.max(2) as f32;
    let mut rng = Rng::new(p.seed ^ 0x5EED_7EA2);
    let mut prof = |len: usize, on: bool| if on { Some(tear_profile(len, depth, tooth, &mut rng)) } else { None };
    let top = prof(w, p.sides.top);
    let bottom = prof(w, p.sides.bottom);
    let left = prof(h, p.sides.left);
    let right = prof(h, p.sides.right);
    let slope = |v: &[f32], i: usize| {
        let a = v[i.saturating_sub(1)];
        let b = v[(i + 1).min(v.len() - 1)];
        (b - a) / 2.0
    };
    let rim_w = 2.2f32;
    // Pixels farther than this from every torn side are untouched.
    let reach = depth.ceil() as usize + 4;
    let sd = p.sides;
    let mut out = img.clone();
    par_rows(&mut out.data, w * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % w, y0 + i / w);
            let near = (sd.top && y < reach)
                || (sd.bottom && h - y <= reach)
                || (sd.left && x < reach)
                || (sd.right && w - x <= reach);
            if !near {
                continue;
            }
            let (xf, yf) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut cov = 1.0f32;
            let mut rim = 0.0f32;
            let mut side = |prof: &Option<Vec<f32>>, along: usize, s: f32| {
                if let Some(v) = prof {
                    let (c, dist) = tear_cov(s, v[along], slope(v, along));
                    cov *= c;
                    if dist > -0.5 {
                        rim = rim.max(1.0 - smoothstep(dist / rim_w));
                    }
                }
            };
            side(&top, x, yf);
            side(&bottom, x, h as f32 - yf);
            side(&left, y, xf);
            side(&right, y, w as f32 - xf);
            let mut o = [px[0], px[1], px[2], px[3]];
            if p.paper_rim && rim > 0.0 {
                let white = [o[3], o[3], o[3], o[3]];
                o = lerp_px(o, white, (rim * 0.85 * 255.0) as u32);
            }
            if cov < 1.0 {
                o = scale_px(o, (cov * 255.0).round() as u32);
            }
            px.copy_from_slice(&o);
        }
    });
    let out = if p.shadow_enabled {
        drop_shadow(&out, &ShadowParams { background: Color::TRANSPARENT, corner_radius: 0, ..p.shadow.clone() })
    } else {
        out
    };
    flatten_onto(&out, p.background)
}

// ------------------------------------------------------------------ fade edge

/// Fade the chosen sides to transparent (or to `color`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FadeEdgeParams {
    pub sides: Sides,
    /// Width of the fade in pixels.
    pub size: u32,
    /// Fade target; transparent fades the alpha.
    pub color: Color,
}

impl Default for FadeEdgeParams {
    fn default() -> Self {
        Self { sides: Sides::ALL, size: 40, color: Color::TRANSPARENT }
    }
}

pub fn fade_edge(img: &Image, p: &FadeEdgeParams) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let size = p.size.max(1) as f32;
    let target = p.color.premul_bgra();
    let f = |d: f32| smoothstep(d / size);
    let mut out = img.clone();
    par_rows(&mut out.data, w * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % w) as f32 + 0.5, (y0 + i / w) as f32 + 0.5);
            let mut k = 1.0f32;
            if p.sides.top {
                k *= f(y);
            }
            if p.sides.bottom {
                k *= f(h as f32 - y);
            }
            if p.sides.left {
                k *= f(x);
            }
            if p.sides.right {
                k *= f(w as f32 - x);
            }
            if k < 1.0 {
                let o = lerp_px(target, [px[0], px[1], px[2], px[3]], (k * 255.0).round() as u32);
                px.copy_from_slice(&o);
            }
        }
    });
    out
}

// ------------------------------------------------------------------ reflection

/// Mirrored, fading copy below the image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReflectionParams {
    /// Reflection height in percent of the image height.
    pub height: u8,
    /// Gap between the image and its reflection in pixels.
    pub gap: u32,
    /// Opacity at the top of the reflection, in percent.
    pub opacity: u8,
    /// Canvas background; transparent by default.
    pub background: Color,
}

impl Default for ReflectionParams {
    fn default() -> Self {
        Self { height: 35, gap: 2, opacity: 40, background: Color::TRANSPARENT }
    }
}

pub fn reflection(img: &Image, p: &ReflectionParams) -> Image {
    let (w, h) = (img.width, img.height);
    let rh = ((h as u64 * p.height.min(100) as u64) / 100) as u32;
    let total = h + p.gap + rh;
    let mut out = Image::new(w, total);
    out.blit(img, 0, 0);
    let rl = w as usize * 4;
    let k0 = p.opacity.min(100) as f32 / 100.0;
    for i in 0..rh {
        let t = i as f32 / rh.max(1) as f32;
        let k = (k0 * (1.0 - t).powf(1.6) * 255.0).round() as u32;
        let src = &img.data[(h - 1 - i) as usize * rl..(h - i) as usize * rl];
        let dy = (h + p.gap + i) as usize;
        let dst = &mut out.data[dy * rl..(dy + 1) * rl];
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d.copy_from_slice(&scale_px([s[0], s[1], s[2], s[3]], k));
        }
    }
    flatten_onto(&out, p.background)
}

// ------------------------------------------------------------------ watermark

/// Placement of a watermark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WatermarkParams {
    pub anchor: Anchor,
    /// Distance from the edges in pixels (ignored for `Center` and when tiling).
    pub margin: u32,
    /// Scale of the watermark in percent of its own size.
    pub scale: f32,
    /// Opacity in percent.
    pub opacity: u8,
    /// Repeat across the whole image (brick pattern).
    pub tile: bool,
    /// Gap between tiles in pixels.
    pub spacing: u32,
}

impl Default for WatermarkParams {
    fn default() -> Self {
        Self { anchor: Anchor::BottomRight, margin: 16, scale: 100.0, opacity: 50, tile: false, spacing: 60 }
    }
}

/// Overlay `mark` on the image.
pub fn watermark(img: &Image, mark: &Image, p: &WatermarkParams) -> Image {
    if mark.is_empty() {
        return img.clone();
    }
    let s = (p.scale.max(1.0) / 100.0) as f64;
    let mw = ((mark.width as f64 * s).round() as u32).max(1);
    let mh = ((mark.height as f64 * s).round() as u32).max(1);
    let m = super::geometry::resize_to(mark, mw, mh, super::geometry::ResampleFilter::Lanczos3);
    let k = (p.opacity.min(100) as u32 * 255 / 100) as u8;
    let mut out = img.clone();
    if p.tile {
        let sx = (mw + p.spacing).max(1) as i32;
        let sy = (mh + p.spacing).max(1) as i32;
        let mut row = 0;
        let mut y = (p.spacing / 2) as i32 - sy;
        while y < img.height as i32 {
            let shift = if row % 2 == 1 { sx / 2 } else { 0 };
            let mut x = (p.spacing / 2) as i32 - sx + shift;
            while x < img.width as i32 {
                composite(&mut out, &m, x, y, k);
                x += sx;
            }
            y += sy;
            row += 1;
        }
    } else {
        let mg = if p.anchor == Anchor::Center { 0 } else { p.margin };
        let inner_w = img.width.saturating_sub(2 * mg);
        let inner_h = img.height.saturating_sub(2 * mg);
        let (x, y) = p.anchor.offset(inner_w, inner_h, mw, mh);
        composite(&mut out, &m, x + mg as i32, y + mg as i32, k);
    }
    out
}

/// Text watermark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextWatermarkParams {
    pub text: String,
    pub font: FontSpec,
    pub color: Color,
    /// Subtle dark outline/shadow so the text reads on light and dark backgrounds.
    pub outline: bool,
    pub placement: WatermarkParams,
}

impl Default for TextWatermarkParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            font: FontSpec { size: 28, bold: true, ..Default::default() },
            color: Color::WHITE,
            outline: true,
            placement: WatermarkParams { scale: 100.0, opacity: 70, ..Default::default() },
        }
    }
}

pub fn watermark_text(img: &Image, p: &TextWatermarkParams) -> Image {
    let txt = render_text_styled(&p.text, &p.font, p.color, 0, TextAlign::Left);
    if txt.is_empty() {
        return img.clone();
    }
    let mark = if p.outline {
        // A soft dark halo behind the text.
        let halo = ShadowParams {
            offset_x: 0,
            offset_y: 1,
            blur: (p.font.size as f32 / 6.0).max(2.0),
            opacity: 70,
            color: Color::BLACK,
            corner_radius: 0,
            background: Color::TRANSPARENT,
        };
        drop_shadow(&txt, &halo)
    } else {
        txt
    };
    watermark(img, &mark, &p.placement)
}

// ------------------------------------------------------------------ caption

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionPosition {
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionBorder {
    None,
    /// A line between the image and the bar.
    #[default]
    Line,
    /// A border around image and bar, plus the separating line.
    Frame,
}

/// A text bar above or below the image. Long text wraps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionParams {
    pub text: String,
    pub position: CaptionPosition,
    pub font: FontSpec,
    pub text_color: Color,
    pub background: Color,
    pub align: TextAlign,
    /// Space around the text in pixels.
    pub padding: u32,
    pub border: CaptionBorder,
    pub border_color: Color,
    pub border_width: u32,
}

impl Default for CaptionParams {
    fn default() -> Self {
        Self {
            text: String::new(),
            position: CaptionPosition::Bottom,
            font: FontSpec { size: 18, ..Default::default() },
            text_color: Color::rgb(32, 33, 36),
            background: Color::rgb(246, 247, 249),
            align: TextAlign::Center,
            padding: 12,
            border: CaptionBorder::Line,
            border_color: Color::rgb(200, 203, 208),
            border_width: 1,
        }
    }
}

pub fn caption(img: &Image, p: &CaptionParams) -> Image {
    if p.text.trim().is_empty() {
        return img.clone();
    }
    let bw = if p.border == CaptionBorder::None { 0 } else { p.border_width };
    let frame_w = if p.border == CaptionBorder::Frame { bw } else { 0 };
    let inner_w = img.width;
    let text_w = inner_w.saturating_sub(2 * p.padding).max(1);
    let txt = render_text_styled(&p.text, &p.font, p.text_color, text_w, p.align);
    let bar_h = txt.height.max(p.font.size) + 2 * p.padding;
    let (w, h) = (inner_w + 2 * frame_w, img.height + bar_h + bw + 2 * frame_w);
    let mut out = Image::filled(w, h, p.border_color.premul_bgra());
    let (img_y, sep_y, bar_y) = match p.position {
        CaptionPosition::Bottom => (frame_w, frame_w + img.height, frame_w + img.height + bw),
        CaptionPosition::Top => (frame_w + bar_h + bw, frame_w + bar_h, frame_w),
    };
    let _ = sep_y; // The separator is the border colour already painted underneath.
    let bar = Image::filled(inner_w, bar_h, p.background.premul_bgra());
    out.blit(&bar, frame_w as i32, bar_y as i32);
    out.blit(img, frame_w as i32, img_y as i32);
    let tx = match p.align {
        TextAlign::Left => p.padding as i32,
        TextAlign::Center => (inner_w as i32 - txt.width as i32) / 2,
        TextAlign::Right => inner_w as i32 - p.padding as i32 - txt.width as i32,
    };
    let ty = (bar_h as i32 - txt.height as i32) / 2;
    composite(&mut out, &txt, frame_w as i32 + tx, bar_y as i32 + ty, 255);
    out
}

// ------------------------------------------------------------------ oil paint (Kuwahara)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OilPaintParams {
    /// Brush radius in pixels (1–15).
    pub radius: u32,
}

impl Default for OilPaintParams {
    fn default() -> Self {
        Self { radius: 4 }
    }
}

/// Kuwahara filter: each pixel takes the mean of the least varying of its four quadrants.
/// O(1) per pixel via running box sums, computed in chunks of rows on each thread.
pub fn oil_paint(img: &Image, p: &OilPaintParams) -> Image {
    let r = p.radius.clamp(1, 15) as usize;
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 {
        return img.clone();
    }
    let n = ((r + 1) * (r + 1)) as u64;
    let ew = w + 2 * r;
    let mut out = Image::new(img.width, img.height);
    par_rows(&mut out.data, w * 4, |y0, band| {
        const CHUNK: usize = 32;
        let rows = band.len() / (w * 4);
        let mut ext = vec![[0u32; 6]; ew];
        let mut hs: Vec<[u32; 6]> = Vec::new();
        let mut ss: Vec<[u32; 6]> = Vec::new();
        for cs in (0..rows).step_by(CHUNK) {
            let c0 = y0 + cs;
            let c1 = (c0 + CHUNK).min(y0 + rows);
            let nh = c1 - c0 + 2 * r;
            hs.clear();
            hs.resize(nh * ew, [0; 6]);
            // Horizontal window sums: H[ex] = sum of ext[ex - r ..= ex].
            for k in 0..nh {
                let sy = (c0 + k).saturating_sub(r).min(h - 1);
                let srow = &img.data[sy * w * 4..(sy + 1) * w * 4];
                for (i, e) in ext.iter_mut().enumerate() {
                    let sx = i.saturating_sub(r).min(w - 1);
                    let px = &srow[sx * 4..sx * 4 + 4];
                    let l = super::luma(px[0], px[1], px[2]) as u32;
                    *e = [px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32, l, l * l];
                }
                let hrow = &mut hs[k * ew..(k + 1) * ew];
                let mut acc = [0u32; 6];
                for i in 0..ew {
                    for c in 0..6 {
                        acc[c] += ext[i][c];
                    }
                    if i > r {
                        for c in 0..6 {
                            acc[c] -= ext[i - r - 1][c];
                        }
                    }
                    if i >= r {
                        hrow[i] = acc;
                    }
                }
            }
            // Vertical: S[k] (ext row c0 + r + k) = sum of H rows k ..= k + r.
            let ns = c1 - c0 + r;
            ss.clear();
            ss.resize(ns * ew, [0; 6]);
            for ex in r..ew {
                let mut acc = [0u32; 6];
                for k in 0..nh {
                    let v = hs[k * ew + ex];
                    for c in 0..6 {
                        acc[c] += v[c];
                    }
                    if k > r {
                        let o = hs[(k - r - 1) * ew + ex];
                        for c in 0..6 {
                            acc[c] -= o[c];
                        }
                    }
                    if k >= r {
                        ss[(k - r) * ew + ex] = acc;
                    }
                }
            }
            for y in c0..c1 {
                let orow = &mut band[(y - y0) * w * 4..(y - y0 + 1) * w * 4];
                let sy = y - c0; // S index for the quadrant rows y-r..=y; +r for y..=y+r.
                for x in 0..w {
                    let cands = [
                        ss[sy * ew + x + r],
                        ss[sy * ew + x + 2 * r],
                        ss[(sy + r) * ew + x + r],
                        ss[(sy + r) * ew + x + 2 * r],
                    ];
                    let mut best = 0;
                    let mut best_v = u64::MAX;
                    for (qi, q) in cands.iter().enumerate() {
                        let var = (n * q[5] as u64).saturating_sub(q[4] as u64 * q[4] as u64);
                        if var < best_v {
                            best_v = var;
                            best = qi;
                        }
                    }
                    let q = cands[best];
                    let o = &mut orow[x * 4..x * 4 + 4];
                    for c in 0..4 {
                        o[c] = ((q[c] as u64 + n / 2) / n) as u8;
                    }
                }
            }
        }
    });
    out
}

// ------------------------------------------------------------------ sketch

/// Pencil sketch: edge detection on the luminance, drawn dark on white.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SketchParams {
    /// Pencil pressure in percent (darker, more lines).
    pub strength: u8,
    /// Keep a pastel tint of the original colours.
    pub color: bool,
}

impl Default for SketchParams {
    fn default() -> Self {
        Self { strength: 60, color: false }
    }
}

pub fn sketch(img: &Image, p: &SketchParams) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 {
        return img.clone();
    }
    // Luma over white, lightly smoothed to tame noise.
    let lum: Vec<u8> = img
        .data
        .chunks_exact(4)
        .map(|p| {
            let ia = 255 - p[3];
            super::luma(p[0].saturating_add(ia), p[1].saturating_add(ia), p[2].saturating_add(ia))
        })
        .collect();
    let lum = gaussian_blur_raw(&lum, w, h, 1, 0.7);
    let gain = 0.4 + p.strength.min(100) as f32 / 100.0 * 2.6;
    // Pencil darkness by gradient magnitude (Sobel magnitude is at most ~1443).
    let lut: Vec<f32> = (0..1448)
        .map(|m| {
            // Noise floor keeps smooth gradients paper-white; soft response keeps faint lines faint.
            let mag = (m as f32 / 4.0 - 3.0).max(0.0);
            let dark = (1.0 - (-mag * gain / 110.0).exp()).clamp(0.0, 1.0);
            1.0 - dark * 0.88
        })
        .collect();
    let mut out = img.clone();
    par_rows(&mut out.data, w * 4, |y0, band| {
        for (i, px) in band.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % w, y0 + i / w);
            let rm = &lum[y.saturating_sub(1) * w..][..w];
            let r0 = &lum[y * w..][..w];
            let rp = &lum[(y + 1).min(h - 1) * w..][..w];
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let v = |r: &[u8], xx: usize| r[xx] as i32;
            let gx = v(rm, xp) + 2 * v(r0, xp) + v(rp, xp) - v(rm, xm) - 2 * v(r0, xm) - v(rp, xm);
            let gy = v(rp, xm) + 2 * v(rp, x) + v(rp, xp) - v(rm, xm) - 2 * v(rm, x) - v(rm, xp);
            let m = ((gx * gx + gy * gy) as f32).sqrt() as usize;
            let pencil = lut[m.min(lut.len() - 1)];
            let a = px[3] as f32;
            let o = if p.color {
                let tint = |c: u8| (pencil * (c as f32 * 0.45 + a * 0.55)).round().clamp(0.0, a) as u8;
                [tint(px[0]), tint(px[1]), tint(px[2]), px[3]]
            } else {
                let v = (pencil * a).round() as u8;
                [v, v, v, px[3]]
            };
            px.copy_from_slice(&o);
        }
    });
    out
}
