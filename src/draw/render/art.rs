//! Built-in vector art (3D block arrows, mouse pointers) and inserted images.

use super::{fill_and_border_union, frame_transform, paint_color, paint_shader, stroke_of, Ctx};
use crate::draw::model::*;
use crate::draw::path::{self, sk};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tiny_skia::{
    FillRule, FilterQuality, GradientStop, LinearGradient, Path, PathBuilder, Pixmap, PixmapPaint, SpreadMode,
    Transform,
};

// ----- 3D block arrows -----

/// Natural content size of a sticker (its frame keeps this aspect when created).
pub fn sticker_size(shape: StickerShape) -> (f32, f32) {
    match shape {
        StickerShape::Arrow | StickerShape::DoubleArrow => (100.0, 60.0),
        StickerShape::CurvedArrow => (100.0, 72.0),
        StickerShape::UTurnArrow | StickerShape::CornerArrow | StickerShape::CircularArrow => (100.0, 100.0),
    }
}

/// Block arrow outline around a centre line: (shaft width, head width, head length, both ends).
fn block_arrow(center: &[Pt], shaft: f32, head_w: f32, head_len: f32, double: bool) -> Option<Path> {
    let total = path::length(center);
    if total < head_len * 1.2 {
        return None;
    }
    let s0 = if double { head_len } else { 0.0 };
    let body = path::sub_polyline(center, s0, total - head_len);
    if body.len() < 2 {
        return None;
    }
    let left = path::offset(&body, -shaft / 2.0);
    let right = path::offset(&body, shaft / 2.0);
    let mut poly: Vec<Pt> = left.clone();
    let end_tip = center[center.len() - 1];
    let end_base = body[body.len() - 1];
    let end_dir = (end_tip - end_base).norm();
    let n = end_dir.perp();
    poly.push(end_base - n * (head_w / 2.0));
    poly.push(end_tip);
    poly.push(end_base + n * (head_w / 2.0));
    poly.extend(right.iter().rev());
    if double {
        let tip = center[0];
        let base = body[0];
        let d = (tip - base).norm();
        let n = d.perp();
        poly.push(base - n * (head_w / 2.0));
        poly.push(tip);
        poly.push(base + n * (head_w / 2.0));
    }
    path::polyline_path(&poly, true)
}

fn sticker_path(shape: StickerShape) -> Option<Path> {
    let tol = 0.5;
    match shape {
        StickerShape::Arrow => block_arrow(&[Pt::new(4.0, 30.0), Pt::new(96.0, 30.0)], 26.0, 56.0, 38.0, false),
        StickerShape::DoubleArrow => block_arrow(&[Pt::new(4.0, 30.0), Pt::new(96.0, 30.0)], 24.0, 54.0, 30.0, true),
        StickerShape::CurvedArrow => {
            let mut c = vec![Pt::new(6.0, 66.0)];
            path::flatten_quad(&mut c, Pt::new(6.0, 66.0), Pt::new(30.0, 6.0), Pt::new(96.0, 26.0), tol);
            block_arrow(&c, 20.0, 48.0, 30.0, false)
        }
        StickerShape::UTurnArrow => {
            let mut c = vec![Pt::new(76.0, 96.0), Pt::new(76.0, 44.0)];
            let mut pb_pts = Vec::new();
            for i in 1..=32 {
                let a = std::f32::consts::PI * i as f32 / 32.0;
                pb_pts.push(Pt::new(50.0 + 26.0 * a.cos(), 44.0 - 26.0 * a.sin()));
            }
            c.extend(pb_pts);
            c.push(Pt::new(24.0, 96.0));
            block_arrow(&c, 20.0, 46.0, 28.0, false)
        }
        StickerShape::CornerArrow => {
            let mut c = vec![Pt::new(18.0, 96.0), Pt::new(18.0, 44.0)];
            for i in 1..=16 {
                let a = std::f32::consts::FRAC_PI_2 * i as f32 / 16.0;
                c.push(Pt::new(44.0 - 26.0 * a.cos(), 44.0 - 26.0 * a.sin()));
            }
            c.push(Pt::new(97.0, 18.0));
            block_arrow(&c, 22.0, 50.0, 30.0, false)
        }
        StickerShape::CircularArrow => {
            let c: Vec<Pt> = (0..=64)
                .map(|i| {
                    let a = (-80.0f32 + 290.0 * i as f32 / 64.0).to_radians();
                    Pt::new(50.0 + 34.0 * a.cos(), 52.0 + 34.0 * a.sin())
                })
                .collect();
            block_arrow(&c, 16.0, 38.0, 22.0, false)
        }
    }
}

pub(super) fn draw_sticker(pm: &mut Pixmap, s: &Sticker, st: &Style, ctx: &Ctx) {
    let Some(p) = sticker_path(s.shape) else { return };
    let (cw, ch) = sticker_size(s.shape);
    let fts = frame_transform(&s.frame, cw, ch);
    let ts = ctx.ts.pre_concat(fts);
    let base = if st.fill.is_visible() { st.fill } else { Color::BLUE };
    // Extruded side: stacked copies shifted down-right in screen space.
    let depth = 7.0;
    let side = paint_color(base.darken(0.5));
    for i in (1..=depth as i32).rev() {
        let k = i as f32 / depth;
        let sc = frame_scale(&fts);
        let shift = Transform::from_translate(k * 2.5 * sc, k * depth * sc);
        pm.fill_path(&p, &side, FillRule::Winding, ctx.ts.pre_concat(shift).pre_concat(fts), None);
    }
    let b = p.bounds();
    let face =
        vertical_gradient(b.top(), b.bottom(), &[(0.0, base.lighten(0.35)), (0.55, base), (1.0, base.darken(0.18))]);
    pm.fill_path(&p, &face, FillRule::Winding, ts, None);
    // Gloss on the upper half.
    let gloss = vertical_gradient(
        b.top(),
        b.top() + b.height() * 0.6,
        &[(0.0, Color::WHITE.with_alpha(120)), (1.0, Color::WHITE.with_alpha(0))],
    );
    pm.fill_path(&p, &gloss, FillRule::Winding, ts, None);
    pm.stroke_path(&p, &paint_color(base.darken(0.42)), &stroke_of(1.6, Dash::Solid), ts, None);
}

/// Approximate uniform scale of a transform (content units → image pixels).
fn frame_scale(ts: &Transform) -> f32 {
    let sx = (ts.sx * ts.sx + ts.ky * ts.ky).sqrt();
    let sy = (ts.kx * ts.kx + ts.sy * ts.sy).sqrt();
    (sx * sy).sqrt()
}

fn vertical_gradient(y0: f32, y1: f32, stops: &[(f32, Color)]) -> tiny_skia::Paint<'static> {
    let fallback = stops.first().map(|s| s.1).unwrap_or(Color::BLUE);
    let stops: Vec<GradientStop> = stops.iter().map(|(t, c)| GradientStop::new(*t, c.to_skia())).collect();
    match LinearGradient::new(
        sk(Pt::new(0.0, y0)),
        sk(Pt::new(0.0, y1.max(y0 + 0.01))),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        Some(sh) => paint_shader(sh),
        None => paint_color(fallback),
    }
}

// ----- Mouse pointers -----

/// Natural content size of a cursor icon.
pub fn cursor_size(shape: CursorShape) -> (f32, f32) {
    match shape {
        CursorShape::Arrow => (13.0, 20.0),
        CursorShape::ArrowClick => (22.0, 26.0),
        CursorShape::LeftClick | CursorShape::RightClick => (30.0, 40.0),
        CursorShape::Hand => (24.0, 27.0),
    }
}

fn arrow_points(ox: f32, oy: f32) -> Vec<Pt> {
    [(0.0, 0.0), (0.0, 16.6), (4.0, 12.9), (6.9, 19.2), (9.4, 18.1), (6.6, 12.0), (11.8, 12.0)]
        .iter()
        .map(|&(x, y)| Pt::new(ox + x, oy + y))
        .collect()
}

pub(super) fn draw_cursor(pm: &mut Pixmap, c: &Cursor, st: &Style, ctx: &Ctx) {
    let (cw, ch) = cursor_size(c.shape);
    let ts = ctx.ts.pre_concat(frame_transform(&c.frame, cw, ch));
    let accent = if st.fill.is_visible() { st.fill } else { Color::ORANGE };
    let ink = Color::rgb(0x12, 0x12, 0x12);
    match c.shape {
        CursorShape::Arrow | CursorShape::ArrowClick => {
            let (ox, oy) = if c.shape == CursorShape::ArrowClick { (8.0, 6.0) } else { (0.6, 0.6) };
            if c.shape == CursorShape::ArrowClick {
                // Click ripples around the tip.
                for r in [3.2f32, 6.2] {
                    if let Some(p) = PathBuilder::from_circle(ox, oy, r) {
                        let a = if r < 4.0 { 200 } else { 110 };
                        pm.stroke_path(&p, &paint_color(accent.with_alpha(a)), &stroke_of(1.3, Dash::Solid), ts, None);
                    }
                }
                for k in 0..3 {
                    let a = (200.0f32 + 40.0 * k as f32).to_radians();
                    let (s, co) = a.sin_cos();
                    let p0 = Pt::new(ox + 8.5 * co, oy + 8.5 * s);
                    let p1 = Pt::new(ox + 12.0 * co, oy + 12.0 * s);
                    if let Some(p) = path::polyline_path(&[p0, p1], false) {
                        pm.stroke_path(&p, &paint_color(accent), &stroke_of(1.6, Dash::Solid), ts, None);
                    }
                }
            }
            if let Some(p) = path::polyline_path(&arrow_points(ox, oy), true) {
                pm.fill_path(&p, &paint_color(Color::WHITE), FillRule::Winding, ts, None);
                let mut s = stroke_of(1.1, Dash::Solid);
                s.line_join = tiny_skia::LineJoin::Miter;
                pm.stroke_path(&p, &paint_color(ink), &s, ts, None);
            }
        }
        CursorShape::LeftClick | CursorShape::RightClick => {
            let left = c.shape == CursorShape::LeftClick;
            let body = RectF::new(5.0, 6.0, 20.0, 31.0);
            let mut pb = PathBuilder::new();
            path::push_round_rect(&mut pb, body, 10.0);
            let Some(shell) = pb.finish() else { return };
            let shade =
                vertical_gradient(body.y, body.bottom(), &[(0.0, Color::WHITE), (1.0, Color::rgb(0xD9, 0xDE, 0xE5))]);
            pm.fill_path(&shell, &shade, FillRule::Winding, ts, None);
            // Highlighted button: the matching quarter of the shell above the split line.
            let split_y = body.y + 13.0;
            let mut clip = PathBuilder::new();
            let half = if left {
                RectF::new(body.x, body.y, body.w / 2.0, split_y - body.y)
            } else {
                RectF::new(body.x + body.w / 2.0, body.y, body.w / 2.0, split_y - body.y)
            };
            if let Some(r) = tiny_skia::Rect::from_xywh(half.x, half.y, half.w, half.h) {
                clip.push_rect(r);
            }
            if let (Some(clip), Some(mut mask)) = (clip.finish(), tiny_skia::Mask::new(pm.width(), pm.height())) {
                mask.fill_path(&clip, FillRule::Winding, true, ts);
                pm.fill_path(&shell, &paint_color(accent), FillRule::Winding, ts, Some(&mask));
            }
            let line = paint_color(Color::rgb(0x5A, 0x63, 0x70));
            let thin = stroke_of(0.9, Dash::Solid);
            if let Some(p) = path::polyline_path(&[Pt::new(body.x, split_y), Pt::new(body.right(), split_y)], false) {
                pm.stroke_path(&p, &line, &thin, ts, None);
            }
            let cx = body.center().x;
            if let Some(p) = path::polyline_path(&[Pt::new(cx, body.y), Pt::new(cx, split_y)], false) {
                pm.stroke_path(&p, &line, &thin, ts, None);
            }
            if let Some(wheel) = path::round_rect_path(RectF::new(cx - 1.6, body.y + 4.0, 3.2, 6.0), 1.6) {
                pm.fill_path(&wheel, &paint_color(Color::rgb(0x5A, 0x63, 0x70)), FillRule::Winding, ts, None);
            }
            pm.stroke_path(&shell, &paint_color(ink), &stroke_of(1.2, Dash::Solid), ts, None);
            // Click marks beside the highlighted button.
            let (x0, dir) = if left { (body.x - 1.0, -1.0) } else { (body.right() + 1.0, 1.0) };
            for (dy, len) in [(-3.0f32, 3.5f32), (2.0, 4.0), (7.0, 3.5)] {
                let a = Pt::new(x0, body.y + 6.0 + dy);
                let b = Pt::new(x0 + dir * len, body.y + 6.0 + dy * 1.5);
                if let Some(p) = path::polyline_path(&[a, b], false) {
                    pm.stroke_path(&p, &paint_color(accent), &stroke_of(1.4, Dash::Solid), ts, None);
                }
            }
        }
        CursorShape::Hand => {
            let parts = hand_parts();
            fill_and_border_union(pm, &parts, Color::WHITE, Some((ink, 1.1, Dash::Solid)), ts);
            // Finger creases.
            let crease = paint_color(Color::rgb(0x70, 0x70, 0x70));
            for x in [12.5f32, 16.5, 20.2] {
                if let Some(p) = path::polyline_path(&[Pt::new(x, 13.5), Pt::new(x, 16.5)], false) {
                    pm.stroke_path(&p, &crease, &stroke_of(0.7, Dash::Solid), ts, None);
                }
            }
        }
    }
}

fn hand_parts() -> Vec<Path> {
    let mut out = Vec::new();
    let rr = |x: f32, y: f32, w: f32, h: f32, r: f32| path::round_rect_path(RectF::new(x, y, w, h), r);
    out.extend(rr(8.0, 0.5, 4.6, 16.0, 2.3)); // index finger
    out.extend(rr(12.4, 8.5, 4.3, 9.0, 2.1)); // middle
    out.extend(rr(16.4, 9.3, 4.1, 8.5, 2.0)); // ring
    out.extend(rr(20.0, 10.6, 3.6, 7.5, 1.8)); // little
    out.extend(rr(8.0, 12.5, 15.6, 14.0, 5.0)); // palm
                                                // Thumb: a capsule from the palm out to the left.
    if let Some(line) = path::polyline_path(&[Pt::new(10.0, 20.0), Pt::new(3.0, 13.6)], false) {
        if let Some(cap) = line.stroke(&stroke_of(4.6, Dash::Solid), 1.0) {
            out.push(cap);
        }
    }
    out
}

// ----- Inserted images -----

type DecodedCache = HashMap<usize, (ImageData, Option<Rc<Pixmap>>)>;

thread_local! {
    /// Decoded images keyed by the address of their shared bytes (the Arc is kept alive).
    static DECODED: RefCell<DecodedCache> = RefCell::new(HashMap::new());
}

/// Decoded pixels of an image object (cached).
pub(crate) fn decoded(data: &ImageData) -> Option<Rc<Pixmap>> {
    let key = data.0.as_ptr() as usize;
    if let Some(hit) = DECODED.with(|c| c.borrow().get(&key).map(|(_, p)| p.clone())) {
        return hit;
    }
    let pm = crate::output::save::decode(&data.0, "png").ok().and_then(|img| super::image_to_pixmap(&img)).map(Rc::new);
    DECODED.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 64 {
            c.clear();
        }
        c.insert(key, (data.clone(), pm.clone()));
    });
    pm
}

pub(super) fn draw_picture(pm: &mut Pixmap, p: &Picture, ctx: &Ctx) {
    match decoded(&p.png) {
        Some(img) => {
            let ts = ctx.ts.pre_concat(frame_transform(&p.frame, img.width() as f32, img.height() as f32));
            let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
            pm.draw_pixmap(0, 0, img.as_ref().as_ref(), &paint, ts, None);
        }
        None => {
            // Placeholder for undecodable data: a grey box with a cross.
            let ts = ctx.ts.pre_concat(frame_transform(&p.frame, 100.0, 100.0));
            let r = RectF::new(0.0, 0.0, 100.0, 100.0);
            if let Some(b) = path::round_rect_path(r, 0.0) {
                pm.fill_path(&b, &paint_color(Color::rgb(0xE0, 0xE0, 0xE0)), FillRule::Winding, ts, None);
            }
            if let Some(x) = path::polyline_path(&[r.tl(), r.br()], false) {
                pm.stroke_path(&x, &paint_color(Color::rgb(0x90, 0x90, 0x90)), &stroke_of(2.0, Dash::Solid), ts, None);
            }
        }
    }
}
