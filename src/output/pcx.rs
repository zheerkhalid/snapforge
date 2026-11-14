//! Minimal ZSoft PCX codec.
//!
//! Writing: 24-bit (3 planes of 8 bits, RLE). Reading: 8-bit palette (with the trailing
//! 256-colour palette, or greyscale when it is missing), 24-bit RGB and 32-bit RGBA (4 planes).

use anyhow::{bail, ensure, Result};

const HEADER_LEN: usize = 128;

/// Encode straight RGB (3 bytes per pixel) as a 24-bit RLE PCX.
pub fn encode_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>> {
    ensure!(width > 0 && height > 0, "PCX: empty image");
    ensure!(width <= 65535 && height <= 65535, "PCX: image too large ({width}x{height}, max 65535)");
    ensure!(rgb.len() == width as usize * height as usize * 3, "PCX: pixel buffer has the wrong size");
    // Bytes per plane line must be even.
    let bpl = (width as usize).div_ceil(2) * 2;

    let mut out = Vec::with_capacity(HEADER_LEN + rgb.len() / 2);
    let mut h = [0u8; HEADER_LEN];
    h[0] = 0x0A; // manufacturer
    h[1] = 5; // version 3.0+
    h[2] = 1; // RLE
    h[3] = 8; // bits per pixel per plane
              // xmin, ymin = 0
    h[8..10].copy_from_slice(&((width - 1) as u16).to_le_bytes());
    h[10..12].copy_from_slice(&((height - 1) as u16).to_le_bytes());
    h[12..14].copy_from_slice(&96u16.to_le_bytes());
    h[14..16].copy_from_slice(&96u16.to_le_bytes());
    h[65] = 3; // planes
    h[66..68].copy_from_slice(&(bpl as u16).to_le_bytes());
    h[68] = 1; // colour palette info
    out.extend_from_slice(&h);

    let mut line = vec![0u8; bpl];
    let w = width as usize;
    for y in 0..height as usize {
        let row = &rgb[y * w * 3..(y + 1) * w * 3];
        for plane in 0..3 {
            for x in 0..w {
                line[x] = row[x * 3 + plane];
            }
            // Pad byte (if any) repeats the last pixel so it folds into the run.
            if bpl > w {
                line[w] = line[w - 1];
            }
            rle_line(&line, &mut out);
        }
    }
    Ok(out)
}

fn rle_line(line: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        let mut run = 1;
        while i + run < line.len() && run < 63 && line[i + run] == b {
            run += 1;
        }
        if run > 1 || b >= 0xC0 {
            out.push(0xC0 | run as u8);
        }
        out.push(b);
        i += run;
    }
}

/// True if `data` starts with a plausible PCX header.
pub fn is_pcx(data: &[u8]) -> bool {
    data.len() >= HEADER_LEN && data[0] == 0x0A && matches!(data[1], 0 | 2 | 3 | 4 | 5) && data[2] <= 1
}

/// Decode to straight RGBA. Returns (width, height, rgba).
pub fn decode(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    ensure!(is_pcx(data), "not a PCX file");
    let le = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]) as usize;
    let bpp = data[3];
    let (xmin, ymin, xmax, ymax) = (le(4), le(6), le(8), le(10));
    ensure!(xmax >= xmin && ymax >= ymin, "PCX: invalid dimensions");
    let (w, h) = (xmax - xmin + 1, ymax - ymin + 1);
    let planes = data[65] as usize;
    let bpl = le(66);
    ensure!(bpl >= w, "PCX: bytes per line ({bpl}) smaller than width ({w})");
    let rle = data[2] == 1;
    match (bpp, planes) {
        (8, 1) | (8, 3) | (8, 4) => {}
        _ => bail!("PCX: unsupported layout ({bpp} bits x {planes} planes)"),
    }

    // Decode the whole scanline stream.
    let total = bpl * planes * h;
    let mut raw = Vec::with_capacity(total);
    let mut i = HEADER_LEN;
    while raw.len() < total && i < data.len() {
        let b = data[i];
        i += 1;
        if rle && b >= 0xC0 {
            let n = (b & 0x3F) as usize;
            let Some(&v) = data.get(i) else { break };
            i += 1;
            let n = n.min(total - raw.len());
            raw.extend(std::iter::repeat(v).take(n));
        } else {
            raw.push(b);
        }
    }
    ensure!(raw.len() == total, "PCX: truncated image data");

    let mut rgba = vec![0u8; w * h * 4];
    if planes == 1 {
        // Trailing 256-colour palette: 0x0C marker + 768 bytes.
        let pal: Vec<[u8; 3]> = if data.len() >= HEADER_LEN + 769 && data[data.len() - 769] == 0x0C {
            data[data.len() - 768..].chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
        } else {
            (0..=255u8).map(|v| [v, v, v]).collect()
        };
        for y in 0..h {
            for x in 0..w {
                let c = pal[raw[y * bpl + x] as usize];
                let o = (y * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    } else {
        for y in 0..h {
            let line = &raw[y * bpl * planes..(y + 1) * bpl * planes];
            for x in 0..w {
                let o = (y * w + x) * 4;
                rgba[o] = line[x];
                rgba[o + 1] = line[bpl + x];
                rgba[o + 2] = line[2 * bpl + x];
                rgba[o + 3] = if planes == 4 { line[3 * bpl + x] } else { 255 };
            }
        }
    }
    Ok((w as u32, h as u32, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                // Runs, values >= 0xC0 and noise.
                let r = if x < w / 2 { 0xC5 } else { (x * 7) as u8 };
                v.extend_from_slice(&[r, (y * 13) as u8, ((x ^ y) * 31) as u8]);
            }
        }
        v
    }

    #[test]
    fn roundtrip_odd_and_even_widths() {
        for (w, h) in [(1, 1), (7, 5), (8, 3), (200, 9)] {
            let rgb = pattern(w, h);
            let enc = encode_rgb(w, h, &rgb).unwrap();
            assert!(is_pcx(&enc));
            let (dw, dh, rgba) = decode(&enc).unwrap();
            assert_eq!((dw, dh), (w, h));
            for (i, px) in rgba.chunks_exact(4).enumerate() {
                assert_eq!(&px[..3], &rgb[i * 3..i * 3 + 3]);
                assert_eq!(px[3], 255);
            }
        }
    }

    #[test]
    fn rle_compresses_flat_runs() {
        let rgb = vec![10u8; 64 * 64 * 3];
        let enc = encode_rgb(64, 64, &rgb).unwrap();
        // 64 px per plane line → two runs (63 + 1) → at most 3 bytes per plane line.
        assert!(enc.len() <= HEADER_LEN + 64 * 3 * 3);
    }

    #[test]
    fn decodes_8bit_palette() {
        let (w, h) = (3usize, 2usize);
        let bpl = 4usize;
        let mut d = vec![0u8; HEADER_LEN];
        d[0] = 0x0A;
        d[1] = 5;
        d[2] = 1;
        d[3] = 8;
        d[8] = (w - 1) as u8;
        d[10] = (h - 1) as u8;
        d[65] = 1;
        d[66] = bpl as u8;
        // Row 0: indices 1,2,0xC1 (escaped), pad; row 1: run of four 2s.
        d.extend_from_slice(&[1, 2, 0xC1, 0xC1, 0]);
        d.extend_from_slice(&[0xC4, 2]);
        d.push(0x0C);
        let mut pal = vec![0u8; 768];
        pal[3..6].copy_from_slice(&[255, 0, 0]);
        pal[6..9].copy_from_slice(&[0, 255, 0]);
        pal[0xC1 * 3..0xC1 * 3 + 3].copy_from_slice(&[1, 2, 3]);
        d.extend_from_slice(&pal);
        let (dw, dh, rgba) = decode(&d).unwrap();
        assert_eq!((dw, dh), (3, 2));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        assert_eq!(&rgba[8..12], &[1, 2, 3, 255]);
        assert_eq!(&rgba[12..16], &[0, 255, 0, 255]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"hello").is_err());
        let mut d = vec![0u8; HEADER_LEN];
        d[0] = 0x0A;
        d[1] = 5;
        d[2] = 1;
        d[3] = 8;
        d[8] = 10;
        d[10] = 10;
        d[65] = 3;
        d[66] = 12;
        assert!(decode(&d).is_err()); // truncated
    }
}
