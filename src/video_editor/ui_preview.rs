//! The preview: current frame (with zoom & pan), title cards, timed annotations rendered with
//! `draw::render_overlay`, and annotation / zoom-key editing with the mouse.

use super::compose::{render_title, view_for};
use super::model::ZoomKey;
use super::timeline::{format_ms, Pos};
use super::window::{CanvasDrag, Selection, Session};
use super::zoom::{self, ZoomView};
use crate::common::Image;
use crate::draw::{self, HandleKind, Kind, Object, Pt, RectF, RenderOptions, Timing, Tool, ToolOptions};
use crate::i18n::t;
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, ColorImage, FontId, ImageData, PointerButton, Rect, Sense, Stroke, StrokeKind,
    TextureOptions,
};
use std::sync::Arc;

/// What the overlay layer was rendered from.
#[derive(Clone, PartialEq)]
pub(super) struct OverlayKey {
    objects: Vec<Object>,
    view: [f32; 3],
    size: [u32; 2],
    frame: Option<u64>,
}

const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x6B, 0xFF);

fn color_image_to_bgra(ci: &ColorImage) -> Image {
    let mut data = Vec::with_capacity(ci.pixels.len() * 4);
    for p in &ci.pixels {
        data.extend_from_slice(&[p.b(), p.g(), p.r(), p.a()]);
    }
    Image::from_bgra(ci.size[0] as u32, ci.size[1] as u32, data)
}

fn bgra_to_color_image(img: &Image) -> ColorImage {
    let pixels = img.data.chunks_exact(4).map(|p| Color32::from_rgba_premultiplied(p[2], p[1], p[0], p[3])).collect();
    ColorImage { size: [img.width as usize, img.height as usize], pixels }
}

fn needs_backdrop(o: &Object) -> bool {
    matches!(o.kind, Kind::Highlight(_) | Kind::Magnifier(_))
}

/// Screen ↔ source mapping for the displayed frame.
#[derive(Clone, Copy)]
struct Map {
    disp: Rect,
    zr: RectF,
}

impl Map {
    fn scale(self) -> f32 {
        self.disp.width() / self.zr.w.max(1e-3)
    }
    fn to_src(self, p: egui::Pos2) -> Pt {
        let s = self.scale();
        Pt::new(self.zr.x + (p.x - self.disp.min.x) / s, self.zr.y + (p.y - self.disp.min.y) / s)
    }
    fn to_screen(self, p: Pt) -> egui::Pos2 {
        let s = self.scale();
        pos2(self.disp.min.x + (p.x - self.zr.x) * s, self.disp.min.y + (p.y - self.zr.y) * s)
    }
    fn rect_to_screen(self, r: RectF) -> Rect {
        Rect::from_min_max(self.to_screen(r.tl()), self.to_screen(r.br()))
    }
}

impl Session {
    pub(super) fn preview(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(avail, Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        let Some(info) = self.source.as_ref().map(|s| s.info) else {
            self.empty_state(ui, avail);
            return;
        };
        let (vw, vh) = (info.width as f32, info.height as f32);
        let inner = avail.shrink(14.0);
        let s = (inner.width() / vw).min(inner.height() / vh).max(0.01);
        let disp = Rect::from_center_size(inner.center(), vec2(vw * s, vh * s));
        painter.rect_filled(disp, 0.0, Color32::BLACK);
        match self.tl.locate(self.playhead) {
            Pos::Clip { src_ms, .. } => self.preview_clip(ui, &resp, disp, src_ms, vw, vh),
            Pos::Title { index, .. } => self.preview_title(ui, disp, index),
            Pos::End => {
                if let Some(tex) = &self.frame_tex {
                    painter.image(tex.id(), disp, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
        }
        // Time badge.
        let text = format!("{} / {}", format_ms(self.playhead), format_ms(self.tl.duration_ms));
        let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), Color32::WHITE);
        let r = Rect::from_min_size(disp.left_bottom() + vec2(8.0, -28.0), galley.size() + vec2(12.0, 6.0));
        painter.rect_filled(r, 4.0, Color32::from_black_alpha(150));
        painter.galley(r.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, avail: Rect) {
        let mut child =
            ui.new_child(egui::UiBuilder::new().max_rect(avail).layout(egui::Layout::top_down(egui::Align::Center)));
        child.add_space(avail.height() * 0.36);
        if self.is_loading() {
            child.spinner();
            return;
        }
        child.label(egui::RichText::new(t("ve-empty-title")).size(20.0).strong());
        child.add_space(4.0);
        child.weak(t("ve-empty-hint"));
        child.add_space(12.0);
        if child.button(egui::RichText::new(t("ve-open")).size(15.0)).clicked() {
            self.open_dialog();
        }
    }

    fn preview_title(&mut self, ui: &mut egui::Ui, disp: Rect, index: usize) {
        let Some(card) = self.edit.titles.get(index).cloned() else { return };
        let ppp = ui.ctx().pixels_per_point();
        let info = self.source.as_ref().map(|s| s.info);
        let (sw, sh) = info.map(|i| (i.width, i.height)).unwrap_or((1920, 1080));
        let w = ((disp.width() * ppp) as u32).clamp(16, sw.max(16));
        let h = ((w as u64 * sh as u64 / sw.max(1) as u64) as u32).max(2);
        let fresh = match &self.title_tex {
            Some((c, size, _)) => *c != card || *size != [w, h],
            None => true,
        };
        if fresh {
            let img = render_title(&card, w, h);
            let tex = ui.ctx().load_texture("ve-title", bgra_to_color_image(&img), TextureOptions::LINEAR);
            self.title_tex = Some((card.clone(), [w, h], tex));
        }
        if let Some((_, _, tex)) = &self.title_tex {
            ui.painter().image(tex.id(), disp, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        let selected = self.selection == Selection::Title(card.id);
        if selected {
            ui.painter().rect_stroke(disp, 0.0, Stroke::new(2.0_f32, ACCENT), StrokeKind::Outside);
        }
    }

    fn update_frame_texture(&mut self, ctx: &egui::Context, src_ms: u64, vw: f32, vh: f32) {
        let Some(f) = self.decoder.as_ref().and_then(|d| d.frame_at(src_ms)) else { return };
        let opts = RenderOptions { time_ms: Some(src_ms), hidden: Vec::new() };
        let full_size = f.image.size == [vw as usize, vh as usize];
        let need_raster = full_size && self.edit.annotations.iter().any(|o| o.is_raster() && opts.shows(o));
        let key = (f.pts_ms, if need_raster { self.rev } else { 0 });
        if self.frame_key == Some(key) && self.frame_tex.is_some() {
            return;
        }
        let data: ImageData = if need_raster {
            let base = color_image_to_bgra(&f.image);
            let fx = draw::raster::apply(&base, &self.edit.annotations, &opts);
            ImageData::Color(Arc::new(bgra_to_color_image(&fx)))
        } else {
            ImageData::Color(f.image.clone())
        };
        let topts = TextureOptions::LINEAR;
        match &mut self.frame_tex {
            Some(t) => t.set(data, topts),
            None => self.frame_tex = Some(ctx.load_texture("ve-frame", data, topts)),
        }
        self.frame_key = Some(key);
        if self.frame_bgra.as_ref().is_some_and(|(p, _)| *p != f.pts_ms) {
            self.frame_bgra = None;
        }
    }

    /// Source-size BGRA of the current frame (backdrop for highlighters and magnifiers).
    fn frame_backdrop(&mut self, src_ms: u64, vw: f32, vh: f32) -> Arc<Image> {
        let Some(f) = self.decoder.as_ref().and_then(|d| d.frame_at(src_ms)) else {
            return Arc::new(Image::new(0, 0));
        };
        if let Some((p, img)) = &self.frame_bgra {
            if *p == f.pts_ms {
                return img.clone();
            }
        }
        let img = color_image_to_bgra(&f.image);
        let img = if img.width != vw as u32 {
            super::compose::scale_region(
                &img,
                RectF::new(0.0, 0.0, img.width as f32, img.height as f32),
                vw as u32,
                vh as u32,
            )
        } else {
            img
        };
        let img = Arc::new(img);
        self.frame_bgra = Some((f.pts_ms, img.clone()));
        img
    }

    fn preview_clip(&mut self, ui: &mut egui::Ui, resp: &egui::Response, disp: Rect, src_ms: u64, vw: f32, vh: f32) {
        let ctx = ui.ctx().clone();
        let full = RectF::new(0.0, 0.0, vw, vh);
        let key_sel = match self.selection {
            Selection::Zoom(i) if self.zoom_full_frame && !self.is_playing() => {
                self.edit.zoom.get(i).copied().map(|k| (i, k))
            }
            _ => None,
        };
        let zr = if key_sel.is_some() { full } else { zoom::view_at(&self.edit.zoom, src_ms).rect(vw, vh) };
        let map = Map { disp, zr };
        self.update_frame_texture(&ctx, src_ms, vw, vh);
        let painter = ui.painter_at(resp.rect);
        if let Some(tex) = &self.frame_tex {
            let uv = Rect::from_min_max(pos2(zr.x / vw, zr.y / vh), pos2(zr.right() / vw, zr.bottom() / vh));
            painter.image(tex.id(), disp, uv, Color32::WHITE);
        } else {
            painter.text(
                disp.center(),
                Align2::CENTER_CENTER,
                t("ve-decoding"),
                FontId::proportional(14.0),
                Color32::GRAY,
            );
        }

        // Overlay layer: visible vector annotations plus the object being drawn.
        let opts = RenderOptions { time_ms: Some(src_ms), hidden: Vec::new() };
        let mut objs: Vec<Object> =
            self.edit.annotations.iter().filter(|o| !o.is_raster() && opts.shows(o)).cloned().collect();
        let creating = self.creating_object(src_ms);
        if let Some(o) = &creating {
            if !o.is_raster() {
                objs.push(o.clone());
            }
        }
        let ppp = ctx.pixels_per_point();
        let (lw, lh) = (((disp.width() * ppp).round() as u32).max(1), ((disp.height() * ppp).round() as u32).max(1));
        if objs.is_empty() {
            self.overlay_key = None;
        } else {
            let backdrop = objs.iter().any(needs_backdrop);
            let frame =
                if backdrop { self.decoder.as_ref().and_then(|d| d.frame_at(src_ms)).map(|f| f.pts_ms) } else { None };
            let view = view_for(zr, lw, lh);
            let key = OverlayKey { objects: objs.clone(), view: [view.x, view.y, view.zoom], size: [lw, lh], frame };
            if self.overlay_key.as_ref() != Some(&key) || self.overlay_tex.is_none() {
                let base = if backdrop { self.frame_backdrop(src_ms, vw, vh) } else { Arc::new(Image::new(0, 0)) };
                let layer = draw::render_overlay(&base, &objs, &view, &RenderOptions::default());
                let ci = bgra_to_color_image(&layer);
                match &mut self.overlay_tex {
                    Some(t) => t.set(ci, TextureOptions::LINEAR),
                    None => self.overlay_tex = Some(ctx.load_texture("ve-overlay", ci, TextureOptions::LINEAR)),
                }
                self.overlay_key = Some(key);
            }
            if let Some(tex) = &self.overlay_tex {
                painter.image(tex.id(), disp, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        // Raster tool being drawn: dashed outline.
        if let Some(o) = creating.as_ref().filter(|o| o.is_raster()) {
            let r = map.rect_to_screen(draw::bounds(o));
            painter.rect_stroke(r, 0.0, Stroke::new(1.5_f32, Color32::WHITE), StrokeKind::Middle);
            painter.rect_stroke(r.expand(1.5), 0.0, Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Middle);
        }

        if let Some((i, k)) = key_sel {
            self.zoom_key_editor(ui, resp, map, i, k, vw, vh);
            return;
        }
        self.selection_handles(&painter, map, src_ms);
        self.canvas_input(ui, resp, map, src_ms);
    }

    /// The object for the drag-to-create gesture in progress.
    fn creating_object(&self, src_ms: u64) -> Option<Object> {
        let Some(CanvasDrag::Create { start, cur }) = &self.canvas_drag else { return None };
        let mut o = self.build_object(*start, *cur)?;
        o.timing = Some(Timing { start_ms: src_ms, end_ms: src_ms + 1 });
        Some(o)
    }

    fn build_object(&self, a: Pt, b: Pt) -> Option<Object> {
        let tool = self.tool;
        let id = draw::next_id(&self.edit.annotations);
        let style = draw::default_style(tool);
        let opts = ToolOptions::for_tool(tool);
        let mut o = draw::create(tool, id, &style, &opts, a, b, &self.edit.annotations)?;
        if let Kind::Text(tx) = &mut o.kind {
            tx.text = t("ve-text-default");
        }
        Some(o)
    }

    fn selection_handles(&self, painter: &egui::Painter, map: Map, src_ms: u64) {
        let Selection::Annotation(id) = self.selection else { return };
        let Some(o) = self.edit.annotations.iter().find(|o| o.id == id) else { return };
        if !o.visible_at(Some(src_ms)) {
            return;
        }
        let b = map.rect_to_screen(draw::bounds(o));
        painter.rect_stroke(b, 0.0, Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7)), StrokeKind::Outside);
        for h in draw::handles(o, map.scale()) {
            let p = map.to_screen(h.pos);
            match h.kind {
                HandleKind::Resize => {
                    let r = Rect::from_center_size(p, vec2(8.0, 8.0));
                    painter.rect_filled(r, 1.0, Color32::WHITE);
                    painter.rect_stroke(r, 1.0, Stroke::new(1.5_f32, ACCENT), StrokeKind::Inside);
                }
                _ => {
                    painter.circle_filled(p, 4.5, Color32::WHITE);
                    painter.circle_stroke(p, 4.5, Stroke::new(1.5_f32, ACCENT));
                }
            }
        }
    }

    fn canvas_input(&mut self, ui: &mut egui::Ui, resp: &egui::Response, map: Map, src_ms: u64) {
        let Some(pos) = resp.interact_pointer_pos().or(resp.hover_pos()) else { return };
        let p = map.to_src(pos);
        let scale = map.scale();
        let mods = ui.input(|i| i.modifiers);
        let visible: Vec<Object> =
            self.edit.annotations.iter().filter(|o| o.visible_at(Some(src_ms))).cloned().collect();
        if self.tool != Tool::Select && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.drag_started_by(PointerButton::Primary) || resp.clicked_by(PointerButton::Primary) {
            // A drag starts where the button went down, not where the drag threshold was crossed.
            let p =
                ui.input(|i| i.pointer.press_origin()).filter(|_| resp.dragged()).map(|o| map.to_src(o)).unwrap_or(p);
            self.stop_playback();
            if self.tool != Tool::Select {
                self.canvas_drag = Some(CanvasDrag::Create { start: p, cur: p });
            } else {
                let sel = match self.selection {
                    Selection::Annotation(id) => visible.iter().find(|o| o.id == id).cloned(),
                    _ => None,
                };
                let handle =
                    sel.as_ref().and_then(|o| draw::handle_at(o, p, 7.0 / scale, scale).map(|h| (o.clone(), h)));
                if let Some((o, h)) = handle {
                    self.canvas_drag = Some(CanvasDrag::Handle { id: o.id, handle: h, snapshot: o, start: p });
                } else if let Some(id) = draw::hit_test(&visible, p, 5.0 / scale) {
                    self.selection = Selection::Annotation(id);
                    if let Some(o) = visible.iter().find(|o| o.id == id) {
                        self.canvas_drag = Some(CanvasDrag::Move { id, snapshot: o.clone(), start: p });
                    }
                } else {
                    self.selection = Selection::None;
                }
            }
        }
        // Hover cursor over handles / objects.
        if self.tool == Tool::Select && self.canvas_drag.is_none() && resp.hovered() {
            if let Selection::Annotation(id) = self.selection {
                if visible.iter().find(|o| o.id == id).and_then(|o| draw::handle_at(o, p, 7.0 / scale, scale)).is_some()
                {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            }
            if draw::hit_test(&visible, p, 5.0 / scale).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            }
        }
        if resp.dragged() {
            let c = draw::DragConstraints {
                keep_aspect: mods.shift,
                from_center: mods.ctrl || mods.alt,
                snap_angle: mods.shift,
            };
            match &mut self.canvas_drag {
                Some(CanvasDrag::Create { cur, .. }) => *cur = p,
                Some(CanvasDrag::Move { id, snapshot, start }) => {
                    let mut o = snapshot.clone();
                    draw::translate(&mut o, p - *start);
                    let id = *id;
                    if let Some(slot) = self.edit.annotations.iter_mut().find(|x| x.id == id) {
                        *slot = o;
                        self.touch();
                    }
                }
                Some(CanvasDrag::Handle { id, handle, snapshot, start }) => {
                    let mut o = snapshot.clone();
                    draw::drag_handle(&mut o, *handle, p - *start, &c);
                    let id = *id;
                    if let Some(slot) = self.edit.annotations.iter_mut().find(|x| x.id == id) {
                        *slot = o;
                        self.touch();
                    }
                }
                _ => {}
            }
        }
        if resp.drag_stopped() || resp.clicked() {
            if let Some(CanvasDrag::Create { start, cur }) = self.canvas_drag.take() {
                self.finish_create(start, cur, src_ms);
            } else {
                self.canvas_drag = None;
            }
        }
    }

    fn finish_create(&mut self, a: Pt, b: Pt, src_ms: u64) {
        let Some(mut o) = self.build_object(a, b) else { return };
        let end = (src_ms + 3000).min(self.source_duration().max(src_ms + 100));
        o.timing = Some(Timing { start_ms: src_ms, end_ms: end });
        let id = o.id;
        self.edit.annotations.push(o);
        self.commit();
        self.selection = Selection::Annotation(id);
        self.tool = Tool::Select;
    }

    #[allow(clippy::too_many_arguments)]
    fn zoom_key_editor(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        map: Map,
        i: usize,
        k: ZoomKey,
        vw: f32,
        vh: f32,
    ) {
        let painter = ui.painter_at(resp.rect);
        let kr = ZoomView { cx: k.cx, cy: k.cy, scale: k.scale }.rect(vw, vh);
        let r = map.rect_to_screen(kr);
        // Dim outside the key's rectangle.
        let d = map.disp;
        let dim = Color32::from_black_alpha(120);
        painter.rect_filled(Rect::from_min_max(d.min, pos2(d.max.x, r.min.y)), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(pos2(d.min.x, r.max.y), d.max), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(pos2(d.min.x, r.min.y), pos2(r.min.x, r.max.y)), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(pos2(r.max.x, r.min.y), pos2(d.max.x, r.max.y)), 0.0, dim);
        painter.rect_stroke(r, 0.0, Stroke::new(2.0_f32, ACCENT), StrokeKind::Outside);
        painter.text(
            r.left_top() + vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            format!("{:.2}×", k.scale),
            FontId::proportional(13.0),
            Color32::WHITE,
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            let scroll = ui.input(|inp| inp.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                if let Some(key) = self.edit.zoom.get_mut(i) {
                    key.scale = (key.scale * (scroll * 0.003).exp()).clamp(1.0, zoom::MAX_SCALE);
                    self.touch();
                }
            }
        }
        if resp.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.canvas_drag = Some(CanvasDrag::ZoomPan { index: i, snapshot: k, start: pos });
            }
        }
        if resp.dragged() {
            if let (Some(CanvasDrag::ZoomPan { index, snapshot, start }), Some(pos)) =
                (&self.canvas_drag, resp.interact_pointer_pos())
            {
                let (dx, dy) = ((pos.x - start.x) / map.disp.width(), (pos.y - start.y) / map.disp.height());
                let (index, snap) = (*index, *snapshot);
                if let Some(key) = self.edit.zoom.get_mut(index) {
                    key.cx = (snap.cx + dx).clamp(0.0, 1.0);
                    key.cy = (snap.cy + dy).clamp(0.0, 1.0);
                    self.touch();
                }
            }
        }
        if resp.drag_stopped() {
            self.canvas_drag = None;
        }
    }
}
