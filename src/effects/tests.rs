//! Golden and assertion tests for the effects, plus (ignored) sample-PNG and 4K timing runs.

use super::text::{render_text, render_text_styled, FontSpec, TextAlign};
use super::*;
use crate::common::{Image, Point, Rect};

/// FNV-1a over the pixels and size.
fn hash(img: &Image) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in img.width.to_le_bytes().iter().chain(&img.height.to_le_bytes()).chain(&img.data) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn opaque(c: Color) -> [u8; 4] {
    c.premul_bgra()
}

/// Deterministic test picture: gradient, a few shapes, distinct quadrants.
pub(crate) fn test_image(w: u32, h: u32) -> Image {
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            let b = ((x + y) % 64 * 4) as u8;
            img.set_pixel(x, y, [b, g, r, 255]);
        }
    }
    // A dark square and a white bar.
    for y in h / 4..h / 2 {
        for x in w / 4..w / 2 {
            img.set_pixel(x, y, [20, 20, 20, 255]);
        }
    }
    for y in h * 3 / 4..(h * 3 / 4 + h / 10).min(h) {
        for x in 0..w {
            img.set_pixel(x, y, [255, 255, 255, 255]);
        }
    }
    img
}

fn is_valid_premul(img: &Image) -> bool {
    img.data.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
}

// ------------------------------------------------------------------ geometry

#[test]
fn resize_dims_math() {
    let pct = |percent| ResizeParams { percent, ..Default::default() };
    assert_eq!(resize_dims(200, 100, &pct(50.0)), (100, 50));
    assert_eq!(resize_dims(200, 100, &pct(200.0)), (400, 200));
    assert_eq!(resize_dims(3, 3, &pct(1.0)), (1, 1));
    let px = |width, height, keep_aspect| ResizeParams {
        unit: ResizeUnit::Pixels,
        width,
        height,
        keep_aspect,
        ..Default::default()
    };
    assert_eq!(resize_dims(200, 100, &px(100, 0, true)), (100, 50));
    assert_eq!(resize_dims(200, 100, &px(0, 25, true)), (50, 25));
    // Both given with keep_aspect: fit inside the box.
    assert_eq!(resize_dims(200, 100, &px(100, 100, true)), (100, 50));
    assert_eq!(resize_dims(200, 100, &px(120, 10, true)), (20, 10));
    assert_eq!(resize_dims(200, 100, &px(120, 10, false)), (120, 10));
    assert_eq!(resize_dims(200, 100, &px(120, 0, false)), (120, 100));
    assert_eq!(quick_resize_percent(1), Some(25.0));
    assert_eq!(quick_resize_percent(2), Some(50.0));
    assert_eq!(quick_resize_percent(3), Some(75.0));
    assert_eq!(quick_resize_percent(4), Some(200.0));
    assert_eq!(quick_resize_percent(5), None);
}

#[test]
fn resize_quality() {
    // A flat colour stays exactly flat with every filter, up and down.
    let flat = Image::filled(37, 23, [10, 120, 200, 255]);
    for f in ResampleFilter::ALL {
        for (w, h) in [(10, 7), (80, 51), (37, 50)] {
            let out = resize_to(&flat, w, h, f);
            assert_eq!((out.width, out.height), (w, h));
            assert!(out.data.chunks_exact(4).all(|p| p == [10, 120, 200, 255]), "{f:?} {w}x{h}");
        }
    }
    // 2x2 checker of 2px blocks downscaled 2x with bilinear averages each block exactly.
    let mut img = Image::new(8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let v = if (x / 2 + y / 2) % 2 == 0 { 200 } else { 40 };
            img.set_pixel(x, y, [v, v, v, 255]);
        }
    }
    // Downscaling preserves the mean brightness and keeps the pattern's phase.
    let half = resize_to(&img, 4, 4, ResampleFilter::Bilinear);
    let mean = half.data.chunks_exact(4).map(|p| p[0] as u32).sum::<u32>() / 16;
    assert!((115..=125).contains(&mean), "{mean}");
    assert!(half.pixel(0, 0)[0] > half.pixel(1, 0)[0]);
    // Nearest 2x upscale duplicates pixels.
    let n = resize_to(&img, 16, 16, ResampleFilter::Nearest);
    assert_eq!(n.pixel(3, 3), img.pixel(1, 1));
    assert_eq!(n.pixel(4, 0), img.pixel(2, 0));
    // Lanczos ringing never produces invalid premultiplied pixels with transparency.
    let mut t = test_image(64, 64);
    for y in 0..64 {
        for x in 32..64 {
            t.set_pixel(x, y, [0, 0, 0, 0]);
        }
    }
    for f in ResampleFilter::ALL {
        assert!(is_valid_premul(&resize_to(&t, 23, 41, f)));
        assert!(is_valid_premul(&resize_to(&t, 150, 99, f)));
    }
    assert_eq!((quick_resize(&t, 1).width, quick_resize(&t, 4).width), (16, 128));
}

#[test]
fn crop_flip_rotate() {
    let img = test_image(7, 5);
    let c = crop(&img, Rect::new(2, 1, 3, 2));
    assert_eq!((c.width, c.height), (3, 2));
    assert_eq!(c.pixel(0, 0), img.pixel(2, 1));

    let fh = flip_horizontal(&img);
    assert_eq!(fh.pixel(0, 2), img.pixel(6, 2));
    assert_eq!(flip_horizontal(&fh), img);
    let fv = flip_vertical(&img);
    assert_eq!(fv.pixel(3, 0), img.pixel(3, 4));
    assert_eq!(flip_vertical(&fv), img);

    let r90 = rotate_quarter(&img, QuarterTurn::Cw90);
    assert_eq!((r90.width, r90.height), (5, 7));
    // Clockwise: the top-left pixel ends at the top-right.
    assert_eq!(r90.pixel(4, 0), img.pixel(0, 0));
    assert_eq!(r90.pixel(0, 0), img.pixel(0, 4));
    let r270 = rotate_quarter(&img, QuarterTurn::Cw270);
    assert_eq!(r270.pixel(0, 6), img.pixel(0, 0));
    assert_eq!(rotate_quarter(&r90, QuarterTurn::Cw270), img);
    let r180 = rotate_quarter(&img, QuarterTurn::Cw180);
    assert_eq!(r180.pixel(0, 0), img.pixel(6, 4));
    assert_eq!(rotate_quarter(&r90, QuarterTurn::Cw90), r180);
    // Arbitrary-angle entry point uses the exact paths for right angles.
    assert_eq!(rotate(&img, &RotateParams { angle: 90.0, ..Default::default() }), r90);
    assert_eq!(rotate(&img, &RotateParams { angle: -90.0, ..Default::default() }), r270);
    assert_eq!(rotate(&img, &RotateParams { angle: 360.0, ..Default::default() }), img);
}

#[test]
fn rotate_any_angle() {
    assert_eq!(rotated_size(100, 50, 0.0), (100, 50));
    assert_eq!(rotated_size(100, 100, 45.0), (142, 142));
    assert_eq!(rotated_size(100, 50, 30.0), (112, 94));
    let img = Image::filled(60, 40, [0, 0, 255, 255]);
    let out = rotate(&img, &RotateParams { angle: 30.0, fill: Color::TRANSPARENT });
    assert_eq!((out.width, out.height), rotated_size(60, 40, 30.0));
    // Centre is solid, corners are transparent, edges anti-aliased.
    assert_eq!(out.pixel(out.width / 2, out.height / 2), [0, 0, 255, 255]);
    assert_eq!(out.pixel(0, 0), [0, 0, 0, 0]);
    assert!(out.data.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
    assert!(is_valid_premul(&out));
    let filled = rotate(&img, &RotateParams { angle: 30.0, fill: Color::WHITE });
    assert_eq!(filled.pixel(0, 0), [255, 255, 255, 255]);
    assert!(!filled.has_transparency());
    assert_eq!(hash(&out), hash(&rotate(&img, &RotateParams { angle: 30.0, fill: Color::TRANSPARENT })));
}

#[test]
fn canvas_math() {
    // Anchor offsets for a 4x2 image in a 10x6 canvas.
    let expect = [(0, 0), (3, 0), (6, 0), (0, 2), (3, 2), (6, 2), (0, 4), (3, 4), (6, 4)];
    for (a, e) in Anchor::ALL.iter().zip(expect) {
        assert_eq!(a.offset(10, 6, 4, 2), e, "{a:?}");
    }
    // Shrinking crops from the anchored side.
    assert_eq!(Anchor::BottomRight.offset(4, 2, 10, 6), (-6, -4));
    assert_eq!(Anchor::Center.offset(4, 2, 9, 5), (-3, -2));

    let img = Image::filled(4, 2, [1, 2, 3, 255]);
    let c = canvas_size(&img, &CanvasParams { width: 10, height: 6, anchor: Anchor::Bottom, fill: Color::WHITE });
    assert_eq!((c.width, c.height), (10, 6));
    assert_eq!(c.pixel(3, 4), [1, 2, 3, 255]);
    assert_eq!(c.pixel(2, 4), [255, 255, 255, 255]);
    assert_eq!(c.pixel(3, 3), [255, 255, 255, 255]);
    let t = canvas_size(&img, &CanvasParams { width: 6, height: 2, anchor: Anchor::Left, fill: Color::TRANSPARENT });
    assert_eq!(t.pixel(5, 0), [0, 0, 0, 0]);

    let e = expand_canvas(
        &img,
        &ExpandParams { edges: Edges { left: 1, top: 2, right: 3, bottom: 4 }, fill: Color::rgb(9, 9, 9) },
    );
    assert_eq!((e.width, e.height), (8, 8));
    assert_eq!(e.pixel(1, 2), [1, 2, 3, 255]);
    assert_eq!(e.pixel(0, 0), opaque(Color::rgb(9, 9, 9)));
    assert_eq!(e.pixel(7, 7), opaque(Color::rgb(9, 9, 9)));
}

// ------------------------------------------------------------------ filters and colour

#[test]
fn blur_sharpen_pixelate() {
    let radii = filter::box_radii(5.0);
    assert!(radii.iter().all(|r| (3..=6).contains(r)), "{radii:?}");
    let flat = Image::filled(50, 40, [30, 60, 90, 255]);
    assert_eq!(blur(&flat, &BlurParams { radius: 10.0 }), flat);
    assert_eq!(sharpen(&flat, &SharpenParams::default()), flat);

    let img = test_image(120, 80);
    let b = blur(&img, &BlurParams { radius: 8.0 });
    // Edge of the dark square gets soft: the pixel just outside is darker than before.
    let (x, y) = (29, 30);
    assert!(b.pixel(x, y)[2] < img.pixel(x, y)[2]);
    assert_eq!(hash(&b), hash(&blur(&img, &BlurParams { radius: 8.0 })));
    let s = sharpen(&img, &SharpenParams { amount: 150.0, radius: 2.0, threshold: 0 });
    // The dark square's inner edge gets darker (overshoot), the outside lighter.
    assert!(s.pixel(30, 30)[2] <= img.pixel(30, 30)[2]);
    assert!(s.pixel(29, 30)[2] >= img.pixel(29, 30)[2]);
    assert!(is_valid_premul(&s));

    let p = pixelate(&img, &PixelateParams { block: 10 });
    assert_eq!((p.width, p.height), (120, 80));
    for by in 0..8 {
        for bx in 0..12 {
            let c = p.pixel(bx * 10, by * 10);
            assert_eq!(p.pixel(bx * 10 + 9, by * 10 + 9), c);
        }
    }
    // A partial edge block (block 7 doesn't divide 120).
    let p7 = pixelate(&img, &PixelateParams { block: 7 });
    // Columns 119.. form a 1-px-wide block; rows 77..80 a 3-row block.
    assert_eq!(p7.pixel(119, 79), p7.pixel(119, 77));
    assert_ne!(p7.pixel(119, 79), p7.pixel(118, 79));
}

#[test]
fn color_adjustments() {
    let img = test_image(64, 48);
    assert_eq!(adjust_colors(&img, &AdjustParams::default()), img);
    let bright = adjust_colors(&img, &AdjustParams { brightness: 50, ..Default::default() });
    assert!(bright.pixel(10, 10)[1] > img.pixel(10, 10)[1]);
    let gray = adjust_colors(&img, &AdjustParams { saturation: -100, ..Default::default() });
    let p = gray.pixel(50, 5);
    assert!(p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1, "{p:?}");
    // Hue 120 degrees turns pure red into (roughly) green.
    let red = Image::filled(2, 2, [0, 0, 255, 255]);
    let g = adjust_colors(&red, &AdjustParams { hue: 120, ..Default::default() }).pixel(0, 0);
    assert!(g[1] > g[2] && g[1] > g[0], "{g:?}");
    let more_red = adjust_colors(&img, &AdjustParams { red: 40, ..Default::default() });
    assert!(more_red.pixel(5, 5)[2] > img.pixel(5, 5)[2]);
    assert_eq!(more_red.pixel(5, 5)[0], img.pixel(5, 5)[0]);
    let gam =
        adjust_colors(&Image::filled(1, 1, [128, 128, 128, 255]), &AdjustParams { gamma: 2.2, ..Default::default() });
    assert!(gam.pixel(0, 0)[0] > 170);

    // Auto levels stretches a low-contrast image to the full range.
    let mut dull = Image::new(100, 1);
    for x in 0..100 {
        let v = 100 + (x as u8) / 2;
        dull.set_pixel(x, 0, [v, v, v, 255]);
    }
    for f in [auto_levels, auto_contrast] {
        let s = f(&dull);
        assert!(s.pixel(1, 0)[0] < 10, "{:?}", s.pixel(1, 0));
        assert!(s.pixel(98, 0)[0] > 245);
    }

    let n = negative(&img);
    assert_eq!(n.pixel(3, 3)[2], 255 - img.pixel(3, 3)[2]);
    assert_eq!(negative(&n), img);
    let gs = grayscale(&img);
    let q = gs.pixel(40, 40);
    assert!(q[0] == q[1] && q[1] == q[2]);
    let sp = sepia(&Image::filled(1, 1, [128, 128, 128, 255])).pixel(0, 0);
    assert!(sp[2] > sp[1] && sp[1] > sp[0], "sepia is warm: {sp:?}");
    // Alpha is preserved by the tone effects.
    let half = Image::filled(2, 2, Color::rgba(200, 100, 50, 128).premul_bgra());
    for f in [negative, grayscale, sepia, auto_levels] {
        let o = f(&half);
        assert_eq!(o.pixel(0, 0)[3], 128);
        assert!(is_valid_premul(&o));
    }
}

#[test]
fn color_count_and_reduction() {
    let mut img = Image::new(4, 4);
    img.set_pixel(0, 0, [1, 2, 3, 255]);
    img.set_pixel(1, 0, [1, 2, 3, 255]);
    img.set_pixel(2, 0, [9, 9, 9, 255]);
    img.set_pixel(3, 0, [1, 1, 1, 128]);
    // Remaining pixels are transparent black: one more colour.
    assert_eq!(count_colors(&img), 4);

    let big = test_image(256, 128);
    assert!(count_colors(&big) > 1000);
    for n in REDUCE_PRESETS {
        for dither in [false, true] {
            let r = reduce_colors(&big, &ReduceParams { colors: n, dither });
            let c = count_colors(&r);
            assert!(c <= n as usize, "{n} colours, dither {dither}: got {c}");
            assert!(c >= (n as usize).min(2), "{n}: palette collapsed to {c}");
            assert_eq!(hash(&r), hash(&reduce_colors(&big, &ReduceParams { colors: n, dither })));
        }
    }
    // An image already within budget is unchanged.
    let few = Image::filled(10, 10, [5, 6, 7, 255]);
    assert_eq!(reduce_colors(&few, &ReduceParams { colors: 2, dither: true }), few);
    // Two colours reduce to exactly black and white for a black/white image with grey noise.
    let mut bw = Image::new(20, 1);
    for x in 0..20 {
        let v = if x < 10 { 10 + x as u8 } else { 240 + (x as u8 - 10) };
        bw.set_pixel(x, 0, [v, v, v, 255]);
    }
    let two = reduce_colors(&bw, &ReduceParams { colors: 2, dither: false });
    assert_eq!(count_colors(&two), 2);
    assert!(two.pixel(0, 0)[0] < 30 && two.pixel(19, 0)[0] > 230);
}

#[test]
fn transparency_tolerance() {
    // White background, near-white noise, a black ring separating an inner white island.
    let mut img = Image::filled(30, 30, [255, 255, 255, 255]);
    img.set_pixel(1, 1, [245, 245, 245, 255]); // 4 % off white
    img.set_pixel(2, 1, [200, 200, 200, 255]); // 22 % off
    for i in 10..20 {
        for (x, y) in [(i, 10), (i, 19), (10, i), (19, i)] {
            img.set_pixel(x, y, [0, 0, 0, 255]);
        }
    }
    let base = TransparencyParams { color: Color::WHITE, smooth_edges: false, ..Default::default() };
    let g0 =
        make_transparent(&img, &TransparencyParams { tolerance: 0, mode: TransparencyMode::Global, ..base.clone() });
    assert_eq!(g0.pixel(0, 0)[3], 0);
    assert_eq!(g0.pixel(1, 1)[3], 255, "tolerance 0 keeps near-white");
    assert_eq!(g0.pixel(15, 15)[3], 0, "global reaches the island");
    let g10 =
        make_transparent(&img, &TransparencyParams { tolerance: 10, mode: TransparencyMode::Global, ..base.clone() });
    assert_eq!(g10.pixel(1, 1)[3], 0);
    assert_eq!(g10.pixel(2, 1)[3], 255);
    assert_eq!(g10.pixel(10, 10)[3], 255);
    // Contiguous from a corner does not cross the ring.
    let c = make_transparent(
        &img,
        &TransparencyParams {
            tolerance: 10,
            mode: TransparencyMode::Contiguous,
            seed: Point::new(0, 0),
            ..base.clone()
        },
    );
    assert_eq!(c.pixel(29, 29)[3], 0);
    assert_eq!(c.pixel(15, 15)[3], 255, "island stays");
    // Seed on a non-matching pixel removes nothing.
    let none = transparency_mask(&img, &TransparencyParams { tolerance: 10, seed: Point::new(10, 10), ..base.clone() });
    assert!(none.data.iter().all(|&m| m == 0));
    // Smooth edges: a mid-grey pixel next to the background is partially removed and
    // decontaminated (no white fringe left in its colour).
    let mut aa = Image::filled(5, 1, [255, 255, 255, 255]);
    aa.set_pixel(3, 0, [0, 0, 0, 255]);
    aa.set_pixel(2, 0, [235, 235, 235, 255]); // between background and black, close to white
    let s = make_transparent(&aa, &TransparencyParams { tolerance: 5, smooth_edges: true, ..base.clone() });
    let p = s.pixel(2, 0);
    assert!(p[3] > 0 && p[3] < 255, "{p:?}");
    // Without decontamination the colour would be 235 * alpha; the white share is removed.
    assert!((p[0] as u32) < 235 * p[3] as u32 / 255 - 5, "decontaminated: {p:?}");
    assert_eq!(s.pixel(3, 0), [0, 0, 0, 255]);
    let prev = mask_preview(&img, &transparency_mask(&img, &base), Color::rgba(255, 0, 255, 128));
    assert_ne!(prev.pixel(0, 0), img.pixel(0, 0));
    assert_eq!(prev.pixel(10, 10), img.pixel(10, 10));
}

// ------------------------------------------------------------------ pixel tools and selection

#[test]
fn strips() {
    let img = test_image(20, 10);
    let h = remove_strip(
        &img,
        &StripParams { orientation: Orientation::Horizontal, position: 3, size: 4, ..Default::default() },
    );
    assert_eq!((h.width, h.height), (20, 6));
    assert_eq!(h.pixel(5, 2), img.pixel(5, 2));
    assert_eq!(h.pixel(5, 3), img.pixel(5, 7));
    let v = remove_strip(
        &img,
        &StripParams { orientation: Orientation::Vertical, position: 15, size: 99, ..Default::default() },
    );
    assert_eq!((v.width, v.height), (15, 10));
    assert_eq!(v.pixel(14, 4), img.pixel(14, 4));

    let red = Color::rgb(255, 0, 0);
    let ih =
        insert_strip(&img, &StripParams { orientation: Orientation::Horizontal, position: 2, size: 5, color: red });
    assert_eq!((ih.width, ih.height), (20, 15));
    assert_eq!(ih.pixel(0, 1), img.pixel(0, 1));
    assert_eq!(ih.pixel(0, 2), opaque(red));
    assert_eq!(ih.pixel(0, 6), opaque(red));
    assert_eq!(ih.pixel(0, 7), img.pixel(0, 2));
    let iv = insert_strip(&img, &StripParams { orientation: Orientation::Vertical, position: 20, size: 3, color: red });
    assert_eq!((iv.width, iv.height), (23, 10));
    assert_eq!(iv.pixel(21, 0), opaque(red));
    // Insert then remove is the identity.
    let p = StripParams { orientation: Orientation::Vertical, position: 7, size: 4, color: red };
    assert_eq!(remove_strip(&insert_strip(&img, &p), &p), img);
}

#[test]
fn clone_stamp_copies() {
    let mut img = Image::filled(40, 40, [255, 255, 255, 255]);
    for y in 0..10 {
        for x in 0..10 {
            img.set_pixel(x, y, [0, 0, 255, 255]);
        }
    }
    clone_stamp(&mut img, Point::new(5, 5), Point::new(30, 30), 4, 1.0);
    assert_eq!(img.pixel(30, 30), [0, 0, 255, 255]);
    assert_eq!(img.pixel(33, 30), [0, 0, 255, 255]);
    assert_eq!(img.pixel(36, 36), [255, 255, 255, 255]);
    // Soft brush: centre copied, rim blended.
    let mut soft = Image::filled(40, 40, [255, 255, 255, 255]);
    for y in 0..20 {
        for x in 0..20 {
            soft.set_pixel(x, y, [0, 0, 0, 255]);
        }
    }
    clone_stamp(&mut soft, Point::new(10, 10), Point::new(30, 30), 6, 0.0);
    assert!(soft.pixel(30, 30)[0] < 30);
    let rim = soft.pixel(35, 30)[0];
    assert!(rim > 30 && rim < 250, "{rim}");
    // Off-image source/target is ignored safely.
    clone_stamp(&mut soft, Point::new(-50, -50), Point::new(500, 500), 10, 0.5);
}

#[test]
fn selection_helpers() {
    let img = test_image(50, 40);
    let m = rect_mask(50, 40, Rect::new(10, 5, 20, 10));
    let st = selection_stats(&img, &m);
    assert_eq!(st.pixel_count, 200);
    assert_eq!(st.bounds, Rect::new(10, 5, 20, 10));
    assert!(st.unique_colors > 1 && st.unique_colors <= 200);

    // Polygon: a triangle covering half of a 20x20 square.
    let tri = polygon_mask(50, 40, &[Point::new(0, 0), Point::new(20, 0), Point::new(0, 20)]);
    let area: u64 = tri.data.iter().map(|&v| v as u64).sum::<u64>() / 255;
    assert!((195..=225).contains(&area), "{area}");
    assert_eq!(tri.get(2, 2), 255);
    assert_eq!(tri.get(19, 19), 0);
    let e = ellipse_mask(50, 40, Rect::new(0, 0, 50, 40));
    let ea: u64 = e.data.iter().map(|&v| v as u64).sum::<u64>() / 255;
    let ideal = std::f64::consts::PI * 25.0 * 20.0;
    assert!((ea as f64 - ideal).abs() < ideal * 0.02, "{ea} vs {ideal}");

    let (piece, from) = extract_selection(&img, &m);
    assert_eq!(from, Rect::new(10, 5, 20, 10));
    assert_eq!(piece.pixel(0, 0), img.pixel(10, 5));
    let moved = move_selection(&img, &m, &MoveParams { dx: 5, dy: 20, copy: false, fill: Color::WHITE });
    assert_eq!(moved.pixel(15, 25), img.pixel(10, 5));
    assert_eq!(moved.pixel(10, 5), [255, 255, 255, 255]);
    let copied = move_selection(&img, &m, &MoveParams { dx: 5, dy: 20, copy: true, fill: Color::WHITE });
    assert_eq!(copied.pixel(10, 5), img.pixel(10, 5));
    let neg = negative(&img);
    let partial = apply_masked(&img, &neg, &m);
    assert_eq!(partial.pixel(12, 7), neg.pixel(12, 7));
    assert_eq!(partial.pixel(0, 0), img.pixel(0, 0));
}

// ------------------------------------------------------------------ effects

#[test]
fn effect_dimensions_and_determinism() {
    let img = test_image(160, 100);
    let run = |name: &str, f: &dyn Fn(&Image) -> Image, w: u32, h: u32| {
        let a = f(&img);
        let b = f(&img);
        assert_eq!((a.width, a.height), (w, h), "{name}");
        assert_eq!(hash(&a), hash(&b), "{name} is not deterministic");
        assert!(is_valid_premul(&a), "{name} produced invalid premultiplied pixels");
        a
    };
    let spot = run(
        "spotlight",
        &|i| spotlight(i, &SpotlightParams { rect: Rect::new(20, 10, 120, 80), ..Default::default() }),
        160,
        100,
    );
    assert_eq!(spot.pixel(80, 50), img.pixel(80, 50), "inside unchanged");
    assert!(spot.pixel(2, 2)[2] < img.pixel(2, 2)[2] / 2 + 10, "outside dimmed");

    let sp = ShadowParams::default();
    let (l, t, r, b) = shadow_margins(&sp);
    let sh = run("drop_shadow", &|i| drop_shadow(i, &sp), 160 + l + r, 100 + t + b);
    assert_eq!(sh.pixel(l + 10, t + 10), img.pixel(10, 10));
    assert_eq!(sh.pixel(0, 0)[3], 0, "transparent corner");
    assert!(sh.pixel(l + 80, t + 100 + 4)[3] > 20, "shadow under the image");

    let fr = run("frame", &|i| frame(i, &FrameParams::default()), 176, 116);
    assert_eq!(fr.pixel(0, 0), opaque(FrameParams::default().color));
    assert_eq!(fr.pixel(8, 8), img.pixel(0, 0));
    let dbl = FrameParams { style: FrameStyle::Double, width: 9, corner_radius: 12, ..Default::default() };
    let fd = run("frame double", &|i| frame(i, &dbl), 178, 118);
    assert_eq!(fd.pixel(0, 0)[3], 0, "rounded corner is transparent");
    assert_eq!(fd.pixel(89, 4), opaque(Color::WHITE), "gap between the lines");
    assert_eq!(fd.pixel(89, 1), opaque(dbl.color));

    let tp = TornEdgeParams::default();
    let (l, t, r, b) = shadow_margins(&tp.shadow);
    let torn = run("torn_edge", &|i| torn_edge(i, &tp), 160 + l + r, 100 + t + b);
    assert_eq!(torn.pixel(l + 10, t + 10), img.pixel(10, 10), "top untouched");
    let other_seed = torn_edge(&img, &TornEdgeParams { seed: 2, ..tp.clone() });
    assert_ne!(hash(&torn), hash(&other_seed), "seed changes the edge");
    let torn_all = torn_edge(&img, &TornEdgeParams { sides: Sides::ALL, shadow_enabled: false, ..tp.clone() });
    assert_eq!((torn_all.width, torn_all.height), (160, 100));
    assert!(torn_all.pixel(80, 50)[3] == 255 && torn_all.pixel(0, 0)[3] < 255);

    let fade = run("fade_edge", &|i| fade_edge(i, &FadeEdgeParams::default()), 160, 100);
    assert_eq!(fade.pixel(0, 50)[3], 0);
    assert_eq!(fade.pixel(80, 50), img.pixel(80, 50));

    let rp = ReflectionParams::default();
    let refl = run("reflection", &|i| reflection(i, &rp), 160, 100 + 2 + 35);
    assert_eq!(refl.pixel(5, 101)[3], 0, "gap");
    let top_of_reflection = refl.pixel(5, 102);
    assert!(top_of_reflection[3] > 80 && top_of_reflection[3] < 120, "{top_of_reflection:?}");

    let mark = Image::filled(20, 10, [0, 0, 255, 255]);
    let wm = WatermarkParams { opacity: 100, ..Default::default() };
    let w1 = run("watermark", &|i| watermark(i, &mark, &wm), 160, 100);
    assert_eq!(w1.pixel(160 - 16 - 1, 100 - 16 - 1), [0, 0, 255, 255]);
    assert_eq!(w1.pixel(160 - 16, 100 - 16), img.pixel(144, 84));
    let tiled = run(
        "watermark tiled",
        &|i| watermark(i, &mark, &WatermarkParams { tile: true, spacing: 10, scale: 50.0, ..wm.clone() }),
        160,
        100,
    );
    let marked = tiled.data.chunks_exact(4).filter(|p| *p == [0, 0, 255, 255]).count();
    assert!(marked > 500, "{marked}");

    let oil = run("oil_paint", &|i| oil_paint(i, &OilPaintParams::default()), 160, 100);
    assert_eq!(oil.pixel(60, 37), [20, 20, 20, 255], "flat areas stay flat");
    let sk = run("sketch", &|i| sketch(i, &SketchParams::default()), 160, 100);
    let s0 = sk.pixel(5, 5);
    assert!(s0[0] > 200, "smooth areas are paper-white: {s0:?}");
    assert!(sk.pixel(40, 30)[0] < 200, "the square's edge is drawn");
}

#[test]
fn oil_paint_matches_naive_kuwahara() {
    let img = test_image(23, 17);
    let r = 2usize;
    let out = oil_paint(&img, &OilPaintParams { radius: r as u32 });
    let (w, h) = (23i64, 17i64);
    for y in 0..h {
        for x in 0..w {
            let mut best = (u64::MAX, [0u8; 4]);
            for (qx, qy) in [(-(r as i64), -(r as i64)), (0, -(r as i64)), (-(r as i64), 0), (0, 0)] {
                let mut s = [0u64; 4];
                let (mut sl, mut sl2) = (0u64, 0u64);
                for dy in 0..=r as i64 {
                    for dx in 0..=r as i64 {
                        let px = img.pixel((x + qx + dx).clamp(0, w - 1) as u32, (y + qy + dy).clamp(0, h - 1) as u32);
                        let l = luma(px[0], px[1], px[2]) as u64;
                        sl += l;
                        sl2 += l * l;
                        for c in 0..4 {
                            s[c] += px[c] as u64;
                        }
                    }
                }
                let n = ((r + 1) * (r + 1)) as u64;
                let var = (n * sl2).saturating_sub(sl * sl);
                if var < best.0 {
                    best = (var, s.map(|v| ((v + n / 2) / n) as u8));
                }
            }
            assert_eq!(out.pixel(x as u32, y as u32), best.1, "({x},{y})");
        }
    }
}

#[test]
fn text_and_caption() {
    let t = render_text("Hello, SnapForge", "Segoe UI", 20, Color::BLACK, 0);
    assert!(t.width > 60 && t.height >= 20 && t.height < 40, "{}x{}", t.width, t.height);
    assert!(t.data.chunks_exact(4).any(|p| p[3] == 255));
    assert!(is_valid_premul(&t));
    assert!(render_text("", "Segoe UI", 20, Color::BLACK, 0).is_empty());
    // Wrapping at a narrow width makes more lines, never wider than asked.
    let long = "The quick brown fox jumps over the lazy dog. ".repeat(4);
    let wrapped = render_text(&long, "Segoe UI", 16, Color::BLACK, 200);
    assert!(wrapped.width <= 200 && wrapped.height > 60, "{}x{}", wrapped.width, wrapped.height);
    let centered = render_text_styled("Hi", &FontSpec::default(), Color::BLACK, 200, TextAlign::Center);
    assert_eq!(centered.width, 200);

    let img = test_image(300, 120);
    let p = CaptionParams { text: long.clone(), ..Default::default() };
    let c = caption(&img, &p);
    assert_eq!(c.width, 300);
    assert!(c.height > 120 + 60, "wrapped caption adds a tall bar: {}", c.height);
    assert_eq!(c.pixel(0, 0), img.pixel(0, 0));
    assert_eq!(c.pixel(0, 120), opaque(p.border_color), "separator line");
    assert_eq!(c.pixel(1, c.height - 2), opaque(p.background));
    let top = caption(
        &img,
        &CaptionParams {
            text: "Top".into(),
            position: CaptionPosition::Top,
            border: CaptionBorder::Frame,
            border_width: 2,
            ..Default::default()
        },
    );
    assert_eq!(top.width, 304);
    assert_eq!(top.pixel(2, top.height - 3), img.pixel(0, 119));
    assert_eq!(caption(&img, &CaptionParams::default()), img, "empty text: unchanged");

    let wm = watermark_text(&img, &TextWatermarkParams { text: "zheer.dev".into(), ..Default::default() });
    assert_eq!((wm.width, wm.height), (300, 120));
    assert_ne!(hash(&wm), hash(&img));
    assert_eq!(wm.pixel(0, 0), img.pixel(0, 0));
}

#[test]
fn params_roundtrip_serde() {
    let p = TornEdgeParams { seed: 42, sides: Sides::ALL, ..Default::default() };
    let s = toml::to_string(&p).unwrap();
    let back: TornEdgeParams = toml::from_str(&s).unwrap();
    assert_eq!(back, p);
    // Missing fields fall back to defaults (old presets keep loading).
    let partial: CaptionParams = toml::from_str("text = \"x\"").unwrap();
    assert_eq!(partial.padding, CaptionParams::default().padding);
}

// ------------------------------------------------------------------ samples and timings

/// A UI-like sample picture: window chrome, text, buttons, a photo-ish gradient.
pub(crate) fn sample_screenshot(w: u32, h: u32) -> Image {
    let mut img = Image::filled(w, h, [250, 248, 246, 255]);
    let title = Image::filled(w, 36, Color::rgb(32, 38, 52).premul_bgra());
    img.blit(&title, 0, 0);
    let ttl = render_text_styled(
        "SnapForge — Untitled.png",
        &FontSpec { size: 15, bold: true, ..Default::default() },
        Color::WHITE,
        0,
        TextAlign::Left,
    );
    composite(&mut img, &ttl, 14, (36 - ttl.height as i32) / 2, 255);
    for (i, c) in [Color::rgb(255, 95, 86), Color::rgb(255, 189, 46), Color::rgb(39, 201, 63)].iter().enumerate() {
        let dot = selection::ellipse_mask(12, 12, Rect::new(0, 0, 12, 12));
        let mut d = Image::filled(12, 12, c.premul_bgra());
        d.apply_mask(&dot.data);
        composite(&mut img, &d, w as i32 - 70 + i as i32 * 20, 12, 255);
    }
    // Side panel with a photo-like gradient circle.
    let pw = w * 2 / 5;
    let mut photo = Image::new(pw - 40, h - 96);
    for y in 0..photo.height {
        for x in 0..photo.width {
            let fx = x as f32 / photo.width as f32;
            let fy = y as f32 / photo.height as f32;
            let r = (40.0 + 180.0 * fy) as u8;
            let g = (120.0 + 100.0 * (1.0 - fx)) as u8;
            let b = (200.0 + 50.0 * fx) as u8;
            photo.set_pixel(x, y, [b, g, r, 255]);
        }
    }
    let sun = selection::ellipse_mask(
        photo.width,
        photo.height,
        Rect::new(photo.width as i32 / 2, photo.height as i32 / 5, photo.width as i32 / 3, photo.width as i32 / 3),
    );
    let mut sunimg = Image::filled(photo.width, photo.height, Color::rgb(255, 214, 102).premul_bgra());
    sunimg.apply_mask(&sun.data);
    composite(&mut photo, &sunimg, 0, 0, 255);
    let hill = selection::polygon_mask(
        photo.width,
        photo.height,
        &[
            Point::new(0, photo.height as i32),
            Point::new(0, photo.height as i32 * 2 / 3),
            Point::new(photo.width as i32 / 3, photo.height as i32 / 2),
            Point::new(photo.width as i32 * 2 / 3, photo.height as i32 * 3 / 4),
            Point::new(photo.width as i32, photo.height as i32 * 3 / 5),
            Point::new(photo.width as i32, photo.height as i32),
        ],
    );
    let mut hillimg = Image::filled(photo.width, photo.height, Color::rgb(46, 125, 80).premul_bgra());
    hillimg.apply_mask(&hill.data);
    composite(&mut photo, &hillimg, 0, 0, 255);
    img.blit(&photo, 20, 56);
    // Text column.
    let x0 = pw as i32 + 10;
    let head = render_text_styled(
        "Release notes",
        &FontSpec { size: 26, bold: true, ..Default::default() },
        Color::rgb(25, 28, 34),
        0,
        TextAlign::Left,
    );
    composite(&mut img, &head, x0, 56, 255);
    let body = "SnapForge captures regions, windows and scrolling pages. The editor adds drop shadows, torn edges, captions and watermarks, and exports straight to PDF. Developed by Zheer Khalid (zheer.dev).";
    let bt = render_text(body, "Segoe UI", 16, Color::rgb(60, 64, 72), w - x0 as u32 - 24);
    composite(&mut img, &bt, x0, 100, 255);
    // Buttons.
    for (i, (label, bg, fg)) in
        [("Save", Color::rgb(37, 99, 235), Color::WHITE), ("Cancel", Color::rgb(229, 231, 235), Color::rgb(30, 30, 30))]
            .iter()
            .enumerate()
    {
        let b = Image::filled(96, 34, bg.premul_bgra());
        let b = super::fx::round_corners(&b, 6);
        let by = h as i32 - 56;
        let bx = x0 + i as i32 * 110;
        composite(&mut img, &b, bx, by, 255);
        let lt = render_text_styled(
            label,
            &FontSpec { size: 15, bold: true, ..Default::default() },
            *fg,
            0,
            TextAlign::Left,
        );
        composite(&mut img, &lt, bx + (96 - lt.width as i32) / 2, by + (34 - lt.height as i32) / 2, 255);
    }
    img
}

fn sample_dir() -> std::path::PathBuf {
    std::env::var_os("SNAPFORGE_SAMPLES")
        .map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("effect-samples"))
}

fn save(img: &Image, name: &str) {
    let dir = sample_dir();
    std::fs::create_dir_all(&dir).unwrap();
    crate::output::save::save_image_as(img, &dir.join(format!("{name}.png")), crate::settings::ImageFormat::Png, 100)
        .unwrap();
}

/// Writes one PNG per effect: `cargo test write_effect_samples -- --ignored`.
#[test]
#[ignore]
fn write_effect_samples() {
    let img = sample_screenshot(720, 420);
    // Show transparency on a checkerboard so the PNGs are easy to judge.
    let show = |i: &Image| checkerboard(i, 10);
    save(&img, "00_source");
    save(&quick_resize(&img, 2), "01_resize_50");
    save(&show(&rotate(&img, &RotateParams { angle: 12.0, ..Default::default() })), "02_rotate_12");
    save(
        &show(&canvas_size(
            &img,
            &CanvasParams { width: 820, height: 480, anchor: Anchor::TopLeft, fill: Color::TRANSPARENT },
        )),
        "03_canvas",
    );
    save(&blur(&img, &BlurParams { radius: 10.0 }), "04_blur");
    save(&sharpen(&img, &SharpenParams { amount: 200.0, ..Default::default() }), "05_sharpen");
    save(&pixelate(&img, &PixelateParams { block: 12 }), "06_pixelate");
    save(
        &adjust_colors(&img, &AdjustParams { contrast: 30, saturation: 40, hue: 30, ..Default::default() }),
        "07_adjust",
    );
    save(&sepia(&img), "08_sepia");
    save(&negative(&img), "09_negative");
    save(&grayscale(&img), "10_grayscale");
    save(&reduce_colors(&img, &ReduceParams { colors: 16, dither: true }), "11_reduce16_dither");
    save(&reduce_colors(&img, &ReduceParams { colors: 4, dither: false }), "12_reduce4");
    let tp = TransparencyParams {
        color: Color::rgb(246, 248, 250),
        tolerance: 6,
        seed: Point::new(700, 300),
        ..Default::default()
    };
    save(&show(&make_transparent(&img, &tp)), "13_transparent");
    save(
        &spotlight(&img, &SpotlightParams { rect: Rect::new(300, 50, 380, 160), ..Default::default() }),
        "14_spotlight",
    );
    save(&show(&drop_shadow(&img, &ShadowParams { corner_radius: 10, ..Default::default() })), "15_drop_shadow");
    save(
        &drop_shadow(&img, &ShadowParams { corner_radius: 10, background: Color::WHITE, ..Default::default() }),
        "15b_drop_shadow_white",
    );
    save(&show(&frame(&img, &FrameParams { width: 10, corner_radius: 16, ..Default::default() })), "16_frame");
    save(
        &show(&frame(&img, &FrameParams { style: FrameStyle::Double, width: 12, ..Default::default() })),
        "17_frame_double",
    );
    save(&torn_edge(&img, &TornEdgeParams { background: Color::WHITE, ..Default::default() }), "18_torn_bottom");
    save(
        &torn_edge(
            &img,
            &TornEdgeParams { sides: Sides::ALL, background: Color::WHITE, seed: 7, ..Default::default() },
        ),
        "18b_torn_all",
    );
    save(&show(&fade_edge(&img, &FadeEdgeParams::default())), "19_fade");
    save(&reflection(&img, &ReflectionParams { background: Color::WHITE, ..Default::default() }), "20_reflection");
    let logo = render_text_styled(
        "SF",
        &FontSpec { size: 48, bold: true, ..Default::default() },
        Color::rgb(37, 99, 235),
        0,
        TextAlign::Left,
    );
    save(&watermark(&img, &logo, &WatermarkParams { opacity: 60, ..Default::default() }), "21_watermark");
    save(
        &watermark_text(
            &img,
            &TextWatermarkParams {
                text: "CONFIDENTIAL".into(),
                placement: WatermarkParams { tile: true, opacity: 25, spacing: 50, ..Default::default() },
                ..Default::default()
            },
        ),
        "22_watermark_tiled",
    );
    save(&caption(&img, &CaptionParams { text: "Figure 1: The SnapForge editor with the effects panel. Long captions wrap onto several lines and stay centred under the image.".into(), ..Default::default() }), "23_caption");
    save(
        &caption(
            &img,
            &CaptionParams {
                text: "Dark top bar".into(),
                position: CaptionPosition::Top,
                background: Color::rgb(32, 38, 52),
                text_color: Color::WHITE,
                border: CaptionBorder::Frame,
                border_width: 3,
                border_color: Color::rgb(32, 38, 52),
                font: FontSpec { size: 20, bold: true, ..Default::default() },
                ..Default::default()
            },
        ),
        "24_caption_top",
    );
    save(&oil_paint(&img, &OilPaintParams { radius: 5 }), "25_oil");
    save(&sketch(&img, &SketchParams::default()), "26_sketch");
    save(&sketch(&img, &SketchParams { color: true, strength: 80 }), "26b_sketch_color");
    let mut cl = img.clone();
    for i in 0..10 {
        clone_stamp(&mut cl, Point::new(560, 380), Point::new(340 + i * 8, 380), 14, 0.5);
    }
    save(&cl, "27_clone");
    save(&remove_strip(&img, &StripParams { position: 100, size: 120, ..Default::default() }), "28_remove_strip");
    save(
        &text::render_text(
            "render_text helper: wrapped at 260 px, any font and colour.",
            "Georgia",
            22,
            Color::rgb(180, 30, 60),
            260,
        ),
        "29_text",
    );
}

/// 4K timings: `cargo test --release bench_4k -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_4k() {
    let img = test_image(3840, 2160);
    let t = |name: &str, f: &dyn Fn() -> Image| {
        let _ = f();
        let start = std::time::Instant::now();
        let out = f();
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("{name:<28} {ms:8.1} ms  -> {}x{}", out.width, out.height);
    };
    t("resize 50% lanczos3", &|| resize(&img, &ResizeParams { percent: 50.0, ..Default::default() }));
    t("resize 75% mitchell", &|| {
        resize(&img, &ResizeParams { percent: 75.0, filter: ResampleFilter::Mitchell, ..Default::default() })
    });
    t("resize 200% lanczos3", &|| resize(&img, &ResizeParams { percent: 200.0, ..Default::default() }));
    t("rotate 90", &|| rotate_quarter(&img, QuarterTurn::Cw90));
    t("rotate 17 deg", &|| rotate(&img, &RotateParams { angle: 17.0, ..Default::default() }));
    t("flip horizontal", &|| flip_horizontal(&img));
    t("blur r=10", &|| blur(&img, &BlurParams { radius: 10.0 }));
    t("blur r=50", &|| blur(&img, &BlurParams { radius: 50.0 }));
    t("sharpen", &|| sharpen(&img, &SharpenParams::default()));
    t("pixelate 10", &|| pixelate(&img, &PixelateParams { block: 10 }));
    t("adjust (all)", &|| {
        adjust_colors(
            &img,
            &AdjustParams {
                brightness: 10,
                contrast: 10,
                gamma: 1.2,
                saturation: 20,
                hue: 15,
                red: 5,
                green: 0,
                blue: -5,
            },
        )
    });
    t("auto levels", &|| auto_levels(&img));
    t("grayscale", &|| grayscale(&img));
    t("sepia", &|| sepia(&img));
    t("negative", &|| negative(&img));
    t("count colors (as image)", &|| {
        let _ = count_colors(&img);
        Image::new(1, 1)
    });
    t("reduce 256 dither", &|| reduce_colors(&img, &ReduceParams { colors: 256, dither: true }));
    t("reduce 16 no dither", &|| reduce_colors(&img, &ReduceParams { colors: 16, dither: false }));
    t("transparent global", &|| {
        make_transparent(&img, &TransparencyParams { mode: TransparencyMode::Global, ..Default::default() })
    });
    t("spotlight", &|| {
        spotlight(&img, &SpotlightParams { rect: Rect::new(1000, 500, 1500, 900), ..Default::default() })
    });
    t("drop shadow", &|| drop_shadow(&img, &ShadowParams::default()));
    t("frame rounded", &|| frame(&img, &FrameParams { corner_radius: 20, ..Default::default() }));
    t("torn edge (all sides)", &|| torn_edge(&img, &TornEdgeParams { sides: Sides::ALL, ..Default::default() }));
    t("fade edge", &|| fade_edge(&img, &FadeEdgeParams::default()));
    t("reflection", &|| reflection(&img, &ReflectionParams::default()));
    t("oil paint r=4", &|| oil_paint(&img, &OilPaintParams::default()));
    t("sketch", &|| sketch(&img, &SketchParams::default()));
}
