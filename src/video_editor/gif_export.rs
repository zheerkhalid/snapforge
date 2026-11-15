//! Animated GIF encoding: per-frame median-cut palettes (`effects::build_palette`), optional
//! Floyd–Steinberg dithering, and frame differencing (only the changed rectangle is stored,
//! unchanged pixels inside it are transparent, identical frames extend the previous delay).

use crate::common::Image;
use anyhow::{Context, Result};
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GifOptions {
    pub fps: f32,
    pub dither: bool,
    /// Loop forever (otherwise play once).
    pub looping: bool,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self { fps: 12.0, dither: true, looping: true }
    }
}

pub struct GifWriter<W: Write> {
    enc: gif::Encoder<W>,
    width: u16,
    height: u16,
    opts: GifOptions,
    prev: Option<Image>,
    pending: Option<gif::Frame<'static>>,
    frames: u64,
    /// Centiseconds already assigned (to spread rounding).
    cs_done: u64,
}

impl<W: Write> GifWriter<W> {
    pub fn new(out: W, width: u32, height: u32, opts: GifOptions) -> Result<Self> {
        let w = u16::try_from(width.clamp(1, 65535)).unwrap_or(u16::MAX);
        let h = u16::try_from(height.clamp(1, 65535)).unwrap_or(u16::MAX);
        let mut enc = gif::Encoder::new(out, w, h, &[]).context("could not start the GIF")?;
        enc.set_repeat(if opts.looping { gif::Repeat::Infinite } else { gif::Repeat::Finite(0) })
            .context("could not write the GIF loop setting")?;
        Ok(Self { enc, width: w, height: h, opts, prev: None, pending: None, frames: 0, cs_done: 0 })
    }

    /// Delay of the next frame in centiseconds, from the frame clock (no drift).
    fn next_delay(&mut self) -> u16 {
        self.frames += 1;
        let fps = if self.opts.fps.is_finite() { self.opts.fps.clamp(1.0, 50.0) } else { 10.0 } as f64;
        let total = (self.frames as f64 * 100.0 / fps).round() as u64;
        let d = total.saturating_sub(self.cs_done).max(1);
        self.cs_done += d;
        d.min(u16::MAX as u64) as u16
    }

    /// Add a frame (`width`×`height`, opaque BGRA).
    pub fn add_frame(&mut self, img: &Image) -> Result<()> {
        let delay = self.next_delay();
        if img.width != self.width as u32 || img.height != self.height as u32 {
            anyhow::bail!("GIF frame size mismatch");
        }
        let changed = match &self.prev {
            Some(p) => diff_rect(p, img),
            None => Some((0, 0, img.width, img.height)),
        };
        let Some((x, y, w, h)) = changed else {
            // Identical frame: show the previous one longer.
            if let Some(p) = self.pending.as_mut() {
                p.delay = p.delay.saturating_add(delay);
            }
            return Ok(());
        };
        let crop = img.crop(crate::common::Rect::new(x as i32, y as i32, w as i32, h as i32));
        let prev_crop =
            self.prev.as_ref().map(|p| p.crop(crate::common::Rect::new(x as i32, y as i32, w as i32, h as i32)));
        let q = quantize(&crop, prev_crop.as_ref(), self.opts.dither);
        let mut frame = gif::Frame {
            left: x as u16,
            top: y as u16,
            width: w as u16,
            height: h as u16,
            delay,
            dispose: gif::DisposalMethod::Keep,
            transparent: q.transparent,
            palette: Some(q.palette),
            buffer: std::borrow::Cow::Owned(q.indices),
            ..gif::Frame::default()
        };
        frame.needs_user_input = false;
        if let Some(p) = self.pending.replace(frame) {
            self.enc.write_frame(&p).context("could not write a GIF frame")?;
        }
        self.prev = Some(img.clone());
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        if let Some(p) = self.pending.take() {
            self.enc.write_frame(&p).context("could not write a GIF frame")?;
        }
        self.enc.into_inner().context("could not finish the GIF")
    }
}

/// Bounding box `(x, y, w, h)` of pixels that differ, `None` when identical.
fn diff_rect(a: &Image, b: &Image) -> Option<(u32, u32, u32, u32)> {
    let w = a.width as usize;
    let (mut l, mut t, mut r, mut bt) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..a.height as usize {
        let ra = &a.data[y * w * 4..(y + 1) * w * 4];
        let rb = &b.data[y * w * 4..(y + 1) * w * 4];
        if ra == rb {
            continue;
        }
        let first = ra.chunks_exact(4).zip(rb.chunks_exact(4)).position(|(p, q)| p != q).unwrap_or(0);
        let last =
            w - 1 - ra.chunks_exact(4).rev().zip(rb.chunks_exact(4).rev()).position(|(p, q)| p != q).unwrap_or(0);
        l = l.min(first);
        r = r.max(last);
        t = t.min(y);
        bt = y;
    }
    (l != usize::MAX).then(|| (l as u32, t as u32, (r - l + 1) as u32, (bt - t + 1) as u32))
}

pub struct Quantized {
    /// RGB triples.
    pub palette: Vec<u8>,
    pub indices: Vec<u8>,
    pub transparent: Option<u8>,
}

/// Nearest palette entry with a 15-bit colour cache.
struct Nearest<'a> {
    pal: &'a [[u8; 3]],
    cache: Vec<u16>,
}

impl<'a> Nearest<'a> {
    fn new(pal: &'a [[u8; 3]]) -> Self {
        Self { pal, cache: vec![u16::MAX; 1 << 15] }
    }
    fn find(&mut self, b: u8, g: u8, r: u8) -> u8 {
        let k = ((r as usize >> 3) << 10) | ((g as usize >> 3) << 5) | (b as usize >> 3);
        if self.cache[k] != u16::MAX {
            return self.cache[k] as u8;
        }
        let (cb, cg, cr) = ((b | 4) as i32, (g | 4) as i32, (r | 4) as i32);
        let mut best = (0usize, i32::MAX);
        for (i, p) in self.pal.iter().enumerate() {
            let d = (p[0] as i32 - cb).pow(2) * 2 + (p[1] as i32 - cg).pow(2) * 4 + (p[2] as i32 - cr).pow(2) * 3;
            if d < best.1 {
                best = (i, d);
            }
        }
        self.cache[k] = best.0 as u16;
        best.0 as u8
    }
}

/// Palette-map `img`. With `prev` (same size), pixels equal to it become transparent.
pub fn quantize(img: &Image, prev: Option<&Image>, dither: bool) -> Quantized {
    let (w, h) = (img.width as usize, img.height as usize);
    let keep = |i: usize| prev.map_or(true, |p| p.data[i * 4..i * 4 + 4] != img.data[i * 4..i * 4 + 4]);
    let colors = if prev.is_some() { 255 } else { 256 };
    let mut pal = crate::effects::build_palette(img, colors); // BGR
    if pal.is_empty() {
        pal.push([0, 0, 0]);
    }
    let transparent = prev.map(|_| pal.len() as u8);
    let mut near = Nearest::new(&pal);
    let mut indices = vec![0u8; w * h];
    if !dither {
        for (i, idx) in indices.iter_mut().enumerate() {
            *idx = if keep(i) {
                let p = &img.data[i * 4..i * 4 + 4];
                near.find(p[0], p[1], p[2])
            } else {
                transparent.unwrap_or(0)
            };
        }
    } else {
        // Serpentine Floyd–Steinberg; errors in 1/16 units, two rows of (w + 2) × 3.
        let mut cur = vec![0i32; (w + 2) * 3];
        let mut next = vec![0i32; (w + 2) * 3];
        for y in 0..h {
            let ltr = y % 2 == 0;
            for step in 0..w {
                let x = if ltr { step } else { w - 1 - step };
                let i = y * w + x;
                if !keep(i) {
                    indices[i] = transparent.unwrap_or(0);
                    continue;
                }
                let p = &img.data[i * 4..i * 4 + 4];
                let e = (x + 1) * 3;
                let want = [
                    (p[0] as i32 + cur[e] / 16).clamp(0, 255),
                    (p[1] as i32 + cur[e + 1] / 16).clamp(0, 255),
                    (p[2] as i32 + cur[e + 2] / 16).clamp(0, 255),
                ];
                let k = near.find(want[0] as u8, want[1] as u8, want[2] as u8);
                indices[i] = k;
                let got = pal[k as usize];
                let (fwd, back) = if ltr { (e + 3, e - 3) } else { (e - 3, e + 3) };
                for c in 0..3 {
                    let err = want[c] - got[c] as i32;
                    cur[fwd + c] += err * 7;
                    next[back + c] += err * 3;
                    next[e + c] += err * 5;
                    next[fwd + c] += err;
                }
            }
            std::mem::swap(&mut cur, &mut next);
            next.iter_mut().for_each(|v| *v = 0);
        }
    }
    let mut palette: Vec<u8> = pal.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
    if transparent.is_some() {
        palette.extend_from_slice(&[0, 0, 0]);
    }
    Quantized { palette, indices, transparent }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(n: u32, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                let b = (n * 40 % 256) as u8;
                img.set_pixel(x, y, [b, g, r, 255]);
            }
        }
        // A moving block.
        for y in 10..20 {
            for x in (n * 5)..(n * 5 + 10).min(w) {
                img.set_pixel(x, y, [255, 255, 255, 255]);
            }
        }
        img
    }

    fn decode(bytes: &[u8]) -> (u16, u16, usize, Vec<u16>) {
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        let mut d = opts.read_info(bytes).expect("decode");
        let (w, h) = (d.width(), d.height());
        let mut delays = Vec::new();
        while let Some(f) = d.read_next_frame().expect("frame") {
            delays.push(f.delay);
        }
        (w, h, delays.len(), delays)
    }

    #[test]
    fn encodes_synthetic_frames_and_decodes_back() {
        for dither in [false, true] {
            let mut out = Vec::new();
            let mut g = GifWriter::new(&mut out, 64, 48, GifOptions { fps: 10.0, dither, looping: true }).expect("new");
            for n in 0..6 {
                g.add_frame(&synthetic(n, 64, 48)).expect("frame");
            }
            g.finish().expect("finish");
            let (w, h, count, delays) = decode(&out);
            assert_eq!((w, h), (64, 48));
            assert_eq!(count, 6);
            assert!(delays.iter().all(|&d| d == 10), "{delays:?}");
        }
    }

    #[test]
    fn identical_frames_merge_into_longer_delay() {
        let mut out = Vec::new();
        let mut g =
            GifWriter::new(&mut out, 32, 32, GifOptions { fps: 20.0, dither: false, looping: false }).expect("new");
        let a = synthetic(0, 32, 32);
        g.add_frame(&a).expect("a");
        g.add_frame(&a).expect("a");
        g.add_frame(&a).expect("a");
        g.add_frame(&synthetic(1, 32, 32)).expect("b");
        g.finish().expect("finish");
        let (_, _, count, delays) = decode(&out);
        assert_eq!(count, 2);
        assert_eq!(delays, vec![15, 5]);
    }

    #[test]
    fn frame_differencing_keeps_pixels_exact_enough() {
        // Two-colour images quantize exactly; the second frame only changes a small area.
        let mut a = Image::filled(40, 30, [0, 0, 255, 255]);
        let mut out = Vec::new();
        let mut g =
            GifWriter::new(&mut out, 40, 30, GifOptions { fps: 10.0, dither: false, looping: true }).expect("new");
        g.add_frame(&a).expect("a");
        for y in 5..10 {
            for x in 5..10 {
                a.set_pixel(x, y, [255, 0, 0, 255]);
            }
        }
        g.add_frame(&a).expect("b");
        g.finish().expect("finish");
        let mut opts = gif::DecodeOptions::new();
        opts.set_color_output(gif::ColorOutput::RGBA);
        let mut d = opts.read_info(out.as_slice()).expect("decode");
        let f1 = d.read_next_frame().expect("f1").expect("f1").clone();
        assert_eq!((f1.width, f1.height), (40, 30));
        let f2 = d.read_next_frame().expect("f2").expect("f2").clone();
        assert_eq!((f2.left, f2.top, f2.width, f2.height), (5, 5, 5, 5));
        // Pixel (0,0) of the second frame is blue in RGBA.
        assert_eq!(&f2.buffer[0..4], &[0, 0, 255, 255]);
    }
}
