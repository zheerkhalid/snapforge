//! Where to cut a tall image into pages without slicing through a line of text.
//!
//! Each row gets a "busyness" score (variance of its luma, composited over white). Near
//! every page boundary we look back for a quiet row (variance below a threshold) and cut
//! there, preferring white rows, so text lines stay whole.

use crate::common::Image;

/// Per-row luma statistics: (mean, variance), composited over white.
pub fn row_stats(img: &Image) -> Vec<(f32, f32)> {
    let w = img.width as usize;
    if w == 0 {
        return vec![(255.0, 0.0); img.height as usize];
    }
    img.data
        .chunks_exact(w * 4)
        .map(|row| {
            let mut s = 0u64;
            let mut s2 = 0u64;
            for p in row.chunks_exact(4) {
                let ia = 255 - p[3] as u32;
                let (b, g, r) = (p[0] as u32 + ia, p[1] as u32 + ia, p[2] as u32 + ia);
                let l = ((r.min(255) * 77 + g.min(255) * 150 + b.min(255) * 29 + 128) >> 8) as u64;
                s += l;
                s2 += l * l;
            }
            let n = w as f64;
            let mean = s as f64 / n;
            ((mean) as f32, (s2 as f64 / n - mean * mean).max(0.0) as f32)
        })
        .collect()
}

/// Rows whose luma variance is at most this count as blank (whitespace).
pub const BLANK_VARIANCE: f32 = 12.0;

/// Chunk end rows (exclusive) for splitting `stats.len()` rows into chunks of at most
/// `max_rows`. With `avoid_lines`, each cut moves up by at most `search` rows to just below
/// a blank row (white blank rows first, then any uniform row). The last entry is the height.
pub fn split_points(stats: &[(f32, f32)], max_rows: u32, search: u32, avoid_lines: bool) -> Vec<u32> {
    let h = stats.len() as u32;
    let max_rows = max_rows.max(1);
    let mut cuts = Vec::new();
    let mut start = 0u32;
    while h - start > max_rows {
        let target = start + max_rows;
        let mut cut = target;
        if avoid_lines {
            // Never shrink a page below half its height.
            let lo = target.saturating_sub(search).max(start + max_rows / 2).max(start + 1);
            let blank = |y: u32, white: bool| {
                let (mean, var) = stats[(y - 1) as usize];
                var <= BLANK_VARIANCE && (!white || mean >= 200.0)
            };
            let find = |white: bool| (lo..=target).rev().find(|&c| blank(c, white));
            if let Some(c) = find(true).or_else(|| find(false)) {
                cut = c;
            }
        }
        cuts.push(cut);
        start = cut;
    }
    cuts.push(h);
    cuts
}

/// [`split_points`] for an image, searching back a quarter of a page.
pub fn split_image_rows(img: &Image, max_rows: u32, avoid_lines: bool) -> Vec<u32> {
    if img.height <= max_rows {
        return vec![img.height];
    }
    let stats = if avoid_lines { row_stats(img) } else { vec![(255.0, 0.0); img.height as usize] };
    split_points(&stats, max_rows, max_rows / 4, avoid_lines)
}
