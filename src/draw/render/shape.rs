//! Shapes, brackets, highlighters, magnifier callouts and step stamps.

use super::text::{fit_label, LabelFit};
use super::{
    composite_backdrop, device_rect, fill_and_border_union, invert_mask, paint_color, paint_shader, stroke_of, Ctx,
};
use crate::draw::fx;
use crate::draw::model::*;
use crate::draw::path::{self, sk};
use tiny_skia::{
    BlendMode, FillRule, FilterQuality, GradientStop, IntRect, LinearGradient, Mask, Path, PathBuilder, Pattern,
    Pixmap, PixmapPaint, SpreadMode, Transform,
};

// ----- Outline paths (shared with hit-testing) -----

/// Closed outline of a shape (brackets: the bracket stroke path, closed when `closed`).
pub(crate) fn shape_path(s: &Shape, closed: bool) -> Option<Path> {
    match s.kind {
        ShapeKind::Rect => path::round_rect_path(s.rect, s.radius),
        ShapeKind::Ellipse => path::ellipse_path(s.rect),
        ShapeKind::Polygon => path::polyline_path(&s.points, true),
        ShapeKind::CurlyBracket | ShapeKind::SquareBracket => bracket_path(s, closed),
    }
}

/// Bracket inside `s.rect`, its point facing `s.facing`. Built in a canonical frame where `u`
/// runs from the point (0) to the arms (`depth`) and `v` along the bracket (0..`len`).
fn bracket_path(s: &Shape, closed: bool) -> Option<Path> {
    let r = s.rect;
    let (depth, len) = match s.facing {
        Side::Left | Side::Right => (r.w, r.h),
        Side::Top | Side::Bottom => (r.h, r.w),
    };
    let map = |u: f32, v: f32| -> (f32, f32) {
        match s.facing {
            Side::Left => (r.x + u, r.y + v),
            Side::Right => (r.right() - u, r.y + v),
            Side::Top => (r.x + v, r.y + u),
            Side::Bottom => (r.x + v, r.bottom() - u),
        }
    };
    let mut pb = PathBuilder::new();
    let mv = |pb: &mut PathBuilder, u: f32, v: f32| {
        let (x, y) = map(u, v);
        pb.move_to(x, y);
    };
    let ln = |pb: &mut PathBuilder, u: f32, v: f32| {
        let (x, y) = map(u, v);
        pb.line_to(x, y);
    };
    let qd = |pb: &mut PathBuilder, cu: f32, cv: f32, u: f32, v: f32| {
        let (cx, cy) = map(cu, cv);
        let (x, y) = map(u, v);
        pb.quad_to(cx, cy, x, y);
    };
    match s.kind {
        ShapeKind::SquareBracket => {
            mv(&mut pb, depth, 0.0);
            ln(&mut pb, 0.0, 0.0);
            ln(&mut pb, 0.0, len);
            ln(&mut pb, depth, len);
        }
        _ => {
            let m = depth / 2.0;
            let rr = (len / 4.0).min(depth * 1.2).max(0.0);
            let c = len / 2.0;
            mv(&mut pb, depth, 0.0);
            qd(&mut pb, m, 0.0, m, rr.min(c));
            ln(&mut pb, m, (c - rr).max(rr.min(c)));
            qd(&mut pb, m, c, 0.0, c);
            qd(&mut pb, m, c, m, (c + rr).min(len - rr.min(c)));
            ln(&mut pb, m, len - rr.min(c));
            qd(&mut pb, m, len, depth, len);
        }
    }
    if closed {
        pb.close();
    }
    pb.finish()
}

pub(crate) fn highlight_path(h: &Highlight, inset: f32, tol: f32) -> Option<Path> {
    let r = h.rect.inflate(-inset.max(0.0));
    match h.shape {
        HighlightShape::Rect => path::round_rect_path(r, 0.0),
        HighlightShape::Rounded => path::round_rect_path(r, h.radius),
        HighlightShape::Oval => path::ellipse_path(r),
        HighlightShape::Line => path::polyline_path(&path::flatten_smooth(&h.points, tol), false),
    }
}

pub(crate) fn magnifier_path(r: RectF, shape: MagnifierShape) -> Option<Path> {
    match shape {
        MagnifierShape::Circle => path::ellipse_path(r),
        MagnifierShape::Rect => path::round_rect_path(r, 0.0),
        MagnifierShape::Rounded => path::round_rect_path(r, r.w.min(r.h) * 0.12),
    }
}

pub(crate) fn step_path(s: &Step) -> Option<Path> {
    let r = (s.size / 2.0).max(1.0);
    let c = s.center;
    match s.shape {
        StepShape::Circle => PathBuilder::from_circle(c.x, c.y, r),
        StepShape::Square => path::round_rect_path(RectF::from_center(c, r * 2.0, r * 2.0), r * 0.28),
        StepShape::Teardrop => {
            // A circle with a pointed corner toward `angle`.
            let th = s.angle.to_radians();
            let d = r * 1.5;
            let phi = (r / d).acos();
            let tip = Pt::new(c.x + d * th.cos(), c.y + d * th.sin());
            let mut pb = PathBuilder::new();
            pb.move_to(tip.x, tip.y);
            let a0 = th + phi;
            pb.line_to(c.x + r * a0.cos(), c.y + r * a0.sin());
            path::arc_to(&mut pb, c, r, a0, th - phi + std::f32::consts::TAU);
            pb.close();
            pb.finish()
        }
    }
}

// ----- Shapes -----

pub(super) fn draw_shape(pm: &mut Pixmap, s: &Shape, st: &Style, ctx: &Ctx) {
    let bracket = matches!(s.kind, ShapeKind::CurlyBracket | ShapeKind::SquareBracket);
    if bracket {
        if st.fill.is_visible() {
            if let Some(p) = shape_path(s, true) {
                pm.fill_path(&p, &paint_color(st.fill), FillRule::Winding, ctx.ts, None);
            }
        }
        if let Some(p) = shape_path(s, false) {
            let w = st.width.max(0.5);
            pm.stroke_path(&p, &paint_color(st.color), &stroke_of(w, st.dash), ctx.ts, None);
        }
        return;
    }
    let Some(p) = shape_path(s, true) else { return };
    if st.fill.is_visible() {
        pm.fill_path(&p, &paint_color(st.fill), FillRule::Winding, ctx.ts, None);
    }
    if st.width > 0.0 && st.color.is_visible() {
        pm.stroke_path(&p, &paint_color(st.color), &stroke_of(st.width, st.dash), ctx.ts, None);
    }
}

// ----- Highlighter -----

pub(super) fn draw_highlight(pm: &mut Pixmap, h: &Highlight, st: &Style, opacity: f32, ctx: &Ctx) {
    let full = IntRect::from_xywh(0, 0, pm.width(), pm.height());
    let is_line = h.shape == HighlightShape::Line;
    let tol = ctx.tol();
    let pen_w = st.width.max(1.0);

    // Area outside the highlight: blur and/or dim (rect-type highlighters only).
    if !is_line && (h.blur_outside > 0.0 || h.dim_outside > 0.0) {
        if let (Some(full), Some(shape)) = (full, highlight_path(h, 0.0, tol)) {
            composite_backdrop(pm, ctx, full);
            if let Some(mut outside) = Mask::new(pm.width(), pm.height()) {
                outside.fill_path(&shape, FillRule::Winding, true, ctx.ts);
                invert_mask(&mut outside);
                if h.blur_outside > 0.0 {
                    let mut blurred = pm.clone();
                    let (w, hh) = (blurred.width() as usize, blurred.height() as usize);
                    fx::blur(blurred.data_mut(), w, hh, h.blur_outside * ctx.zoom);
                    let paint = PixmapPaint { opacity, ..Default::default() };
                    pm.draw_pixmap(0, 0, blurred.as_ref(), &paint, Transform::identity(), Some(&outside));
                }
                if h.dim_outside > 0.0 {
                    let dim = Color::BLACK.fade(h.dim_outside.clamp(0.0, 1.0) * opacity);
                    if let Some(r) = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32) {
                        pm.fill_rect(r, &paint_color(dim), Transform::identity(), Some(&outside));
                    }
                }
            }
        }
    }

    // The highlight itself, multiplied onto what is below.
    let fill = st.fill.fade(opacity);
    if fill.is_visible() {
        let shape = if is_line { highlight_path(h, 0.0, tol) } else { highlight_path(h, h.indent, tol) };
        if let Some(shape) = shape {
            let bounds = if is_line { RectF::bounding(&h.points).inflate(pen_w) } else { h.rect.inflate(2.0) };
            if let Some(area) = device_rect(&ctx.ts, bounds, pm.width(), pm.height()) {
                composite_backdrop(pm, ctx, area);
            }
            let mut paint = paint_color(fill);
            paint.blend_mode = BlendMode::Multiply;
            if is_line {
                let mut s = stroke_of(pen_w, Dash::Solid);
                s.line_cap = tiny_skia::LineCap::Round;
                pm.stroke_path(&shape, &paint, &s, ctx.ts, None);
            } else {
                pm.fill_path(&shape, &paint, FillRule::Winding, ctx.ts, None);
            }
        }
    }

    // Optional border.
    if !is_line && st.width > 0.0 && st.color.is_visible() {
        if let Some(border) = highlight_path(h, 0.0, tol) {
            let paint = paint_color(st.color.fade(opacity));
            pm.stroke_path(&border, &paint, &stroke_of(st.width, st.dash), ctx.ts, None);
        }
    }
}

// ----- Magnifier -----

pub(super) fn draw_magnifier(pm: &mut Pixmap, m: &Magnifier, st: &Style, ctx: &Ctx) {
    let border = st.color;
    let bw = st.width.max(0.0);
    let thin = (bw * 0.6).max(1.0);
    let src_shape = if m.shape == MagnifierShape::Circle { MagnifierShape::Circle } else { MagnifierShape::Rect };

    if m.connector && !m.source.intersects(&m.target) {
        for (a, b) in connector_lines(m) {
            if let Some(p) = path::polyline_path(&[a, b], false) {
                pm.stroke_path(&p, &paint_color(border.fade(0.8)), &stroke_of(thin, st.dash), ctx.ts, None);
            }
        }
    }
    if let Some(p) = magnifier_path(m.source, src_shape) {
        pm.stroke_path(&p, &paint_color(border), &stroke_of(thin, st.dash), ctx.ts, None);
    }

    let Some(target) = magnifier_path(m.target, m.shape) else { return };
    // Magnified pixels: a pattern of the source area of the effective base.
    let src = m.source;
    let crop_r = crate::common::Rect::new(
        src.x.floor() as i32 - 1,
        src.y.floor() as i32 - 1,
        src.w.ceil() as i32 + 3,
        src.h.ceil() as i32 + 3,
    );
    let crop = ctx.base.crop(crop_r);
    if let (Some(cpm), false) = (super::image_to_pixmap(&crop), src.w < 0.5 || src.h < 0.5) {
        let clip = crop_r.intersect(&ctx.base.bounds());
        let (sx, sy) = (m.target.w / src.w, m.target.h / src.h);
        let pat_ts = Transform::from_translate(m.target.x, m.target.y)
            .pre_scale(sx, sy)
            .pre_translate(clip.x as f32 - src.x, clip.y as f32 - src.y);
        let quality = if sx.max(sy) * ctx.zoom >= 3.0 { FilterQuality::Bicubic } else { FilterQuality::Bilinear };
        let shader = Pattern::new(cpm.as_ref(), SpreadMode::Pad, quality, 1.0, pat_ts);
        pm.fill_path(&target, &paint_shader(shader), FillRule::Winding, ctx.ts, None);
    } else {
        pm.fill_path(&target, &paint_color(Color::rgb(0xEE, 0xEE, 0xEE)), FillRule::Winding, ctx.ts, None);
    }
    if bw > 0.0 && border.is_visible() {
        pm.stroke_path(&target, &paint_color(border), &stroke_of(bw, st.dash), ctx.ts, None);
    }
}

/// The two lines of the convex hull of source + target that bridge them.
pub(crate) fn connector_lines(m: &Magnifier) -> Vec<(Pt, Pt)> {
    let ring = |r: RectF, circle: bool| -> Vec<Pt> {
        if circle {
            (0..48)
                .map(|i| {
                    let a = i as f32 / 48.0 * std::f32::consts::TAU;
                    Pt::new(r.center().x + r.w / 2.0 * a.cos(), r.center().y + r.h / 2.0 * a.sin())
                })
                .collect()
        } else {
            r.corners().to_vec()
        }
    };
    let circle = m.shape == MagnifierShape::Circle;
    let mut pts = ring(m.source, circle);
    pts.extend(ring(m.target, circle));
    let hull = path::convex_hull(&pts);
    let in_src = |p: Pt| m.source.inflate(0.01).contains(p);
    let n = hull.len();
    (0..n)
        .filter_map(|i| {
            let (a, b) = (hull[i], hull[(i + 1) % n]);
            (in_src(a) != in_src(b)).then_some((a, b))
        })
        .collect()
}

// ----- Step stamp -----

pub(super) fn draw_step(pm: &mut Pixmap, s: &Step, st: &Style, ctx: &Ctx) {
    let Some(shape) = step_path(s) else { return };
    let r = s.size / 2.0;
    let c = s.center;
    let fill = if st.fill.is_visible() { st.fill } else { Color::RED };
    let stops =
        vec![GradientStop::new(0.0, fill.lighten(0.14).to_skia()), GradientStop::new(1.0, fill.darken(0.1).to_skia())];
    let paint = match LinearGradient::new(
        sk(Pt::new(c.x, c.y - r)),
        sk(Pt::new(c.x, c.y + r)),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        Some(sh) => paint_shader(sh),
        None => paint_color(fill),
    };
    pm.fill_path(&shape, &paint, FillRule::Winding, ctx.ts, None);
    if st.width > 0.0 && st.color.is_visible() {
        fill_and_border_union(pm, &[shape], Color::TRANSPARENT, Some((st.color, st.width, Dash::Solid)), ctx.ts);
    }
    let label = s.value.label();
    let text_color = if st.font.color.is_visible() { st.font.color } else { fill.contrast() };
    let box_w = r * if label.chars().count() >= 3 { 1.5 } else { 1.25 };
    fit_label(pm, &label, &st.font.family, LabelFit { center: c, max_w: box_w, max_h: r * 1.0 }, text_color, ctx);
}
