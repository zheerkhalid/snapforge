//! Image stitching with overlap detection for scrolling capture.
//!
//! CONTRACT: pure, deterministic, unit-tested; no Win32 calls.
//!
//! # Algorithm
//!
//! Everything runs in "vertical space": horizontal frames are transposed on the way in and
//! the result is transposed back in [`Stitcher::finish`], so one matcher serves both axes.
//!
//! 1. Every frame line (row) is hashed once to 64 bits, and flagged *uniform* when all of
//!    its pixels are equal. Columns within `ignore_margin` of either side are left out of
//!    the hash (scrollbar thumbs move every step and would spoil every row).
//! 2. Sticky bands: the longest prefix (header) and suffix (footer) of lines that are equal
//!    at the same position in the previous and the new frame, trimmed so they never end in
//!    uniform lines, are excluded from matching.
//! 3. For every shift `d` of the middle band, lines `cur[i]` and `prev[i + d]` are compared
//!    by hash. A matching line scores `1 / (occurrences of that hash in prev)`, so uniform
//!    lines score 0 and repetitive lines score little; they cannot carry a match on their
//!    own. A few mismatching lines are tolerated (1/64 of the overlap) for hover effects.
//! 4. The best-scoring shift wins. With a hint (expected shift from the scroll driver), the
//!    candidate closest to the hint among those with the fewest mismatching lines wins,
//!    which resolves periodic content.
//! 5. Only the new lines (the bottom `d` lines of the middle band) are appended. The body
//!    always ends with the previous frame's footer, which is replaced by the new frame's
//!    footer, so the header appears once at the top and the footer once at the bottom.
//!
//! # Mismatch policy
//!
//! When no shift lines up and the frame is not just a minor repaint, the default policy
//! ([`MismatchPolicy::AppendWithSeparator`]) appends the whole frame (minus the known sticky
//! header) after a dashed red separator, so no content is ever lost; the result is
//! reported as [`PushResult::Mismatch`] so the caller can stop after repeated mismatches.
//! [`MismatchPolicy::Stop`] rejects the frame and all later ones instead.

use crate::common::Image;
use std::borrow::Cow;
use std::collections::HashMap;

/// Scroll direction of a stitch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchPolicy {
    /// Append the whole frame after a separator line and keep going.
    AppendWithSeparator,
    /// Reject the frame; every later push is rejected too.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StitchOptions {
    /// Pixels on both sides across the scroll axis that are ignored when matching
    /// (e.g. the scrollbar of a window client area).
    pub ignore_margin: u32,
    pub mismatch: MismatchPolicy,
}

impl Default for StitchOptions {
    fn default() -> Self {
        Self { ignore_margin: 0, mismatch: MismatchPolicy::AppendWithSeparator }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    /// `n` new lines were appended (for the first frame: its full extent).
    Added(u32),
    /// The frame shows the same content (end reached, or only a minor repaint).
    NoChange,
    /// The content changed unexpectedly. `appended` tells whether the policy kept the frame.
    Mismatch { appended: bool },
}

/// Lines of the separator drawn before a mismatched frame.
pub const SEPARATOR_LINES: u32 = 2;
/// Minimum match score (sum of line weights) for a shift to be trusted.
const MIN_INFO: f32 = 2.0;
/// Score penalty per mismatching line.
const MISMATCH_PENALTY: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Line {
    hash: u64,
    uniform: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    Same,
    Minor,
    Shift { d: usize, header: usize, footer: usize },
    None,
}

/// Incremental one-axis stitcher. Push frames in scroll order, then [`finish`](Self::finish).
pub struct Stitcher {
    axis: Axis,
    opts: StitchOptions,
    /// Line length in pixels (vertical space).
    width: u32,
    /// Stitched lines so far, `width * 4` bytes each (vertical space).
    body: Body,
    prev: Vec<Line>,
    frames: u32,
    header: u32,
    footer: u32,
    stopped: bool,
}

impl Stitcher {
    pub fn new(axis: Axis) -> Self {
        Self::with_options(axis, StitchOptions::default())
    }

    pub fn with_options(axis: Axis, opts: StitchOptions) -> Self {
        Self {
            axis,
            opts,
            width: 0,
            body: Body::default(),
            prev: Vec::new(),
            frames: 0,
            header: 0,
            footer: 0,
            stopped: false,
        }
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// Frames kept so far (first frame, shifted frames and appended mismatches).
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Current stitched length along the scroll axis, in pixels.
    pub fn extent(&self) -> u32 {
        if self.width == 0 {
            0
        } else {
            (self.body.len / (self.width as usize * 4)) as u32
        }
    }

    /// Last detected sticky header and footer sizes (along the scroll axis).
    pub fn sticky(&self) -> (u32, u32) {
        (self.header, self.footer)
    }

    /// Frame extent minus the sticky bands: the part that actually scrolls.
    pub fn scrolling_extent(&self) -> Option<u32> {
        (self.frames > 0).then(|| (self.prev.len() as u32).saturating_sub(self.header + self.footer))
    }

    pub fn push(&mut self, frame: &Image) -> PushResult {
        self.push_hint(frame, None)
    }

    /// Push a frame; `hint` is the expected shift in pixels (used to break ties).
    pub fn push_hint(&mut self, frame: &Image, hint: Option<u32>) -> PushResult {
        if frame.is_empty() {
            return PushResult::Mismatch { appended: false };
        }
        let frame: Cow<Image> = match self.axis {
            Axis::Vertical => Cow::Borrowed(frame),
            Axis::Horizontal => Cow::Owned(transpose(frame)),
        };
        if self.frames == 0 {
            self.width = frame.width;
            self.body = Body::default();
            self.body.append(&frame.data);
            self.prev = hash_lines(&frame, self.opts.ignore_margin);
            self.frames = 1;
            return PushResult::Added(frame.height);
        }
        if frame.width != self.width || frame.height as usize != self.prev.len() {
            log::warn!(
                "stitch: frame size changed ({}x{} -> {}x{} in vertical space)",
                self.width,
                self.prev.len(),
                frame.width,
                frame.height
            );
            return PushResult::Mismatch { appended: false };
        }
        if self.stopped {
            return PushResult::Mismatch { appended: false };
        }
        let cur = hash_lines(&frame, self.opts.ignore_margin);
        let n = cur.len();
        let row = self.width as usize * 4;
        match find_shift(&self.prev, &cur, hint) {
            Match::Same | Match::Minor => PushResult::NoChange,
            Match::Shift { d, header, footer } => {
                // The body ends with the previous frame's footer; replace it with the new
                // content followed by the new frame's footer.
                self.body.truncate_end(footer * row);
                self.body.append(&frame.data[(n - footer - d) * row..]);
                self.prev = cur;
                self.frames += 1;
                self.header = header as u32;
                self.footer = footer as u32;
                PushResult::Added(d as u32)
            }
            Match::None => match self.opts.mismatch {
                MismatchPolicy::Stop => {
                    self.stopped = true;
                    PushResult::Mismatch { appended: false }
                }
                MismatchPolicy::AppendWithSeparator => {
                    let mut sep = Vec::with_capacity(SEPARATOR_LINES as usize * row);
                    for y in 0..SEPARATOR_LINES {
                        for x in 0..self.width {
                            let dash = ((x + y * 4) / 8) % 2 == 0;
                            let px: [u8; 4] = if dash { [0x30, 0x30, 0xE0, 0xFF] } else { [0xFF; 4] };
                            sep.extend_from_slice(&px);
                        }
                    }
                    self.body.append(&sep);
                    let skip = (self.header as usize).min(n.saturating_sub(1));
                    self.body.append(&frame.data[skip * row..]);
                    self.prev = cur;
                    self.frames += 1;
                    PushResult::Mismatch { appended: true }
                }
            },
        }
    }

    /// The stitched image (empty if no frame was pushed).
    pub fn finish(self) -> Image {
        if self.width == 0 {
            return Image::new(0, 0);
        }
        let h = (self.body.len / (self.width as usize * 4)) as u32;
        let img = Image::from_bgra(self.width, h, self.body.into_vec());
        match self.axis {
            Axis::Vertical => img,
            Axis::Horizontal => transpose(&img),
        }
    }
}

/// Stitched bytes kept as segments, joined once in `finish` (no regrowth copies).
#[derive(Default)]
struct Body {
    segments: Vec<Vec<u8>>,
    len: usize,
}

impl Body {
    fn append(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.segments.push(bytes.to_vec());
            self.len += bytes.len();
        }
    }

    /// Drop `n` bytes from the end.
    fn truncate_end(&mut self, mut n: usize) {
        while n > 0 {
            let Some(last) = self.segments.last_mut() else { break };
            let k = n.min(last.len());
            last.truncate(last.len() - k);
            self.len -= k;
            n -= k;
            if last.is_empty() {
                self.segments.pop();
            }
        }
    }

    fn into_vec(mut self) -> Vec<u8> {
        if self.segments.len() == 1 {
            return self.segments.pop().unwrap_or_default();
        }
        let mut out = Vec::with_capacity(self.len);
        for s in &self.segments {
            out.extend_from_slice(s);
        }
        out
    }
}

/// 2D stitching ("Both" direction): each column is stitched vertically, then the
/// columns are joined horizontally with the same matcher.
pub struct GridStitcher {
    opts: StitchOptions,
    columns: Vec<Image>,
    current: Stitcher,
}

impl GridStitcher {
    pub fn new(opts: StitchOptions) -> Self {
        Self { opts, columns: Vec::new(), current: Stitcher::with_options(Axis::Vertical, opts) }
    }

    /// Push a frame of the current column (top to bottom).
    pub fn push(&mut self, frame: &Image) -> PushResult {
        self.current.push(frame)
    }

    pub fn push_hint(&mut self, frame: &Image, hint: Option<u32>) -> PushResult {
        self.current.push_hint(frame, hint)
    }

    /// Close the current column; the next push starts a new one (scrolled right).
    pub fn next_column(&mut self) {
        let done = std::mem::replace(&mut self.current, Stitcher::with_options(Axis::Vertical, self.opts));
        if done.frames() > 0 {
            self.columns.push(done.finish());
        }
    }

    pub fn finish(mut self) -> Image {
        self.next_column();
        join_columns(&self.columns, self.opts)
    }
}

/// Join vertically stitched columns (left to right) into one image. Columns are cropped
/// to the shortest height so their lines line up.
pub fn join_columns(columns: &[Image], opts: StitchOptions) -> Image {
    let Some(min_h) = columns.iter().map(|c| c.height).min() else {
        return Image::new(0, 0);
    };
    if columns.len() == 1 {
        return columns[0].clone();
    }
    let mut st = Stitcher::with_options(Axis::Horizontal, opts);
    for c in columns {
        let r = st.push(&c.crop(crate::common::Rect::new(0, 0, c.width as i32, min_h as i32)));
        log::debug!("join column: {r:?}");
    }
    st.finish()
}

// ------------------------------------------------------------------ matching

fn find_shift(prev: &[Line], cur: &[Line], hint: Option<u32>) -> Match {
    let n = cur.len();
    if prev.len() != n || n == 0 {
        return Match::None;
    }
    if prev == cur {
        return Match::Same;
    }
    // Sticky bands: equal at the same position, never ending in uniform lines.
    let mut header = 0;
    while header < n && prev[header].hash == cur[header].hash {
        header += 1;
    }
    while header > 0 && cur[header - 1].uniform {
        header -= 1;
    }
    let mut footer = 0;
    while footer < n - header && prev[n - 1 - footer].hash == cur[n - 1 - footer].hash {
        footer += 1;
    }
    while footer > 0 && cur[n - footer].uniform {
        footer -= 1;
    }
    let end = n - footer;
    let mid = end - header;

    // Line weights: uniform lines carry no information, repeated lines little.
    let mut counts: HashMap<u64, u32> = HashMap::with_capacity(mid);
    for l in &prev[header..end] {
        if !l.uniform {
            *counts.entry(l.hash).or_insert(0) += 1;
        }
    }
    let weight: Vec<f32> = cur
        .iter()
        .map(|l| if l.uniform { 0.0 } else { counts.get(&l.hash).map_or(0.0, |&c| 1.0 / c as f32) })
        .collect();

    // (d, score, mismatches)
    let mut cands: Vec<(usize, f32, usize)> = Vec::new();
    let mut hinted_clean = false;
    for d in 1..mid {
        let overlap = mid - d;
        let tol = overlap / 64;
        let mut info = 0.0f32;
        let mut mis = 0usize;
        for i in header..end - d {
            if cur[i].hash == prev[i + d].hash {
                info += weight[i];
            } else {
                mis += 1;
                if mis > tol {
                    break;
                }
            }
        }
        if mis > tol {
            continue;
        }
        if hint == Some(d as u32) && mis == 0 {
            hinted_clean = true;
        }
        if info >= MIN_INFO {
            cands.push((d, info - MISMATCH_PENALTY * mis as f32, mis));
        }
    }

    let pick = match hint {
        // Periodic content matches at several shifts: among the cleanest candidates take
        // the one closest to the expected shift.
        Some(h) => {
            let min_mis = cands.iter().map(|c| c.2).min().unwrap_or(0);
            cands
                .iter()
                .filter(|c| c.2 == min_mis)
                .min_by(|a, b| {
                    let da = (a.0 as i64 - h as i64).abs();
                    let db = (b.0 as i64 - h as i64).abs();
                    da.cmp(&db).then(b.1.total_cmp(&a.1))
                })
                .map(|c| c.0)
        }
        // Highest score; on ties the smallest shift (iteration order, strict >).
        None => cands
            .iter()
            .fold(None::<(usize, f32, usize)>, |acc, &c| match acc {
                Some(a) if a.1 >= c.1 => Some(a),
                _ => Some(c),
            })
            .map(|c| c.0),
    };
    if let Some(d) = pick {
        return Match::Shift { d, header, footer };
    }
    // Nothing informative in the overlap (e.g. blank areas): trust a clean hinted shift.
    if hinted_clean {
        if let Some(h) = hint {
            return Match::Shift { d: h as usize, header, footer };
        }
    }
    let differing = prev.iter().zip(cur).filter(|(a, b)| a != b).count();
    if differing <= (n / 100).max(2) {
        Match::Minor
    } else {
        Match::None
    }
}

fn hash_lines(img: &Image, margin: u32) -> Vec<Line> {
    let w = img.width as usize;
    let m = if (margin as usize) * 2 + 1 >= w { 0 } else { margin as usize };
    (0..img.height).map(|y| hash_line(&img.row(y)[m * 4..(w - m) * 4])).collect()
}

#[inline]
fn hash_line(px: &[u8]) -> Line {
    const K: u64 = 0x517C_C1B7_2722_0A95;
    let mut h = [0x243F_6A88_85A3_08D3u64, 0x1319_8A2E_0370_7344, 0xA409_3822_299F_31D0, 0x082E_FA98_EC4E_6C89];
    let chunks = px.chunks_exact(32);
    let rem = chunks.remainder();
    for c in chunks {
        for (lane, hv) in h.iter_mut().enumerate() {
            let o = lane * 8;
            let v = u64::from_le_bytes([c[o], c[o + 1], c[o + 2], c[o + 3], c[o + 4], c[o + 5], c[o + 6], c[o + 7]]);
            *hv = (hv.rotate_left(5) ^ v).wrapping_mul(K);
        }
    }
    let mut acc = h[0] ^ h[1].rotate_left(17) ^ h[2].rotate_left(31) ^ h[3].rotate_left(47) ^ px.len() as u64;
    for &b in rem {
        acc = (acc.rotate_left(5) ^ b as u64).wrapping_mul(K);
    }
    // murmur3 fmix64
    acc ^= acc >> 33;
    acc = acc.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    acc ^= acc >> 33;
    acc = acc.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    acc ^= acc >> 33;
    // Uniform: every pixel equals its neighbour (one memcmp of the row against itself).
    let uniform = px.len() <= 4 || px[4..] == px[..px.len() - 4];
    Line { hash: acc, uniform }
}

/// Swap rows and columns (cache-blocked).
pub fn transpose(img: &Image) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = vec![0u8; w * h * 4];
    const B: usize = 32;
    let src = &img.data;
    for by in (0..h).step_by(B) {
        for bx in (0..w).step_by(B) {
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    let s = (y * w + x) * 4;
                    let d = (x * h + y) * 4;
                    out[d..d + 4].copy_from_slice(&src[s..s + 4]);
                }
            }
        }
    }
    Image::from_bgra(img.height, img.width, out)
}

// ------------------------------------------------------------------ bench

/// Deterministic "document-like" pixel: text-ish strokes on white with blank gaps.
fn synth_pixel(x: u32, y: u32) -> [u8; 4] {
    let line = y / 20;
    let in_text = y % 20 < 13 && line % 9 != 4;
    if !in_text {
        return [0xFF; 4];
    }
    let mut v = (x / 3) as u64 ^ ((y as u64) << 21) ^ 0x9E37_79B9_7F4A_7C15;
    v = (v ^ (v >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^= v >> 29;
    // Words: dark strokes on about 40 % of pixels in the left 85 % of each text line.
    if x < 1600 + (line % 7) * 40 && v % 5 < 2 {
        let c = (v >> 8) as u8 & 0x3F;
        [c, c, c, 0xFF]
    } else {
        [0xFF; 4]
    }
}

fn synth_frame(top: u32, w: u32, h: u32, header: u32) -> Image {
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let px = if y < header { [0x60, 0x40, 0x20 + (x % 64) as u8, 0xFF] } else { synth_pixel(x, top + y) };
            data.extend_from_slice(&px);
        }
    }
    Image::from_bgra(w, h, data)
}

/// Stitch `screens` synthetic 1920x1080 frames (75 % scroll steps, sticky 60 px header)
/// and return the elapsed stitching time in milliseconds (frame generation excluded).
pub fn bench_synthetic(screens: u32) -> f64 {
    let (w, h, header) = (1920u32, 1080u32, 60u32);
    let step = (h - header) * 3 / 4;
    let frames: Vec<Image> = (0..screens).map(|i| synth_frame(i * step, w, h, header)).collect();
    let t = std::time::Instant::now();
    let mut st = Stitcher::new(Axis::Vertical);
    for f in &frames {
        let r = st.push_hint(f, Some(step));
        if !matches!(r, PushResult::Added(_)) {
            log::warn!("bench_synthetic: unexpected {r:?}");
        }
    }
    let img = st.finish();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    log::debug!("bench_synthetic: {} frames -> {}x{} in {ms:.1} ms", screens, img.width, img.height);
    ms
}

#[cfg(test)]
#[path = "stitch_tests.rs"]
pub(crate) mod tests;
