//! Lines and arrows: geometry kinds, arrowheads, dash styles, decorative styles, outline effect.

use super::{device_rect, paint_color, paint_shader, stroke_of, Ctx};
use crate::common::rng::Rng;
use crate::draw::model::*;
use crate::draw::path::{self, sk};
use tiny_skia::{
    BlendMode, FillRule, GradientStop, LinearGradient, Paint, Path, PathBuilder, Pixmap, PixmapPaint, RadialGradient,
    SpreadMode, Stroke, Transform,
};

/// Arrowhead dimensions for a line width: (length, width, how far the line is pulled back).
pub(crate) fn head_dims(head: ArrowHead, w: f32, size: f32) -> (f32, f32, f32) {
    let size = if size.is_finite() { size.clamp(0.2, 5.0) } else { 1.0 };
    let l = (w * 2.6 + 8.0) * size;
    match head {
        ArrowHead::None => (0.0, 0.0, 0.0),
        ArrowHead::Triangle => (l, l * 0.85, l * 0.6),
        ArrowHead::ThreeD => (l * 1.1, l * 0.95, l * 0.65),
        ArrowHead::Fancy => (l * 1.1, l * 1.0, l * 0.55),
        ArrowHead::Stealth => (l * 1.05, l * 0.9, l * 0.5),
        ArrowHead::Open => (l * 0.85, l * 0.9, w * 0.5),
        ArrowHead::Diamond => (l * 1.1, l * 0.6, l * 0.55),
        ArrowHead::Circle => {
            let r = (w * 1.3 + 2.5) * size;
            (r * 2.0, r * 2.0, r)
        }
        ArrowHead::Bar => (w, l * 0.9, 0.0),
    }
}

/// Painter that can also draw an "outline pass": everything in one color, grown by `g`.
struct Pen<'a> {
    pm: &'a mut Pixmap,
    ts: Transform,
    outline: Option<(Color, f32)>,
}

impl Pen<'_> {
    fn fill(&mut self, path: &Path, paint: &Paint) {
        match self.outline {
            None => {
                self.pm.fill_path(path, paint, FillRule::Winding, self.ts, None);
            }
            Some((c, g)) => {
                let p = paint_color(c);
                self.pm.fill_path(path, &p, FillRule::Winding, self.ts, None);
                self.pm.stroke_path(path, &p, &stroke_of(2.0 * g, Dash::Solid), self.ts, None);
            }
        }
    }
    fn stroke(&mut self, path: &Path, paint: &Paint, stroke: &Stroke) {
        match self.outline {
            None => {
                self.pm.stroke_path(path, paint, stroke, self.ts, None);
            }
            Some((c, g)) => {
                let s = Stroke { width: stroke.width + 2.0 * g, ..stroke.clone() };
                self.pm.stroke_path(path, &paint_color(c), &s, self.ts, None);
            }
        }
    }
    fn is_outline(&self) -> bool {
        self.outline.is_some()
    }
}

pub(super) fn draw_line(pm: &mut Pixmap, l: &Line, st: &Style, id: ObjectId, ctx: &Ctx) {
    let spine = path::line_spine(l.kind, &l.points, ctx.tol());
    if spine.len() < 2 {
        return;
    }
    let w = if st.width.is_finite() { st.width.clamp(0.25, 500.0) } else { 1.0 };
    let total = path::length(&spine);
    let head_start = head_at(&spine, true, l.start, w, l.head_size, total);
    let head_end = head_at(&spine, false, l.end, w, l.head_size, total);
    let inset_s = head_start.as_ref().map(|h| h.inset).unwrap_or(0.0).min(total * 0.45);
    let inset_e = head_end.as_ref().map(|h| h.inset).unwrap_or(0.0).min(total * 0.45);
    let body = if inset_s > 0.0 || inset_e > 0.0 {
        path::sub_polyline(&spine, inset_s, total - inset_e)
    } else {
        spine.clone()
    };
    let (c1, c2) = if l.decor == LineDecor::Gradient { (st.color, l.color2) } else { (st.color, st.color) };

    // Outline pass first, underneath.
    if let Some(o) = st.outline.filter(|o| o.width > 0.0 && o.color.is_visible()) {
        let mut pen = Pen { pm: &mut *pm, ts: ctx.ts, outline: Some((o.color, o.width)) };
        if body.len() >= 2 {
            draw_body(&mut pen, &body, l, st, w, id, ctx);
        }
        for h in [&head_start, &head_end].into_iter().flatten() {
            draw_head(&mut pen, h, st.color, w);
        }
    }
    let mut pen = Pen { pm, ts: ctx.ts, outline: None };
    if body.len() >= 2 {
        draw_body(&mut pen, &body, l, st, w, id, ctx);
    }
    if let Some(h) = &head_start {
        draw_head(&mut pen, h, c1, w);
    }
    if let Some(h) = &head_end {
        draw_head(&mut pen, h, c2, w);
    }
}

struct Head {
    kind: ArrowHead,
    tip: Pt,
    dir: Pt,
    len: f32,
    width: f32,
    inset: f32,
}

fn head_at(spine: &[Pt], start: bool, kind: ArrowHead, w: f32, size: f32, total: f32) -> Option<Head> {
    if kind == ArrowHead::None || total < 1e-3 {
        return None;
    }
    let (len, width, inset) = head_dims(kind, w, size);
    let cum = path::cumulative(spine);
    // Aim the head along the chord over its own length, so it sits well on curves.
    let reach = len.min(total * 0.5).max(1e-3);
    let (tip, back) = if start {
        (spine[0], path::sample_at(spine, &cum, reach).0)
    } else {
        (spine[spine.len() - 1], path::sample_at(spine, &cum, total - reach).0)
    };
    let dir = (tip - back).norm();
    if dir.len() < 0.5 {
        return None;
    }
    Some(Head { kind, tip, dir, len, width, inset })
}

/// Build a path from local head coordinates (x forward from the tip, y to the right).
fn head_path(h: &Head, pts: &[(f32, f32)], closed: bool) -> Option<Path> {
    let n = h.dir.perp();
    let world: Vec<Pt> = pts.iter().map(|&(x, y)| h.tip + h.dir * x + n * y).collect();
    path::polyline_path(&world, closed)
}

fn draw_head(pen: &mut Pen, h: &Head, color: Color, w: f32) {
    let (l, hw) = (h.len, h.width / 2.0);
    let paint = paint_color(color);
    match h.kind {
        ArrowHead::None => {}
        ArrowHead::Triangle => {
            if let Some(p) = head_path(h, &[(0.0, 0.0), (-l, hw), (-l, -hw)], true) {
                pen.fill(&p, &paint);
                // Soften the sharp corners slightly.
                pen.stroke(&p, &paint, &stroke_of((w * 0.25).min(2.0), Dash::Solid));
            }
        }
        ArrowHead::Stealth => {
            if let Some(p) = head_path(h, &[(0.0, 0.0), (-l, hw), (-l * 0.68, 0.0), (-l, -hw)], true) {
                pen.fill(&p, &paint);
            }
        }
        ArrowHead::Open => {
            let pts = [(-l, hw), (0.0, 0.0), (-l, -hw)];
            if let Some(p) = head_path(h, &pts, false) {
                pen.stroke(&p, &paint, &stroke_of(w, Dash::Solid));
            }
        }
        ArrowHead::Diamond => {
            if let Some(p) = head_path(h, &[(0.0, 0.0), (-l * 0.5, hw), (-l, 0.0), (-l * 0.5, -hw)], true) {
                pen.fill(&p, &paint);
            }
        }
        ArrowHead::Circle => {
            let c = h.tip - h.dir * hw;
            if let Some(p) = PathBuilder::from_circle(c.x, c.y, hw) {
                pen.fill(&p, &paint);
            }
        }
        ArrowHead::Bar => {
            if let Some(p) = head_path(h, &[(0.0, hw), (0.0, -hw)], false) {
                pen.stroke(&p, &paint, &stroke_of(w, Dash::Solid));
            }
        }
        ArrowHead::Fancy => {
            let n = h.dir.perp();
            let at = |x: f32, y: f32| h.tip + h.dir * x + n * y;
            let mut pb = PathBuilder::new();
            let p0 = at(0.0, 0.0);
            pb.move_to(p0.x, p0.y);
            let (c, e) = (at(-l * 0.42, hw * 0.18), at(-l * 1.0, hw * 1.0));
            pb.quad_to(c.x, c.y, e.x, e.y);
            let (c, e) = (at(-l * 0.72, hw * 0.25), at(-l * 0.62, 0.0));
            pb.quad_to(c.x, c.y, e.x, e.y);
            let (c, e) = (at(-l * 0.72, -hw * 0.25), at(-l * 1.0, -hw * 1.0));
            pb.quad_to(c.x, c.y, e.x, e.y);
            let (c, e) = (at(-l * 0.42, -hw * 0.18), p0);
            pb.quad_to(c.x, c.y, e.x, e.y);
            pb.close();
            if let Some(p) = pb.finish() {
                pen.fill(&p, &paint);
            }
        }
        ArrowHead::ThreeD => {
            let notch = -l * 0.8;
            if pen.is_outline() {
                if let Some(p) = head_path(h, &[(0.0, 0.0), (-l, hw), (notch, 0.0), (-l, -hw)], true) {
                    pen.fill(&p, &paint);
                }
                return;
            }
            let lit = head_path(h, &[(0.0, 0.0), (-l, -hw), (notch, 0.0)], true);
            let shade = head_path(h, &[(0.0, 0.0), (notch, 0.0), (-l, hw)], true);
            if let Some(p) = lit {
                pen.fill(&p, &paint_color(color.lighten(0.3)));
            }
            if let Some(p) = shade {
                pen.fill(&p, &paint_color(color.darken(0.28)));
            }
            if let Some(p) = head_path(h, &[(0.0, 0.0), (-l, hw), (notch, 0.0), (-l, -hw)], true) {
                pen.stroke(&p, &paint_color(color.darken(0.45)), &stroke_of((w * 0.18).clamp(0.6, 1.6), Dash::Solid));
            }
        }
    }
}

fn draw_body(pen: &mut Pen, body: &[Pt], l: &Line, st: &Style, w: f32, id: ObjectId, ctx: &Ctx) {
    let color = st.color;
    let Some(spine_path) = path::polyline_path(body, false) else { return };
    let plain = paint_color(color);
    match l.decor {
        LineDecor::Plain => pen.stroke(&spine_path, &plain, &stroke_of(w, st.dash)),
        LineDecor::Gradient => {
            let first = body[0];
            let last = body[body.len() - 1];
            let stops = vec![GradientStop::new(0.0, color.to_skia()), GradientStop::new(1.0, l.color2.to_skia())];
            let paint = match LinearGradient::new(sk(first), sk(last), stops, SpreadMode::Pad, Transform::identity()) {
                Some(sh) => paint_shader(sh),
                None => plain.clone(),
            };
            pen.stroke(&spine_path, &paint, &stroke_of(w, st.dash));
        }
        LineDecor::Double => {
            if pen.is_outline() {
                pen.stroke(&spine_path, &plain, &stroke_of(w * 1.8, st.dash));
                return;
            }
            let outer = w * 1.8;
            with_layer(pen, body, outer, ctx, |layer, ts| {
                layer.stroke_path(&spine_path, &plain, &stroke_of(outer, st.dash), ts, None);
                let mut hole = paint_color(Color::BLACK);
                hole.blend_mode = BlendMode::DestinationOut;
                let s = Stroke { width: outer * 0.36, ..stroke_of(outer * 0.36, st.dash) };
                layer.stroke_path(&spine_path, &hole, &s, ts, None);
            });
        }
        LineDecor::Neon => {
            if pen.is_outline() {
                pen.stroke(&spine_path, &plain, &stroke_of(w * 1.4, st.dash));
                return;
            }
            let glow = w * 2.6;
            with_layer(pen, body, glow * 3.0, ctx, |layer, ts| {
                layer.stroke_path(&spine_path, &paint_color(color.fade(0.9)), &stroke_of(glow, st.dash), ts, None);
                let (lw, lh) = (layer.width() as usize, layer.height() as usize);
                crate::draw::fx::blur(layer.data_mut(), lw, lh, glow * 0.6 * ctx.zoom);
                let glow_layer = layer.clone();
                layer.draw_pixmap(0, 0, glow_layer.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            });
            pen.stroke(&spine_path, &plain, &stroke_of(w, st.dash));
            let core = paint_color(color.lighten(0.82));
            pen.stroke(&spine_path, &core, &stroke_of(w * 0.42, st.dash));
        }
        LineDecor::Chalk => {
            if pen.is_outline() {
                pen.stroke(&spine_path, &plain, &stroke_of(w, st.dash));
                return;
            }
            let seed = id ^ 0x5EED_C4A1_u64;
            with_layer(pen, body, w * 2.0, ctx, |layer, ts| {
                let mut rng = Rng::new(seed);
                layer.stroke_path(&spine_path, &paint_color(color.fade(0.82)), &stroke_of(w * 0.9, st.dash), ts, None);
                for k in 0..3 {
                    let off = (k as f32 - 1.0) * w * 0.28;
                    let jitter: Vec<Pt> = path::resample(body, (w * 1.5).max(3.0))
                        .into_iter()
                        .map(|(p, t, _)| p + t.perp() * (off + (unit(&mut rng) - 0.5) * w * 0.22))
                        .collect();
                    if let Some(p) = path::polyline_path(&jitter, false) {
                        layer.stroke_path(&p, &paint_color(color.fade(0.55)), &stroke_of(w * 0.32, st.dash), ts, None);
                    }
                }
                // Grain: punch tiny holes along the stroke.
                let mut hole = paint_color(Color::BLACK);
                hole.blend_mode = BlendMode::DestinationOut;
                let samples = path::resample(body, (w * 0.22).max(0.6));
                for (p, t, _) in samples {
                    for _ in 0..2 {
                        let q =
                            p + t.perp() * ((unit(&mut rng) - 0.5) * w * 1.1) + t * ((unit(&mut rng) - 0.5) * w * 0.3);
                        let r = w * (0.05 + unit(&mut rng) * 0.11);
                        hole.set_color(Color::BLACK.fade(0.35 + unit(&mut rng) * 0.65).to_skia());
                        if let Some(c) = PathBuilder::from_circle(q.x, q.y, r.max(0.3)) {
                            layer.fill_path(&c, &hole, FillRule::Winding, ts, None);
                        }
                    }
                }
            });
        }
        LineDecor::Beads => {
            let r = w * 0.85;
            for (p, _, _) in path::resample(body, w * 2.15) {
                let Some(circle) = PathBuilder::from_circle(p.x, p.y, r) else { continue };
                if pen.is_outline() {
                    pen.fill(&circle, &plain);
                    continue;
                }
                let hi = p + Pt::new(-0.32, -0.38) * r;
                let stops = vec![
                    GradientStop::new(0.0, color.lighten(0.75).to_skia()),
                    GradientStop::new(0.45, color.to_skia()),
                    GradientStop::new(1.0, color.darken(0.4).to_skia()),
                ];
                let paint =
                    match RadialGradient::new(sk(hi), sk(p), r * 1.25, stops, SpreadMode::Pad, Transform::identity()) {
                        Some(sh) => paint_shader(sh),
                        None => plain.clone(),
                    };
                pen.fill(&circle, &paint);
            }
        }
        LineDecor::Tapered => {
            let total = path::length(body).max(1e-3);
            let pts = path::resample(body, (w * 0.5).max(1.0));
            let hw = |s: f32| w * (0.62 - 0.54 * (s / total));
            let left: Vec<Pt> = pts.iter().map(|(p, t, s)| *p - t.perp() * hw(*s)).collect();
            let right: Vec<Pt> = pts.iter().rev().map(|(p, t, s)| *p + t.perp() * hw(*s)).collect();
            let mut poly = left;
            poly.extend(right);
            if let Some(p) = path::polyline_path(&poly, true) {
                pen.fill(&p, &plain);
            }
            if let Some(c) = PathBuilder::from_circle(body[0].x, body[0].y, hw(0.0)) {
                pen.fill(&c, &plain);
            }
        }
        LineDecor::Wavy | LineDecor::Zigzag => {
            let wl = (w * 7.0).max(14.0);
            let amp = (w * 1.2).max(2.5);
            let zig = l.decor == LineDecor::Zigzag;
            let step = if zig { wl / 2.0 } else { (wl / 24.0).max(0.5) };
            let pts: Vec<Pt> = path::resample(body, step)
                .into_iter()
                .enumerate()
                .map(|(i, (p, t, s))| {
                    let o = if zig {
                        if i % 2 == 0 {
                            0.0
                        } else {
                            amp * if (i / 2) % 2 == 0 { 1.0 } else { -1.0 }
                        }
                    } else {
                        (s / wl * std::f32::consts::TAU).sin() * amp
                    };
                    p + t.perp() * o
                })
                .collect();
            if let Some(p) = path::polyline_path(&pts, false) {
                let mut s = stroke_of(w, st.dash);
                if zig {
                    s.line_join = tiny_skia::LineJoin::Miter;
                }
                pen.stroke(&p, &plain, &s);
            }
        }
        LineDecor::Railroad => {
            let gap = w * 1.1;
            let rail = (w * 0.38).max(1.0);
            for side in [-1.0f32, 1.0] {
                if let Some(p) = path::polyline_path(&path::offset(body, gap * side), false) {
                    pen.stroke(&p, &plain, &stroke_of(rail, Dash::Solid));
                }
            }
            let tie = stroke_of((w * 0.5).max(1.0), Dash::Solid);
            let tie = Stroke { line_cap: tiny_skia::LineCap::Butt, ..tie };
            for (p, t, _) in path::resample(body, (w * 2.6).max(5.0)) {
                let n = t.perp() * (w * 1.85);
                if let Some(seg) = path::polyline_path(&[p - n, p + n], false) {
                    pen.stroke(&seg, &plain, &tie);
                }
            }
        }
        LineDecor::Brush => {
            let total = path::length(body).max(1e-3);
            let mut rng = Rng::new(id ^ 0xB2C5_11AA);
            let phase = unit(&mut rng) * 6.0;
            let pts = path::resample(body, (w * 0.5).max(1.0));
            let hw = |s: f32| {
                let t = s / total;
                let env = (std::f32::consts::PI * t).sin().powf(0.45).max(0.08);
                w * 0.62 * env * (1.0 + 0.16 * (s / (w * 2.5).max(1.0) + phase).sin())
            };
            let left: Vec<Pt> = pts.iter().map(|(p, t, s)| *p - t.perp() * hw(*s)).collect();
            let right: Vec<Pt> = pts.iter().rev().map(|(p, t, s)| *p + t.perp() * hw(*s)).collect();
            let mut poly = left;
            poly.extend(right);
            if let Some(p) = path::polyline_path(&poly, true) {
                pen.fill(&p, &plain);
            }
            if !pen.is_outline() {
                // Dry-bristle streaks.
                for k in [-0.45f32, 0.2, 0.55] {
                    let streak: Vec<Pt> = pts
                        .iter()
                        .filter(|(_, _, s)| *s > total * 0.15 && *s < total * 0.9)
                        .map(|(p, t, s)| *p + t.perp() * (hw(*s) * k))
                        .collect();
                    if let Some(p) = path::polyline_path(&streak, false) {
                        let paint = paint_color(color.lighten(0.35).fade(0.45));
                        pen.stroke(&p, &paint, &stroke_of((w * 0.07).max(0.5), Dash::Solid));
                    }
                }
            }
        }
    }
}

fn unit(rng: &mut Rng) -> f32 {
    (rng.next_u64() >> 40) as f32 / (1u64 << 24) as f32
}

/// Draw into a temporary layer covering the polyline (inflated by `margin` image px), then
/// composite it onto the pen's target.
fn with_layer(pen: &mut Pen, body: &[Pt], margin: f32, ctx: &Ctx, f: impl FnOnce(&mut Pixmap, Transform)) {
    let b = RectF::bounding(body).inflate(margin + 2.0);
    let Some(area) = device_rect(&pen.ts, b, pen.pm.width(), pen.pm.height()) else { return };
    let Some(mut layer) = Pixmap::new(area.width(), area.height()) else { return };
    let ts = pen.ts.post_translate(-(area.x() as f32), -(area.y() as f32));
    let _ = ctx;
    f(&mut layer, ts);
    pen.pm.draw_pixmap(area.x(), area.y(), layer.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
}
