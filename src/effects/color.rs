//! Colour adjustments, tone effects, colour counting and reduction, background removal.

use super::{map_pixels, map_straight, par_rows, Color, Mask};
use crate::common::{Image, Point};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

// ------------------------------------------------------------------ adjust colours

/// Colour adjustment. All sliders are -100..=100 (0 = unchanged) except `gamma`
/// (0.1..=5.0, 1 = unchanged; above 1 brightens mid-tones) and `hue` (-180..=180 degrees).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdjustParams {
    pub brightness: i32,
    pub contrast: i32,
    pub gamma: f32,
    pub saturation: i32,
    pub hue: i32,
    pub red: i32,
    pub green: i32,
    pub blue: i32,
}

impl Default for AdjustParams {
    fn default() -> Self {
        Self { brightness: 0, contrast: 0, gamma: 1.0, saturation: 0, hue: 0, red: 0, green: 0, blue: 0 }
    }
}

impl AdjustParams {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }
}

fn channel_lut(balance: i32, p: &AdjustParams) -> [u8; 256] {
    let b = p.brightness.clamp(-100, 100) as f32 / 200.0;
    let bal = balance.clamp(-100, 100) as f32 / 200.0;
    let c = p.contrast.clamp(-100, 100) as f32 / 100.0;
    let f = if c <= 0.0 { 1.0 + c } else { 1.0 / (1.0 - c * 0.99) };
    let g = p.gamma.clamp(0.1, 5.0);
    let mut lut = [0u8; 256];
    for (v, o) in lut.iter_mut().enumerate() {
        let mut x = v as f32 / 255.0 + b + bal;
        x = (x - 0.5) * f + 0.5;
        x = x.clamp(0.0, 1.0).powf(1.0 / g);
        *o = (x * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

/// Hue rotation then saturation as one 3x3 matrix on RGB (fixed point, 1.0 = 4096).
fn hue_sat_matrix(hue_deg: i32, sat: i32) -> [[i32; 3]; 3] {
    let t = (hue_deg as f32).to_radians();
    let (c, s) = (t.cos(), t.sin());
    // SVG feColorMatrix hueRotate.
    let h = [
        [0.213 + c * 0.787 - s * 0.213, 0.715 - c * 0.715 - s * 0.715, 0.072 - c * 0.072 + s * 0.928],
        [0.213 - c * 0.213 + s * 0.143, 0.715 + c * 0.285 + s * 0.140, 0.072 - c * 0.072 - s * 0.283],
        [0.213 - c * 0.213 - s * 0.787, 0.715 - c * 0.715 + s * 0.715, 0.072 + c * 0.928 + s * 0.072],
    ];
    let k = 1.0 + sat.clamp(-100, 100) as f32 / 100.0;
    // SVG saturate.
    let sm = [
        [0.213 + 0.787 * k, 0.715 - 0.715 * k, 0.072 - 0.072 * k],
        [0.213 - 0.213 * k, 0.715 + 0.285 * k, 0.072 - 0.072 * k],
        [0.213 - 0.213 * k, 0.715 - 0.715 * k, 0.072 + 0.928 * k],
    ];
    let mut m = [[0i32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let x: f32 = (0..3).map(|k2| sm[i][k2] * h[k2][j]).sum();
            *v = (x * 4096.0).round() as i32;
        }
    }
    m
}

pub fn adjust_colors(img: &Image, p: &AdjustParams) -> Image {
    if p.is_identity() {
        return img.clone();
    }
    let (lr, lg, lb) = (channel_lut(p.red, p), channel_lut(p.green, p), channel_lut(p.blue, p));
    let use_matrix = p.hue != 0 || p.saturation != 0;
    let m = hue_sat_matrix(p.hue, p.saturation);
    map_straight(img, |[b, g, r]| {
        let (r, g, b) = (lr[r as usize] as i32, lg[g as usize] as i32, lb[b as usize] as i32);
        if !use_matrix {
            return [b as u8, g as u8, r as u8];
        }
        let ch = |row: [i32; 3]| ((row[0] * r + row[1] * g + row[2] * b + 2048) >> 12).clamp(0, 255) as u8;
        [ch(m[2]), ch(m[1]), ch(m[0])]
    })
}

/// Per-channel histograms (B, G, R) of straight colours of visible pixels.
fn histograms(img: &Image) -> ([u64; 256], [u64; 256], [u64; 256], u64) {
    let (mut hb, mut hg, mut hr) = ([0u64; 256], [0u64; 256], [0u64; 256]);
    let mut n = 0;
    for p in img.data.chunks_exact(4) {
        if p[3] == 0 {
            continue;
        }
        let c = Color::from_premul([p[0], p[1], p[2], p[3]]);
        hb[c.b as usize] += 1;
        hg[c.g as usize] += 1;
        hr[c.r as usize] += 1;
        n += 1;
    }
    (hb, hg, hr, n)
}

/// Low and high values after clipping 0.5 % of the pixels at each end.
fn clip_range(h: &[u64; 256], n: u64) -> (u8, u8) {
    let clip = n / 200;
    let mut acc = 0;
    let mut lo = 0;
    for (i, &c) in h.iter().enumerate() {
        acc += c;
        if acc > clip {
            lo = i;
            break;
        }
    }
    acc = 0;
    let mut hi = 255;
    for (i, &c) in h.iter().enumerate().rev() {
        acc += c;
        if acc > clip {
            hi = i;
            break;
        }
    }
    (lo as u8, hi as u8)
}

fn stretch_lut(lo: u8, hi: u8) -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (v, o) in lut.iter_mut().enumerate() {
        *o = if hi <= lo {
            v as u8
        } else {
            (((v as i32 - lo as i32) * 255 + (hi - lo) as i32 / 2) / (hi - lo) as i32).clamp(0, 255) as u8
        };
    }
    lut
}

/// Stretch each channel to the full range independently (also fixes colour casts).
pub fn auto_levels(img: &Image) -> Image {
    let (hb, hg, hr, n) = histograms(img);
    if n == 0 {
        return img.clone();
    }
    let lb = {
        let (l, h) = clip_range(&hb, n);
        stretch_lut(l, h)
    };
    let lg = {
        let (l, h) = clip_range(&hg, n);
        stretch_lut(l, h)
    };
    let lr = {
        let (l, h) = clip_range(&hr, n);
        stretch_lut(l, h)
    };
    map_straight(img, |[b, g, r]| [lb[b as usize], lg[g as usize], lr[r as usize]])
}

/// Stretch all channels by the same amount (keeps the colour balance).
pub fn auto_contrast(img: &Image) -> Image {
    let (hb, hg, hr, n) = histograms(img);
    if n == 0 {
        return img.clone();
    }
    let ranges = [clip_range(&hb, n), clip_range(&hg, n), clip_range(&hr, n)];
    let lo = ranges.iter().map(|r| r.0).min().unwrap_or(0);
    let hi = ranges.iter().map(|r| r.1).max().unwrap_or(255);
    let lut = stretch_lut(lo, hi);
    map_straight(img, |[b, g, r]| [lut[b as usize], lut[g as usize], lut[r as usize]])
}

pub fn grayscale(img: &Image) -> Image {
    // Works directly on premultiplied data: luma is linear in the channels.
    map_pixels(img, |p| {
        let y = super::luma(p[0], p[1], p[2]);
        [y, y, y, p[3]]
    })
}

pub fn sepia(img: &Image) -> Image {
    map_straight(img, |[b, g, r]| {
        let (r, g, b) = (r as u32, g as u32, b as u32);
        let nr = ((r * 393 + g * 769 + b * 189) / 1000).min(255) as u8;
        let ng = ((r * 349 + g * 686 + b * 168) / 1000).min(255) as u8;
        let nb = ((r * 272 + g * 534 + b * 131) / 1000).min(255) as u8;
        [nb, ng, nr]
    })
}

/// Invert colours (alpha kept).
pub fn negative(img: &Image) -> Image {
    // Premultiplied inverse of c/a is (a - c).
    map_pixels(img, |p| [p[3] - p[0].min(p[3]), p[3] - p[1].min(p[3]), p[3] - p[2].min(p[3]), p[3]])
}

// ------------------------------------------------------------------ colour count

/// Number of distinct pixel values (colour + alpha).
pub fn count_colors(img: &Image) -> usize {
    let mut bits = vec![0u64; (1 << 24) / 64];
    let mut opaque = 0usize;
    let mut others: HashSet<u32> = HashSet::new();
    for p in img.data.chunks_exact(4) {
        if p[3] == 255 {
            let k = (p[2] as usize) << 16 | (p[1] as usize) << 8 | p[0] as usize;
            let (w, b) = (k >> 6, 1u64 << (k & 63));
            if bits[w] & b == 0 {
                bits[w] |= b;
                opaque += 1;
            }
        } else {
            others.insert(u32::from_le_bytes([p[0], p[1], p[2], p[3]]));
        }
    }
    opaque + others.len()
}

// ------------------------------------------------------------------ colour reduction

/// Palette sizes offered in the UI.
pub const REDUCE_PRESETS: [u16; 4] = [256, 16, 4, 2];

/// Reduce to at most `colors` colours (median cut), optionally with Floyd–Steinberg dithering.
/// Alpha is preserved; only RGB is quantised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReduceParams {
    pub colors: u16,
    pub dither: bool,
}

impl Default for ReduceParams {
    fn default() -> Self {
        Self { colors: 256, dither: true }
    }
}

const QBITS: u32 = 5;
const QN: usize = 1 << (3 * QBITS);

struct Bucket {
    count: u64,
    sum: [u64; 3],
}

/// Median-cut palette (BGR) of at most `n` colours.
pub fn build_palette(img: &Image, n: usize) -> Vec<[u8; 3]> {
    let n = n.clamp(1, 256);
    let mut hist: Vec<Bucket> = (0..QN).map(|_| Bucket { count: 0, sum: [0; 3] }).collect();
    for p in img.data.chunks_exact(4) {
        if p[3] == 0 {
            continue;
        }
        let c = Color::from_premul([p[0], p[1], p[2], p[3]]);
        let k = qkey(c.b, c.g, c.r);
        let bk = &mut hist[k];
        bk.count += 1;
        bk.sum[0] += c.b as u64;
        bk.sum[1] += c.g as u64;
        bk.sum[2] += c.r as u64;
    }
    // Mean colour of each used bucket.
    let used: Vec<(usize, [u8; 3])> = hist
        .iter()
        .enumerate()
        .filter(|(_, b)| b.count > 0)
        .map(|(i, b)| (i, [0, 1, 2].map(|c| ((b.sum[c] + b.count / 2) / b.count) as u8)))
        .collect();
    if used.is_empty() {
        return vec![[0, 0, 0]];
    }
    let mut boxes: Vec<Vec<(usize, [u8; 3])>> = vec![used];
    while boxes.len() < n {
        // Box with the largest (range x population); deterministic tie-break by index.
        let mut best: Option<(usize, usize, u64)> = None;
        for (bi, bx) in boxes.iter().enumerate() {
            if bx.len() < 2 {
                continue;
            }
            let (axis, range) = widest_axis(bx);
            if range == 0 {
                continue;
            }
            let pop: u64 = bx.iter().map(|(i, _)| hist[*i].count).sum();
            let score = range as u64 * pop;
            if best.map_or(true, |b| score > b.2) {
                best = Some((bi, axis, score));
            }
        }
        let Some((bi, axis, _)) = best else { break };
        let mut bx = boxes.swap_remove(bi);
        bx.sort_by_key(|(i, c)| (c[axis], *i));
        let total: u64 = bx.iter().map(|(i, _)| hist[*i].count).sum();
        let mut acc = 0;
        let mut cut = 1;
        for (j, (i, _)) in bx.iter().enumerate() {
            acc += hist[*i].count;
            if acc * 2 >= total {
                cut = j + 1;
                break;
            }
        }
        let cut = cut.clamp(1, bx.len() - 1);
        let tail = bx.split_off(cut);
        boxes.push(bx);
        boxes.push(tail);
    }
    let mut pal: Vec<[u8; 3]> = boxes
        .iter()
        .map(|bx| {
            let mut s = [0u64; 3];
            let mut cnt = 0;
            for (i, _) in bx {
                let b = &hist[*i];
                cnt += b.count;
                for c in 0..3 {
                    s[c] += b.sum[c];
                }
            }
            [0, 1, 2].map(|c| ((s[c] + cnt / 2) / cnt.max(1)) as u8)
        })
        .collect();
    pal.sort();
    pal.dedup();
    pal
}

fn widest_axis(bx: &[(usize, [u8; 3])]) -> (usize, u8) {
    let mut best = (0, 0u8);
    for axis in 0..3 {
        let lo = bx.iter().map(|(_, c)| c[axis]).min().unwrap_or(0);
        let hi = bx.iter().map(|(_, c)| c[axis]).max().unwrap_or(0);
        // Green first on ties: the eye is most sensitive to it.
        let r = hi - lo;
        if r > best.1 || (r == best.1 && axis == 1) {
            best = (axis, r);
        }
    }
    best
}

#[inline]
fn qkey(b: u8, g: u8, r: u8) -> usize {
    let s = 8 - QBITS;
    ((r as usize >> s) << (2 * QBITS)) | ((g as usize >> s) << QBITS) | (b as usize >> s)
}

/// Nearest palette entry, cached on a 6-bit-per-channel grid.
struct Nearest<'a> {
    pal: &'a [[u8; 3]],
    cache: Vec<u16>,
}

impl<'a> Nearest<'a> {
    fn new(pal: &'a [[u8; 3]]) -> Self {
        Self { pal, cache: vec![u16::MAX; 1 << 18] }
    }
    fn find(&mut self, b: u8, g: u8, r: u8) -> usize {
        let k = ((r as usize >> 2) << 12) | ((g as usize >> 2) << 6) | (b as usize >> 2);
        let c = self.cache[k];
        if c != u16::MAX {
            return c as usize;
        }
        // Search with the cell's centre.
        let (cb, cg, cr) = ((b & !3) as i32 + 2, (g & !3) as i32 + 2, (r & !3) as i32 + 2);
        let mut best = (0usize, i32::MAX);
        for (i, p) in self.pal.iter().enumerate() {
            let d = (p[0] as i32 - cb).pow(2) * 2 + (p[1] as i32 - cg).pow(2) * 4 + (p[2] as i32 - cr).pow(2) * 3;
            if d < best.1 {
                best = (i, d);
            }
        }
        self.cache[k] = best.0 as u16;
        best.0
    }
}

pub fn reduce_colors(img: &Image, p: &ReduceParams) -> Image {
    let n = (p.colors as usize).clamp(2, 256);
    if img.is_empty() {
        return img.clone();
    }
    // Already within budget: nothing to do (keeps exact colours).
    let mut distinct: HashSet<[u8; 3]> = HashSet::new();
    let mut last = [0u8; 4];
    for px in img.data.chunks_exact(4) {
        // Screenshots have long runs of one colour: skip the hash for repeats.
        if px[3] > 0 && px != last {
            last.copy_from_slice(px);
            let c = Color::from_premul([px[0], px[1], px[2], px[3]]);
            distinct.insert([c.b, c.g, c.r]);
            if distinct.len() > n {
                break;
            }
        }
    }
    if distinct.len() <= n {
        return img.clone();
    }
    let pal = build_palette(img, n);
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = img.clone();
    if !p.dither {
        // Independent per pixel: parallel bands, each with its own lookup cache.
        par_rows(&mut out.data, w * 4, |_, band| {
            let mut near = Nearest::new(&pal);
            for px in band.chunks_exact_mut(4) {
                if px[3] == 0 {
                    continue;
                }
                let a = px[3];
                let c = Color::from_premul([px[0], px[1], px[2], a]);
                let q = pal[near.find(c.b, c.g, c.r)];
                px.copy_from_slice(&Color::rgba(q[2], q[1], q[0], a).premul_bgra());
            }
        });
        return out;
    }
    // Floyd–Steinberg, serpentine. Errors in 1/16 units.
    let mut near = Nearest::new(&pal);
    let mut err_cur = vec![[0i32; 3]; w + 2];
    let mut err_next = vec![[0i32; 3]; w + 2];
    for y in 0..h {
        let ltr = y % 2 == 0;
        for i in 0..w {
            let x = if ltr { i } else { w - 1 - i };
            let o = (y * w + x) * 4;
            let a = out.data[o + 3];
            if a == 0 {
                continue;
            }
            let c = Color::from_premul([out.data[o], out.data[o + 1], out.data[o + 2], a]);
            let e = err_cur[x + 1];
            let want =
                [c.b as i32 + e[0] / 16, c.g as i32 + e[1] / 16, c.r as i32 + e[2] / 16].map(|v| v.clamp(0, 255));
            let q = pal[near.find(want[0] as u8, want[1] as u8, want[2] as u8)];
            out.data[o..o + 4].copy_from_slice(&Color::rgba(q[2], q[1], q[0], a).premul_bgra());
            let d = [want[0] - q[0] as i32, want[1] - q[1] as i32, want[2] - q[2] as i32];
            let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
            for c in 0..3 {
                err_cur[fwd][c] += d[c] * 7;
                err_next[back][c] += d[c] * 3;
                err_next[x + 1][c] += d[c] * 5;
                err_next[fwd][c] += d[c];
            }
        }
        std::mem::swap(&mut err_cur, &mut err_next);
        err_next.iter_mut().for_each(|e| *e = [0; 3]);
    }
    out
}

// ------------------------------------------------------------------ background removal

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransparencyMode {
    /// Every matching pixel in the image.
    Global,
    /// Only matching pixels connected to `seed` (flood fill).
    #[default]
    Contiguous,
}

/// Make a colour transparent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransparencyParams {
    /// Colour to remove (alpha ignored).
    pub color: Color,
    /// 0..=100: how far (largest channel difference, in percent) a pixel may be from `color`.
    pub tolerance: u8,
    pub mode: TransparencyMode,
    /// Start pixel for `Contiguous` mode (usually where the user clicked).
    pub seed: Point,
    /// Partially remove pixels just outside the tolerance and strip the removed colour from
    /// them, so anti-aliased edges blend cleanly onto any new background.
    pub smooth_edges: bool,
}

impl Default for TransparencyParams {
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            tolerance: 10,
            mode: TransparencyMode::Contiguous,
            seed: Point::new(0, 0),
            smooth_edges: true,
        }
    }
}

/// Removal amount per pixel: 255 = fully removed, 0 = kept.
pub fn transparency_mask(img: &Image, p: &TransparencyParams) -> Mask {
    let (w, h) = (img.width as usize, img.height as usize);
    let tol = p.tolerance.min(100) as i32 * 255 / 100;
    let soft = if p.smooth_edges { (tol / 2).max(16) } else { 0 };
    let key = [p.color.b as i32, p.color.g as i32, p.color.r as i32];
    let dist = |i: usize| -> i32 {
        let px = &img.data[i * 4..i * 4 + 4];
        if px[3] == 0 {
            return 0;
        }
        let c = Color::from_premul([px[0], px[1], px[2], px[3]]);
        (c.b as i32 - key[0]).abs().max((c.g as i32 - key[1]).abs()).max((c.r as i32 - key[2]).abs())
    };
    let amount = |d: i32| -> u8 {
        if d <= tol {
            255
        } else if d < tol + soft {
            (255 - (d - tol) * 255 / soft).clamp(0, 255) as u8
        } else {
            0
        }
    };
    let mut mask = Mask::new(img.width, img.height);
    match p.mode {
        TransparencyMode::Global => {
            par_rows(&mut mask.data, w, |y0, band| {
                for (i, m) in band.iter_mut().enumerate() {
                    *m = amount(dist(y0 * w + i));
                }
            });
        }
        TransparencyMode::Contiguous => {
            let (sx, sy) = (p.seed.x, p.seed.y);
            if sx < 0 || sy < 0 || sx as usize >= w || sy as usize >= h {
                return mask;
            }
            let start = sy as usize * w + sx as usize;
            if dist(start) > tol {
                return mask;
            }
            let mut core = vec![false; w * h];
            let mut q = VecDeque::new();
            core[start] = true;
            q.push_back(start);
            while let Some(i) = q.pop_front() {
                mask.data[i] = 255;
                let (x, y) = (i % w, i / w);
                let mut visit = |j: usize| {
                    if !core[j] && dist(j) <= tol {
                        core[j] = true;
                        q.push_back(j);
                    }
                };
                if x > 0 {
                    visit(i - 1);
                }
                if x + 1 < w {
                    visit(i + 1);
                }
                if y > 0 {
                    visit(i - w);
                }
                if y + 1 < h {
                    visit(i + w);
                }
            }
            if soft > 0 {
                // Soft edge: pixels touching the flooded area (8-neighbourhood).
                for y in 0..h {
                    for x in 0..w {
                        let i = y * w + x;
                        if core[i] {
                            continue;
                        }
                        let touches = (y.saturating_sub(1)..(y + 2).min(h))
                            .any(|yy| (x.saturating_sub(1)..(x + 2).min(w)).any(|xx| core[yy * w + xx]));
                        if touches {
                            mask.data[i] = amount(dist(i));
                        }
                    }
                }
            }
        }
    }
    mask
}

/// Apply [`transparency_mask`].
pub fn make_transparent(img: &Image, p: &TransparencyParams) -> Image {
    let mask = transparency_mask(img, p);
    apply_removal(img, &mask, p.color, p.smooth_edges)
}

/// Remove `mask` (255 = fully removed) from the image; with `decontaminate`, partially removed
/// pixels also lose the `key` colour's contribution.
pub fn apply_removal(img: &Image, mask: &Mask, key: Color, decontaminate: bool) -> Image {
    let mut out = img.clone();
    let kk = [key.b as u32, key.g as u32, key.r as u32];
    for (px, &m) in out.data.chunks_exact_mut(4).zip(&mask.data) {
        if m == 0 {
            continue;
        }
        if m == 255 {
            px.copy_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let a = px[3] as u32;
        let keep = 255 - m as u32;
        let na = (a * keep + 127) / 255;
        for c in 0..3 {
            let v = px[c] as u32;
            px[c] = if decontaminate {
                // c_premul - m * key * a  (in 0..=255 units), clamped to the new alpha.
                let sub = (m as u32 * kk[c] * a + 32512) / 65025;
                v.saturating_sub(sub).min(na) as u8
            } else {
                ((v * keep + 127) / 255) as u8
            };
        }
        px[3] = na as u8;
    }
    out
}

/// Preview of a mask: the image with masked pixels tinted by `tint` (e.g. 50 % magenta).
pub fn mask_preview(img: &Image, mask: &Mask, tint: Color) -> Image {
    let t = tint.premul_bgra();
    let mut out = img.clone();
    for (px, &m) in out.data.chunks_exact_mut(4).zip(&mask.data) {
        if m == 0 {
            continue;
        }
        let tinted = super::over(t, [px[0], px[1], px[2], px[3]]);
        let o = super::lerp_px([px[0], px[1], px[2], px[3]], tinted, m as u32);
        px.copy_from_slice(&o);
    }
    out
}

/// The image composited over a grey checkerboard (shows transparency).
pub fn checkerboard(img: &Image, cell: u32) -> Image {
    let cell = cell.max(1);
    let mut out = img.clone();
    let w = img.width as usize;
    par_rows(&mut out.data, w * 4, |y0, band| {
        for (r, row) in band.chunks_exact_mut(w * 4).enumerate() {
            let y = (y0 + r) as u32;
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let light = ((x as u32 / cell) + (y / cell)) % 2 == 0;
                let v = if light { 255 } else { 204 };
                let o = super::over([px[0], px[1], px[2], px[3]], [v, v, v, 255]);
                px.copy_from_slice(&o);
            }
        }
    });
    out
}
