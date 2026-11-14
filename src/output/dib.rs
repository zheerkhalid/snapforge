//! Packed DIB (BITMAPINFOHEADER / BITMAPV5HEADER + pixels) building and parsing.
//! Used by the clipboard (CF_DIB / CF_DIBV5) and the printer.

use crate::common::Image;
use anyhow::{bail, ensure, Result};

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const LCS_SRGB: u32 = 0x7352_4742; // 'sRGB'
const LCS_GM_IMAGES: u32 = 4;
const V5_SIZE: usize = 124;
const INFO_SIZE: usize = 40;

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_i32(v: &mut Vec<u8>, x: i32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// Packed CF_DIBV5: 32-bit BI_BITFIELDS, straight alpha, bottom-up (most compatible).
pub fn build_dibv5(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let size_image = (w * h * 4) as u32;
    let mut v = Vec::with_capacity(V5_SIZE + w * h * 4);
    put_u32(&mut v, V5_SIZE as u32);
    put_i32(&mut v, w as i32);
    put_i32(&mut v, h as i32); // positive = bottom-up
    put_u16(&mut v, 1);
    put_u16(&mut v, 32);
    put_u32(&mut v, BI_BITFIELDS);
    put_u32(&mut v, size_image);
    put_i32(&mut v, 3780); // 96 DPI in pixels per metre
    put_i32(&mut v, 3780);
    put_u32(&mut v, 0);
    put_u32(&mut v, 0);
    put_u32(&mut v, 0x00FF_0000); // red
    put_u32(&mut v, 0x0000_FF00); // green
    put_u32(&mut v, 0x0000_00FF); // blue
    put_u32(&mut v, 0xFF00_0000); // alpha
    put_u32(&mut v, LCS_SRGB);
    v.extend_from_slice(&[0u8; 36]); // endpoints
    v.extend_from_slice(&[0u8; 12]); // gamma
    put_u32(&mut v, LCS_GM_IMAGES);
    v.extend_from_slice(&[0u8; 12]); // profile data, size, reserved
    debug_assert_eq!(v.len(), V5_SIZE);
    let rgba = img.to_rgba_straight();
    for y in (0..h).rev() {
        for px in rgba[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            v.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    v
}

/// Packed CF_DIB fallback: 24-bit BI_RGB, bottom-up, composited over white.
pub fn build_dib24(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let stride = (w * 3).div_ceil(4) * 4;
    let mut v = Vec::with_capacity(INFO_SIZE + stride * h);
    put_u32(&mut v, INFO_SIZE as u32);
    put_i32(&mut v, w as i32);
    put_i32(&mut v, h as i32);
    put_u16(&mut v, 1);
    put_u16(&mut v, 24);
    put_u32(&mut v, BI_RGB);
    put_u32(&mut v, (stride * h) as u32);
    put_i32(&mut v, 3780);
    put_i32(&mut v, 3780);
    put_u32(&mut v, 0);
    put_u32(&mut v, 0);
    let rgb = img.to_rgb_over_white();
    for y in (0..h).rev() {
        let start = v.len();
        for px in rgb[y * w * 3..(y + 1) * w * 3].chunks_exact(3) {
            v.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        v.resize(start + stride, 0);
    }
    v
}

/// BGRX pixels composited over white, top-down (for StretchDIBits with a negative height).
pub fn bgrx_over_white(img: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.data.len());
    for px in img.data.chunks_exact(4) {
        let ia = 255 - px[3] as u32;
        out.extend_from_slice(&[
            (px[0] as u32 + ia).min(255) as u8,
            (px[1] as u32 + ia).min(255) as u8,
            (px[2] as u32 + ia).min(255) as u8,
            255,
        ]);
    }
    out
}

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Extract a channel by mask and scale it to 0..=255.
fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let max = if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 };
    let val = (v & mask) >> shift;
    if bits == 8 {
        val as u8
    } else {
        ((val as u64 * 255 + max as u64 / 2) / max as u64) as u8
    }
}

/// Parse a packed DIB (as found in CF_DIB / CF_DIBV5). Handles bottom-up and top-down,
/// 1/4/8-bit palettes, 16/24/32-bit, BI_BITFIELDS (masks in the V4/V5 header or after a
/// 40-byte header). A 32-bit bitmap whose alpha is all zero is treated as opaque.
pub fn parse_dib(b: &[u8]) -> Result<Image> {
    ensure!(b.len() >= INFO_SIZE, "DIB too short");
    let hsize = rd_u32(b, 0) as usize;
    ensure!((INFO_SIZE..=b.len()).contains(&hsize), "unsupported DIB header size {hsize}");
    let width = rd_u32(b, 4) as i32;
    let height = rd_u32(b, 8) as i32;
    let bpp = rd_u16(b, 14);
    let compression = rd_u32(b, 16);
    let clr_used = rd_u32(b, 32) as usize;
    ensure!(width > 0 && height != 0, "invalid DIB size {width}x{height}");
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    ensure!(w <= 1 << 16 && h <= 1 << 16, "DIB too large");
    let top_down = height < 0;

    let mut off = hsize;
    let (mut rm, mut gm, mut bm, mut am) = match bpp {
        16 => (0x7C00, 0x03E0, 0x001F, 0),
        _ => (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0),
    };
    match compression {
        BI_RGB => {}
        BI_BITFIELDS => {
            ensure!(bpp == 16 || bpp == 32, "BI_BITFIELDS with {bpp} bits");
            if hsize >= 52 {
                (rm, gm, bm) = (rd_u32(b, 40), rd_u32(b, 44), rd_u32(b, 48));
                if hsize >= 56 {
                    am = rd_u32(b, 52);
                }
            } else {
                ensure!(b.len() >= off + 12, "DIB truncated (masks)");
                (rm, gm, bm) = (rd_u32(b, off), rd_u32(b, off + 4), rd_u32(b, off + 8));
                off += 12;
            }
        }
        c => bail!("unsupported DIB compression {c}"),
    }
    if bpp == 32 && am == 0 && compression == BI_RGB {
        am = 0xFF00_0000; // commonly carries alpha; checked below
    }

    let palette: Vec<[u8; 3]> = if bpp <= 8 {
        let n = if clr_used == 0 { 1usize << bpp } else { clr_used.min(256) };
        ensure!(b.len() >= off + n * 4, "DIB truncated (palette)");
        let p = (0..n).map(|i| [b[off + i * 4 + 2], b[off + i * 4 + 1], b[off + i * 4]]).collect();
        off += n * 4;
        p
    } else {
        if clr_used > 0 {
            off += clr_used * 4; // optional colour table we ignore
        }
        Vec::new()
    };

    let stride = (w * bpp as usize).div_ceil(32) * 4;
    ensure!(matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32), "unsupported DIB bit depth {bpp}");
    ensure!(b.len() >= off + stride * h, "DIB truncated (pixels)");
    let px = &b[off..];

    let mut rgba = vec![0u8; w * h * 4];
    let mut alpha_seen = false;
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let line = &px[src_row * stride..src_row * stride + stride];
        for x in 0..w {
            let o = (row * w + x) * 4;
            let (r, g, bl, a) = match bpp {
                1 | 4 | 8 => {
                    let bits = bpp as usize;
                    let bit = x * bits;
                    let byte = line[bit / 8];
                    let idx = (byte >> (8 - bits - bit % 8)) & ((1u16 << bits) - 1) as u8;
                    let c = palette.get(idx as usize).copied().unwrap_or([0, 0, 0]);
                    (c[0], c[1], c[2], 255)
                }
                24 => (line[x * 3 + 2], line[x * 3 + 1], line[x * 3], 255),
                16 => {
                    let v = rd_u16(line, x * 2) as u32;
                    (channel(v, rm), channel(v, gm), channel(v, bm), 255)
                }
                _ => {
                    let v = rd_u32(line, x * 4);
                    let a = if am != 0 { channel(v, am) } else { 255 };
                    if a != 0 {
                        alpha_seen = true;
                    }
                    (channel(v, rm), channel(v, gm), channel(v, bm), a)
                }
            };
            rgba[o..o + 4].copy_from_slice(&[r, g, bl, a]);
        }
    }
    if bpp == 32 && !alpha_seen {
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Ok(Image::from_rgba_straight(w as u32, h as u32, &rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Image {
        let mut rgba = Vec::new();
        for y in 0..5u32 {
            for x in 0..7u32 {
                rgba.extend_from_slice(&[(x * 30) as u8, (y * 50) as u8, 77, if x == 6 { 0 } else { 255 - y as u8 }]);
            }
        }
        Image::from_rgba_straight(7, 5, &rgba)
    }

    #[test]
    fn dibv5_roundtrip_keeps_alpha() {
        let img = sample();
        let back = parse_dib(&build_dibv5(&img)).unwrap();
        assert_eq!((back.width, back.height), (7, 5));
        for (a, b) in img.data.iter().zip(&back.data) {
            assert!(a.abs_diff(*b) <= 1);
        }
    }

    #[test]
    fn dib24_roundtrip_is_opaque_over_white() {
        let img = sample();
        let back = parse_dib(&build_dib24(&img)).unwrap();
        assert!(!back.has_transparency());
        assert_eq!(back.pixel(6, 0), [255, 255, 255, 255]);
        assert_eq!(back.pixel(0, 0), [77, 0, 0, 255]);
    }

    fn header(w: i32, h: i32, bpp: u16, comp: u32) -> Vec<u8> {
        let mut v = Vec::new();
        put_u32(&mut v, 40);
        put_i32(&mut v, w);
        put_i32(&mut v, h);
        put_u16(&mut v, 1);
        put_u16(&mut v, bpp);
        put_u32(&mut v, comp);
        v.extend_from_slice(&[0u8; 20]);
        v
    }

    #[test]
    fn zero_alpha_32bit_is_opaque_and_top_down_works() {
        let mut d = header(2, -2, 32, BI_RGB);
        // Top-down: row 0 = red, blue; row 1 = green, white. Alpha all zero.
        for px in [[0, 0, 255, 0], [255, 0, 0, 0], [0, 255, 0, 0], [255, 255, 255, 0]] {
            d.extend_from_slice(&px);
        }
        let img = parse_dib(&d).unwrap();
        assert_eq!(img.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(img.pixel(1, 0), [255, 0, 0, 255]);
        assert_eq!(img.pixel(0, 1), [0, 255, 0, 255]);
    }

    #[test]
    fn bitfields_after_info_header() {
        // 16-bit 565 with masks following a 40-byte header, bottom-up, 1x2 (row padding).
        let mut d = header(1, 2, 16, BI_BITFIELDS);
        put_u32(&mut d, 0xF800);
        put_u32(&mut d, 0x07E0);
        put_u32(&mut d, 0x001F);
        d.extend_from_slice(&0x001Fu16.to_le_bytes()); // bottom row: blue
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&0xF800u16.to_le_bytes()); // top row: red
        d.extend_from_slice(&[0, 0]);
        let img = parse_dib(&d).unwrap();
        assert_eq!(img.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(img.pixel(0, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn palette_8bit() {
        let mut d = header(3, 1, 8, BI_RGB);
        d[32] = 2; // clrUsed
        d.extend_from_slice(&[0, 0, 0, 0, 10, 20, 30, 0]);
        d.extend_from_slice(&[1, 0, 1, 0]);
        let img = parse_dib(&d).unwrap();
        assert_eq!(img.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(img.pixel(1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_dib(&[1, 2, 3]).is_err());
        assert!(parse_dib(&header(100, 100, 24, BI_RGB)).is_err());
    }
}
