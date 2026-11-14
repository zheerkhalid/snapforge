//! Tiling for images larger than `OcrEngine.MaxImageDimension`, and merging the per-tile
//! results: ownership zones drop words seen twice in an overlap, a text/IoU pass removes
//! remaining duplicates, and line fragments split by a tile edge are joined again.

use super::{BoxF, OcrLine, OcrWord};
use crate::common::Rect;

/// Default overlap between neighbouring tiles. Must exceed the tallest text line we expect
/// so every word lies wholly inside the tile that owns its centre.
pub const OVERLAP: u32 = 400;

/// Start offsets and length of tiles covering `len` pixels: each at most `max` long,
/// neighbours overlapping by at least `overlap` (spread evenly).
pub fn axis_tiles(len: u32, max: u32, overlap: u32) -> Vec<(u32, u32)> {
    if len <= max || max == 0 {
        return vec![(0, len)];
    }
    let overlap = overlap.min(max / 2);
    let stride = (max - overlap) as u64;
    let n = (len as u64 - overlap as u64).div_ceil(stride).max(2);
    let span = (len - max) as f64;
    (0..n)
        .map(|i| {
            let start = (span * i as f64 / (n - 1) as f64).round() as u32;
            (start, max)
        })
        .collect()
}

/// Tiles for a `w`×`h` image (row-major).
pub fn tiles(w: u32, h: u32, max: u32, overlap: u32) -> Vec<Rect> {
    let xs = axis_tiles(w, max, overlap);
    let ys = axis_tiles(h, max, overlap);
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for &(y, th) in &ys {
        for &(x, tw) in &xs {
            out.push(Rect::new(x as i32, y as i32, tw as i32, th as i32));
        }
    }
    out
}

/// The half-open range of an axis a tile "owns": from the middle of the overlap with its
/// previous neighbour to the middle of the overlap with the next one.
fn owned_range(axis: &[(u32, u32)], i: usize) -> (f32, f32) {
    let lo = if i == 0 {
        f32::MIN
    } else {
        let (ps, pl) = axis[i - 1];
        (axis[i].0 as f32 + (ps + pl) as f32) / 2.0
    };
    let hi = if i + 1 >= axis.len() {
        f32::MAX
    } else {
        let (s, l) = axis[i];
        (axis[i + 1].0 as f32 + (s + l) as f32) / 2.0
    };
    (lo, hi)
}

fn iou(a: &BoxF, b: &BoxF) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let inter = (x1 - x0) * (y1 - y0);
    inter / (a.w * a.h + b.w * b.h - inter)
}

fn v_overlap(a: &BoxF, b: &BoxF) -> f32 {
    ((a.y + a.h).min(b.y + b.h) - a.y.max(b.y)).max(0.0)
}

/// One tile's recognized lines, in tile coordinates.
pub struct TileResult {
    pub tile: Rect,
    pub lines: Vec<OcrLine>,
}

struct Frag {
    tile: usize,
    words: Vec<OcrWord>,
}

/// Merge per-tile results for a `w`×`h` image tiled with `max`/`overlap` (the same values
/// passed to [`tiles`]) into image-coordinate lines in reading order.
pub fn merge(results: Vec<TileResult>, w: u32, h: u32, max: u32, overlap: u32) -> Vec<OcrLine> {
    let xs = axis_tiles(w, max, overlap);
    let ys = axis_tiles(h, max, overlap);

    // 1. Ownership: keep each word only from the tile that owns its centre.
    let mut frags: Vec<Frag> = Vec::new();
    for (ti, tr) in results.into_iter().enumerate() {
        let ix = xs.iter().position(|&(s, _)| s as i32 == tr.tile.x).unwrap_or(0);
        let iy = ys.iter().position(|&(s, _)| s as i32 == tr.tile.y).unwrap_or(0);
        let (x_lo, x_hi) = owned_range(&xs, ix);
        let (y_lo, y_hi) = owned_range(&ys, iy);
        for line in tr.lines {
            let words: Vec<OcrWord> = line
                .words
                .into_iter()
                .map(|mut wd| {
                    wd.rect.x += tr.tile.x as f32;
                    wd.rect.y += tr.tile.y as f32;
                    wd
                })
                .filter(|wd| {
                    let (cx, cy) = wd.rect.center();
                    cx >= x_lo && cx < x_hi && cy >= y_lo && cy < y_hi
                })
                .collect();
            if !words.is_empty() {
                frags.push(Frag { tile: ti, words });
            }
        }
    }

    // 2. Safety net: drop words that duplicate one from another tile (same text, IoU > 0.5).
    let mut all: Vec<(usize, usize, BoxF, String)> = Vec::new();
    for (fi, f) in frags.iter().enumerate() {
        for (wi, wd) in f.words.iter().enumerate() {
            all.push((fi, wi, wd.rect, wd.text.to_lowercase()));
        }
    }
    all.sort_by(|a, b| a.2.y.total_cmp(&b.2.y));
    let mut dead: std::collections::HashSet<(usize, usize)> = Default::default();
    for i in 0..all.len() {
        if dead.contains(&(all[i].0, all[i].1)) {
            continue;
        }
        for j in i + 1..all.len() {
            if all[j].2.y > all[i].2.y + all[i].2.h {
                break;
            }
            let other_tile = frags[all[j].0].tile != frags[all[i].0].tile;
            if other_tile && all[j].3 == all[i].3 && iou(&all[i].2, &all[j].2) > 0.5 {
                dead.insert((all[j].0, all[j].1));
            }
        }
    }
    for (fi, f) in frags.iter_mut().enumerate() {
        let mut wi = 0;
        f.words.retain(|_| {
            let keep = !dead.contains(&(fi, wi));
            wi += 1;
            keep
        });
    }
    frags.retain(|f| !f.words.is_empty());

    // 3. Join fragments of one visual line that a vertical tile edge split.
    frags.sort_by(|a, b| a.words[0].rect.x.total_cmp(&b.words[0].rect.x));
    let mut lines: Vec<(Vec<usize>, OcrLine)> = Vec::new();
    for f in frags {
        let first = f.words[0].rect;
        let target = lines.iter_mut().find(|(tiles, l)| {
            let Some(last) = l.words.last() else { return false };
            let lb = last.rect;
            let hmin = lb.h.min(first.h).max(1.0);
            let gap = first.x - (lb.x + lb.w);
            !tiles.contains(&f.tile) && v_overlap(&lb, &first) >= 0.5 * hmin && gap > -hmin && gap < 2.5 * hmin
        });
        match target {
            Some((tiles, l)) => {
                tiles.push(f.tile);
                l.words.extend(f.words);
            }
            None => lines.push((vec![f.tile], OcrLine { words: f.words })),
        }
    }
    let mut sorted: Vec<OcrLine> = lines.into_iter().map(|(_, l)| l).collect();

    // 4. Reading order: rows of lines whose vertical extents overlap by more than half,
    // top to bottom, each row left to right.
    sorted.sort_by(|a, b| a.bounds().y.total_cmp(&b.bounds().y));
    let mut rows: Vec<Vec<OcrLine>> = Vec::new();
    for line in sorted {
        let lb = line.bounds();
        let same_row = rows.last().is_some_and(|row| {
            let rb = row[0].bounds();
            v_overlap(&rb, &lb) > 0.5 * rb.h.min(lb.h)
        });
        match rows.last_mut() {
            Some(row) if same_row => row.push(line),
            _ => rows.push(vec![line]),
        }
    }
    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by(|a, b| a.bounds().x.total_cmp(&b.bounds().x));
        out.extend(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Point;

    fn word(text: &str, x: f32, y: f32, w: f32, h: f32) -> OcrWord {
        OcrWord { text: text.into(), rect: BoxF { x, y, w, h } }
    }

    #[test]
    fn axis_tiling_covers_with_overlap() {
        assert_eq!(axis_tiles(5000, 10000, OVERLAP), vec![(0, 5000)]);
        assert_eq!(axis_tiles(10000, 10000, OVERLAP), vec![(0, 10000)]);
        for len in [10001u32, 12000, 19600, 19601, 25000, 99999] {
            let t = axis_tiles(len, 10000, OVERLAP);
            assert!(t.len() >= 2);
            assert_eq!(t[0].0, 0);
            let (ls, ll) = t[t.len() - 1];
            assert_eq!(ls + ll, len, "last tile ends at the edge for {len}");
            for p in t.windows(2) {
                let ov = (p[0].0 + p[0].1) as i64 - p[1].0 as i64;
                assert!(ov >= OVERLAP as i64, "overlap {ov} for {len}");
            }
            assert!(t.iter().all(|&(_, l)| l <= 10000));
        }
    }

    #[test]
    fn tiles_for_25000_by_12000() {
        let max = 10000;
        let t = tiles(25000, 12000, max, OVERLAP);
        // 3 columns × 2 rows.
        assert_eq!(t.len(), 6);
        for r in &t {
            assert!(r.w as u32 <= max && r.h as u32 <= max);
            assert!(r.x >= 0 && r.y >= 0 && r.right() <= 25000 && r.bottom() <= 12000);
        }
        // Every pixel is covered: check a grid of sample points and the far corner.
        for y in (0..12000).step_by(397) {
            for x in (0..25000).step_by(397) {
                assert!(t.iter().any(|r| r.contains(Point::new(x, y))), "({x},{y})");
            }
        }
        assert!(t.iter().any(|r| r.contains(Point::new(24999, 11999))));
        // Ownership ranges partition each axis.
        let xs = axis_tiles(25000, max, OVERLAP);
        for i in 0..xs.len() - 1 {
            assert_eq!(owned_range(&xs, i).1, owned_range(&xs, i + 1).0);
        }
    }

    #[test]
    fn merge_dedupes_overlap_and_joins_split_lines() {
        let (w, h, max) = (25000u32, 12000u32, 10000u32);
        let t = tiles(w, h, max, OVERLAP);
        let xs = axis_tiles(w, max, OVERLAP);
        // Overlap between column 0 and column 1.
        let ov_start = xs[1].0 as f32;
        let ov_end = (xs[0].0 + xs[0].1) as f32;
        let mid = (ov_start + ov_end) / 2.0;
        let y = 500.0;

        // Column 0 sees "hello" (left of the overlap), "big" (inside the overlap, left of
        // its middle) and "world" (centre right of the middle).
        let t0 = TileResult {
            tile: t[0],
            lines: vec![OcrLine {
                words: vec![
                    word("hello", ov_start - 200.0, y, 120.0, 30.0),
                    word("big", mid - 100.0, y, 60.0, 30.0),
                    word("world", mid - 20.0, y, 100.0, 30.0),
                ],
            }],
        };
        // Column 1 sees the same "big" and "world" (tile coordinates), and "again".
        let off = t[1].x as f32;
        let t1 = TileResult {
            tile: t[1],
            lines: vec![OcrLine {
                words: vec![
                    word("big", mid - 100.0 - off, y, 60.0, 30.0),
                    word("world", mid - 20.0 - off, y, 100.0, 30.0),
                    word("again", mid + 110.0 - off, y, 100.0, 30.0),
                ],
            }],
        };
        // A second line lower down, only in column 1, and one in the bottom-left tile.
        let t1b =
            TileResult { tile: t[1], lines: vec![OcrLine { words: vec![word("second", 3000.0, 900.0, 140.0, 30.0)] }] };
        let t3 = TileResult {
            tile: t[3],
            lines: vec![OcrLine { words: vec![word("bottom", 50.0, 11000.0 - t[3].y as f32, 140.0, 30.0)] }],
        };
        let lines = merge(vec![t0, t1, t1b, t3], w, h, max, OVERLAP);
        let texts: Vec<String> = lines.iter().map(|l| l.text("en")).collect();
        assert_eq!(texts, vec!["hello big world again", "second", "bottom"]);
        // Coordinates are global.
        assert_eq!(lines[1].words[0].rect.x, t[1].x as f32 + 3000.0);
        assert_eq!(lines[2].words[0].rect.y, 11000.0);
    }

    #[test]
    fn merge_single_tile_keeps_lines() {
        let tr = TileResult {
            tile: Rect::new(0, 0, 800, 600),
            lines: vec![
                OcrLine { words: vec![word("b", 10.0, 100.0, 10.0, 10.0)] },
                OcrLine { words: vec![word("a", 10.0, 10.0, 10.0, 10.0)] },
            ],
        };
        let lines = merge(vec![tr], 800, 600, 10000, OVERLAP);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text("en"), "a");
    }
}
