//! The image canvas: zoom/pan, selection tools, floating pixels, image-edge resize
//! handles and painting (checkerboard, tiles, marching ants).

use super::commands::ToolId;
use super::document::{Document, TexDirty};
use super::ext::{CanvasTool, ToolInput};
use super::selection::{self, drag_rect, edge_drag_bounds, push_point, CanvasMap, Edge, Selection};
use super::texture::{image_options, tile_to_color_image, ImageTextures, TileRect};
use crate::common::Rect as IRect;
use eframe::egui::{
    self, pos2, vec2, Color32, CursorIcon, PointerButton, Pos2, Rect, Sense, Shape, Stroke, TextureHandle,
};
use std::sync::Arc;

/// Physical px per checkerboard cell.
const CHECKER_CELL: f32 = 8.0;
/// Edge handle half-size and hit slop, in points.
const HANDLE: f32 = 4.0;
const HANDLE_HIT: f32 = 7.0;

/// An in-progress canvas drag.
#[derive(Default)]
pub enum Drag {
    #[default]
    None,
    Pan,
    SelectRect {
        start: Pos2,
    },
    Freehand {
        points: Vec<[f32; 2]>,
    },
    /// Dragging floating pixels; `grab` is the image point grabbed.
    MoveFloating {
        grab: [f64; 2],
        moved: (i32, i32),
    },
    Edge {
        edge: Edge,
        start: Pos2,
        bounds: IRect,
    },
    /// Forwarded to an external canvas tool.
    Tool,
}

/// Canvas state that outlives a frame (owned by the app).
#[derive(Default)]
pub struct CanvasState {
    pub drag: Drag,
    /// Texture of the floating pixels: (Arc pointer identity, version, texture).
    float_tex: Option<(usize, u64, TextureHandle)>,
}

impl CanvasState {
    pub fn is_dragging(&self) -> bool {
        !matches!(self.drag, Drag::None)
    }
}

/// Per-frame inputs from the app.
pub struct CanvasCtx<'a> {
    pub tool: &'a ToolId,
    pub external: Option<&'a mut dyn CanvasTool>,
    /// Space is held (temporary pan).
    pub space_down: bool,
    /// Premultiplied BGRA fill for moved pixels.
    pub background: [u8; 4],
    pub checker: &'a TextureHandle,
}

pub struct CanvasOutput {
    /// The document's pixels were painted this frame (bench hook).
    pub painted: bool,
    /// Pixel under the pointer, if over the image.
    pub hover_px: Option<(i32, i32)>,
    /// Name of an edit to report (e.g. canvas resized) — for the toast / status.
    pub edited: Option<&'static str>,
}

/// Pan, zoom and tool input, then paint.
pub fn show(ui: &mut egui::Ui, doc: &mut Document, st: &mut CanvasState, cx: CanvasCtx<'_>) -> CanvasOutput {
    let ctx = ui.ctx().clone();
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let ppp = ctx.pixels_per_point();
    let view_px = [rect.width() * ppp, rect.height() * ppp];
    let img_px = doc.size();
    doc.view.layout(img_px, view_px);
    let mut out = CanvasOutput { painted: false, hover_px: None, edited: None };

    // ---- zoom / scroll
    let tool_wheel = cx.external.as_ref().is_some_and(|t| t.wants_wheel());
    let mut tool_scroll = 0.0;
    if resp.hovered() {
        let (zoom, scroll, pointer) = ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom != 1.0 {
            let anchor = pointer.map(|p| (p - rect.min) * ppp).unwrap_or(vec2(view_px[0], view_px[1]) / 2.0);
            let z = doc.view.zoom * zoom;
            doc.view.zoom_around(z, [anchor.x, anchor.y], img_px, view_px);
        } else if tool_wheel {
            // Raw wheel notches, not the smoothed delta: one notch = one step.
            tool_scroll = ctx.input(|i| i.raw_scroll_delta.y);
        } else if scroll != egui::Vec2::ZERO {
            let d = scroll * ppp;
            doc.view.pan([d.x, d.y], img_px, view_px);
        }
    }

    let map = canvas_map(rect, doc, ppp);
    let img_rect = image_rect(&map, doc);
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    if let Some(p) = pointer.filter(|p| rect.contains(*p)) {
        let (x, y) = map.pixel_at([p.x, p.y]);
        if x >= 0 && y >= 0 && (x as u32) < doc.image.width && (y as u32) < doc.image.height {
            out.hover_px = Some((x, y));
        }
    }

    // ---- input
    let pan_tool = *cx.tool == ToolId::Pan;
    let handles_on =
        cx.external.is_none() && !matches!(cx.tool, ToolId::External(_)) && doc.floating.is_none() && !doc.busy();
    let hovered_edge = if handles_on && !st.is_dragging() { pointer.and_then(|p| edge_at(img_rect, p)) } else { None };
    let mut external = cx.external;
    handle_input(
        &ctx,
        &resp,
        doc,
        st,
        &map,
        cx.space_down,
        pan_tool,
        hovered_edge,
        cx.tool,
        cx.background,
        &mut external,
        tool_scroll,
        &mut out,
    );

    // Cursor feedback.
    match (&st.drag, hovered_edge) {
        (Drag::Pan, _) => ctx.set_cursor_icon(CursorIcon::Grabbing),
        (Drag::Edge { edge, .. }, _) => ctx.set_cursor_icon(edge_cursor(*edge)),
        (Drag::None, Some(edge)) => ctx.set_cursor_icon(edge_cursor(edge)),
        (Drag::MoveFloating { .. }, _) => ctx.set_cursor_icon(CursorIcon::Move),
        (Drag::None, None) if resp.hovered() => {
            if cx.space_down || pan_tool {
                ctx.set_cursor_icon(CursorIcon::Grab);
            } else if let Some(t) = external.as_ref() {
                ctx.set_cursor_icon(t.cursor());
            } else if out.hover_px.is_some_and(|(x, y)| over_selection(doc, x, y)) {
                ctx.set_cursor_icon(CursorIcon::Move);
            } else {
                ctx.set_cursor_icon(CursorIcon::Crosshair);
            }
        }
        _ => {}
    }

    // ---- textures (first display, after an edit: only the changed rectangle)
    sync_textures(&ctx, doc);

    // ---- paint
    let map = canvas_map(rect, doc, ppp);
    let img_rect = image_rect(&map, doc);
    let painter = ui.painter_at(rect);
    let shadow = egui::Shadow { offset: [0, 2], blur: 12, spread: 0, color: Color32::from_black_alpha(50) };
    painter.add(shadow.as_shape(img_rect, 0));

    let visible = img_rect.intersect(rect);
    let checker_needed = doc.has_alpha || doc.floating.as_ref().is_some_and(|f| f.lifted.is_some());
    if checker_needed && visible.is_positive() {
        let cells = visible.size() * ppp / (CHECKER_CELL * 2.0);
        let uv = Rect::from_min_size(pos2(0.0, 0.0), cells);
        painter.image(cx.checker.id(), visible, uv, Color32::WHITE);
    }

    if let Some(tex) = &doc.textures {
        let k = map.scale;
        for tile in &tex.tiles {
            let r = Rect::from_min_size(
                img_rect.min + vec2(tile.rect.x as f32, tile.rect.y as f32) * k,
                vec2(tile.rect.w as f32, tile.rect.h as f32) * k,
            );
            if r.intersects(rect) {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(tile.tex.id(), r, uv, Color32::WHITE);
                out.painted = true;
            }
        }
    }

    paint_floating(&ctx, &painter, doc, st, &map);
    if let Some(o) = doc.overlay.as_mut() {
        o.paint(&ctx, &painter, &map, &doc.image);
    }

    // Selection outline (marching ants).
    let time = ctx.input(|i| i.time);
    if let Some(sel) = &doc.selection {
        let pts: Vec<Pos2> = sel.outline.iter().map(|p| to_pos(&map, *p)).collect();
        marching_ants(&painter, &pts, true, time);
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }
    match &st.drag {
        Drag::SelectRect { start } => {
            if let Some(p) = pointer {
                let r = drag_rect(&map, [start.x, start.y], [p.x, p.y], doc.image.bounds());
                if let Some(s) = Selection::rect(r) {
                    let pts: Vec<Pos2> = s.outline.iter().map(|q| to_pos(&map, *q)).collect();
                    marching_ants(&painter, &pts, true, time);
                }
            }
        }
        Drag::Freehand { points } => {
            let pts: Vec<Pos2> = points.iter().map(|q| to_pos(&map, *q)).collect();
            marching_ants(&painter, &pts, false, time);
        }
        Drag::Edge { bounds, .. } => {
            let r = Rect::from_min_max(
                to_pos(&map, [bounds.x as f32, bounds.y as f32]),
                to_pos(&map, [bounds.right() as f32, bounds.bottom() as f32]),
            );
            painter.rect_stroke(r, 0.0, Stroke::new(1.5_f32, super::style::ACCENT), egui::StrokeKind::Outside);
            painter.text(
                r.center_bottom() + vec2(0.0, 6.0),
                egui::Align2::CENTER_TOP,
                format!("{} × {}", bounds.w, bounds.h),
                egui::FontId::proportional(12.0),
                super::style::ACCENT,
            );
        }
        _ => {}
    }

    if let Some(t) = external.as_mut() {
        t.paint(doc, &painter, &map);
    }

    // Edge handles (canvas size by dragging).
    if handles_on && !matches!(st.drag, Drag::SelectRect { .. } | Drag::Freehand { .. }) {
        for e in Edge::ALL {
            let c = handle_pos(img_rect, e);
            let hot = hovered_edge == Some(e) || matches!(&st.drag, Drag::Edge { edge, .. } if *edge == e);
            let r = Rect::from_center_size(c, vec2(HANDLE * 2.0, HANDLE * 2.0));
            painter.rect_filled(r, 1.5, if hot { super::style::ACCENT } else { Color32::WHITE });
            painter.rect_stroke(
                r,
                1.5,
                Stroke::new(1.0_f32, Color32::from_black_alpha(140)),
                egui::StrokeKind::Outside,
            );
        }
    }
    out
}

fn canvas_map(rect: Rect, doc: &Document, ppp: f32) -> CanvasMap {
    // Snap the image origin to whole physical pixels so 100% is pixel-exact.
    let origin_px = (rect.min.to_vec2() * ppp).round() + vec2(doc.view.offset[0], doc.view.offset[1]).round();
    CanvasMap { origin: [origin_px.x / ppp, origin_px.y / ppp], scale: doc.view.zoom / ppp }
}

fn image_rect(map: &CanvasMap, doc: &Document) -> Rect {
    Rect::from_min_size(
        pos2(map.origin[0], map.origin[1]),
        vec2(doc.image.width as f32, doc.image.height as f32) * map.scale,
    )
}

fn to_pos(map: &CanvasMap, p: [f32; 2]) -> Pos2 {
    let s = map.to_screen([p[0] as f64, p[1] as f64]);
    pos2(s[0], s[1])
}

fn over_selection(doc: &Document, x: i32, y: i32) -> bool {
    match &doc.floating {
        Some(f) => {
            f.selection.contains(x, y) || f.rect().contains(crate::common::Point::new(x, y)) && f.selection.is_rect()
        }
        None => doc.selection.as_ref().is_some_and(|s| s.contains(x, y)),
    }
}

fn handle_pos(img_rect: Rect, e: Edge) -> Pos2 {
    let a = e.anchor();
    pos2(img_rect.left() + img_rect.width() * a[0], img_rect.top() + img_rect.height() * a[1])
}

fn edge_at(img_rect: Rect, p: Pos2) -> Option<Edge> {
    // Corners first so they win where they overlap the sides.
    let order = [
        Edge::TopLeft,
        Edge::TopRight,
        Edge::BottomLeft,
        Edge::BottomRight,
        Edge::Left,
        Edge::Right,
        Edge::Top,
        Edge::Bottom,
    ];
    order.into_iter().find(|&e| handle_pos(img_rect, e).distance(p) <= HANDLE_HIT)
}

fn edge_cursor(e: Edge) -> CursorIcon {
    match e {
        Edge::Left | Edge::Right => CursorIcon::ResizeHorizontal,
        Edge::Top | Edge::Bottom => CursorIcon::ResizeVertical,
        Edge::TopLeft | Edge::BottomRight => CursorIcon::ResizeNwSe,
        Edge::TopRight | Edge::BottomLeft => CursorIcon::ResizeNeSw,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_input(
    ctx: &egui::Context,
    resp: &egui::Response,
    doc: &mut Document,
    st: &mut CanvasState,
    map: &CanvasMap,
    space_down: bool,
    pan_tool: bool,
    hovered_edge: Option<Edge>,
    tool: &ToolId,
    background: [u8; 4],
    external: &mut Option<&mut dyn CanvasTool>,
    tool_scroll: f32,
    out: &mut CanvasOutput,
) {
    let ppp = ctx.pixels_per_point();
    let img_px = doc.size();
    let view_px = [resp.rect.width() * ppp, resp.rect.height() * ppp];
    let (pointer, press_origin, mods, primary_down, primary_released, primary_pressed) = ctx.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.press_origin(),
            i.modifiers,
            i.pointer.primary_down(),
            i.pointer.primary_released(),
            i.pointer.primary_pressed(),
        )
    });

    // Middle-drag always pans.
    if resp.dragged_by(PointerButton::Middle) {
        let d = resp.drag_delta() * ppp;
        doc.view.pan([d.x, d.y], img_px, view_px);
        return;
    }

    if resp.drag_started_by(PointerButton::Primary) {
        let start = press_origin.or(pointer).unwrap_or(resp.rect.center());
        let (px, py) = map.pixel_at([start.x, start.y]);
        st.drag = if space_down || pan_tool {
            Drag::Pan
        } else if let Some(edge) = hovered_edge {
            Drag::Edge { edge, start, bounds: doc.image.bounds() }
        } else if external.is_some() {
            Drag::Tool
        } else if doc.floating.is_some() && over_selection(doc, px, py) {
            Drag::MoveFloating { grab: map.to_image([start.x, start.y]), moved: (0, 0) }
        } else if doc.selection.as_ref().is_some_and(|s| s.contains(px, py)) && !doc.busy() {
            // Drag inside the selection: move it (Ctrl: copy it).
            if doc.lift_selection(mods.ctrl || mods.command, background) {
                Drag::MoveFloating { grab: map.to_image([start.x, start.y]), moved: (0, 0) }
            } else {
                Drag::None
            }
        } else {
            doc.deselect();
            match tool {
                ToolId::SelectFree => {
                    let p = map.to_image([start.x, start.y]);
                    Drag::Freehand { points: vec![[p[0] as f32, p[1] as f32]] }
                }
                _ => Drag::SelectRect { start },
            }
        };
    }

    // External tools get every frame's pointer state while active.
    if let Some(t) = external.as_mut() {
        if matches!(st.drag, Drag::Tool | Drag::None) && !space_down && hovered_edge.is_none()
            || matches!(st.drag, Drag::Tool)
        {
            let input = ToolInput {
                map: *map,
                pos: pointer.map(|p| map.to_image([p.x, p.y])),
                pressed: primary_pressed && resp.hovered(),
                down: primary_down && (resp.is_pointer_button_down_on() || matches!(st.drag, Drag::Tool)),
                released: primary_released,
                double_clicked: resp.double_clicked(),
                modifiers: mods,
                scroll: tool_scroll,
                hovered: resp.hovered(),
            };
            if resp.hovered() || matches!(st.drag, Drag::Tool) {
                t.on_input(doc, &input);
            }
        }
    }

    let dragging = resp.dragged_by(PointerButton::Primary);
    if dragging {
        match &mut st.drag {
            Drag::Pan => {
                let d = resp.drag_delta() * ppp;
                doc.view.pan([d.x, d.y], img_px, view_px);
            }
            Drag::Freehand { points } => {
                if let Some(p) = pointer {
                    let q = map.to_image([p.x, p.y]);
                    // About one screen pixel between points keeps outlines smooth but small.
                    push_point(points, [q[0] as f32, q[1] as f32], 1.0 / map.scale.max(1e-3));
                }
            }
            Drag::MoveFloating { grab, moved } => {
                if let Some(p) = pointer {
                    let q = map.to_image([p.x, p.y]);
                    let want = ((q[0] - grab[0]).round() as i32, (q[1] - grab[1]).round() as i32);
                    let (dx, dy) = (want.0 - moved.0, want.1 - moved.1);
                    if dx != 0 || dy != 0 {
                        doc.move_floating(dx, dy);
                        *moved = want;
                    }
                }
            }
            Drag::Edge { edge, start, bounds } => {
                if let Some(p) = pointer {
                    let d = (p - *start) / map.scale.max(1e-6);
                    *bounds = edge_drag_bounds(
                        doc.image.width,
                        doc.image.height,
                        *edge,
                        d.x.round() as i32,
                        d.y.round() as i32,
                    );
                }
            }
            _ => {}
        }
    }

    if resp.drag_stopped_by(PointerButton::Primary) || (primary_released && st.is_dragging() && !dragging) {
        match std::mem::take(&mut st.drag) {
            Drag::SelectRect { start } => {
                let end = pointer.unwrap_or(start);
                let r = drag_rect(map, [start.x, start.y], [end.x, end.y], doc.image.bounds());
                doc.set_selection(Selection::rect(r));
            }
            Drag::Freehand { points } => {
                doc.set_selection(Selection::freehand(&points, doc.image.bounds()));
            }
            Drag::Edge { bounds, .. } if bounds != doc.image.bounds() => {
                let fill = background;
                if doc.apply_image_edit("Resize Canvas", |img| selection::resize_canvas(img, bounds, fill)) {
                    // Keep the image where it was on screen.
                    doc.view.initialized = true;
                    doc.view.offset[0] += bounds.x as f32 * doc.view.zoom;
                    doc.view.offset[1] += bounds.y as f32 * doc.view.zoom;
                    out.edited = Some("Resize Canvas");
                }
            }
            _ => {}
        }
    }

    // A plain click (no drag) outside the selection deselects.
    if resp.clicked_by(PointerButton::Primary) && external.is_none() && !space_down && !pan_tool {
        if let Some(p) = pointer {
            let (x, y) = map.pixel_at([p.x, p.y]);
            if !over_selection(doc, x, y) {
                doc.deselect();
            }
        }
    }
}

/// Upload textures on first display; afterwards re-upload only the changed rectangle.
fn sync_textures(ctx: &egui::Context, doc: &mut Document) {
    let size_ok = doc.textures.as_ref().is_some_and(|t| {
        let (w, h) = t
            .tiles
            .iter()
            .fold((0, 0), |(w, h), tile| (w.max(tile.rect.x + tile.rect.w), h.max(tile.rect.y + tile.rect.h)));
        (w, h) == (doc.image.width, doc.image.height)
    });
    match doc.tex_dirty {
        TexDirty::Region(r) if size_ok => {
            let r = r.intersect(&doc.image.bounds());
            if let Some(tex) = doc.textures.as_mut() {
                for tile in tex.tiles.iter_mut() {
                    let tr = IRect::new(tile.rect.x as i32, tile.rect.y as i32, tile.rect.w as i32, tile.rect.h as i32);
                    let sub = r.intersect(&tr);
                    if sub.is_empty() {
                        continue;
                    }
                    let ci = tile_to_color_image(
                        &doc.image,
                        TileRect { x: sub.x as u32, y: sub.y as u32, w: sub.w as u32, h: sub.h as u32 },
                    );
                    tile.tex.set_partial([(sub.x - tr.x) as usize, (sub.y - tr.y) as usize], ci, image_options());
                }
            }
        }
        TexDirty::Clean if doc.textures.is_some() => {}
        _ => {
            let t0 = std::time::Instant::now();
            doc.textures = Some(ImageTextures::upload(ctx, &doc.image, &format!("doc{}", doc.id)));
            log::debug!("editor: prepared {}x{} textures in {:?}", doc.image.width, doc.image.height, t0.elapsed());
        }
    }
    doc.tex_dirty = TexDirty::Clean;
}

fn paint_floating(ctx: &egui::Context, painter: &egui::Painter, doc: &Document, st: &mut CanvasState, map: &CanvasMap) {
    let Some(f) = &doc.floating else {
        st.float_tex = None;
        return;
    };
    let key = Arc::as_ptr(&f.image) as usize;
    let stale = !matches!(&st.float_tex, Some((k, v, _)) if *k == key && *v == f.version);
    if stale {
        let r = TileRect { x: 0, y: 0, w: f.image.width, h: f.image.height };
        let tex = ctx.load_texture("editor-floating", tile_to_color_image(&f.image, r), image_options());
        st.float_tex = Some((key, f.version, tex));
    }
    if let Some((_, _, tex)) = &st.float_tex {
        let r = Rect::from_min_max(
            to_pos(map, [f.x as f32, f.y as f32]),
            to_pos(map, [(f.x + f.image.width as i32) as f32, (f.y + f.image.height as i32) as f32]),
        );
        painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}

/// Dashed black/white outline whose dashes crawl over time.
fn marching_ants(painter: &egui::Painter, pts: &[Pos2], closed: bool, time: f64) {
    if pts.len() < 2 {
        return;
    }
    const DASH: f32 = 4.0;
    let clip = painter.clip_rect().expand(2.0);
    let phase = ((time * 12.0) % (2.0 * DASH as f64)) as f32;
    let white = Stroke::new(1.0_f32, Color32::WHITE);
    let black = Stroke::new(1.0_f32, Color32::BLACK);
    let n = if closed { pts.len() } else { pts.len() - 1 };
    let mut shapes = Vec::new();
    let mut dist = phase;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let len = a.distance(b);
        if len <= f32::EPSILON {
            continue;
        }
        // Skip off-screen segments cheaply (huge outlines at high zoom).
        if !Rect::from_two_pos(a, b).intersects(clip) {
            dist += len;
            continue;
        }
        shapes.push(Shape::line_segment([a, b], white));
        let dir = (b - a) / len;
        let mut t = -(dist % (2.0 * DASH));
        while t < len {
            let s0 = t.max(0.0);
            let s1 = (t + DASH).min(len);
            if s1 > s0 {
                shapes.push(Shape::line_segment([a + dir * s0, a + dir * s1], black));
            }
            t += 2.0 * DASH;
        }
        dist += len;
    }
    painter.extend(shapes);
}
