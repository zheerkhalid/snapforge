//! Text objects (straight, arc, path; boxes and callout tails), step labels and emoji.

use super::{fill_and_border_union, paint_color, Ctx};
use crate::draw::dwrite::{self, TextLayout, TextSpec};
use crate::draw::model::*;
use crate::draw::path;
use std::sync::Arc;
use tiny_skia::{FillRule, Path, PathBuilder, Pixmap, Rect, Transform};

/// Resolved geometry of a text object.
pub(crate) struct TextGeom {
    /// The box (grown to fit the text). For path text: the bounding box of the glyphs.
    pub frame: RectF,
    pub layout: Arc<TextLayout>,
    /// Per-glyph transforms (glyph outline space → image space), parallel to `layout.glyphs`.
    pub glyph_ts: Vec<Transform>,
    /// Top-left of the layout box (straight text only).
    pub origin: Pt,
    /// Ink bounds of the glyphs in image coordinates.
    pub ink: RectF,
}

fn spec<'a>(st: &'a Style, max_width: Option<f32>, single_line: bool, text: &'a str) -> TextSpec<'a> {
    TextSpec {
        text,
        family: &st.font.family,
        size: st.font.px(),
        bold: st.font.bold,
        italic: st.font.italic,
        underline: st.font.underline && !single_line,
        align: st.font.align,
        max_width,
    }
}

pub(crate) fn text_geom(t: &Text, st: &Style) -> TextGeom {
    let em = st.font.px();
    let pad = t.padding.max(0.0);
    match &t.flow {
        TextFlow::Straight => {
            // An oval box only has ~70% of its width and height usable for text.
            let k = if t.boxed == TextBox::Oval { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            let rect = t.rect;
            let (layout, frame) = if t.wrap {
                let text_w = ((rect.w - 2.0 * pad) * k).max(em);
                let layout = dwrite::layout(&spec(st, Some(text_w), false, &t.text));
                let w = rect.w.max(text_w / k + 2.0 * pad);
                let h = rect.h.max(layout.height / k + 2.0 * pad);
                (layout, RectF::new(rect.x, rect.y, w, h))
            } else {
                let layout = dwrite::layout(&spec(st, None, false, &t.text));
                let w = rect.w.max(layout.width / k + 2.0 * pad);
                let h = rect.h.max(layout.height / k + 2.0 * pad);
                (layout, RectF::new(rect.x, rect.y, w, h))
            };
            // Wrapped text is aligned by DirectWrite inside its wrap width; unwrapped text is
            // aligned as a block inside the (possibly larger) box.
            let ox = match (t.wrap || t.boxed == TextBox::Oval, st.font.align) {
                (true, _) | (false, TextAlign::Center) => frame.x + (frame.w - layout.width) / 2.0,
                (false, TextAlign::Left) => frame.x + pad,
                (false, TextAlign::Right) => frame.right() - pad - layout.width,
            };
            let oy = frame.y + (frame.h - layout.height) / 2.0;
            let origin = Pt::new(ox, oy);
            let glyph_ts: Vec<Transform> =
                layout.glyphs.iter().map(|g| Transform::from_translate(ox + g.x, oy + g.y)).collect();
            let ink = ink_bounds(&layout, &glyph_ts).unwrap_or(frame);
            TextGeom { frame, layout, glyph_ts, origin, ink }
        }
        TextFlow::Arc { bend } => {
            let line = single_line_text(&t.text);
            let layout = dwrite::layout(&spec(st, None, true, &line));
            // Centre the glyph band (not the baseline) on the arc: the visual middle of a line
            // of text sits about 0.35 em above its baseline.
            let baseline = arc_baseline(t.rect.translate(Pt::new(0.0, em * 0.35)), *bend);
            let glyph_ts = place_on_path(&layout, &baseline, st.font.align);
            let ink = ink_bounds(&layout, &glyph_ts).unwrap_or(t.rect);
            TextGeom { frame: t.rect, layout, glyph_ts, origin: t.rect.tl(), ink }
        }
        TextFlow::Path { points, smooth } => {
            let line = single_line_text(&t.text);
            let layout = dwrite::layout(&spec(st, None, true, &line));
            let baseline = if *smooth { path::flatten_smooth(points, 0.25) } else { points.clone() };
            let glyph_ts = place_on_path(&layout, &baseline, st.font.align);
            let pts_box = RectF::bounding(points);
            let ink = ink_bounds(&layout, &glyph_ts).map(|r| r.union(&pts_box)).unwrap_or(pts_box);
            TextGeom { frame: ink, layout, glyph_ts, origin: pts_box.tl(), ink }
        }
    }
}

fn single_line_text(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Circular arc across `r` (left to right) as a polyline. `bend` > 0 bulges upward.
pub(crate) fn arc_baseline(r: RectF, bend: f32) -> Vec<Pt> {
    let bend = if bend.is_finite() { bend.clamp(-1.0, 1.0) } else { 0.0 };
    let chord = r.w.max(1.0);
    let sag = bend * chord / 2.0;
    let cy = r.center().y + sag / 2.0;
    if sag.abs() < 0.5 {
        return vec![Pt::new(r.x, r.center().y), Pt::new(r.right(), r.center().y)];
    }
    let s = sag.abs();
    let radius = (chord * chord / 4.0 + s * s) / (2.0 * s);
    let half = (chord / 2.0 / radius).clamp(-1.0, 1.0).asin();
    let half = if s > radius { std::f32::consts::PI - half } else { half };
    let cx = r.center().x;
    let n = 96;
    (0..=n)
        .map(|i| {
            let a = -half + 2.0 * half * i as f32 / n as f32;
            if sag > 0.0 {
                // Centre below the chord; apex at the top.
                let center = Pt::new(cx, cy - sag + radius);
                Pt::new(center.x + radius * a.sin(), center.y - radius * a.cos())
            } else {
                let center = Pt::new(cx, cy + s - radius);
                Pt::new(center.x + radius * a.sin(), center.y + radius * a.cos())
            }
        })
        .collect()
}

/// Place each glyph on the polyline, centred on its advance, rotated to the tangent.
fn place_on_path(layout: &TextLayout, baseline: &[Pt], align: TextAlign) -> Vec<Transform> {
    let cum = path::cumulative(baseline);
    let total = cum.last().copied().unwrap_or(0.0);
    let text_w = layout.content_width;
    let start = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => (total - text_w) / 2.0,
        TextAlign::Right => total - text_w,
    };
    let first_y = layout.glyphs.first().map(|g| g.y).unwrap_or(0.0);
    let left = layout.glyphs.iter().map(|g| g.x).fold(f32::INFINITY, f32::min);
    let left = if left.is_finite() { left } else { 0.0 };
    layout
        .glyphs
        .iter()
        .map(|g| {
            let mid = start + (g.x - left) + g.advance / 2.0;
            let (p, t) = point_on(baseline, &cum, total, mid);
            Transform::from_translate(p.x, p.y)
                .pre_concat(Transform::from_rotate(t.angle_deg()))
                .pre_translate(-g.advance / 2.0, g.y - first_y)
        })
        .collect()
}

/// Like `sample_at`, but extends straight past either end.
fn point_on(pts: &[Pt], cum: &[f32], total: f32, s: f32) -> (Pt, Pt) {
    if pts.len() < 2 {
        let p = pts.first().copied().unwrap_or_default();
        return (Pt::new(p.x + s, p.y), Pt::new(1.0, 0.0));
    }
    if s < 0.0 {
        let t = (pts[1] - pts[0]).norm();
        return (pts[0] + t * s, t);
    }
    if s > total {
        let n = pts.len();
        let t = (pts[n - 1] - pts[n - 2]).norm();
        return (pts[n - 1] + t * (s - total), t);
    }
    path::sample_at(pts, cum, s)
}

fn ink_bounds(layout: &TextLayout, ts: &[Transform]) -> Option<RectF> {
    let mut out: Option<RectF> = None;
    for (g, t) in layout.glyphs.iter().zip(ts) {
        let Some(p) = g.path.clone().transform(*t) else { continue };
        let b = p.bounds();
        let r = RectF::new(b.x(), b.y(), b.width(), b.height());
        out = Some(match out {
            Some(o) => o.union(&r),
            None => r,
        });
    }
    out
}

/// Box outline (+ tail) for a straight text object, all wound clockwise.
pub(crate) fn box_paths(t: &Text, frame: RectF) -> Vec<Path> {
    let mut out = Vec::new();
    let body = match t.boxed {
        TextBox::None => return out,
        TextBox::Rect => path::round_rect_path(frame, 0.0),
        TextBox::Rounded => path::round_rect_path(frame, t.radius),
        TextBox::Oval => path::ellipse_path(frame),
    };
    let Some(body) = body else { return out };
    let tail = t.tail.filter(|tl| !frame.contains(tl.tip)).and_then(|tl| {
        // Wind the tail like the box so a non-zero fill gives their union.
        let box_area = path::flatten_path(&body, 1.0).first().map(|c| path::signed_area(c)).unwrap_or(1.0);
        tail_path(frame, tl, t.boxed, t.radius, box_area >= 0.0)
    });
    out.push(body);
    out.extend(tail);
    out
}

/// Callout pointer: base points on the box edge (`width` apart) around where the line from the
/// centre to the tip leaves the box, the tip, and the box centre (so the union has no gaps).
pub(crate) fn tail_path(frame: RectF, tail: Tail, boxed: TextBox, radius: f32, clockwise: bool) -> Option<Path> {
    let c = frame.center();
    let d = tail.tip - c;
    if d.len() < 1.0 || frame.w < 1.0 || frame.h < 1.0 {
        return None;
    }
    let (hw_box, hh_box) = (frame.w / 2.0, frame.h / 2.0);
    let hw = tail.width.max(2.0) / 2.0;
    let (b1, b2) = if boxed == TextBox::Oval {
        // Parametric angle of the exit point, then step along the ellipse by ±hw.
        let th = (d.y / hh_box).atan2(d.x / hw_box);
        let delta = (hw / ((hw_box + hh_box) / 2.0)).min(1.2);
        let at = |a: f32| Pt::new(c.x + hw_box * a.cos(), c.y + hh_box * a.sin());
        (at(th - delta), at(th + delta))
    } else {
        let r = if boxed == TextBox::Rounded { radius.min(hw_box).min(hh_box).max(0.0) } else { 0.0 };
        let tx = if d.x.abs() > 1e-6 { hw_box / d.x.abs() } else { f32::INFINITY };
        let ty = if d.y.abs() > 1e-6 { hh_box / d.y.abs() } else { f32::INFINITY };
        if tx < ty {
            // Leaves through the left or right edge.
            let x = if d.x > 0.0 { frame.right() } else { frame.x };
            let y = (c.y + d.y * tx).clamp(frame.y + r + hw.min(hh_box - r), frame.bottom() - r - hw.min(hh_box - r));
            (Pt::new(x, y - hw.min(hh_box - r)), Pt::new(x, y + hw.min(hh_box - r)))
        } else {
            let y = if d.y > 0.0 { frame.bottom() } else { frame.y };
            let x = (c.x + d.x * ty).clamp(frame.x + r + hw.min(hw_box - r), frame.right() - r - hw.min(hw_box - r));
            (Pt::new(x - hw.min(hw_box - r), y), Pt::new(x + hw.min(hw_box - r), y))
        }
    };
    let mut poly = vec![b1, tail.tip, b2, c];
    if (path::signed_area(&poly) >= 0.0) != clockwise {
        poly.reverse();
    }
    path::polyline_path(&poly, true)
}

pub(super) fn draw_text(pm: &mut Pixmap, t: &Text, st: &Style, ctx: &Ctx) {
    let g = text_geom(t, st);
    if t.flow == TextFlow::Straight && t.boxed != TextBox::None {
        let border = (st.width > 0.0).then_some((st.color, st.width, st.dash));
        fill_and_border_union(pm, &box_paths(t, g.frame), st.fill, border, ctx.ts);
    }
    let color = st.font.color;
    let decos: Vec<Rect> = if t.flow == TextFlow::Straight {
        g.layout.decos.iter().filter_map(|d| Rect::from_xywh(g.origin.x + d.x, g.origin.y + d.y, d.w, d.h)).collect()
    } else {
        Vec::new()
    };
    if let Some(o) = st.outline.filter(|o| o.width > 0.0 && o.color.is_visible()) {
        let paint = paint_color(o.color);
        let stroke = super::stroke_of(o.width * 2.0, Dash::Solid);
        for (gl, ts) in g.layout.glyphs.iter().zip(&g.glyph_ts) {
            pm.stroke_path(&gl.path, &paint, &stroke, ctx.ts.pre_concat(*ts), None);
        }
        for r in &decos {
            let p = PathBuilder::from_rect(*r);
            pm.stroke_path(&p, &paint, &stroke, ctx.ts, None);
        }
    }
    for (gl, ts) in g.layout.glyphs.iter().zip(&g.glyph_ts) {
        let paint = paint_color(gl.color.unwrap_or(color));
        pm.fill_path(&gl.path, &paint, FillRule::Winding, ctx.ts.pre_concat(*ts), None);
    }
    for r in decos {
        pm.fill_rect(r, &paint_color(color), ctx.ts, None);
    }
}

// ----- Labels and emoji -----

pub(crate) struct LabelFit {
    pub center: Pt,
    pub max_w: f32,
    pub max_h: f32,
}

/// Draw a short bold label centred (by ink) in a box, scaled to fit.
pub(crate) fn fit_label(pm: &mut Pixmap, label: &str, family: &str, fit: LabelFit, color: Color, ctx: &Ctx) {
    let spec = TextSpec {
        text: label,
        family,
        size: 100.0,
        bold: true,
        italic: false,
        underline: false,
        align: TextAlign::Left,
        max_width: None,
    };
    let layout = dwrite::layout(&spec);
    let ts: Vec<Transform> = layout.glyphs.iter().map(|g| Transform::from_translate(g.x, g.y)).collect();
    let Some(ink) = ink_bounds(&layout, &ts) else { return };
    // Fit by cap height so "1" and "8" get the same size.
    let s = (fit.max_h / ink.h.max(1.0)).min(fit.max_w / ink.w.max(1.0));
    let place = Transform::from_translate(fit.center.x, fit.center.y)
        .pre_scale(s, s)
        .pre_translate(-ink.center().x, -ink.center().y);
    let paint = paint_color(color);
    for (g, t) in layout.glyphs.iter().zip(&ts) {
        pm.fill_path(&g.path, &paint, FillRule::Winding, ctx.ts.pre_concat(place).pre_concat(*t), None);
    }
}

const EMOJI_FONT: &str = "Segoe UI Emoji";

/// Laid-out emoji and the ink box of its glyphs (layout space).
pub(crate) fn emoji_layout(text: &str) -> Option<(Arc<TextLayout>, RectF)> {
    let spec = TextSpec {
        text,
        family: EMOJI_FONT,
        size: 100.0,
        bold: false,
        italic: false,
        underline: false,
        align: TextAlign::Left,
        max_width: None,
    };
    let layout = dwrite::layout(&spec);
    let ts: Vec<Transform> = layout.glyphs.iter().map(|g| Transform::from_translate(g.x, g.y)).collect();
    let ink = ink_bounds(&layout, &ts)?;
    Some((layout, ink))
}

pub(super) fn draw_emoji(pm: &mut Pixmap, e: &Emoji, st: &Style, ctx: &Ctx) {
    let Some((layout, ink)) = emoji_layout(&e.text) else { return };
    // Fit the ink box into the frame, keeping the aspect ratio.
    let side = ink.w.max(ink.h).max(1.0);
    let content = RectF::new(ink.center().x - side / 2.0, ink.center().y - side / 2.0, side, side);
    let fts = super::frame_transform(&e.frame, side, side).pre_translate(-content.x, -content.y);
    for g in &layout.glyphs {
        let paint = paint_color(g.color.unwrap_or(st.font.color));
        let ts = ctx.ts.pre_concat(fts).pre_translate(g.x, g.y);
        pm.fill_path(&g.path, &paint, FillRule::Winding, ts, None);
    }
}
