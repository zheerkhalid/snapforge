//! Pixel effects on 4-channel premultiplied buffers (channel order does not matter).

/// Gaussian-like blur (three box passes per axis) of a `w`×`h` 4-channel buffer in place.
/// `sigma` is in pixels; edges are clamped.
pub fn blur(data: &mut [u8], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 || sigma < 0.5 || data.len() < w * h * 4 {
        return;
    }
    // Three boxes of half-width b give sigma = sqrt(b(b+1)).
    let b = ((sigma * sigma + 0.25).sqrt() - 0.5).round().max(1.0) as usize;
    let mut tmp = vec![0u8; w.max(h) * 4];
    for _ in 0..3 {
        for y in 0..h {
            let row = &mut data[y * w * 4..(y + 1) * w * 4];
            box_line(row, 4, w, b, &mut tmp);
        }
        for x in 0..w {
            box_column(data, x, w, h, b, &mut tmp);
        }
    }
}

/// Box filter along one line with element stride `stride` (in bytes, 4 for rows).
fn box_line(line: &mut [u8], stride: usize, n: usize, b: usize, tmp: &mut [u8]) {
    let win = (2 * b + 1) as u32;
    let get =
        |i: isize, c: usize, line: &[u8]| -> u32 { line[(i.clamp(0, n as isize - 1) as usize) * stride + c] as u32 };
    for c in 0..4 {
        let mut sum: u32 = 0;
        for i in -(b as isize)..=(b as isize) {
            sum += get(i, c, line);
        }
        for i in 0..n {
            tmp[i * 4 + c] = ((sum + win / 2) / win) as u8;
            sum += get(i as isize + b as isize + 1, c, line);
            sum -= get(i as isize - b as isize, c, line);
        }
    }
    for i in 0..n {
        line[i * stride..i * stride + 4].copy_from_slice(&tmp[i * 4..i * 4 + 4]);
    }
}

fn box_column(data: &mut [u8], x: usize, w: usize, h: usize, b: usize, tmp: &mut [u8]) {
    let win = (2 * b + 1) as u32;
    let idx = |y: isize| (y.clamp(0, h as isize - 1) as usize * w + x) * 4;
    for c in 0..4 {
        let mut sum: u32 = 0;
        for i in -(b as isize)..=(b as isize) {
            sum += data[idx(i) + c] as u32;
        }
        for y in 0..h {
            tmp[y * 4 + c] = ((sum + win / 2) / win) as u8;
            sum += data[idx(y as isize + b as isize + 1) + c] as u32;
            sum -= data[idx(y as isize - b as isize) + c] as u32;
        }
    }
    for y in 0..h {
        let i = (y * w + x) * 4;
        data[i..i + 4].copy_from_slice(&tmp[y * 4..y * 4 + 4]);
    }
}

/// Replace each `block`×`block` cell with its average (cells aligned to `origin`).
pub fn pixelate(data: &mut [u8], w: usize, h: usize, block: usize, origin: (i64, i64)) {
    let block = block.max(2);
    if w == 0 || h == 0 || data.len() < w * h * 4 {
        return;
    }
    let off_x = origin.0.rem_euclid(block as i64) as usize;
    let off_y = origin.1.rem_euclid(block as i64) as usize;
    let mut y0 = 0usize;
    let mut first_row = true;
    while y0 < h {
        let y1 = if first_row && off_y > 0 { off_y.min(h) } else { (y0 + block).min(h) };
        first_row = false;
        let mut x0 = 0usize;
        let mut first_col = true;
        while x0 < w {
            let x1 = if first_col && off_x > 0 { off_x.min(w) } else { (x0 + block).min(w) };
            first_col = false;
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    for c in 0..4 {
                        sum[c] += data[i + c] as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as u64;
            let avg = sum.map(|s| ((s + n / 2) / n) as u8);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    data[i..i + 4].copy_from_slice(&avg);
                }
            }
            x0 = x1;
        }
        y0 = y1;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn blur_preserves_flat_and_spreads_impulse() {
        let (w, h) = (9, 9);
        let mut flat = vec![100u8; w * h * 4];
        super::blur(&mut flat, w, h, 2.0);
        assert!(flat.iter().all(|&v| v == 100));
        let mut imp = vec![0u8; w * h * 4];
        let c = (4 * w + 4) * 4;
        imp[c..c + 4].copy_from_slice(&[255; 4]);
        super::blur(&mut imp, w, h, 1.0);
        assert!(imp[c] < 255 && imp[c] > 0);
        assert!(imp[c + 4] > 0);
    }

    #[test]
    fn pixelate_averages_cells() {
        let (w, h) = (4, 2);
        let mut d: Vec<u8> = (0..w * h).flat_map(|i| [i as u8 * 10; 4]).collect();
        super::pixelate(&mut d, w, h, 2, (0, 0));
        assert_eq!(d[0], d[4]);
        assert_eq!(d[0], d[(w) * 4]);
        assert_ne!(d[0], d[8]);
    }
}
