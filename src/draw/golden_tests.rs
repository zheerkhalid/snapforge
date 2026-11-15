//! Golden-image tests: every object type rendered on a small deterministic base and compared
//! with reference PNGs in `src/draw/golden/` (per-channel tolerance). Regenerate the references
//! with `SNAPFORGE_UPDATE_GOLDEN=1 cargo test golden`. Mismatches are written to
//! `target/golden-actual/` for inspection.

use super::tests::{p, rect, Scene};
use super::*;
use crate::common::Image;

const GW: u32 = 260;
const GH: u32 = 150;

/// Small deterministic base: a checkerboard, a blue block and a line of text.
fn golden_base() -> Image {
    let mut img = Image::filled(GW, GH, [0xF4, 0xF1, 0xEE, 0xFF]);
    for y in 0..GH {
        for x in 0..GW {
            if (x / 20 + y / 20) % 2 == 0 {
                img.set_pixel(x, y, [0xE6, 0xE2, 0xDE, 0xFF]);
            }
        }
    }
    let mut s = Scene::new();
    s.add(
        Kind::Shape(Shape { kind: ShapeKind::Rect, rect: rect(150.0, 20.0, 90.0, 60.0), ..Default::default() }),
        Style { fill: Color::rgb(0x42, 0x85, 0xF4), width: 0.0, ..Style::default() },
    );
    s.text(10.0, 100.0, 240.0, "Golden sample text 123", 12.0, Color::rgb(0x22, 0x22, 0x22), false);
    render(&img, &s.objects, &RenderOptions::default())
}

fn png_of(img: &Image) -> Vec<u8> {
    crate::output::save::encode(img, crate::settings::ImageFormat::Png, 100).expect("png")
}

pub(super) fn golden_scenes() -> Vec<(&'static str, Vec<Object>)> {
    let mut out = Vec::new();

    let mut s = Scene::new();
    s.text(10.0, 8.0, 200.0, "Plain bold", 16.0, Color::RED, true);
    let t = s.text(10.0, 40.0, 200.0, "Outline", 18.0, Color::WHITE, true);
    s.style(t).outline = Some(Outline { color: Color::BLACK, width: 2.0 });
    let mut st = Style { fill: Color::WHITE, color: Color::BLUE, width: 2.0, ..Style::default() };
    st.font.size = 11.0;
    st.shadow = Some(Shadow::default());
    s.add(
        Kind::Text(Text {
            rect: rect(120.0, 10.0, 120.0, 30.0),
            text: "Callout wraps to lines".into(),
            boxed: TextBox::Rounded,
            tail: Some(Tail { tip: p(110.0, 120.0), width: 18.0 }),
            ..Default::default()
        }),
        st.clone(),
    );
    st.shadow = None;
    s.add(
        Kind::Text(Text {
            rect: rect(150.0, 90.0, 90.0, 30.0),
            text: "Oval".into(),
            boxed: TextBox::Oval,
            ..Default::default()
        }),
        st,
    );
    out.push(("text_boxes", s.objects));

    let mut s = Scene::new();
    let mut st = Style::default();
    st.font.size = 14.0;
    st.font.color = Color::PURPLE;
    st.font.align = TextAlign::Center;
    s.add(
        Kind::Text(Text {
            rect: rect(10.0, 10.0, 240.0, 70.0),
            text: "Arc text sample".into(),
            flow: TextFlow::Arc { bend: 0.7 },
            ..Default::default()
        }),
        st.clone(),
    );
    st.font.align = TextAlign::Left;
    s.add(
        Kind::Text(Text {
            text: "along a wavy path".into(),
            flow: TextFlow::Path {
                points: vec![p(10.0, 130.0), p(80.0, 90.0), p(160.0, 140.0), p(250.0, 100.0)],
                smooth: true,
            },
            ..Default::default()
        }),
        st,
    );
    out.push(("text_flow", s.objects));

    for (name, decors) in [("lines_decor_a", &LineDecor::ALL[0..6]), ("lines_decor_b", &LineDecor::ALL[6..])] {
        let mut s = Scene::new();
        for (i, d) in decors.iter().enumerate() {
            let y = 15.0 + i as f32 * 23.0;
            let id = s.line(LineKind::Straight, &[p(15.0, y), p(245.0, y + 8.0)], Color::rgb(0xE5, 0x39, 0x35), 5.0);
            let l = s.line_mut(id);
            l.decor = *d;
            l.color2 = Color::BLUE;
        }
        out.push((name, s.objects));
    }

    let mut s = Scene::new();
    for (i, h) in ArrowHead::ALL[1..].iter().enumerate() {
        let x = 15.0 + (i % 4) as f32 * 62.0;
        let y = 20.0 + (i / 4) as f32 * 65.0;
        let id = s.line(LineKind::Straight, &[p(x, y + 45.0), p(x + 45.0, y)], Color::rgb(0x1E, 0x88, 0xE5), 3.0);
        s.line_mut(id).end = *h;
        if i == 0 {
            s.line_mut(id).start = ArrowHead::Circle;
        }
    }
    out.push(("arrowheads", s.objects));

    let mut s = Scene::new();
    let a = s.line(LineKind::Quad, &[p(10.0, 60.0), p(50.0, 0.0), p(90.0, 60.0)], Color::GREEN, 4.0);
    s.style(a).outline = Some(Outline { color: Color::BLACK, width: 1.5 });
    s.line(LineKind::Cubic, &[p(100.0, 60.0), p(110.0, 0.0), p(170.0, 80.0), p(180.0, 10.0)], Color::PURPLE, 4.0);
    let e = s.line(LineKind::ElbowH, &[p(190.0, 10.0), p(250.0, 60.0)], Color::ORANGE, 4.0);
    s.line_mut(e).end = ArrowHead::Triangle;
    s.line(LineKind::Smooth, &[p(10.0, 80.0), p(40.0, 70.0), p(60.0, 95.0), p(90.0, 75.0)], Color::RED, 3.0);
    for (i, d) in Dash::ALL.iter().enumerate() {
        let y = 80.0 + i as f32 * 14.0;
        let id = s.line(LineKind::Straight, &[p(110.0, y), p(250.0, y)], Color::BLACK, 2.5);
        s.style(id).dash = *d;
    }
    out.push(("line_kinds", s.objects));

    let mut s = Scene::new();
    let shadow = Some(Shadow { dx: 2.0, dy: 3.0, blur: 3.0, color: Color::rgba(0, 0, 0, 90) });
    s.add(
        Kind::Shape(Shape {
            kind: ShapeKind::Rect,
            rect: rect(10.0, 10.0, 70.0, 50.0),
            radius: 10.0,
            ..Default::default()
        }),
        Style { fill: Color::YELLOW, color: Color::RED, width: 3.0, shadow, ..Style::default() },
    );
    s.add(
        Kind::Shape(Shape { kind: ShapeKind::Ellipse, rect: rect(95.0, 10.0, 70.0, 50.0), ..Default::default() }),
        Style {
            fill: Color::BLUE.with_alpha(120),
            color: Color::BLUE,
            width: 2.0,
            dash: Dash::Dash,
            opacity: 0.8,
            ..Style::default()
        },
    );
    s.add(
        Kind::Shape(Shape {
            kind: ShapeKind::Polygon,
            points: vec![p(200.0, 10.0), p(245.0, 60.0), p(180.0, 55.0)],
            ..Default::default()
        }),
        Style { fill: Color::GREEN, color: Color::BLACK, width: 2.0, ..Style::default() },
    );
    let brackets = [
        (ShapeKind::CurlyBracket, Side::Left, rect(20.0, 75.0, 25.0, 65.0)),
        (ShapeKind::CurlyBracket, Side::Bottom, rect(70.0, 90.0, 90.0, 25.0)),
        (ShapeKind::SquareBracket, Side::Right, rect(220.0, 75.0, 25.0, 65.0)),
    ];
    for (k, side, r) in brackets {
        s.add(
            Kind::Shape(Shape { kind: k, rect: r, facing: side, ..Default::default() }),
            Style { fill: Color::PURPLE.with_alpha(50), color: Color::PURPLE, width: 3.0, ..Style::default() },
        );
    }
    out.push(("shapes", s.objects));

    let mut s = Scene::new();
    for (i, shape) in [HighlightShape::Rect, HighlightShape::Rounded, HighlightShape::Oval].iter().enumerate() {
        s.add(
            Kind::Highlight(Highlight {
                shape: *shape,
                rect: rect(10.0 + i as f32 * 80.0, 15.0, 70.0, 40.0),
                indent: if i == 1 { 4.0 } else { 0.0 },
                ..Default::default()
            }),
            Style {
                fill: Color::YELLOW,
                color: Color::ORANGE,
                width: if i == 1 { 2.0 } else { 0.0 },
                ..Style::default()
            },
        );
    }
    s.add(
        Kind::Highlight(Highlight {
            shape: HighlightShape::Line,
            points: vec![p(10.0, 106.0), p(240.0, 106.0)],
            ..Default::default()
        }),
        Style { fill: Color::rgb(0x80, 0xDE, 0xEA), width: 14.0, ..Style::default() },
    );
    out.push(("highlight", s.objects));

    let mut s = Scene::new();
    s.add(
        Kind::Highlight(Highlight {
            shape: HighlightShape::Rounded,
            rect: rect(140.0, 15.0, 110.0, 70.0),
            blur_outside: 3.0,
            dim_outside: 0.35,
            ..Default::default()
        }),
        Style { fill: Color::WHITE, color: Color::YELLOW, width: 2.0, ..Style::default() },
    );
    out.push(("highlight_outside", s.objects));

    let mut s = Scene::new();
    s.add(
        Kind::Magnifier(Magnifier {
            source: rect(140.0, 10.0, 30.0, 30.0),
            target: rect(20.0, 10.0, 90.0, 90.0),
            shape: MagnifierShape::Circle,
            connector: true,
        }),
        default_style(Tool::Magnifier),
    );
    let mut st = default_style(Tool::Magnifier);
    st.color = Color::BLACK;
    s.add(
        Kind::Magnifier(Magnifier {
            source: rect(10.0, 98.0, 60.0, 16.0),
            target: rect(130.0, 95.0, 120.0, 40.0),
            shape: MagnifierShape::Rect,
            connector: true,
        }),
        st,
    );
    out.push(("magnifier", s.objects));

    let mut s = Scene::new();
    let vals = [
        (StepValue::Number(1), StepShape::Circle),
        (StepValue::Number(12), StepShape::Circle),
        (StepValue::Number(999), StepShape::Square),
        (StepValue::Letter(0), StepShape::Square),
        (StepValue::Letter(26), StepShape::Teardrop),
    ];
    let fills = [Color::RED, Color::BLUE, Color::GREEN, Color::PURPLE, Color::ORANGE];
    for (i, (v, sh)) in vals.iter().enumerate() {
        let mut st = default_style(Tool::Step);
        st.fill = fills[i];
        let step = Step { center: p(30.0 + i as f32 * 50.0, 45.0), size: 32.0, value: *v, shape: *sh, angle: 135.0 };
        s.add(Kind::Step(step), st);
    }
    out.push(("steps", s.objects));

    let mut s = Scene::new();
    for (i, e) in ["\u{1F600}", "\u{1F44D}", "\u{2B50}", "\u{1F680}"].iter().enumerate() {
        let mut f = Frame::new(rect(10.0 + i as f32 * 62.0, 20.0, 52.0, 52.0));
        f.rotation = if i == 3 { 30.0 } else { 0.0 };
        s.add(Kind::Emoji(Emoji { frame: f, text: (*e).into() }), Style::default());
    }
    out.push(("emoji", s.objects));

    let mut s = Scene::new();
    let colors = [Color::BLUE, Color::RED, Color::GREEN, Color::ORANGE, Color::PURPLE, Color::rgb(0, 0xAC, 0xC1)];
    for (i, sh) in StickerShape::ALL.iter().enumerate() {
        let (w, h) = render::sticker_size(*sh);
        let r = rect(8.0 + (i % 3) as f32 * 85.0, 6.0 + (i / 3) as f32 * 72.0, w * 0.6, h * 0.6);
        let mut st = default_style(Tool::Sticker);
        st.fill = colors[i];
        s.add(Kind::Sticker(Sticker { frame: Frame::new(r), shape: *sh }), st);
    }
    out.push(("stickers", s.objects));

    let mut s = Scene::new();
    for (i, c) in CursorShape::ALL.iter().enumerate() {
        let (w, h) = render::cursor_size(*c);
        let k = 44.0 / h;
        let mut f = Frame::new(rect(12.0 + i as f32 * 50.0, 20.0, w * k, h * k));
        f.flip_h = i == 4;
        s.add(Kind::Cursor(Cursor { frame: f, shape: *c }), default_style(Tool::Cursor));
    }
    out.push(("cursors", s.objects));

    let mut s = Scene::new();
    let mut thumb = Image::new(16, 10);
    for y in 0..10u32 {
        for x in 0..16u32 {
            thumb.set_pixel(x, y, [(x * 15) as u8, (y * 25) as u8, 200, 255]);
        }
    }
    let mut o = tools::create_picture(1, png_of(&thumb), (16, 10), p(20.0, 20.0), None);
    if let Kind::Image(im) = &mut o.kind {
        im.frame.rect = rect(20.0, 20.0, 120.0, 75.0);
        im.frame.rotation = 15.0;
        im.frame.flip_v = true;
    }
    s.objects.push(o);
    out.push(("image", s.objects));

    let mut s = Scene::new();
    s.add(
        Kind::Blur(Blur { rect: rect(5.0, 95.0, 120.0, 25.0), ellipse: false, mode: BlurMode::Blur, strength: 3.0 }),
        Style::default(),
    );
    s.add(
        Kind::Blur(Blur {
            rect: rect(130.0, 95.0, 120.0, 25.0),
            ellipse: true,
            mode: BlurMode::Pixelate,
            strength: 6.0,
        }),
        Style::default(),
    );
    s.add(
        Kind::Eraser(Eraser { points: vec![p(10.0, 20.0), p(60.0, 60.0)], radius: 8.0, background: None }),
        Style::default(),
    );
    s.add(
        Kind::Fill(Fill { at: p(200.0, 50.0), tolerance: 10, transparent: false }),
        Style { fill: Color::ORANGE, ..Style::default() },
    );
    s.add(
        Kind::Pencil(Pencil { points: vec![p(80.0, 20.0), p(100.0, 60.0), p(130.0, 25.0)] }),
        Style { color: Color::RED, width: 3.0, ..Style::default() },
    );
    out.push(("raster", s.objects));

    out
}

/// Count pixels whose largest channel difference exceeds `tol`; also return the max difference.
fn diff(a: &Image, b: &Image, tol: u8) -> (usize, u8) {
    let mut bad = 0;
    let mut max = 0u8;
    for (pa, pb) in a.data.chunks_exact(4).zip(b.data.chunks_exact(4)) {
        let d = pa.iter().zip(pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        max = max.max(d);
        if d > tol {
            bad += 1;
        }
    }
    (bad, max)
}

#[test]
fn golden_images() {
    let update = std::env::var("SNAPFORGE_UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("src").join("draw").join("golden");
    let actual_dir = root.join("target").join("golden-actual");
    let base = golden_base();
    let mut failures = Vec::new();
    for (name, objects) in golden_scenes() {
        let out = render(&base, &objects, &RenderOptions::default());
        let path = dir.join(format!("{name}.png"));
        let png = png_of(&out);
        if update {
            std::fs::create_dir_all(&dir).expect("golden dir");
            std::fs::write(&path, &png).expect("write golden");
            continue;
        }
        let Ok(expected) = crate::output::save::load_image(&path) else {
            failures.push(format!("{name}: missing {} (run with SNAPFORGE_UPDATE_GOLDEN=1)", path.display()));
            continue;
        };
        // Compare after the same PNG round trip the reference went through.
        let actual = crate::output::save::decode(&png, "png").expect("decode");
        if (expected.width, expected.height) != (actual.width, actual.height) {
            failures.push(format!(
                "{name}: size {}x{} != {}x{}",
                actual.width, actual.height, expected.width, expected.height
            ));
            continue;
        }
        let (bad, max) = diff(&actual, &expected, 12);
        // Allow a few edge pixels to differ (font or rasterizer updates).
        if bad > actual.len_px() / 500 {
            std::fs::create_dir_all(&actual_dir).ok();
            std::fs::write(actual_dir.join(format!("{name}.png")), &png).ok();
            failures.push(format!("{name}: {bad} pixels differ (max {max}); actual saved to target/golden-actual"));
        }
    }
    assert!(failures.is_empty(), "golden mismatches:\n{}", failures.join("\n"));
}

#[test]
fn golden_scenes_cover_every_kind() {
    let names: std::collections::HashSet<&str> =
        golden_scenes().iter().flat_map(|(_, objs)| objs.iter().map(|o| o.kind.name())).collect();
    for k in [
        "text",
        "line",
        "shape",
        "highlight",
        "magnifier",
        "step",
        "emoji",
        "sticker",
        "image",
        "cursor",
        "blur",
        "eraser",
        "fill",
        "pencil",
    ] {
        assert!(names.contains(k), "no golden scene draws a {k}");
    }
}

/// The live view (effective base + transparent overlay) must equal the exported image.
#[test]
fn overlay_composited_matches_export() {
    let base = golden_base();
    for (name, objects) in golden_scenes() {
        let opts = RenderOptions::default();
        let export = render(&base, &objects, &opts);
        let mut cache = RasterCache::new();
        let eff = cache.get(&base, 1, &objects, &opts).clone();
        let layer = render_overlay(&eff, &objects, &View::identity(GW, GH), &opts);
        let mut composed = eff.clone();
        composed.draw_over(&layer, 0, 0);
        let (bad, max) = diff(&composed, &export, 3);
        assert!(bad <= export.len_px() / 1000, "{name}: overlay differs from export in {bad} px (max {max})");
    }
}

#[test]
fn overlay_zoom_and_viewport() {
    let base = golden_base();
    let objects: Vec<Object> = golden_scenes()
        .into_iter()
        .flat_map(|(_, o)| o)
        .enumerate()
        .map(|(i, mut o)| {
            o.id = i as u64 + 1;
            o
        })
        .collect();
    let view = View { x: 50.0, y: 20.0, zoom: 2.5, width: 300, height: 200 };
    let layer = render_overlay(&base, &objects, &view, &RenderOptions::default());
    assert_eq!((layer.width, layer.height), (300, 200));
    assert!(layer.data.chunks_exact(4).any(|px| px[3] > 0));
    assert_eq!(view.to_image(0.0, 0.0), p(50.0, 20.0));
    assert_eq!(view.to_view(p(60.0, 30.0)), (25.0, 25.0));
    // A single object rendered on its own canvas.
    let (img, origin) = render_object(&base, &objects[0], 2.0).expect("object image");
    assert!(img.width > 0 && origin.x.is_finite());
}

#[test]
fn timing_and_hidden_filter_objects() {
    let base = golden_base();
    let mut s = Scene::new();
    let id = s.add(
        Kind::Shape(Shape { kind: ShapeKind::Rect, rect: rect(0.0, 0.0, 50.0, 50.0), ..Default::default() }),
        Style { fill: Color::RED, width: 0.0, ..Style::default() },
    );
    s.objects[0].timing = Some(Timing { start_ms: 1000, end_ms: 2000 });
    let at = |t: Option<u64>, hidden: Vec<ObjectId>| {
        render(&base, &s.objects, &RenderOptions { time_ms: t, hidden }).pixel(10, 10)
    };
    let red = [0x35, 0x39, 0xE5, 0xFF];
    assert_eq!(at(None, vec![]), red);
    assert_eq!(at(Some(1500), vec![]), red);
    assert_ne!(at(Some(2500), vec![]), red);
    assert_ne!(at(None, vec![id]), red);
}
