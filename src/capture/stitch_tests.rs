//! Unit tests for the stitcher, using synthetic pages.

use super::*;
use crate::common::Rect;

/// Small deterministic RNG (splitmix64), so tests need no Win32.
pub(crate) struct TestRng(u64);
impl TestRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Random noise texture: every row and column is unique.
pub(crate) fn noise_page(w: u32, h: u32, seed: u64) -> Image {
    let mut r = TestRng::new(seed);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        let v = r.next();
        data.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 0xFF]);
    }
    Image::from_bgra(w, h, data)
}

/// Document-like page: white with "text lines" of dark random strokes, blank gaps, and
/// optional large blank regions (`blank` = list of (start, len) row ranges).
pub(crate) fn text_page(w: u32, h: u32, seed: u64, blank: &[(u32, u32)]) -> Image {
    let mut img = Image::filled(w, h, [0xFF; 4]);
    let mut r = TestRng::new(seed);
    for y in 0..h {
        if y % 16 >= 10 || blank.iter().any(|&(s, l)| y >= s && y < s + l) {
            continue;
        }
        let len = w / 3 + r.below((w / 2) as u64) as u32;
        for x in 4..len.min(w) {
            if r.below(3) == 0 {
                let c = r.below(80) as u8;
                img.set_pixel(x, y, [c, c, c, 0xFF]);
            }
        }
    }
    img
}

fn band(w: u32, h: u32, color: [u8; 4], seed: u64) -> Image {
    // A header/footer bar with some unique detail so its rows are not uniform.
    let mut img = Image::filled(w, h, color);
    let mut r = TestRng::new(seed);
    for y in 0..h {
        for _ in 0..(w / 10) {
            let x = r.below(w as u64) as u32;
            img.set_pixel(x, y, [0xEE, 0xEE, 0xEE, 0xFF]);
        }
    }
    img
}

fn vcat(parts: &[&Image]) -> Image {
    let w = parts[0].width;
    let mut data = Vec::new();
    for p in parts {
        assert_eq!(p.width, w);
        data.extend_from_slice(&p.data);
    }
    let h = data.len() as u32 / (w * 4);
    Image::from_bgra(w, h, data)
}

fn vcrop(img: &Image, y: u32, h: u32) -> Image {
    img.crop(Rect::new(0, y as i32, img.width as i32, h as i32))
}

fn hcrop(img: &Image, x: u32, w: u32) -> Image {
    img.crop(Rect::new(x as i32, 0, w as i32, img.height as i32))
}

fn stitch_all(axis: Axis, frames: &[Image]) -> (Image, Vec<PushResult>) {
    let mut st = Stitcher::new(axis);
    let res = frames.iter().map(|f| st.push(f)).collect();
    (st.finish(), res)
}

#[test]
fn transpose_roundtrip() {
    let img = noise_page(37, 53, 1);
    let t = transpose(&img);
    assert_eq!((t.width, t.height), (53, 37));
    assert_eq!(t.pixel(5, 9), img.pixel(9, 5));
    assert_eq!(transpose(&t), img);
}

#[test]
fn random_texture_varying_offsets() {
    let page = noise_page(160, 1200, 7);
    let offsets = [0u32, 90, 230, 300, 470, 600, 777, 899, 900];
    let frames: Vec<Image> = offsets.iter().map(|&y| vcrop(&page, y, 300)).collect();
    let (out, res) = stitch_all(Axis::Vertical, &frames);
    assert_eq!(res[0], PushResult::Added(300));
    for (i, r) in res.iter().enumerate().skip(1) {
        assert_eq!(*r, PushResult::Added(offsets[i] - offsets[i - 1]), "frame {i}");
    }
    assert_eq!(out, page);
}

#[test]
fn text_page_exact() {
    let page = text_page(240, 2000, 3, &[]);
    let offsets: Vec<u32> = (0..9).map(|i| i * 210).chain([1700]).collect();
    let frames: Vec<Image> = offsets.iter().map(|&y| vcrop(&page, y, 300)).collect();
    let (out, res) = stitch_all(Axis::Vertical, &frames);
    assert!(res.iter().all(|r| matches!(r, PushResult::Added(_))), "{res:?}");
    assert_eq!(out, page);
}

#[test]
fn sticky_header_and_footer() {
    let w = 200;
    let content = text_page(w, 1500, 11, &[]);
    let header = band(w, 40, [0x80, 0x30, 0x10, 0xFF], 1);
    let footer = band(w, 25, [0x10, 0x60, 0x20, 0xFF], 2);
    let view = 280; // scrolling part of each frame
    let offsets = [0u32, 200, 400, 600, 800, 1000, 1200, 1220];
    let frames: Vec<Image> = offsets.iter().map(|&y| vcat(&[&header, &vcrop(&content, y, view), &footer])).collect();
    let mut st = Stitcher::new(Axis::Vertical);
    for (i, f) in frames.iter().enumerate() {
        let r = st.push(f);
        if i > 0 {
            assert_eq!(r, PushResult::Added(offsets[i] - offsets[i - 1]), "frame {i}");
        }
    }
    assert_eq!(st.sticky(), (40, 25));
    assert_eq!(st.scrolling_extent(), Some(view));
    let out = st.finish();
    let expected = vcat(&[&header, &content, &footer]);
    assert_eq!(out.height, expected.height);
    assert_eq!(out, expected);
}

#[test]
fn large_uniform_regions() {
    // Big blank stretches (but every overlap still holds some text).
    let page = text_page(180, 1600, 5, &[(150, 120), (520, 140), (900, 100), (1350, 80)]);
    let offsets: Vec<u32> = (0..7).map(|i| i * 190).chain([1300]).collect();
    let frames: Vec<Image> = offsets.iter().map(|&y| vcrop(&page, y, 300)).collect();
    let (out, res) = stitch_all(Axis::Vertical, &frames);
    assert!(res.iter().all(|r| matches!(r, PushResult::Added(_))), "{res:?}");
    assert_eq!(out, page);
}

#[test]
fn blank_overlap_needs_hint() {
    // The overlap of frames 0 and 1 is entirely blank: ambiguous without a hint.
    let page = text_page(120, 600, 9, &[(150, 200)]);
    let f0 = vcrop(&page, 0, 300);
    let f1 = vcrop(&page, 150, 300);
    let mut st =
        Stitcher::with_options(Axis::Vertical, StitchOptions { mismatch: MismatchPolicy::Stop, ..Default::default() });
    st.push(&f0);
    assert_eq!(st.push(&f1), PushResult::Mismatch { appended: false });

    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&f0);
    assert_eq!(st.push_hint(&f1, Some(150)), PushResult::Added(150));
    assert_eq!(st.finish(), vcrop(&page, 0, 450));
}

#[test]
fn fully_blank_frames_are_no_change() {
    let blank = Image::filled(100, 200, [0xFF; 4]);
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&blank);
    assert_eq!(st.push(&blank), PushResult::NoChange);
}

#[test]
fn repeating_pattern_with_unique_rows() {
    // A table of identical rows (period 24) with a few unique rows sprinkled in.
    let w = 160;
    let tile = text_page(w, 24, 21, &[]);
    let unique = noise_page(w, 1500, 22);
    let mut page = Image::filled(w, 1500, [0xFF; 4]);
    for y in (0..1500).step_by(24) {
        page.blit(&tile, 0, y);
    }
    for y in (37..1500).step_by(61) {
        page.blit(&vcrop(&unique, y, 2), 0, y as i32);
    }
    let offsets: Vec<u32> = (0..6).map(|i| i * 220).collect();
    let frames: Vec<Image> = offsets.iter().map(|&y| vcrop(&page, y, 300)).collect();
    let (out, res) = stitch_all(Axis::Vertical, &frames);
    assert!(res.iter().all(|r| matches!(r, PushResult::Added(_))), "{res:?}");
    assert_eq!(out, vcrop(&page, 0, 1400));
}

#[test]
fn purely_periodic_uses_hint() {
    let w = 120;
    let tile = text_page(w, 30, 31, &[]);
    let mut page = Image::new(w, 900);
    for y in (0..900).step_by(30) {
        page.blit(&tile, 0, y);
    }
    let f0 = vcrop(&page, 0, 300);
    let f1 = vcrop(&page, 215, 300);
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&f0);
    assert_eq!(st.push_hint(&f1, Some(205)), PushResult::Added(215));
    assert_eq!(st.finish(), vcrop(&page, 0, 515));
}

#[test]
fn end_of_page_and_partial_last_step() {
    let page = noise_page(100, 700, 41);
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&vcrop(&page, 0, 300));
    assert_eq!(st.push(&vcrop(&page, 225, 300)), PushResult::Added(225));
    // Asked for 225 again, but only 175 px were left.
    assert_eq!(st.push(&vcrop(&page, 400, 300)), PushResult::Added(175));
    // End reached: same frame again, twice.
    assert_eq!(st.push(&vcrop(&page, 400, 300)), PushResult::NoChange);
    assert_eq!(st.push(&vcrop(&page, 400, 300)), PushResult::NoChange);
    assert_eq!(st.finish(), page);
}

#[test]
fn tiny_partial_step() {
    let page = text_page(150, 400, 43, &[]);
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&vcrop(&page, 0, 300));
    assert_eq!(st.push(&vcrop(&page, 3, 300)), PushResult::Added(3));
    assert_eq!(st.finish(), vcrop(&page, 0, 303));
}

#[test]
fn minor_repaint_is_no_change() {
    // A blinking caret changes two rows: not a scroll, not a mismatch.
    let page = text_page(150, 300, 45, &[]);
    let mut f1 = page.clone();
    for y in 100..102 {
        f1.set_pixel(140, y, [0, 0, 0xFF, 0xFF]);
    }
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&page);
    assert_eq!(st.push(&f1), PushResult::NoChange);
}

#[test]
fn mismatch_appends_after_separator() {
    let a = noise_page(80, 200, 51);
    let b = noise_page(80, 200, 52);
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&a);
    assert_eq!(st.push(&b), PushResult::Mismatch { appended: true });
    assert_eq!(st.extent(), 200 + SEPARATOR_LINES + 200);
    let out = st.finish();
    assert_eq!(vcrop(&out, 0, 200), a);
    assert_eq!(vcrop(&out, 200 + SEPARATOR_LINES, 200), b);
}

#[test]
fn mismatch_stop_policy() {
    let page = noise_page(80, 600, 53);
    let other = noise_page(80, 200, 54);
    let mut st =
        Stitcher::with_options(Axis::Vertical, StitchOptions { mismatch: MismatchPolicy::Stop, ..Default::default() });
    st.push(&vcrop(&page, 0, 200));
    assert_eq!(st.push(&other), PushResult::Mismatch { appended: false });
    assert_eq!(st.push(&vcrop(&page, 100, 200)), PushResult::Mismatch { appended: false });
    assert_eq!(st.finish(), vcrop(&page, 0, 200));
}

#[test]
fn frame_size_change_is_rejected() {
    let mut st = Stitcher::new(Axis::Vertical);
    st.push(&noise_page(80, 200, 55));
    assert_eq!(st.push(&noise_page(81, 200, 56)), PushResult::Mismatch { appended: false });
    assert_eq!(st.extent(), 200);
}

#[test]
fn scrollbar_margin_is_ignored() {
    // A "scrollbar" strip on the right whose thumb moves every step.
    let w = 160;
    let page = text_page(w, 1000, 61, &[]);
    let offsets = [0u32, 200, 400, 600, 700];
    let frames: Vec<Image> = offsets
        .iter()
        .map(|&y| {
            let mut f = vcrop(&page, y, 300);
            let thumb = y * 300 / 1000;
            for yy in 0..300 {
                let c = if yy >= thumb && yy < thumb + 90 { 0x40 } else { 0xD0 };
                for x in w - 12..w {
                    f.set_pixel(x, yy, [c, c, c, 0xFF]);
                }
            }
            f
        })
        .collect();
    let mut st = Stitcher::with_options(Axis::Vertical, StitchOptions { ignore_margin: 14, ..Default::default() });
    for (i, f) in frames.iter().enumerate() {
        let r = st.push(f);
        if i > 0 {
            assert_eq!(r, PushResult::Added(offsets[i] - offsets[i - 1]), "frame {i}");
        }
    }
    let out = st.finish();
    let inner = Rect::new(0, 0, (w - 14) as i32, 1000);
    assert_eq!(out.crop(inner), page.crop(inner));
}

#[test]
fn horizontal_page() {
    let page = text_page(260, 1500, 71, &[]);
    let page = transpose(&page); // 1500 wide, 260 tall, text runs vertically: fine for matching
    let offsets = [0u32, 250, 500, 750, 1000, 1100];
    let frames: Vec<Image> = offsets.iter().map(|&x| hcrop(&page, x, 400)).collect();
    let (out, res) = stitch_all(Axis::Horizontal, &frames);
    for (i, r) in res.iter().enumerate().skip(1) {
        assert_eq!(*r, PushResult::Added(offsets[i] - offsets[i - 1]), "frame {i}");
    }
    assert_eq!(out, page);
}

#[test]
fn horizontal_noise_with_sticky_left_column() {
    // A frozen first column (like a spreadsheet's row headers).
    let page = noise_page(1000, 120, 73);
    let frozen = noise_page(30, 120, 74);
    let offsets = [0u32, 150, 300, 450, 600, 700];
    let frames: Vec<Image> = offsets
        .iter()
        .map(|&x| {
            let mut f = Image::new(230, 120);
            f.blit(&frozen, 0, 0);
            f.blit(&hcrop(&page, x, 200), 30, 0);
            f
        })
        .collect();
    let (out, _) = stitch_all(Axis::Horizontal, &frames);
    let mut expected = Image::new(930, 120);
    expected.blit(&frozen, 0, 0);
    expected.blit(&hcrop(&page, 0, 900), 30, 0);
    assert_eq!(out, expected);
}

#[test]
fn join_columns_crops_to_shortest() {
    let page = noise_page(300, 200, 83);
    let a = hcrop(&page, 0, 200);
    let b = vcat(&[&hcrop(&page, 100, 200), &noise_page(200, 7, 84)]);
    let out = join_columns(&[a, b], StitchOptions::default());
    assert_eq!(out, page);
}

#[test]
fn empty_stitcher() {
    let st = Stitcher::new(Axis::Horizontal);
    assert!(st.finish().is_empty());
    assert!(join_columns(&[], StitchOptions::default()).is_empty());
}

#[test]
fn bench_runs() {
    let ms = bench_synthetic(3);
    assert!(ms > 0.0);
}

#[test]
fn bench_frames_stitch_exactly() {
    let step = (1080 - 60) * 3 / 4;
    let frames: Vec<Image> = (0..3).map(|i| synth_frame(i * step, 400, 1080, 60)).collect();
    let mut st = Stitcher::new(Axis::Vertical);
    for f in &frames {
        assert!(matches!(st.push(f), PushResult::Added(_)));
    }
    assert_eq!(st.finish().height, 1080 + 2 * step);
}

/// `cargo test --release bench_report -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_report() {
    for screens in [1u32, 5, 20] {
        let ms = bench_synthetic(screens);
        println!("stitch {screens} x 1920x1080: {ms:.1} ms ({:.2} ms/frame)", ms / screens as f64);
    }
    let img = synth_frame(0, 1920, 1080, 0);
    let t = std::time::Instant::now();
    let mut st = Stitcher::new(Axis::Horizontal);
    st.push(&img);
    st.push(&img);
    println!("horizontal push x2 (transpose + hash): {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let b = synth_frame(765, 1920, 1080, 0);
    let t = std::time::Instant::now();
    let (la, lb) = (hash_lines(&img, 0), hash_lines(&b, 0));
    println!("hash_lines x2: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    let m = find_shift(&la, &lb, None);
    println!("find_shift: {:.2} ms ({m:?})", t.elapsed().as_secs_f64() * 1000.0);
    let t = std::time::Instant::now();
    let v = img.data.to_vec();
    println!("copy 8 MB: {:.2} ms ({})", t.elapsed().as_secs_f64() * 1000.0, v.len());
}

#[test]
fn body_segments_truncate_across_boundaries() {
    let mut b = Body::default();
    b.append(&[1, 2, 3]);
    b.append(&[4, 5]);
    b.append(&[]);
    b.truncate_end(3);
    assert_eq!(b.len, 2);
    b.append(&[9]);
    assert_eq!(b.into_vec(), vec![1, 2, 9]);
}
