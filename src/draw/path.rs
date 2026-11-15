//! Geometry helpers: curve flattening, polyline sampling and offsetting, distances, path builders.

use super::model::{LineKind, Pt, RectF};
use tiny_skia::{Path, PathBuilder, PathSegment};

/// Kappa for approximating a quarter circle with one cubic.
const KAPPA: f32 = 0.552_284_8;

pub fn sk(p: Pt) -> tiny_skia::Point {
    tiny_skia::Point::from_xy(p.x, p.y)
}

pub fn from_sk(p: tiny_skia::Point) -> Pt {
    Pt::new(p.x, p.y)
}

fn quad_at(a: Pt, c: Pt, b: Pt, t: f32) -> Pt {
    let u = 1.0 - t;
    a * (u * u) + c * (2.0 * u * t) + b * (t * t)
}

fn cubic_at(a: Pt, c1: Pt, c2: Pt, b: Pt, t: f32) -> Pt {
    let u = 1.0 - t;
    a * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + b * (t * t * t)
}

fn steps_for(len_estimate: f32, tol: f32) -> usize {
    // Chord error of a curve ~ L^2 / (8 R); a simple length-based count is accurate enough here.
    ((len_estimate / (tol.max(0.05) * 8.0).sqrt()).ceil() as usize).clamp(2, 512)
}

/// Append a flattened quadratic (excluding the start point).
pub fn flatten_quad(out: &mut Vec<Pt>, a: Pt, c: Pt, b: Pt, tol: f32) {
    let n = steps_for(a.dist(c) + c.dist(b), tol);
    for i in 1..=n {
        out.push(quad_at(a, c, b, i as f32 / n as f32));
    }
}

/// Append a flattened cubic (excluding the start point).
pub fn flatten_cubic(out: &mut Vec<Pt>, a: Pt, c1: Pt, c2: Pt, b: Pt, tol: f32) {
    let n = steps_for(a.dist(c1) + c1.dist(c2) + c2.dist(b), tol);
    for i in 1..=n {
        out.push(cubic_at(a, c1, c2, b, i as f32 / n as f32));
    }
}

/// Catmull-Rom spline through `pts` as cubic Bezier segments `(c1, c2, end)`.
pub fn catmull_rom(pts: &[Pt]) -> Vec<(Pt, Pt, Pt)> {
    let mut segs = Vec::new();
    if pts.len() < 2 {
        return segs;
    }
    for i in 0..pts.len() - 1 {
        let p0 = if i == 0 { pts[0] } else { pts[i - 1] };
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = if i + 2 < pts.len() { pts[i + 2] } else { pts[i + 1] };
        let c1 = p1 + (p2 - p0) * (1.0 / 6.0);
        let c2 = p2 - (p3 - p1) * (1.0 / 6.0);
        segs.push((c1, c2, p2));
    }
    segs
}

/// Flatten a Catmull-Rom spline through `pts`.
pub fn flatten_smooth(pts: &[Pt], tol: f32) -> Vec<Pt> {
    let Some(&first) = pts.first() else { return Vec::new() };
    let mut out = vec![first];
    let mut prev = first;
    for (c1, c2, b) in catmull_rom(pts) {
        flatten_cubic(&mut out, prev, c1, c2, b, tol);
        prev = b;
    }
    out
}

/// The centre line of a line object as a polyline (curves flattened to `tol` pixels).
pub fn line_spine(kind: LineKind, pts: &[Pt], tol: f32) -> Vec<Pt> {
    let get = |i: usize| pts.get(i).copied().unwrap_or_default();
    let mut out = Vec::new();
    match kind {
        LineKind::Straight => {
            if pts.len() >= 2 {
                out.extend_from_slice(&[get(0), get(pts.len() - 1)]);
            }
        }
        LineKind::Quad if pts.len() >= 3 => {
            out.push(get(0));
            flatten_quad(&mut out, get(0), get(1), get(2), tol);
        }
        LineKind::Cubic if pts.len() >= 4 => {
            out.push(get(0));
            flatten_cubic(&mut out, get(0), get(1), get(2), get(3), tol);
        }
        LineKind::Smooth => out = flatten_smooth(pts, tol),
        LineKind::ElbowH | LineKind::ElbowV if pts.len() >= 2 => {
            let (a, b) = (get(0), get(pts.len() - 1));
            let corner = if kind == LineKind::ElbowH { Pt::new(b.x, a.y) } else { Pt::new(a.x, b.y) };
            out.extend_from_slice(&[a, corner, b]);
        }
        _ => out.extend_from_slice(pts),
    }
    dedup(&mut out);
    out
}

/// Remove consecutive (near-)duplicate points.
pub fn dedup(pts: &mut Vec<Pt>) {
    pts.dedup_by(|a, b| a.dist(*b) < 1e-3);
}

/// Cumulative arc length at each vertex.
pub fn cumulative(pts: &[Pt]) -> Vec<f32> {
    let mut acc = 0.0;
    let mut out = Vec::with_capacity(pts.len());
    for (i, p) in pts.iter().enumerate() {
        if i > 0 {
            acc += p.dist(pts[i - 1]);
        }
        out.push(acc);
    }
    out
}

pub fn length(pts: &[Pt]) -> f32 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

/// Point and unit tangent at arc length `s` (clamped to the polyline).
pub fn sample_at(pts: &[Pt], cum: &[f32], s: f32) -> (Pt, Pt) {
    if pts.len() < 2 {
        return (pts.first().copied().unwrap_or_default(), Pt::new(1.0, 0.0));
    }
    let total = cum.last().copied().unwrap_or(0.0);
    let s = s.clamp(0.0, total);
    let i = match cum.binary_search_by(|v| v.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Less)) {
        Ok(i) => i.min(pts.len() - 2),
        Err(i) => i.saturating_sub(1).min(pts.len() - 2),
    };
    let seg = cum[i + 1] - cum[i];
    let t = if seg > 1e-6 { (s - cum[i]) / seg } else { 0.0 };
    (pts[i].lerp(pts[i + 1], t), (pts[i + 1] - pts[i]).norm())
}

/// Sub-polyline between arc lengths `s0` and `s1`.
pub fn sub_polyline(pts: &[Pt], s0: f32, s1: f32) -> Vec<Pt> {
    let cum = cumulative(pts);
    let total = cum.last().copied().unwrap_or(0.0);
    let (s0, s1) = (s0.clamp(0.0, total), s1.clamp(0.0, total));
    if s1 <= s0 || pts.len() < 2 {
        return Vec::new();
    }
    let mut out = vec![sample_at(pts, &cum, s0).0];
    for (i, p) in pts.iter().enumerate() {
        if cum[i] > s0 && cum[i] < s1 {
            out.push(*p);
        }
    }
    out.push(sample_at(pts, &cum, s1).0);
    dedup(&mut out);
    out
}

/// Points every `step` along the polyline, with tangents (includes both ends).
pub fn resample(pts: &[Pt], step: f32) -> Vec<(Pt, Pt, f32)> {
    let cum = cumulative(pts);
    let total = cum.last().copied().unwrap_or(0.0);
    let n = ((total / step.max(0.5)).ceil() as usize).clamp(1, 20_000);
    (0..=n)
        .map(|i| {
            let s = total * i as f32 / n as f32;
            let (p, t) = sample_at(pts, &cum, s);
            (p, t, s)
        })
        .collect()
}

/// Per-vertex unit normals (right-hand side), mitred at joints and clamped.
pub fn vertex_normals(pts: &[Pt]) -> Vec<Pt> {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let d0 = if i > 0 { (pts[i] - pts[i - 1]).norm() } else { Pt::default() };
            let d1 = if i + 1 < n { (pts[i + 1] - pts[i]).norm() } else { Pt::default() };
            let (n0, n1) = (d0.perp(), d1.perp());
            let sum = n0 + n1;
            if sum.len() < 1e-4 {
                return if i > 0 { n0 } else { n1 };
            }
            let m = sum.norm();
            let cos = m.dot(if i > 0 { n0 } else { n1 }).max(0.35);
            m * (1.0 / cos)
        })
        .collect()
}

/// Polyline offset by `d` along the vertex normals.
pub fn offset(pts: &[Pt], d: f32) -> Vec<Pt> {
    pts.iter().zip(vertex_normals(pts)).map(|(p, n)| *p + n * d).collect()
}

/// Distance from `p` to segment `a`-`b`.
pub fn dist_to_segment(p: Pt, a: Pt, b: Pt) -> f32 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    if l2 < 1e-9 {
        return p.dist(a);
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    p.dist(a + ab * t)
}

/// Distance from `p` to a polyline (`closed` adds the closing segment).
pub fn dist_to_polyline(p: Pt, pts: &[Pt], closed: bool) -> f32 {
    match pts.len() {
        0 => f32::INFINITY,
        1 => p.dist(pts[0]),
        _ => {
            let mut d = pts.windows(2).map(|w| dist_to_segment(p, w[0], w[1])).fold(f32::INFINITY, f32::min);
            if closed {
                d = d.min(dist_to_segment(p, pts[pts.len() - 1], pts[0]));
            }
            d
        }
    }
}

/// Non-zero winding point-in-polygon test.
pub fn point_in_polygon(p: Pt, poly: &[Pt]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut wn = 0i32;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let side = (b - a).cross(p - a);
        if a.y <= p.y {
            if b.y > p.y && side > 0.0 {
                wn += 1;
            }
        } else if b.y <= p.y && side < 0.0 {
            wn -= 1;
        }
    }
    wn != 0
}

/// Flatten a skia path into contours (each a polyline; closed contours end at their start).
pub fn flatten_path(path: &Path, tol: f32) -> Vec<Vec<Pt>> {
    let mut contours: Vec<Vec<Pt>> = Vec::new();
    let mut cur: Vec<Pt> = Vec::new();
    let mut last = Pt::default();
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                if cur.len() > 1 {
                    contours.push(std::mem::take(&mut cur));
                }
                cur.clear();
                last = from_sk(p);
                cur.push(last);
            }
            PathSegment::LineTo(p) => {
                last = from_sk(p);
                cur.push(last);
            }
            PathSegment::QuadTo(c, p) => {
                flatten_quad(&mut cur, last, from_sk(c), from_sk(p), tol);
                last = from_sk(p);
            }
            PathSegment::CubicTo(c1, c2, p) => {
                flatten_cubic(&mut cur, last, from_sk(c1), from_sk(c2), from_sk(p), tol);
                last = from_sk(p);
            }
            PathSegment::Close => {
                if cur.len() > 1 {
                    // Repeat the first point so the closing edge is part of the polyline.
                    let first = cur[0];
                    if cur.last().is_some_and(|l| l.dist(first) > 1e-4) {
                        cur.push(first);
                    }
                    last = first;
                    contours.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
        }
    }
    if cur.len() > 1 {
        contours.push(cur);
    }
    contours
}

/// Andrew's monotone chain convex hull (counter-clockwise in y-up, i.e. clockwise on screen).
pub fn convex_hull(points: &[Pt]) -> Vec<Pt> {
    let mut p: Vec<Pt> = points.iter().copied().filter(|p| p.is_finite()).collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| a.dist(*b) < 1e-4);
    if p.len() < 3 {
        return p;
    }
    let mut hull: Vec<Pt> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &Pt>> = if pass == 0 { Box::new(p.iter()) } else { Box::new(p.iter().rev()) };
        for &q in iter {
            while hull.len() >= start + 2 {
                let (a, b) = (hull[hull.len() - 2], hull[hull.len() - 1]);
                if (b - a).cross(q - a) <= 0.0 {
                    hull.pop();
                } else {
                    break;
                }
            }
            hull.push(q);
        }
        hull.pop();
    }
    hull
}

// ----- Path builders -----

pub fn polyline_path(pts: &[Pt], closed: bool) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let (first, rest) = pts.split_first()?;
    pb.move_to(first.x, first.y);
    for p in rest {
        pb.line_to(p.x, p.y);
    }
    if closed {
        pb.close();
    }
    pb.finish()
}

/// Append an arc of the circle (`c`, `r`) from angle `a0` to `a1` (radians, y-down, any
/// direction) using cubic segments. Assumes the current point is already at the arc start.
pub fn arc_to(pb: &mut PathBuilder, c: Pt, r: f32, a0: f32, a1: f32) {
    let sweep = a1 - a0;
    let n = ((sweep.abs() / std::f32::consts::FRAC_PI_2).ceil() as usize).max(1);
    let step = sweep / n as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    for i in 0..n {
        let t0 = a0 + step * i as f32;
        let t1 = t0 + step;
        let (s0, c0) = t0.sin_cos();
        let (s1, c1) = t1.sin_cos();
        let p0 = Pt::new(c.x + r * c0, c.y + r * s0);
        let p1 = Pt::new(c.x + r * c1, c.y + r * s1);
        let cp1 = p0 + Pt::new(-s0, c0) * (r * k);
        let cp2 = p1 - Pt::new(-s1, c1) * (r * k);
        pb.cubic_to(cp1.x, cp1.y, cp2.x, cp2.y, p1.x, p1.y);
    }
}

/// Add a rounded rectangle contour (radius clamped to half the smaller side).
pub fn push_round_rect(pb: &mut PathBuilder, r: RectF, radius: f32) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    if rad < 0.01 {
        if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)) {
            pb.push_rect(rect);
        }
        return;
    }
    let k = rad * (1.0 - KAPPA);
    let (l, t, rt, b) = (r.x, r.y, r.right(), r.bottom());
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - k, t, rt, t + k, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - k, rt - k, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + k, b, l, b - k, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + k, l + k, t, l + rad, t);
    pb.close();
}

/// Add an ellipse contour inscribed in `r` (clockwise on screen, like rectangles).
pub fn push_ellipse(pb: &mut PathBuilder, r: RectF) {
    let (cx, cy, rx, ry) = (r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0, r.h / 2.0);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    pb.move_to(cx, cy - ry);
    pb.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    pb.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    pb.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    pb.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    pb.close();
}

pub fn round_rect_path(r: RectF, radius: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    push_round_rect(&mut pb, r, radius);
    pb.finish()
}

pub fn ellipse_path(r: RectF) -> Option<Path> {
    let mut pb = PathBuilder::new();
    push_ellipse(&mut pb, r);
    pb.finish()
}

/// Snap `p` so the segment from `anchor` is at a multiple of `step_deg` degrees.
pub fn snap_angle(anchor: Pt, p: Pt, step_deg: f32) -> Pt {
    let d = p - anchor;
    let len = d.len();
    if len < 1e-6 || step_deg <= 0.0 {
        return p;
    }
    let a = (d.angle_deg() / step_deg).round() * step_deg;
    let (s, c) = a.to_radians().sin_cos();
    anchor + Pt::new(c, s) * len
}

/// Signed area (positive = clockwise on screen, y down).
pub fn signed_area(poly: &[Pt]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| poly[i].cross(poly[(i + 1) % n])).sum::<f32>() / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_and_trim() {
        let pts = [Pt::new(0.0, 0.0), Pt::new(10.0, 0.0), Pt::new(10.0, 10.0)];
        let cum = cumulative(&pts);
        assert_eq!(cum, vec![0.0, 10.0, 20.0]);
        let (p, t) = sample_at(&pts, &cum, 15.0);
        assert_eq!(p, Pt::new(10.0, 5.0));
        assert_eq!(t, Pt::new(0.0, 1.0));
        let sub = sub_polyline(&pts, 5.0, 15.0);
        assert_eq!(sub, vec![Pt::new(5.0, 0.0), Pt::new(10.0, 0.0), Pt::new(10.0, 5.0)]);
    }

    #[test]
    fn polygon_tests() {
        let sq = [Pt::new(0.0, 0.0), Pt::new(10.0, 0.0), Pt::new(10.0, 10.0), Pt::new(0.0, 10.0)];
        assert!(point_in_polygon(Pt::new(5.0, 5.0), &sq));
        assert!(!point_in_polygon(Pt::new(15.0, 5.0), &sq));
        assert!((dist_to_polyline(Pt::new(5.0, -3.0), &sq, true) - 3.0).abs() < 1e-5);
        assert!(signed_area(&sq) > 0.0);
        let hull = convex_hull(&[sq[0], sq[1], sq[2], sq[3], Pt::new(5.0, 5.0)]);
        assert_eq!(hull.len(), 4);
    }

    #[test]
    fn snapping() {
        let p = snap_angle(Pt::default(), Pt::new(10.0, 1.0), 45.0);
        assert!((p.y).abs() < 1e-4 && (p.x - 10.05).abs() < 0.01);
    }
}
