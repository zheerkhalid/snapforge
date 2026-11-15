//! Editing: geometry queries, hit-testing, handles and handle drags, moves, alignment, z-order,
//! group style edits and copy/paste. All functions work on plain `Vec<Object>`s; wrap mutations
//! in [`super::History`] for undo.
//!
//! Drags are designed to be applied to a *snapshot* of the object taken when the drag starts,
//! with the *total* pointer delta (`drag_handle(&mut snapshot.clone(), h, total, c)`), so
//! constraints like keep-aspect never accumulate rounding drift.

use super::model::*;
use super::path::{self, dist_to_polyline};
use super::render::text::{box_paths, text_geom};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------------------------

pub fn find(objects: &[Object], id: ObjectId) -> Option<&Object> {
    objects.iter().find(|o| o.id == id)
}

pub fn find_mut(objects: &mut [Object], id: ObjectId) -> Option<&mut Object> {
    objects.iter_mut().find(|o| o.id == id)
}

/// A fresh id (one more than the largest in use; ids start at 1).
pub fn next_id(objects: &[Object]) -> ObjectId {
    objects.iter().map(|o| o.id).max().unwrap_or(0) + 1
}

// ---------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------

fn fill_marker(at: Pt) -> RectF {
    RectF::from_center(at, 16.0, 16.0)
}

/// The editable box of an object (what resize handles and alignment act on).
pub fn frame(obj: &Object) -> RectF {
    match &obj.kind {
        Kind::Text(t) => match &t.flow {
            TextFlow::Path { points, .. } if !points.is_empty() => RectF::bounding(points),
            _ => text_geom(t, &obj.style).frame,
        },
        Kind::Line(l) => RectF::bounding(&l.points),
        Kind::Shape(s) if s.kind == ShapeKind::Polygon => RectF::bounding(&s.points),
        Kind::Shape(s) => s.rect,
        Kind::Highlight(h) if h.shape == HighlightShape::Line => RectF::bounding(&h.points),
        Kind::Highlight(h) => h.rect,
        Kind::Magnifier(m) => m.target,
        Kind::Step(s) => RectF::from_center(s.center, s.size, s.size),
        Kind::Emoji(e) => e.frame.rect,
        Kind::Sticker(s) => s.frame.rect,
        Kind::Image(p) => p.frame.rect,
        Kind::Cursor(c) => c.frame.rect,
        Kind::Blur(b) => b.rect,
        Kind::Eraser(e) => RectF::bounding(&e.points),
        Kind::Fill(f) => fill_marker(f.at),
        Kind::Pencil(p) => RectF::bounding(&p.points),
    }
}

fn rotated_bounds(f: &Frame) -> RectF {
    if f.rotation == 0.0 {
        return f.rect;
    }
    let c = f.rect.center();
    let pts = f.rect.corners().map(|p| p.rotate_around(c, f.rotation));
    RectF::bounding(&pts)
}

/// Visual extent in image pixels (strokes, arrowheads, tails, connectors; not shadows).
pub fn bounds(obj: &Object) -> RectF {
    let st = &obj.style;
    let w = if st.width.is_finite() { st.width.max(0.0) } else { 0.0 };
    let outline = st.outline.map(|o| o.width.max(0.0)).unwrap_or(0.0);
    match &obj.kind {
        Kind::Text(t) => {
            let g = text_geom(t, st);
            let mut r = match t.flow {
                TextFlow::Straight => g.frame.union(&g.ink),
                _ => g.ink,
            };
            if let (Some(tail), TextFlow::Straight, false) = (t.tail, &t.flow, t.boxed == TextBox::None) {
                r = r.union(&RectF::from_center(tail.tip, 1.0, 1.0));
            }
            r.inflate(w / 2.0 + outline + 1.0)
        }
        Kind::Line(l) => {
            let spine = path::line_spine(l.kind, &l.points, 0.5);
            let head = [l.start, l.end]
                .iter()
                .map(|h| super::render::line_head_extent(*h, w.max(0.25), l.head_size))
                .fold(0.0, f32::max);
            let decor = match l.decor {
                LineDecor::Neon => w * 4.0,
                LineDecor::Railroad => w * 1.9,
                LineDecor::Wavy | LineDecor::Zigzag => (w * 1.2).max(2.5) + w / 2.0,
                LineDecor::Double => w,
                _ => w * 0.7,
            };
            RectF::bounding(&spine).inflate(decor.max(w / 2.0).max(head) + outline + 1.0)
        }
        Kind::Shape(_) | Kind::Blur(_) | Kind::Fill(_) => frame(obj).inflate(w / 2.0 + 1.0),
        Kind::Highlight(h) => {
            if h.shape == HighlightShape::Line {
                RectF::bounding(&h.points).inflate(w.max(1.0) / 2.0 + 1.0)
            } else {
                h.rect.inflate(w / 2.0 + 1.0)
            }
        }
        Kind::Magnifier(m) => m.source.union(&m.target).inflate(w / 2.0 + 1.0),
        Kind::Step(s) => {
            let r = s.size / 2.0 * if s.shape == StepShape::Teardrop { 1.5 } else { 1.0 };
            RectF::from_center(s.center, r * 2.0, r * 2.0).inflate(w / 2.0 + 1.0)
        }
        Kind::Emoji(e) => rotated_bounds(&e.frame).inflate(1.0),
        Kind::Sticker(s) => {
            let r = rotated_bounds(&s.frame);
            RectF::new(r.x, r.y, r.w * 1.04, r.h * 1.12).inflate(2.0)
        }
        Kind::Image(p) => rotated_bounds(&p.frame).inflate(1.0),
        Kind::Cursor(c) => rotated_bounds(&c.frame).inflate(2.0),
        Kind::Eraser(e) => RectF::bounding(&e.points).inflate(e.radius.max(0.0) + 1.0),
        Kind::Pencil(p) => RectF::bounding(&p.points).inflate(w.max(1.0) / 2.0 + 1.0),
    }
}

/// Union of the visual bounds of the given objects.
#[cfg(test)]
pub fn selection_bounds(objects: &[Object], ids: &[ObjectId]) -> Option<RectF> {
    objects.iter().filter(|o| ids.contains(&o.id)).map(bounds).reduce(|a, b| a.union(&b))
}

// ---------------------------------------------------------------------------------------------
// Hit-testing
// ---------------------------------------------------------------------------------------------

/// Topmost object under `p` (image coordinates), with `tolerance` in image pixels
/// (pass e.g. `4.0 / zoom` for a 4-pixel screen tolerance).
pub fn hit_test(objects: &[Object], p: Pt, tolerance: f32) -> Option<ObjectId> {
    objects.iter().rev().find(|o| hits(o, p, tolerance)).map(|o| o.id)
}

/// All objects whose visual bounds lie entirely inside `r` (rubber-band selection), in z-order.
pub fn objects_in_rect(objects: &[Object], r: RectF) -> Vec<ObjectId> {
    objects
        .iter()
        .filter(|o| {
            let b = bounds(o);
            r.contains(b.tl()) && r.contains(b.br())
        })
        .map(|o| o.id)
        .collect()
}

fn near_or_inside(path: Option<tiny_skia::Path>, p: Pt, half_width: f32, filled: bool) -> bool {
    let Some(path) = path else { return false };
    let contours = path::flatten_path(&path, 0.5);
    contours.iter().any(|c| (filled && path::point_in_polygon(p, c)) || dist_to_polyline(p, c, false) <= half_width)
}

fn in_frame(f: &Frame, p: Pt, tol: f32) -> bool {
    let q = p.rotate_around(f.rect.center(), -f.rotation);
    f.rect.inflate(tol).contains(q)
}

/// Does `p` touch this object?
pub fn hits(obj: &Object, p: Pt, tol: f32) -> bool {
    let st = &obj.style;
    let tol = tol.max(0.0);
    let hw = st.width.max(0.0) / 2.0 + tol;
    match &obj.kind {
        Kind::Text(t) => {
            let g = text_geom(t, st);
            match &t.flow {
                TextFlow::Straight => {
                    g.frame.inflate(tol).contains(p)
                        || (t.boxed != TextBox::None
                            && box_paths(t, g.frame).into_iter().any(|bp| near_or_inside(Some(bp), p, tol, true)))
                }
                TextFlow::Arc { .. } => g.ink.inflate(tol).contains(p),
                TextFlow::Path { points, .. } => {
                    g.ink.inflate(tol).contains(p)
                        && (points.len() < 2 || dist_to_polyline(p, points, false) <= st.font.px() + tol)
                }
            }
        }
        Kind::Line(l) => {
            let spine = path::line_spine(l.kind, &l.points, 0.5);
            let extra = match l.decor {
                LineDecor::Railroad => st.width * 1.4,
                LineDecor::Wavy | LineDecor::Zigzag => (st.width * 1.2).max(2.5),
                LineDecor::Double => st.width * 0.4,
                _ => 0.0,
            };
            if dist_to_polyline(p, &spine, false) <= hw.max(3.0_f32.min(hw + 2.0)) + extra {
                return true;
            }
            let ends = [(l.start, spine.first()), (l.end, spine.last())];
            ends.iter().any(|(h, e)| match e {
                Some(e) if *h != ArrowHead::None => {
                    p.dist(**e) <= super::render::line_head_extent(*h, st.width.max(0.25), l.head_size) * 0.7 + tol
                }
                _ => false,
            })
        }
        Kind::Shape(s) => {
            let bracket = matches!(s.kind, ShapeKind::CurlyBracket | ShapeKind::SquareBracket);
            let filled = st.fill.is_visible();
            if bracket {
                near_or_inside(super::render::shape_outline(s, false), p, hw.max(tol + 1.0), false)
                    || (filled && near_or_inside(super::render::shape_outline(s, true), p, 0.0, true))
            } else if filled {
                near_or_inside(super::render::shape_outline(s, true), p, hw, true)
            } else {
                near_or_inside(super::render::shape_outline(s, true), p, hw.max(tol + 1.0), false)
            }
        }
        Kind::Highlight(h) => {
            if h.shape == HighlightShape::Line {
                dist_to_polyline(p, &h.points, false) <= st.width.max(1.0) / 2.0 + tol
            } else {
                near_or_inside(super::render::highlight_outline(h), p, hw, true)
            }
        }
        Kind::Magnifier(m) => m.target.inflate(tol).contains(p) || m.source.inflate(tol).contains(p),
        Kind::Step(s) => near_or_inside(super::render::step_outline(s), p, hw, true),
        Kind::Emoji(e) => in_frame(&e.frame, p, tol),
        Kind::Sticker(s) => in_frame(&s.frame, p, tol),
        Kind::Image(i) => in_frame(&i.frame, p, tol),
        Kind::Cursor(c) => in_frame(&c.frame, p, tol),
        Kind::Blur(b) => {
            let shape = if b.ellipse { path::ellipse_path(b.rect) } else { path::round_rect_path(b.rect, 0.0) };
            near_or_inside(shape, p, tol, true)
        }
        Kind::Eraser(e) => dist_to_polyline(p, &e.points, false) <= e.radius.max(0.0) + tol,
        Kind::Fill(f) => p.dist(f.at) <= 8.0 + tol,
        Kind::Pencil(pe) => dist_to_polyline(p, &pe.points, false) <= st.width.max(1.0) / 2.0 + tol,
    }
}

// ---------------------------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------------------------

/// Compass direction of a resize handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dir {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Dir {
    pub const ALL: [Dir; 8] = [Dir::NW, Dir::N, Dir::NE, Dir::E, Dir::SE, Dir::S, Dir::SW, Dir::W];
    pub const CORNERS: [Dir; 4] = [Dir::NW, Dir::NE, Dir::SE, Dir::SW];

    /// (-1, 0, 1) per axis.
    pub fn signs(self) -> (i8, i8) {
        match self {
            Dir::N => (0, -1),
            Dir::NE => (1, -1),
            Dir::E => (1, 0),
            Dir::SE => (1, 1),
            Dir::S => (0, 1),
            Dir::SW => (-1, 1),
            Dir::W => (-1, 0),
            Dir::NW => (-1, -1),
        }
    }
    pub fn is_corner(self) -> bool {
        let (x, y) = self.signs();
        x != 0 && y != 0
    }
    /// Position of this handle on `r`.
    pub fn point(self, r: RectF) -> Pt {
        let (sx, sy) = self.signs();
        Pt::new(r.x + r.w * (sx as f32 + 1.0) / 2.0, r.y + r.h * (sy as f32 + 1.0) / 2.0)
    }
}

/// Identifies a handle of an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HandleId {
    /// Resize the object's frame (rotated with it).
    Resize(Dir),
    /// A path point: line points (including curve control points), polygon vertices, text path.
    Vertex(usize),
    /// Rotation (frame-based objects: emoji, stickers, images, cursors).
    Rotate,
    /// Callout tail tip.
    TailTip,
    /// Arc text bend / teardrop step direction.
    Bend,
    /// Magnifier source area: resize.
    Source(Dir),
    /// Magnifier source area: move.
    SourceMove,
}

/// How the UI should draw a handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandleKind {
    /// Square on the frame.
    Resize,
    /// Round point on the path.
    Vertex,
    /// Curve control point (draw a guide line to its neighbours).
    Control,
    /// Rotation knob (draw a stem to the top edge).
    Rotate,
    /// Other special handles (diamond).
    Special,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handle {
    pub id: HandleId,
    /// Position in image coordinates.
    pub pos: Pt,
    pub kind: HandleKind,
}

/// Distance of the rotation knob above the frame, in screen pixels.
pub const ROTATE_HANDLE_OFFSET: f32 = 24.0;

fn frame_handles(out: &mut Vec<Handle>, r: RectF, rotation: f32, dirs: &[Dir]) {
    let c = r.center();
    for &d in dirs {
        out.push(Handle {
            id: HandleId::Resize(d),
            pos: d.point(r).rotate_around(c, rotation),
            kind: HandleKind::Resize,
        });
    }
}

fn rotate_handle(out: &mut Vec<Handle>, f: &Frame, zoom: f32) {
    let r = f.rect;
    let z = if zoom > 0.0 && zoom.is_finite() { zoom } else { 1.0 };
    let p = Pt::new(r.center().x, r.y - ROTATE_HANDLE_OFFSET / z).rotate_around(r.center(), f.rotation);
    out.push(Handle { id: HandleId::Rotate, pos: p, kind: HandleKind::Rotate });
}

fn vertex_handles(out: &mut Vec<Handle>, pts: &[Pt], control: impl Fn(usize) -> bool) {
    for (i, p) in pts.iter().enumerate() {
        let kind = if control(i) { HandleKind::Control } else { HandleKind::Vertex };
        out.push(Handle { id: HandleId::Vertex(i), pos: *p, kind });
    }
}

/// Handles for an object. `zoom` only sets the rotation knob's distance.
pub fn handles(obj: &Object, zoom: f32) -> Vec<Handle> {
    let mut out = Vec::new();
    match &obj.kind {
        Kind::Text(t) => match &t.flow {
            TextFlow::Path { points, .. } => vertex_handles(&mut out, points, |_| false),
            TextFlow::Arc { bend } => {
                frame_handles(&mut out, t.rect, 0.0, &Dir::ALL);
                let sag = bend * t.rect.w / 2.0;
                let apex = Pt::new(t.rect.center().x, t.rect.center().y - sag / 2.0);
                out.push(Handle { id: HandleId::Bend, pos: apex, kind: HandleKind::Special });
            }
            TextFlow::Straight => {
                let f = frame(obj);
                frame_handles(&mut out, f, 0.0, &Dir::ALL);
                if let (Some(tail), false) = (t.tail, t.boxed == TextBox::None) {
                    out.push(Handle { id: HandleId::TailTip, pos: tail.tip, kind: HandleKind::Special });
                }
            }
        },
        Kind::Line(l) => {
            let kind = l.kind;
            let n = l.points.len();
            vertex_handles(&mut out, &l.points, |i| match kind {
                LineKind::Quad => i == 1,
                LineKind::Cubic => i == 1 || i == 2,
                _ => false,
            });
            if matches!(kind, LineKind::Straight | LineKind::ElbowH | LineKind::ElbowV) && n > 2 {
                out.retain(|h| matches!(h.id, HandleId::Vertex(i) if i == 0 || i == n - 1));
            }
        }
        Kind::Shape(s) if s.kind == ShapeKind::Polygon => vertex_handles(&mut out, &s.points, |_| false),
        Kind::Highlight(h) if h.shape == HighlightShape::Line && h.points.len() == 2 => {
            vertex_handles(&mut out, &h.points, |_| false)
        }
        Kind::Magnifier(m) => {
            frame_handles(&mut out, m.target, 0.0, &Dir::ALL);
            for d in Dir::CORNERS {
                out.push(Handle { id: HandleId::Source(d), pos: d.point(m.source), kind: HandleKind::Resize });
            }
            out.push(Handle { id: HandleId::SourceMove, pos: m.source.center(), kind: HandleKind::Special });
        }
        Kind::Step(s) => {
            frame_handles(&mut out, frame(obj), 0.0, &Dir::CORNERS);
            if s.shape == StepShape::Teardrop {
                let (sn, cs) = s.angle.to_radians().sin_cos();
                let tip = s.center + Pt::new(cs, sn) * (s.size / 2.0 * 1.5);
                out.push(Handle { id: HandleId::Bend, pos: tip, kind: HandleKind::Special });
            }
        }
        Kind::Emoji(Emoji { frame: f, .. })
        | Kind::Sticker(Sticker { frame: f, .. })
        | Kind::Image(Picture { frame: f, .. })
        | Kind::Cursor(Cursor { frame: f, .. }) => {
            frame_handles(&mut out, f.rect, f.rotation, &Dir::ALL);
            rotate_handle(&mut out, f, zoom);
        }
        Kind::Fill(_) => {}
        _ => frame_handles(&mut out, frame(obj), 0.0, &Dir::ALL),
    }
    out
}

/// The handle under `p` (within `radius` image pixels), topmost first.
pub fn handle_at(obj: &Object, p: Pt, radius: f32, zoom: f32) -> Option<HandleId> {
    handles(obj, zoom)
        .into_iter()
        .rev()
        .filter(|h| h.pos.dist(p) <= radius)
        .min_by(|a, b| a.pos.dist(p).total_cmp(&b.pos.dist(p)))
        .map(|h| h.id)
}

/// Modifier-key behaviour for drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DragConstraints {
    /// Shift on a corner: keep the aspect ratio.
    pub keep_aspect: bool,
    /// Alt/Ctrl: resize symmetrically around the centre.
    pub from_center: bool,
    /// Shift on points and rotation: snap angles (45° for points, 15° for rotation).
    pub snap_angle: bool,
}

/// Resize an axis-aligned rect by dragging handle `dir` by `d`.
pub fn resize_rect(r: RectF, dir: Dir, d: Pt, c: &DragConstraints) -> RectF {
    let (sx, sy) = dir.signs();
    let (sx, sy) = (sx as f32, sy as f32);
    let center = r.center();
    // Moving point and anchor in "signed" form.
    let half = Pt::new(r.w / 2.0, r.h / 2.0);
    let mut hx = half.x + sx * d.x * if c.from_center { 1.0 } else { 0.5 };
    let mut hy = half.y + sy * d.y * if c.from_center { 1.0 } else { 0.5 };
    if c.keep_aspect && r.w > 1e-3 && r.h > 1e-3 {
        let aspect = r.w / r.h;
        if dir.is_corner() {
            let s = (hx.abs() / half.x).max(hy.abs() / half.y);
            hx = half.x * s * hx.signum();
            hy = half.y * s * hy.signum();
        } else if sx == 0.0 {
            hx = hy.abs() * aspect;
        } else {
            hy = hx.abs() / aspect;
        }
    }
    let new_center = if c.from_center {
        center
    } else {
        // The opposite side stays put.
        let anchor = Pt::new(center.x - sx * half.x, center.y - sy * half.y);
        Pt::new(
            if sx == 0.0 { center.x } else { anchor.x + sx * hx },
            if sy == 0.0 { center.y } else { anchor.y + sy * hy },
        )
    };
    RectF::from_center(new_center, (hx * 2.0).abs(), (hy * 2.0).abs())
}

/// Resize a rotated frame so the opposite handle stays fixed on screen.
fn resize_frame(f: &mut Frame, dir: Dir, d: Pt, c: &DragConstraints) {
    let center = f.rect.center();
    let local_d = d.rotate_around(Pt::default(), -f.rotation);
    let local = RectF::from_center(Pt::default(), f.rect.w, f.rect.h);
    let nl = resize_rect(local, dir, local_d, c);
    let nc = center + nl.center().rotate_around(Pt::default(), f.rotation);
    f.rect = RectF::from_center(nc, nl.w, nl.h);
}

/// Move one handle by `delta` (image pixels). See the module docs for snapshot-based dragging.
pub fn drag_handle(obj: &mut Object, handle: HandleId, delta: Pt, c: &DragConstraints) {
    if !delta.is_finite() {
        return;
    }
    let current = handles(obj, 1.0).into_iter().find(|h| h.id == handle).map(|h| h.pos);
    let old_frame = frame(obj);
    match (handle, &mut obj.kind) {
        (HandleId::Resize(dir), Kind::Emoji(Emoji { frame: f, .. }))
        | (HandleId::Resize(dir), Kind::Sticker(Sticker { frame: f, .. }))
        | (HandleId::Resize(dir), Kind::Image(Picture { frame: f, .. }))
        | (HandleId::Resize(dir), Kind::Cursor(Cursor { frame: f, .. })) => resize_frame(f, dir, delta, c),
        (HandleId::Resize(dir), Kind::Step(s)) => {
            let c2 = DragConstraints { keep_aspect: true, ..*c };
            let r = resize_rect(old_frame, dir, delta, &c2);
            s.center = r.center();
            s.size = r.w.min(r.h).max(4.0);
        }
        (HandleId::Resize(dir), _) => {
            let r = resize_rect(old_frame, dir, delta, c);
            set_frame(obj, r);
        }
        (HandleId::Vertex(i), kind) => {
            let pts: Option<&mut Vec<Pt>> = match kind {
                Kind::Line(l) => Some(&mut l.points),
                Kind::Shape(s) => Some(&mut s.points),
                Kind::Highlight(h) => Some(&mut h.points),
                Kind::Text(Text { flow: TextFlow::Path { points, .. }, .. }) => Some(points),
                _ => None,
            };
            if let Some(pts) = pts {
                if i < pts.len() {
                    let mut p = pts[i] + delta;
                    if c.snap_angle && pts.len() >= 2 {
                        let anchor = if i == 0 { pts[1] } else { pts[i - 1] };
                        p = path::snap_angle(anchor, p, 45.0);
                    }
                    pts[i] = p;
                }
            }
        }
        (HandleId::Rotate, kind) => {
            let f = match kind {
                Kind::Emoji(e) => &mut e.frame,
                Kind::Sticker(s) => &mut s.frame,
                Kind::Image(p) => &mut p.frame,
                Kind::Cursor(cu) => &mut cu.frame,
                _ => return,
            };
            let Some(pos) = current else { return };
            let target = pos + delta;
            let mut a = (target - f.rect.center()).angle_deg() + 90.0;
            if c.snap_angle {
                a = (a / 15.0).round() * 15.0;
            }
            f.rotation = normalize_deg(a);
        }
        (HandleId::TailTip, Kind::Text(t)) => {
            if let Some(tail) = &mut t.tail {
                tail.tip += delta;
            }
        }
        (HandleId::Bend, Kind::Text(t)) => {
            if let TextFlow::Arc { bend } = &mut t.flow {
                let apex_y = current.map(|p| p.y).unwrap_or(t.rect.center().y) + delta.y;
                let sag = 2.0 * (t.rect.center().y - apex_y);
                *bend = (sag / (t.rect.w / 2.0).max(1.0)).clamp(-1.0, 1.0);
            }
        }
        (HandleId::Bend, Kind::Step(s)) => {
            if let Some(pos) = current {
                let mut a = (pos + delta - s.center).angle_deg();
                if c.snap_angle {
                    a = (a / 45.0).round() * 45.0;
                }
                s.angle = normalize_deg(a);
            }
        }
        (HandleId::Source(dir), Kind::Magnifier(m)) => m.source = resize_rect(m.source, dir, delta, c),
        (HandleId::SourceMove, Kind::Magnifier(m)) => m.source = m.source.translate(delta),
        _ => {}
    }
}

fn normalize_deg(a: f32) -> f32 {
    let a = a.rem_euclid(360.0);
    if a > 180.0 {
        a - 360.0
    } else {
        a
    }
}

/// Shift-drag helper: snap `p` to horizontal, vertical or 45° from `anchor`
/// (e.g. a straight line-highlighter or line while Shift/Ctrl is held).
pub fn constrain_to_axis(anchor: Pt, p: Pt) -> Pt {
    path::snap_angle(anchor, p, 45.0)
}

/// Snap `p` to purely horizontal or vertical from `anchor`, whichever is closer.
pub fn constrain_to_hv(anchor: Pt, p: Pt) -> Pt {
    let d = p - anchor;
    if d.x.abs() >= d.y.abs() {
        Pt::new(p.x, anchor.y)
    } else {
        Pt::new(anchor.x, p.y)
    }
}

// ---------------------------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------------------------

fn map_points(pts: &mut [Pt], from: &RectF, to: &RectF) {
    for p in pts {
        *p = from.map_to(*p, to);
    }
}

/// Move an object by `d`.
pub fn translate(obj: &mut Object, d: Pt) {
    if !d.is_finite() {
        return;
    }
    let shift = |pts: &mut Vec<Pt>| pts.iter_mut().for_each(|p| *p += d);
    match &mut obj.kind {
        Kind::Text(t) => {
            t.rect = t.rect.translate(d);
            if let Some(tail) = &mut t.tail {
                tail.tip += d;
            }
            if let TextFlow::Path { points, .. } = &mut t.flow {
                shift(points);
            }
        }
        Kind::Line(l) => shift(&mut l.points),
        Kind::Shape(s) => {
            s.rect = s.rect.translate(d);
            shift(&mut s.points);
        }
        Kind::Highlight(h) => {
            h.rect = h.rect.translate(d);
            shift(&mut h.points);
        }
        Kind::Magnifier(m) => m.target = m.target.translate(d),
        Kind::Step(s) => s.center += d,
        Kind::Emoji(e) => e.frame.rect = e.frame.rect.translate(d),
        Kind::Sticker(s) => s.frame.rect = s.frame.rect.translate(d),
        Kind::Image(p) => p.frame.rect = p.frame.rect.translate(d),
        Kind::Cursor(c) => c.frame.rect = c.frame.rect.translate(d),
        Kind::Blur(b) => b.rect = b.rect.translate(d),
        Kind::Eraser(e) => shift(&mut e.points),
        Kind::Fill(f) => f.at += d,
        Kind::Pencil(p) => shift(&mut p.points),
    }
}

/// Fit an object's editable frame (see [`frame`]) to `r`, scaling its geometry.
pub fn set_frame(obj: &mut Object, r: RectF) {
    if !r.is_finite() {
        return;
    }
    let old = frame(obj);
    match &mut obj.kind {
        Kind::Text(t) => match &mut t.flow {
            TextFlow::Path { points, .. } => map_points(points, &old, &r),
            _ => t.rect = r,
        },
        Kind::Line(l) => map_points(&mut l.points, &old, &r),
        Kind::Shape(s) => {
            if s.kind == ShapeKind::Polygon {
                map_points(&mut s.points, &old, &r);
            }
            s.rect = r;
        }
        Kind::Highlight(h) => {
            if h.shape == HighlightShape::Line {
                map_points(&mut h.points, &old, &r);
            }
            h.rect = r;
        }
        Kind::Magnifier(m) => m.target = r,
        Kind::Step(s) => {
            s.center = r.center();
            s.size = r.w.min(r.h).max(4.0);
        }
        Kind::Emoji(e) => e.frame.rect = r,
        Kind::Sticker(s) => s.frame.rect = r,
        Kind::Image(p) => p.frame.rect = r,
        Kind::Cursor(c) => c.frame.rect = r,
        Kind::Blur(b) => b.rect = r,
        Kind::Eraser(e) => map_points(&mut e.points, &old, &r),
        Kind::Fill(f) => f.at = r.center(),
        Kind::Pencil(p) => map_points(&mut p.points, &old, &r),
    }
}

/// Move several objects. Returns how many moved.
pub fn move_objects(objects: &mut [Object], ids: &[ObjectId], d: Pt) -> usize {
    let mut n = 0;
    for o in objects.iter_mut().filter(|o| ids.contains(&o.id)) {
        translate(o, d);
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------------------------
// Alignment and z-order
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Align {
    Left,
    Right,
    Top,
    Bottom,
    /// Centres on the same vertical line.
    CenterH,
    /// Centres on the same horizontal line.
    CenterV,
    /// Same width as the first object in `ids`.
    SameWidth,
    SameHeight,
    SameSize,
}

/// Align the objects in `ids` by their frames. Edges align to the selection's extreme edge;
/// sizes match the first id (the "key" object). Returns true if anything changed.
pub fn align(objects: &mut [Object], ids: &[ObjectId], how: Align) -> bool {
    let frames: Vec<(ObjectId, RectF)> =
        ids.iter().filter_map(|id| find(objects, *id).map(|o| (*id, frame(o)))).collect();
    if frames.len() < 2 {
        return false;
    }
    let key = frames[0].1;
    let all = frames.iter().map(|f| f.1).reduce(|a, b| a.union(&b)).unwrap_or(key);
    let mut changed = false;
    for (id, f) in &frames {
        let Some(o) = find_mut(objects, *id) else { continue };
        let before = o.clone();
        match how {
            Align::Left => translate(o, Pt::new(all.x - f.x, 0.0)),
            Align::Right => translate(o, Pt::new(all.right() - f.right(), 0.0)),
            Align::Top => translate(o, Pt::new(0.0, all.y - f.y)),
            Align::Bottom => translate(o, Pt::new(0.0, all.bottom() - f.bottom())),
            Align::CenterH => translate(o, Pt::new(key.center().x - f.center().x, 0.0)),
            Align::CenterV => translate(o, Pt::new(0.0, key.center().y - f.center().y)),
            Align::SameWidth => set_frame(o, RectF::new(f.x, f.y, key.w, f.h)),
            Align::SameHeight => set_frame(o, RectF::new(f.x, f.y, f.w, key.h)),
            Align::SameSize => set_frame(o, RectF::new(f.x, f.y, key.w, key.h)),
        }
        changed |= *o != before;
    }
    changed
}

/// Move the objects to the top of the z-order (keeping their relative order).
pub fn bring_to_front(objects: &mut Vec<Object>, ids: &[ObjectId]) -> bool {
    let before: Vec<ObjectId> = objects.iter().map(|o| o.id).collect();
    let (sel, rest): (Vec<Object>, Vec<Object>) = objects.drain(..).partition(|o| ids.contains(&o.id));
    objects.extend(rest);
    objects.extend(sel);
    objects.iter().map(|o| o.id).ne(before)
}

/// Move the objects to the bottom of the z-order (keeping their relative order).
pub fn send_to_back(objects: &mut Vec<Object>, ids: &[ObjectId]) -> bool {
    let before: Vec<ObjectId> = objects.iter().map(|o| o.id).collect();
    let (sel, rest): (Vec<Object>, Vec<Object>) = objects.drain(..).partition(|o| ids.contains(&o.id));
    objects.extend(sel);
    objects.extend(rest);
    objects.iter().map(|o| o.id).ne(before)
}

/// Move each selected object one step up (past the next unselected object).
pub fn bring_forward(objects: &mut [Object], ids: &[ObjectId]) -> bool {
    let mut changed = false;
    for i in (0..objects.len().saturating_sub(1)).rev() {
        if ids.contains(&objects[i].id) && !ids.contains(&objects[i + 1].id) {
            objects.swap(i, i + 1);
            changed = true;
        }
    }
    changed
}

/// Move each selected object one step down.
pub fn send_backward(objects: &mut [Object], ids: &[ObjectId]) -> bool {
    let mut changed = false;
    for i in 1..objects.len() {
        if ids.contains(&objects[i].id) && !ids.contains(&objects[i - 1].id) {
            objects.swap(i, i - 1);
            changed = true;
        }
    }
    changed
}

/// Remove objects; returns the removed ones in z-order.
pub fn delete(objects: &mut Vec<Object>, ids: &[ObjectId]) -> Vec<Object> {
    let (gone, keep): (Vec<Object>, Vec<Object>) = objects.drain(..).partition(|o| ids.contains(&o.id));
    *objects = keep;
    gone
}

// ---------------------------------------------------------------------------------------------
// Group style edits
// ---------------------------------------------------------------------------------------------

/// A partial style edit applied to many objects at once; `None` fields are left unchanged.
/// Line-only and text-only fields are ignored for objects that don't have them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StylePatch {
    pub color: Option<Color>,
    pub fill: Option<Color>,
    pub width: Option<f32>,
    pub opacity: Option<f32>,
    pub dash: Option<Dash>,
    pub shadow: Option<Option<Shadow>>,
    pub outline: Option<Option<Outline>>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub align: Option<TextAlign>,
    pub text_color: Option<Color>,
    pub head_start: Option<ArrowHead>,
    pub head_end: Option<ArrowHead>,
    pub head_size: Option<f32>,
    pub decor: Option<LineDecor>,
    pub color2: Option<Color>,
    pub text_box: Option<TextBox>,
    pub timing: Option<Option<Timing>>,
}

impl StylePatch {
    /// A patch that copies every [`Style`] field (format painter).
    #[cfg(test)]
    pub fn from_style(s: &Style) -> Self {
        Self {
            color: Some(s.color),
            fill: Some(s.fill),
            width: Some(s.width),
            opacity: Some(s.opacity),
            dash: Some(s.dash),
            shadow: Some(s.shadow),
            outline: Some(s.outline),
            font_family: Some(s.font.family.clone()),
            font_size: Some(s.font.size),
            bold: Some(s.font.bold),
            italic: Some(s.font.italic),
            underline: Some(s.font.underline),
            align: Some(s.font.align),
            text_color: Some(s.font.color),
            ..Default::default()
        }
    }

    /// Apply to one object; returns true if it changed.
    pub fn apply(&self, o: &mut Object) -> bool {
        let before = o.clone();
        let st = &mut o.style;
        set(&mut st.color, &self.color);
        set(&mut st.fill, &self.fill);
        if let Some(w) = self.width.filter(|w| w.is_finite()) {
            st.width = w.clamp(0.0, 500.0);
        }
        if let Some(a) = self.opacity.filter(|a| a.is_finite()) {
            st.opacity = a.clamp(0.0, 1.0);
        }
        set(&mut st.dash, &self.dash);
        set(&mut st.shadow, &self.shadow);
        set(&mut st.outline, &self.outline);
        set(&mut st.font.family, &self.font_family);
        if let Some(s) = self.font_size.filter(|s| s.is_finite()) {
            st.font.size = s.clamp(1.0, Font::MAX_SIZE);
        }
        set(&mut st.font.bold, &self.bold);
        set(&mut st.font.italic, &self.italic);
        set(&mut st.font.underline, &self.underline);
        set(&mut st.font.align, &self.align);
        set(&mut st.font.color, &self.text_color);
        set(&mut o.timing, &self.timing);
        match &mut o.kind {
            Kind::Line(l) => {
                set(&mut l.start, &self.head_start);
                set(&mut l.end, &self.head_end);
                if let Some(s) = self.head_size.filter(|s| s.is_finite()) {
                    l.head_size = s.clamp(0.2, 5.0);
                }
                set(&mut l.decor, &self.decor);
                set(&mut l.color2, &self.color2);
            }
            Kind::Text(t) => set(&mut t.boxed, &self.text_box),
            _ => {}
        }
        *o != before
    }
}

fn set<T: Clone>(dst: &mut T, v: &Option<T>) {
    if let Some(v) = v {
        *dst = v.clone();
    }
}

/// Apply a style patch to the objects in `ids`. Returns how many changed.
#[cfg(test)]
pub fn apply_style(objects: &mut [Object], ids: &[ObjectId], patch: &StylePatch) -> usize {
    objects.iter_mut().filter(|o| ids.contains(&o.id)).map(|o| patch.apply(o)).filter(|c| *c).count()
}

// ---------------------------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------------------------

/// The value for the next step stamp: continues the format of the most recent step (number or
/// letter) from the highest value of that format. `Number(1)` when there are no steps.
#[cfg(test)]
pub fn next_step_value(objects: &[Object]) -> StepValue {
    let last = objects.iter().rev().find_map(|o| match &o.kind {
        Kind::Step(s) => Some(s.value),
        _ => None,
    });
    match last {
        None => StepValue::Number(1),
        Some(StepValue::Number(_)) => next_step_value_as(objects, false),
        Some(StepValue::Letter(_)) => next_step_value_as(objects, true),
    }
}

/// Next value in a specific format (`letters`: A, B, ... else 1, 2, ...).
pub fn next_step_value_as(objects: &[Object], letters: bool) -> StepValue {
    let values = objects.iter().filter_map(|o| match &o.kind {
        Kind::Step(s) => Some(s.value),
        _ => None,
    });
    if letters {
        values
            .filter_map(|v| if let StepValue::Letter(i) = v { Some(i) } else { None })
            .max()
            .map_or(StepValue::Letter(0), |i| StepValue::Letter(i.saturating_add(1)))
    } else {
        values
            .filter_map(|v| if let StepValue::Number(n) = v { Some(n) } else { None })
            .max()
            .map_or(StepValue::Number(1), |n| StepValue::Number((n + 1).min(StepValue::MAX_NUMBER)))
    }
}

// ---------------------------------------------------------------------------------------------
// Copy / paste
// ---------------------------------------------------------------------------------------------

/// Clipboard payload version (same object schema as `.sfcap`'s `annotations.json`).
pub const CLIPBOARD_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ClipPayload {
    version: u32,
    #[serde(default)]
    app: String,
    objects: Vec<Object>,
}

/// JSON for the selected objects (in z-order).
pub fn serialize_selection(objects: &[Object], ids: &[ObjectId]) -> String {
    let payload = ClipPayload {
        version: CLIPBOARD_VERSION,
        app: "SnapForge".into(),
        objects: objects.iter().filter(|o| ids.contains(&o.id)).cloned().collect(),
    };
    serde_json::to_string(&payload).unwrap_or_else(|_| "{\"version\":1,\"objects\":[]}".into())
}

/// Paste objects from [`serialize_selection`] JSON on top, offset by `offset`, with fresh ids.
/// Returns the new ids (to select them).
#[allow(clippy::explicit_counter_loop)]
pub fn paste(objects: &mut Vec<Object>, json: &str, offset: Pt) -> Result<Vec<ObjectId>> {
    let payload: ClipPayload =
        serde_json::from_str(json).context("clipboard does not contain SnapForge annotations")?;
    if payload.version > CLIPBOARD_VERSION {
        bail!("annotations come from a newer SnapForge (format {})", payload.version);
    }
    let mut id = next_id(objects);
    let mut out = Vec::with_capacity(payload.objects.len());
    for mut o in payload.objects {
        o.id = id;
        id += 1;
        translate(&mut o, offset);
        out.push(o.id);
        objects.push(o);
    }
    Ok(out)
}

/// Duplicate objects in place (copies go on top, offset). Returns the new ids.
pub fn duplicate(objects: &mut Vec<Object>, ids: &[ObjectId], offset: Pt) -> Vec<ObjectId> {
    let mut id = next_id(objects);
    let copies: Vec<Object> = objects
        .iter()
        .filter(|o| ids.contains(&o.id))
        .map(|o| {
            let mut c = o.clone();
            c.id = id;
            id += 1;
            translate(&mut c, offset);
            c
        })
        .collect();
    let new_ids = copies.iter().map(|o| o.id).collect();
    objects.extend(copies);
    new_ids
}
