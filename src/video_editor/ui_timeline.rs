//! Transport controls and the timeline: ruler, filmstrip (with trim handles and title cards),
//! waveform, annotation bars and zoom keyframes. Times on screen are output times.

use super::analysis::PEAK_MS;
use super::model::{Background, MIN_ITEM_MS};
use super::timeline::{self, format_ms, ItemKind};
use super::ui_panels::to_c32;
use super::window::{Selection, Session, TlDrag};
use super::zoom;
use crate::draw::{Kind, ObjectId, Timing, Tool};
use crate::i18n::t;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind};

const LABEL_W: f32 = 86.0;
const RULER_H: f32 = 24.0;
const VIDEO_H: f32 = 58.0;
const AUDIO_H: f32 = 40.0;
const LANE_H: f32 = 20.0;
const ZOOM_H: f32 = 28.0;
const PAD: f32 = 14.0;
const MAX_LANES: usize = 6;

const PLAYHEAD: Color32 = Color32::from_rgb(0xFF, 0x4D, 0x4D);
const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x6B, 0xFF);
const TRIM: Color32 = Color32::from_rgb(0xFF, 0xC1, 0x2E);

struct Bar {
    id: ObjectId,
    a: u64,
    b: u64,
    lane: usize,
    label: String,
    color: Color32,
}

#[derive(Clone, Copy)]
struct Geo {
    rect: Rect,
    pps: f32,
    lanes: usize,
}

impl Geo {
    fn x(&self, ms: u64) -> f32 {
        self.rect.min.x + PAD + ms as f32 / 1000.0 * self.pps
    }
    fn t(&self, x: f32) -> u64 {
        ((x - self.rect.min.x - PAD) / self.pps * 1000.0).max(0.0) as u64
    }
    fn ruler(&self) -> Rect {
        Rect::from_min_size(self.rect.min, vec2(self.rect.width(), RULER_H))
    }
    fn video(&self) -> Rect {
        Rect::from_min_size(self.rect.min + vec2(0.0, RULER_H), vec2(self.rect.width(), VIDEO_H))
    }
    fn audio(&self) -> Rect {
        Rect::from_min_size(self.rect.min + vec2(0.0, RULER_H + VIDEO_H), vec2(self.rect.width(), AUDIO_H))
    }
    fn lanes_rect(&self) -> Rect {
        Rect::from_min_size(
            self.rect.min + vec2(0.0, RULER_H + VIDEO_H + AUDIO_H),
            vec2(self.rect.width(), LANE_H * self.lanes as f32),
        )
    }
    fn zoom(&self) -> Rect {
        let y = RULER_H + VIDEO_H + AUDIO_H + LANE_H * self.lanes as f32;
        Rect::from_min_size(self.rect.min + vec2(0.0, y), vec2(self.rect.width(), ZOOM_H))
    }
    fn total_h(lanes: usize) -> f32 {
        RULER_H + VIDEO_H + AUDIO_H + LANE_H * lanes as f32 + ZOOM_H
    }
}

fn kind_color(k: &Kind) -> Color32 {
    match k {
        Kind::Text(_) => Color32::from_rgb(0xF5, 0x9E, 0x0B),
        Kind::Line(_) => Color32::from_rgb(0xE5, 0x39, 0x35),
        Kind::Shape(_) => Color32::from_rgb(0x1E, 0x88, 0xE5),
        Kind::Highlight(_) => Color32::from_rgb(0xD4, 0xB1, 0x06),
        Kind::Step(_) => Color32::from_rgb(0x8E, 0x24, 0xAA),
        Kind::Blur(_) => Color32::from_rgb(0x60, 0x7D, 0x8B),
        _ => Color32::from_rgb(0x26, 0xA6, 0x9A),
    }
}

/// A "nice" ruler step (seconds) giving at least `min_px` between labels.
fn ruler_step(pps: f32, min_px: f32) -> f64 {
    const STEPS: [f64; 14] = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0];
    STEPS.iter().copied().find(|s| *s as f32 * pps >= min_px).unwrap_or(3600.0)
}

fn ruler_label(secs: f64, step: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - m as f64 * 60.0;
    if step < 1.0 {
        format!("{m}:{s:04.1}")
    } else {
        format!("{m}:{:02}", s.round() as u64)
    }
}

impl Session {
    fn bars(&self) -> Vec<Bar> {
        let full = self.source_duration();
        let mut bars: Vec<Bar> = self
            .edit
            .annotations
            .iter()
            .filter_map(|o| {
                let tm = o.timing.unwrap_or(Timing { start_ms: 0, end_ms: full });
                let a = self.tl.source_to_output_clamped(tm.start_ms);
                let b = self.tl.source_to_output_clamped(tm.end_ms);
                let label = match &o.kind {
                    Kind::Text(tx) if !tx.text.trim().is_empty() => {
                        tx.text.lines().next().unwrap_or_default().to_string()
                    }
                    _ => t(&format!("draw-tool-{}", Tool::of(o).id())),
                };
                (b > a).then(|| Bar { id: o.id, a, b, lane: 0, label, color: kind_color(&o.kind) })
            })
            .collect();
        bars.sort_by_key(|b| b.a);
        let mut ends: Vec<u64> = Vec::new();
        for bar in &mut bars {
            let lane = ends.iter().position(|&e| e <= bar.a).unwrap_or(ends.len());
            if lane == ends.len() {
                ends.push(bar.b);
            } else {
                ends[lane] = bar.b;
            }
            bar.lane = lane.min(MAX_LANES - 1);
        }
        bars
    }

    pub(super) fn timeline_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        self.transport(ui);
        ui.add_space(2.0);
        let bars = self.bars();
        let lanes = bars.iter().map(|b| b.lane + 1).max().unwrap_or(1).clamp(1, MAX_LANES);
        let total_h = Geo::total_h(lanes);
        let view_w = (ui.available_width() - LABEL_W - 8.0).max(100.0);
        if self.tl_fit {
            self.tl_fit = false;
            let secs = (self.tl.duration_ms as f32 / 1000.0).max(0.1);
            self.px_per_sec = ((view_w - 2.0 * PAD) / secs).clamp(2.0, 1000.0);
        }
        // Keep the playhead visible while playing.
        if self.is_playing() {
            let px = PAD + self.playhead as f32 / 1000.0 * self.px_per_sec;
            if px < self.tl_scroll || px > self.tl_scroll + view_w - 30.0 {
                self.tl_scroll_to = Some((px - 40.0).max(0.0));
            }
        }
        ui.horizontal_top(|ui| {
            let (lr, _) = ui.allocate_exact_size(vec2(LABEL_W, total_h), Sense::hover());
            let p = ui.painter_at(lr);
            let muted = ui.visuals().weak_text_color();
            let font = FontId::proportional(12.0);
            let rows = [
                (RULER_H, String::new()),
                (VIDEO_H, t("ve-track-video")),
                (AUDIO_H, t("ve-track-audio")),
                (LANE_H * lanes as f32, t("ve-track-annotations")),
                (ZOOM_H, t("ve-track-zoom")),
            ];
            let mut y = lr.min.y;
            for (h, label) in rows {
                p.text(pos2(lr.min.x + 4.0, y + h / 2.0), Align2::LEFT_CENTER, label, font.clone(), muted);
                y += h;
            }
            let mut sa = egui::ScrollArea::horizontal().id_salt("ve-tl-scroll").auto_shrink([false, true]);
            if let Some(x) = self.tl_scroll_to.take() {
                sa = sa.horizontal_scroll_offset(x);
            }
            let out = sa.show_viewport(ui, |ui, viewport| self.tracks(ui, viewport, total_h, lanes, &bars));
            self.tl_scroll = out.state.offset.x;
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let fm = self.frame_ms() as i64;
            if ui.button("⏮").on_hover_text(t("ve-to-start")).clicked() {
                self.seek(0);
            }
            if ui.button("⏪").on_hover_text(t("ve-step-back")).clicked() {
                self.step(-fm);
            }
            let play = if self.is_playing() { "⏸" } else { "▶" };
            if ui.add(egui::Button::new(play).min_size(vec2(36.0, 0.0))).on_hover_text(t("ve-play-tip")).clicked() {
                self.toggle_play();
            }
            if ui.button("⏩").on_hover_text(t("ve-step-forward")).clicked() {
                self.step(fm);
            }
            if ui.button("⏭").on_hover_text(t("ve-to-end")).clicked() {
                self.seek(self.tl.duration_ms);
            }
            ui.label(
                egui::RichText::new(format!("{} / {}", format_ms(self.playhead), format_ms(self.tl.duration_ms)))
                    .monospace(),
            );
            ui.separator();
            if ui.button(t("ve-mark-in")).on_hover_text(t("ve-mark-in-tip")).clicked() {
                self.mark_in();
            }
            if ui.button(t("ve-mark-out")).on_hover_text(t("ve-mark-out-tip")).clicked() {
                self.mark_out();
            }
            if ui
                .add_enabled(self.marked().is_some(), egui::Button::new(t("ve-cut")))
                .on_hover_text(t("ve-cut-tip"))
                .clicked()
            {
                self.cut_marked();
            }
            if (self.mark_in.is_some() || self.mark_out.is_some()) && ui.button(t("ve-clear-marks")).clicked() {
                self.mark_in = None;
                self.mark_out = None;
            }
            if let Some((a, b)) = self.marked() {
                ui.weak(format!("{:.2} s", (b - a) as f32 / 1000.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("ve-zoom-fit")).clicked() {
                    self.tl_fit = true;
                    self.tl_scroll_to = Some(0.0);
                }
                if ui.button("+").clicked() {
                    self.px_per_sec = (self.px_per_sec * 1.5).min(1000.0);
                }
                ui.add(egui::Slider::new(&mut self.px_per_sec, 2.0..=1000.0).logarithmic(true).show_value(false));
                if ui.button("−").clicked() {
                    self.px_per_sec = (self.px_per_sec / 1.5).max(2.0);
                }
                ui.weak(t("ve-timeline-zoom"));
            });
        });
    }

    fn tracks(&mut self, ui: &mut egui::Ui, viewport: Rect, total_h: f32, lanes: usize, bars: &[Bar]) {
        let content_w = (PAD * 2.0 + self.tl.duration_ms as f32 / 1000.0 * self.px_per_sec).max(viewport.width());
        let (rect, resp) = ui.allocate_exact_size(vec2(content_w, total_h), Sense::click_and_drag());
        let g = Geo { rect, pps: self.px_per_sec, lanes };
        let vis = Rect::from_min_max(
            pos2(rect.min.x + viewport.min.x, rect.min.y),
            pos2(rect.min.x + viewport.max.x, rect.max.y),
        );
        let painter = ui.painter_at(vis);
        let dark = ui.visuals().dark_mode;
        let track_bg = if dark { Color32::from_rgb(0x1D, 0x1E, 0x23) } else { Color32::from_rgb(0xEE, 0xF0, 0xF4) };
        let track_bg2 = if dark { Color32::from_rgb(0x23, 0x24, 0x2A) } else { Color32::from_rgb(0xE6, 0xE9, 0xEF) };
        let text = ui.visuals().text_color();
        let muted = ui.visuals().weak_text_color();
        for (i, r) in [g.video(), g.audio(), g.lanes_rect(), g.zoom()].into_iter().enumerate() {
            painter.rect_filled(r, 0.0, if i % 2 == 0 { track_bg } else { track_bg2 });
        }

        // Ruler.
        let step = ruler_step(self.px_per_sec, 70.0);
        let t0 = g.t(vis.min.x) as f64 / 1000.0;
        let t1 = g.t(vis.max.x) as f64 / 1000.0;
        let mut s = (t0 / step).floor() * step;
        let rr = g.ruler();
        while s <= t1 + step {
            let x = g.x((s * 1000.0) as u64);
            painter.line_segment([pos2(x, rr.max.y - 8.0), pos2(x, rr.max.y)], Stroke::new(1.0_f32, muted));
            painter.text(
                pos2(x + 3.0, rr.min.y + 3.0),
                Align2::LEFT_TOP,
                ruler_label(s, step),
                FontId::monospace(10.0),
                muted,
            );
            let minor = step / 5.0;
            for k in 1..5 {
                let xm = g.x(((s + minor * k as f64) * 1000.0) as u64);
                painter.line_segment(
                    [pos2(xm, rr.max.y - 3.0), pos2(xm, rr.max.y)],
                    Stroke::new(1.0_f32, muted.gamma_multiply(0.6)),
                );
            }
            s += step;
        }

        self.paint_video_track(&painter, g, vis, text);
        self.paint_waveform(&painter, g, vis, ui.visuals().selection.bg_fill);
        // Annotation bars.
        let lr = g.lanes_rect();
        for bar in bars {
            let y = lr.min.y + bar.lane as f32 * LANE_H;
            let r =
                Rect::from_min_max(pos2(g.x(bar.a), y + 2.0), pos2(g.x(bar.b).max(g.x(bar.a) + 4.0), y + LANE_H - 2.0));
            if !r.intersects(vis) {
                continue;
            }
            let sel = self.selection == Selection::Annotation(bar.id);
            painter.rect_filled(r, 4.0, bar.color.gamma_multiply(if sel { 1.0 } else { 0.75 }));
            if sel {
                painter.rect_stroke(r, 4.0, Stroke::new(2.0_f32, Color32::WHITE), StrokeKind::Inside);
            }
            let clip = painter.with_clip_rect(r.shrink(2.0).intersect(vis));
            clip.text(
                pos2(r.min.x + 6.0, r.center().y),
                Align2::LEFT_CENTER,
                &bar.label,
                FontId::proportional(11.0),
                Color32::WHITE,
            );
        }
        self.paint_zoom_track(&painter, g, vis, text);

        // Marked range and markers.
        let all = Rect::from_min_max(pos2(rect.min.x, g.video().min.y), rect.max);
        if let Some((a, b)) = self.marked() {
            painter.rect_filled(
                Rect::from_min_max(pos2(g.x(a), all.min.y), pos2(g.x(b), all.max.y)),
                0.0,
                Color32::from_rgba_unmultiplied(0x4F, 0x6B, 0xFF, 50),
            );
        }
        for (m, label) in [(self.mark_in, "I"), (self.mark_out, "O")] {
            if let Some(m) = m {
                let x = g.x(m);
                painter.line_segment([pos2(x, rr.min.y), pos2(x, all.max.y)], Stroke::new(1.5_f32, ACCENT));
                painter.text(
                    pos2(x + 2.0, rr.max.y - 2.0),
                    Align2::LEFT_BOTTOM,
                    label,
                    FontId::proportional(11.0),
                    ACCENT,
                );
            }
        }
        // Playhead.
        let px = g.x(self.playhead);
        painter.line_segment([pos2(px, rr.min.y), pos2(px, rect.max.y)], Stroke::new(2.0_f32, PLAYHEAD));
        painter.add(egui::Shape::convex_polygon(
            vec![pos2(px - 6.0, rr.min.y), pos2(px + 6.0, rr.min.y), pos2(px, rr.min.y + 9.0)],
            PLAYHEAD,
            Stroke::NONE,
        ));

        self.timeline_input(ui, &resp, g, bars, vis);
    }

    fn paint_video_track(&self, painter: &egui::Painter, g: Geo, vis: Rect, text: Color32) {
        let vr = g.video();
        let inner_h = VIDEO_H - 8.0;
        let aspect =
            self.source.as_ref().map(|s| s.info.width as f32 / s.info.height.max(1) as f32).unwrap_or(16.0 / 9.0);
        let tile_w = (inner_h * aspect).max(8.0);
        for it in &self.tl.items {
            let r = Rect::from_min_max(
                pos2(g.x(it.out_start) + 1.0, vr.min.y + 4.0),
                pos2(g.x(it.out_end()) - 1.0, vr.max.y - 4.0),
            );
            if !r.intersects(vis) {
                continue;
            }
            let clip = painter.with_clip_rect(r.intersect(vis));
            match it.kind {
                ItemKind::Clip { src_start, .. } => {
                    clip.rect_filled(r, 3.0, Color32::from_gray(40));
                    let first = (((vis.min.x - r.min.x) / tile_w).floor().max(0.0)) as i64;
                    let mut k = first;
                    loop {
                        let x = r.min.x + k as f32 * tile_w;
                        if x > r.max.x.min(vis.max.x) {
                            break;
                        }
                        let tile = Rect::from_min_size(pos2(x, r.min.y), vec2(tile_w, inner_h));
                        let out_ms = g.t(tile.center().x).clamp(it.out_start, it.out_end().saturating_sub(1));
                        let src = src_start + (out_ms - it.out_start);
                        if let Some(tex) = self.nearest_thumb(src) {
                            clip.image(
                                tex.id(),
                                tile,
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        k += 1;
                    }
                    clip.rect_stroke(r, 3.0, Stroke::new(1.0_f32, Color32::from_gray(90)), StrokeKind::Inside);
                }
                ItemKind::Title(i) => {
                    let Some(card) = self.edit.titles.get(i) else { continue };
                    let (a, b) = match card.background {
                        Background::Solid { color } => (color, color),
                        Background::Gradient { from, to, .. } => (from, to),
                    };
                    let mut mesh = egui::Mesh::default();
                    mesh.colored_vertex(r.left_top(), to_c32(a));
                    mesh.colored_vertex(r.right_top(), to_c32(b));
                    mesh.colored_vertex(r.right_bottom(), to_c32(b));
                    mesh.colored_vertex(r.left_bottom(), to_c32(a));
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(0, 2, 3);
                    clip.add(mesh);
                    let label = if card.title.trim().is_empty() {
                        t("ve-title-card")
                    } else {
                        card.title.lines().next().unwrap_or_default().to_string()
                    };
                    clip.text(
                        r.left_center() + vec2(6.0, 0.0),
                        Align2::LEFT_CENTER,
                        format!("T  {label}"),
                        FontId::proportional(12.0),
                        to_c32(card.text_color),
                    );
                    let sel = self.selection == Selection::Title(card.id);
                    clip.rect_stroke(
                        r,
                        3.0,
                        Stroke::new(
                            if sel { 2.0_f32 } else { 1.0 },
                            if sel { Color32::WHITE } else { text.gamma_multiply(0.4) },
                        ),
                        StrokeKind::Inside,
                    );
                }
            }
        }
        // Trim handles at the ends of the video.
        if let Some((a, b)) = self.tl.clip_bounds() {
            for (x, left) in [(g.x(a), true), (g.x(b), false)] {
                let r = if left {
                    Rect::from_min_max(pos2(x, vr.min.y + 2.0), pos2(x + 7.0, vr.max.y - 2.0))
                } else {
                    Rect::from_min_max(pos2(x - 7.0, vr.min.y + 2.0), pos2(x, vr.max.y - 2.0))
                };
                painter.rect_filled(r, 2.0, TRIM);
                let c = r.center();
                painter.line_segment(
                    [c + vec2(0.0, -8.0), c + vec2(0.0, 8.0)],
                    Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
                );
            }
        }
    }

    fn nearest_thumb(&self, src_ms: u64) -> Option<&egui::TextureHandle> {
        if self.thumbs.is_empty() {
            return None;
        }
        let i = self.thumbs.partition_point(|(t, _)| *t < src_ms);
        let cand = [i.checked_sub(1), (i < self.thumbs.len()).then_some(i)];
        cand.into_iter().flatten().min_by_key(|&j| self.thumbs[j].0.abs_diff(src_ms)).map(|j| &self.thumbs[j].1)
    }

    fn paint_waveform(&self, painter: &egui::Painter, g: Geo, vis: Rect, color: Color32) {
        let ar = g.audio();
        let mid = ar.center().y;
        let half = AUDIO_H / 2.0 - 3.0;
        let Some(peaks) = &self.peaks else {
            let msg = if self.no_audio {
                t("ve-no-audio")
            } else if let Some(p) = self.audio_progress {
                format!("{} {:.0}%", t("ve-reading-audio"), p * 100.0)
            } else {
                String::new()
            };
            painter.text(
                pos2(vis.min.x + 8.0, mid),
                Align2::LEFT_CENTER,
                msg,
                FontId::proportional(11.0),
                Color32::GRAY,
            );
            return;
        };
        painter.line_segment(
            [pos2(vis.min.x, mid), pos2(vis.max.x, mid)],
            Stroke::new(1.0_f32, color.gamma_multiply(0.5)),
        );
        let x0 = vis.min.x.max(g.x(0));
        let x1 = vis.max.x.min(g.x(self.tl.duration_ms));
        let mut x = x0;
        let color = color.gamma_multiply(1.6);
        while x < x1 {
            let (a, b) = (g.t(x), g.t(x + 1.0).max(g.t(x) + 1));
            let mut m = 0u8;
            for r in self.tl.source_ranges(a, b) {
                let (b0, b1) = ((r.start_ms / PEAK_MS) as usize, (r.end_ms.div_ceil(PEAK_MS)) as usize);
                if let Some(slice) = peaks.get(b0.min(peaks.len())..b1.min(peaks.len())) {
                    m = m.max(slice.iter().copied().max().unwrap_or(0));
                }
            }
            if m > 0 {
                let h = (m as f32 / 255.0).sqrt() * half;
                painter.line_segment([pos2(x, mid - h), pos2(x, mid + h)], Stroke::new(1.0_f32, color));
            }
            x += 1.0;
        }
    }

    fn paint_zoom_track(&self, painter: &egui::Painter, g: Geo, vis: Rect, text: Color32) {
        let zr = g.zoom();
        // Zoom level curve (only over video).
        let mut pts = Vec::new();
        let mut x = vis.min.x.max(g.x(0));
        let x1 = vis.max.x.min(g.x(self.tl.duration_ms));
        while x <= x1 {
            let scale = match self.tl.locate(g.t(x)) {
                timeline::Pos::Clip { src_ms, .. } => zoom::view_at(&self.edit.zoom, src_ms).scale,
                _ => 1.0,
            };
            let k = (scale.max(1.0).ln() / 4f32.ln()).min(1.0);
            pts.push(pos2(x, zr.max.y - 4.0 - k * (ZOOM_H - 8.0)));
            x += 3.0;
        }
        if pts.len() > 1 {
            painter.add(egui::Shape::line(pts, Stroke::new(1.5_f32, ACCENT.gamma_multiply(0.8))));
        }
        for (i, k) in self.edit.zoom.iter().enumerate() {
            let x = g.x(self.tl.source_to_output_clamped(k.time_ms));
            if x < vis.min.x - 10.0 || x > vis.max.x + 10.0 {
                continue;
            }
            let c = pos2(x, zr.center().y);
            let sel = self.selection == Selection::Zoom(i);
            let s = if sel { 7.0 } else { 6.0 };
            let fill = if k.scale > 1.001 { ACCENT } else { Color32::from_gray(150) };
            painter.add(egui::Shape::convex_polygon(
                vec![c + vec2(0.0, -s), c + vec2(s, 0.0), c + vec2(0.0, s), c + vec2(-s, 0.0)],
                fill,
                Stroke::new(
                    if sel { 2.0_f32 } else { 1.0 },
                    if sel { Color32::WHITE } else { text.gamma_multiply(0.5) },
                ),
            ));
        }
    }

    fn timeline_input(&mut self, ui: &mut egui::Ui, resp: &egui::Response, g: Geo, bars: &[Bar], vis: Rect) {
        // Ctrl + wheel: zoom around the pointer.
        if let Some(hp) = resp.hover_pos() {
            let zd = ui.input(|i| i.zoom_delta());
            if (zd - 1.0).abs() > 1e-3 {
                let t_ptr = g.t(hp.x);
                let new_pps = (self.px_per_sec * zd).clamp(2.0, 1000.0);
                let left_screen = vis.min.x;
                let new_cx = PAD + t_ptr as f32 / 1000.0 * new_pps;
                self.px_per_sec = new_pps;
                self.tl_scroll_to = Some((new_cx - (hp.x - left_screen)).max(0.0));
            }
        }
        let pressed = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed());
        let Some(ptr) = resp.interact_pointer_pos().or(resp.hover_pos()) else { return };
        let near = |x: f32| (ptr.x - x).abs() <= 6.0;
        // Hover cursors.
        if self.tl_drag.is_none() && resp.hovered() {
            let resize = (g.video().contains(ptr)
                && self.tl.clip_bounds().is_some_and(|(a, b)| near(g.x(a)) || near(g.x(b))))
                || (g.lanes_rect().contains(ptr)
                    && self.bar_at(g, bars, ptr).is_some_and(|b| near(g.x(b.a)) || near(g.x(b.b))));
            if resize {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        if pressed {
            self.stop_playback();
            let mut drag = TlDrag::Scrub;
            if g.video().contains(ptr) {
                let clip = self.tl.clip_bounds();
                if clip.is_some_and(|(a, _)| near(g.x(a))) {
                    drag = TlDrag::TrimStart { orig: self.edit.segments.clone(), x0: ptr.x };
                } else if clip.is_some_and(|(_, b)| near(g.x(b))) {
                    drag = TlDrag::TrimEnd { orig: self.edit.segments.clone(), x0: ptr.x };
                } else if let Some(i) = self.tl.item_at(g.t(ptr.x)) {
                    if let ItemKind::Title(ti) = self.tl.items[i].kind {
                        if let Some(c) = self.edit.titles.get(ti) {
                            self.selection = Selection::Title(c.id);
                        }
                    }
                }
            } else if g.lanes_rect().contains(ptr) {
                if let Some(bar) = self.bar_at(g, bars, ptr) {
                    self.selection = Selection::Annotation(bar.id);
                    let orig = self
                        .edit
                        .annotations
                        .iter()
                        .find(|o| o.id == bar.id)
                        .and_then(|o| o.timing)
                        .unwrap_or(Timing { start_ms: 0, end_ms: self.source_duration() });
                    drag = if near(g.x(bar.a)) {
                        TlDrag::ResizeAnnStart { id: bar.id, orig, x0: ptr.x }
                    } else if near(g.x(bar.b)) {
                        TlDrag::ResizeAnnEnd { id: bar.id, orig, x0: ptr.x }
                    } else {
                        TlDrag::MoveAnn { id: bar.id, orig, x0: ptr.x }
                    };
                    // Show the object: move the playhead into its bar.
                    if !(bar.a..bar.b).contains(&self.playhead) {
                        self.playhead = bar.a;
                    }
                }
            } else if g.zoom().contains(ptr) {
                let hit = self
                    .edit
                    .zoom
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (i, k.time_ms, (g.x(self.tl.source_to_output_clamped(k.time_ms)) - ptr.x).abs()))
                    .filter(|(_, _, d)| *d <= 8.0)
                    .min_by(|a, b| a.2.total_cmp(&b.2));
                if let Some((i, time, _)) = hit {
                    self.selection = Selection::Zoom(i);
                    drag = TlDrag::MoveZoom { index: i, orig: time, x0: ptr.x };
                    self.playhead = self.tl.source_to_output_clamped(time);
                }
            }
            if matches!(drag, TlDrag::Scrub) {
                self.playhead = g.t(ptr.x).min(self.tl.duration_ms.saturating_sub(1));
            }
            self.tl_drag = Some(drag);
        }
        if resp.is_pointer_button_down_on() && !pressed {
            if let Some(drag) = self.tl_drag.clone() {
                self.apply_tl_drag(&drag, g, ptr.x);
            }
        }
        if ui.input(|i| i.pointer.primary_released()) && self.tl_drag.is_some() {
            if let Some(TlDrag::MoveZoom { .. }) = self.tl_drag {
                // Re-sort and keep the moved key selected.
                if let Selection::Zoom(i) = self.selection {
                    if let Some(k) = self.edit.zoom.get(i).copied() {
                        self.edit.sort_zoom();
                        self.selection =
                            self.edit.zoom.iter().position(|x| *x == k).map(Selection::Zoom).unwrap_or(Selection::None);
                    }
                }
            }
            self.tl_drag = None;
        }
    }

    fn bar_at<'b>(&self, g: Geo, bars: &'b [Bar], p: egui::Pos2) -> Option<&'b Bar> {
        let lane = ((p.y - g.lanes_rect().min.y) / LANE_H).floor().max(0.0) as usize;
        bars.iter().find(|b| b.lane == lane && p.x >= g.x(b.a) - 6.0 && p.x <= g.x(b.b) + 6.0)
    }

    fn apply_tl_drag(&mut self, drag: &TlDrag, g: Geo, x: f32) {
        let d = |x0: f32| ((x - x0) / g.pps * 1000.0) as i64;
        let src_max = self.source_duration() as i64;
        match drag {
            TlDrag::Scrub => self.playhead = g.t(x).min(self.tl.duration_ms.saturating_sub(1)),
            TlDrag::TrimStart { orig, x0 } => {
                let mut segs = orig.clone();
                if let Some(first) = orig.first() {
                    timeline::trim_start(&mut segs, (first.start_ms as i64 + d(*x0)).max(0) as u64);
                    if segs != self.edit.segments {
                        self.edit.segments = segs;
                        self.touch();
                    }
                    if let Some((a, _)) = self.tl.clip_bounds() {
                        self.playhead = a;
                    }
                }
            }
            TlDrag::TrimEnd { orig, x0 } => {
                let mut segs = orig.clone();
                if let Some(last) = orig.last() {
                    timeline::trim_end(&mut segs, (last.end_ms as i64 + d(*x0)).max(0) as u64, src_max as u64);
                    if segs != self.edit.segments {
                        self.edit.segments = segs;
                        self.touch();
                    }
                    if let Some((_, b)) = self.tl.clip_bounds() {
                        self.playhead = b.saturating_sub(1);
                    }
                }
            }
            TlDrag::MoveAnn { id, orig, x0 }
            | TlDrag::ResizeAnnStart { id, orig, x0 }
            | TlDrag::ResizeAnnEnd { id, orig, x0 } => {
                let dd = d(*x0);
                let (s, e) = (orig.start_ms as i64, orig.end_ms as i64);
                let min = MIN_ITEM_MS as i64;
                let (ns, ne) = match drag {
                    TlDrag::MoveAnn { .. } => {
                        let shift = dd.clamp(-s, (src_max - e).max(-s));
                        (s + shift, e + shift)
                    }
                    TlDrag::ResizeAnnStart { .. } => ((s + dd).clamp(0, e - min), e),
                    _ => (s, (e + dd).clamp(s + min, src_max.max(s + min))),
                };
                let id = *id;
                if let Some(o) = self.edit.annotations.iter_mut().find(|o| o.id == id) {
                    let nt = Some(Timing { start_ms: ns.max(0) as u64, end_ms: ne.max(0) as u64 });
                    if o.timing != nt {
                        o.timing = nt;
                        self.touch();
                    }
                }
            }
            TlDrag::MoveZoom { index, orig, x0 } => {
                let nt = (*orig as i64 + d(*x0)).clamp(0, src_max) as u64;
                if let Some(k) = self.edit.zoom.get_mut(*index) {
                    if k.time_ms != nt {
                        k.time_ms = nt;
                        self.touch();
                        self.playhead = self.tl.source_to_output_clamped(nt);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_steps_are_readable() {
        assert_eq!(ruler_step(100.0, 70.0), 1.0);
        assert_eq!(ruler_step(1000.0, 70.0), 0.1);
        assert_eq!(ruler_step(1.0, 70.0), 120.0);
        assert_eq!(ruler_label(65.0, 1.0), "1:05");
        assert_eq!(ruler_label(1.5, 0.5), "0:01.5");
    }
}
