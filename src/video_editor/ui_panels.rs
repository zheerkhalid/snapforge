//! Toolbar, tool palette, inspector, segment list, export dialog and status bar.

use super::model::{Anchor, Background};
use super::timeline::{format_ms, ItemKind};
use super::window::{ExportKind, Selection, Session};
use crate::draw::{Color, Kind, Timing, Tool};
use crate::i18n::{t, tf};
use eframe::egui::{self, Align, Color32, Layout, RichText};

/// Annotation tools offered on the video.
pub(super) const TOOLS: [Tool; 10] = [
    Tool::Select,
    Tool::Text,
    Tool::Callout,
    Tool::Arrow,
    Tool::Rect,
    Tool::Ellipse,
    Tool::Highlighter,
    Tool::Step,
    Tool::Blur,
    Tool::Pixelate,
];

pub(super) fn to_c32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

pub(super) fn from_c32(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

fn color_edit(ui: &mut egui::Ui, c: &mut Color) -> bool {
    let mut v = to_c32(*c);
    let r = ui.color_edit_button_srgba(&mut v).changed();
    if r {
        *c = from_c32(v);
    }
    r
}

fn secs_edit(ui: &mut egui::Ui, ms: &mut u64, max_ms: u64) -> bool {
    let mut s = *ms as f64 / 1000.0;
    let r = ui
        .add(egui::DragValue::new(&mut s).speed(0.05).range(0.0..=max_ms as f64 / 1000.0).suffix(" s").max_decimals(2))
        .changed();
    if r {
        *ms = (s * 1000.0).round().max(0.0) as u64;
    }
    r
}

impl Session {
    pub(super) fn panels(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("ve-toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::TopBottomPanel::bottom("ve-status").show(ctx, |ui| self.status_bar(ui));
        if self.source.is_some() {
            egui::TopBottomPanel::bottom("ve-timeline")
                .resizable(true)
                .default_height(300.0)
                .min_height(220.0)
                .show(ctx, |ui| self.timeline_panel(ui));
            egui::SidePanel::left("ve-tools").resizable(false).exact_width(118.0).show(ctx, |ui| self.tools_panel(ui));
            egui::SidePanel::right("ve-inspector").resizable(true).default_width(270.0).min_width(220.0).show(
                ctx,
                |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.inspector(ui));
                },
            );
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(if ctx.style().visuals.dark_mode {
                Color32::from_rgb(0x17, 0x18, 0x1C)
            } else {
                Color32::from_rgb(0xE8, 0xEA, 0xEF)
            }))
            .show(ctx, |ui| self.preview(ui));
        self.export_window(ctx);
        self.close_window(ctx);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let has = self.source.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui.button(t("ve-open")).on_hover_text(t("ve-open-tip")).clicked() {
                self.open_dialog();
            }
            if ui.add_enabled(has, egui::Button::new(t("ve-save-project"))).clicked() {
                self.save_project(false);
            }
            if ui.add_enabled(has, egui::Button::new(t("ve-save-project-as"))).clicked() {
                self.save_project(true);
            }
            ui.separator();
            if ui.add_enabled(!self.undo.is_empty() || self.is_dirty(), egui::Button::new(t("ve-undo"))).clicked() {
                self.undo();
            }
            if ui.add_enabled(!self.redo.is_empty(), egui::Button::new(t("ve-redo"))).clicked() {
                self.redo();
            }
            ui.separator();
            ui.add_enabled_ui(has, |ui| {
                ui.menu_button(t("ve-add-title"), |ui| {
                    if ui.button(t("ve-title-at-start")).clicked() {
                        self.add_title(Anchor::Start);
                        ui.close_menu();
                    }
                    if ui.button(t("ve-title-at-playhead")).clicked() {
                        self.add_title(Anchor::Source(0));
                        ui.close_menu();
                    }
                    if ui.button(t("ve-title-at-end")).clicked() {
                        self.add_title(Anchor::End);
                        ui.close_menu();
                    }
                });
                if ui.button(t("ve-add-zoom")).on_hover_text(t("ve-add-zoom-tip")).clicked() {
                    self.add_zoom(2.0);
                }
                if ui.button(t("ve-add-zoom-out")).on_hover_text(t("ve-add-zoom-out-tip")).clicked() {
                    self.add_zoom(1.0);
                }
            });
            ui.separator();
            let still = has && !self.still_busy();
            if ui.add_enabled(still, egui::Button::new(t("ve-save-frame"))).clicked() {
                self.save_frame(false);
            }
            if ui.add_enabled(still, egui::Button::new(t("ve-frame-to-editor"))).clicked() {
                self.save_frame(true);
            }
            ui.separator();
            let can_export = has && self.export.is_none();
            if ui.add_enabled(can_export, egui::Button::new(t("ve-export-mp4"))).clicked() {
                self.open_export_dialog(ExportKind::Mp4);
            }
            if ui.add_enabled(can_export, egui::Button::new(t("ve-export-gif"))).clicked() {
                self.open_export_dialog(ExportKind::Gif);
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(name) = &self.loading_name {
                ui.spinner();
                ui.label(tf("ve-loading", &[("name", name)]));
            }
            let mut cancel = false;
            if let Some(job) = &self.export {
                let f = job.job.progress.fraction();
                let label = if job.kind == ExportKind::Gif { t("ve-exporting-gif") } else { t("ve-exporting-mp4") };
                ui.label(label);
                ui.add(egui::ProgressBar::new(f).desired_width(220.0).show_percentage());
                cancel = ui.button(t("ve-cancel")).clicked();
            }
            if cancel {
                if let Some(job) = &self.export {
                    job.job.progress.cancel();
                }
            }
            if self.still_busy() {
                ui.spinner();
                ui.label(t("ve-rendering-frame"));
            }
            let mut clear = false;
            if let Some(s) = &self.status {
                let color = if s.error { Color32::from_rgb(0xE5, 0x4B, 0x4B) } else { ui.visuals().text_color() };
                ui.label(RichText::new(&s.text).color(color));
                if let Some(p) = &s.reveal {
                    if ui.link(t("ve-show-in-folder")).clicked() {
                        crate::common::win::reveal_in_explorer(p);
                    }
                    clear = ui.small_button("×").clicked();
                }
            }
            if clear {
                self.status = None;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(src) = &self.source {
                    let i = src.info;
                    ui.weak(format!(
                        "{}×{} · {:.2} fps · {}{}",
                        i.width,
                        i.height,
                        i.frame_rate.fps(),
                        format_ms(i.duration_ms),
                        if self.is_dirty() { " · ●" } else { "" }
                    ));
                }
            });
        });
    }

    fn tools_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new(t("ve-annotate")).strong());
        ui.add_space(2.0);
        let on_video = self.playhead_src().is_some();
        for tool in TOOLS {
            let label = t(&format!("draw-tool-{}", tool.id()));
            let r =
                ui.add_enabled(on_video || tool == Tool::Select, egui::SelectableLabel::new(self.tool == tool, label));
            if r.clicked() {
                self.tool = tool;
            }
        }
        ui.add_space(8.0);
        ui.weak(t("ve-annotate-hint"));
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        match self.selection {
            Selection::Annotation(id) => self.annotation_inspector(ui, id),
            Selection::Title(id) => self.title_inspector(ui, id),
            Selection::Zoom(i) => self.zoom_inspector(ui, i),
            Selection::None => {
                ui.label(RichText::new(t("ve-no-selection")).strong());
                ui.weak(t("ve-no-selection-hint"));
            }
        }
        ui.add_space(10.0);
        ui.separator();
        self.segment_list(ui);
    }

    fn annotation_inspector(&mut self, ui: &mut egui::Ui, id: u64) {
        let max = self.source_duration();
        let Some(idx) = self.edit.annotations.iter().position(|o| o.id == id) else {
            self.selection = Selection::None;
            return;
        };
        let mut changed = false;
        let mut delete = false;
        let mut jump = None;
        let mut jump_start = 0u64;
        {
            let o = &mut self.edit.annotations[idx];
            let name = t(&format!("draw-tool-{}", Tool::of(o).id()));
            ui.label(RichText::new(name).strong());
            ui.add_space(4.0);
            egui::Grid::new("ve-ann-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                let is_text = matches!(o.kind, Kind::Text(_));
                match &mut o.kind {
                    Kind::Text(tx) => {
                        ui.label(t("ve-prop-text"));
                        changed |= ui
                            .add(egui::TextEdit::multiline(&mut tx.text).desired_rows(2).desired_width(170.0))
                            .changed();
                        ui.end_row();
                    }
                    Kind::Blur(b) => {
                        ui.label(t("ve-prop-strength"));
                        changed |= ui.add(egui::Slider::new(&mut b.strength, 1.0..=40.0)).changed();
                        ui.end_row();
                    }
                    _ => {}
                }
                if is_text {
                    ui.label(t("ve-prop-text-color"));
                    changed |= color_edit(ui, &mut o.style.font.color);
                    ui.end_row();
                    ui.label(t("ve-prop-font-size"));
                    changed |= ui.add(egui::Slider::new(&mut o.style.font.size, 6.0..=120.0)).changed();
                    ui.end_row();
                    ui.label(t("ve-prop-fill"));
                    changed |= color_edit(ui, &mut o.style.fill);
                    ui.end_row();
                } else if matches!(o.kind, Kind::Highlight(_) | Kind::Step(_)) {
                    ui.label(t("ve-prop-fill"));
                    changed |= color_edit(ui, &mut o.style.fill);
                    ui.end_row();
                }
                if !matches!(o.kind, Kind::Blur(_) | Kind::Highlight(_)) {
                    ui.label(t("ve-prop-color"));
                    changed |= color_edit(ui, &mut o.style.color);
                    ui.end_row();
                    ui.label(t("ve-prop-width"));
                    changed |= ui.add(egui::Slider::new(&mut o.style.width, 0.0..=24.0)).changed();
                    ui.end_row();
                }
                ui.label(t("ve-prop-opacity"));
                changed |= ui.add(egui::Slider::new(&mut o.style.opacity, 0.1..=1.0)).changed();
                ui.end_row();
                let mut tm = o.timing.unwrap_or(Timing { start_ms: 0, end_ms: max });
                ui.label(t("ve-prop-show-from"));
                let a = secs_edit(ui, &mut tm.start_ms, max);
                ui.end_row();
                ui.label(t("ve-prop-show-to"));
                let b = secs_edit(ui, &mut tm.end_ms, max);
                ui.end_row();
                if a || b {
                    tm.end_ms = tm.end_ms.max(tm.start_ms + 100);
                    o.timing = Some(tm);
                    changed = true;
                }
                jump_start = tm.start_ms;
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(t("ve-jump-to")).clicked() {
                    jump = Some(jump_start);
                }
                delete = ui.button(t("ve-delete")).clicked();
            });
        }
        if changed {
            self.touch();
        }
        if let Some(src) = jump {
            let out = self.tl.source_to_output_clamped(src);
            self.seek(out);
        }
        if delete {
            self.delete_selection();
        }
    }

    fn title_inspector(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(idx) = self.edit.titles.iter().position(|c| c.id == id) else {
            self.selection = Selection::None;
            return;
        };
        let max = self.source_duration();
        let mut changed = false;
        let delete;
        {
            let c = &mut self.edit.titles[idx];
            ui.label(RichText::new(t("ve-title-card")).strong());
            ui.add_space(4.0);
            egui::Grid::new("ve-title-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(t("ve-prop-title"));
                changed |=
                    ui.add(egui::TextEdit::multiline(&mut c.title).desired_rows(2).desired_width(170.0)).changed();
                ui.end_row();
                ui.label(t("ve-prop-subtitle"));
                changed |=
                    ui.add(egui::TextEdit::multiline(&mut c.subtitle).desired_rows(2).desired_width(170.0)).changed();
                ui.end_row();
                ui.label(t("ve-prop-duration"));
                let mut d = c.duration_ms;
                if secs_edit(ui, &mut d, 60_000) {
                    c.duration_ms = d.max(super::model::MIN_ITEM_MS);
                    changed = true;
                }
                ui.end_row();
                ui.label(t("ve-prop-position"));
                let label = match c.anchor {
                    Anchor::Start => t("ve-title-at-start"),
                    Anchor::End => t("ve-title-at-end"),
                    Anchor::Source(ms) => tf("ve-title-at-time", &[("time", &format_ms(ms))]),
                };
                egui::ComboBox::from_id_salt("ve-anchor").selected_text(label).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut c.anchor, Anchor::Start, t("ve-title-at-start")).changed();
                    changed |= ui.selectable_value(&mut c.anchor, Anchor::End, t("ve-title-at-end")).changed();
                });
                ui.end_row();
                if let Anchor::Source(ms) = &mut c.anchor {
                    ui.label(t("ve-prop-source-time"));
                    changed |= secs_edit(ui, ms, max);
                    ui.end_row();
                }
                ui.label(t("ve-prop-background"));
                ui.horizontal(|ui| {
                    let grad = matches!(c.background, Background::Gradient { .. });
                    if ui.selectable_label(!grad, t("ve-bg-solid")).clicked() && grad {
                        let from = match c.background {
                            Background::Gradient { from, .. } => from,
                            Background::Solid { color } => color,
                        };
                        c.background = Background::Solid { color: from };
                        changed = true;
                    }
                    if ui.selectable_label(grad, t("ve-bg-gradient")).clicked() && !grad {
                        let color = match c.background {
                            Background::Solid { color } => color,
                            Background::Gradient { from, .. } => from,
                        };
                        c.background = Background::Gradient { from: color, to: color.lighten(0.35), angle: 35.0 };
                        changed = true;
                    }
                });
                ui.end_row();
                match &mut c.background {
                    Background::Solid { color } => {
                        ui.label(t("ve-prop-color"));
                        changed |= color_edit(ui, color);
                        ui.end_row();
                    }
                    Background::Gradient { from, to, angle } => {
                        ui.label(t("ve-prop-colors"));
                        ui.horizontal(|ui| {
                            changed |= color_edit(ui, from);
                            changed |= color_edit(ui, to);
                        });
                        ui.end_row();
                        ui.label(t("ve-prop-angle"));
                        changed |= ui.add(egui::Slider::new(angle, 0.0..=360.0).suffix("°")).changed();
                        ui.end_row();
                    }
                }
                ui.label(t("ve-prop-text-color"));
                changed |= color_edit(ui, &mut c.text_color);
                ui.end_row();
                ui.label(t("ve-prop-font-size"));
                changed |= ui.add(egui::Slider::new(&mut c.font_size, 12.0..=160.0)).changed();
                ui.end_row();
            });
            ui.add_space(6.0);
            delete = ui.button(t("ve-delete")).clicked();
        }
        if changed {
            self.touch();
        }
        if delete {
            self.delete_selection();
        }
    }

    fn zoom_inspector(&mut self, ui: &mut egui::Ui, i: usize) {
        let max = self.source_duration();
        let Some(k) = self.edit.zoom.get_mut(i) else {
            self.selection = Selection::None;
            return;
        };
        let mut changed = false;
        ui.label(RichText::new(t("ve-zoom-key")).strong());
        ui.add_space(4.0);
        egui::Grid::new("ve-zoom-grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label(t("ve-prop-source-time"));
            changed |= secs_edit(ui, &mut k.time_ms, max);
            ui.end_row();
            ui.label(t("ve-prop-zoom"));
            changed |= ui
                .add(egui::Slider::new(&mut k.scale, 1.0..=super::zoom::MAX_SCALE).logarithmic(true).suffix("×"))
                .changed();
            ui.end_row();
            ui.label(t("ve-prop-center-x"));
            changed |= ui.add(egui::Slider::new(&mut k.cx, 0.0..=1.0)).changed();
            ui.end_row();
            ui.label(t("ve-prop-center-y"));
            changed |= ui.add(egui::Slider::new(&mut k.cy, 0.0..=1.0)).changed();
            ui.end_row();
            ui.label(t("ve-prop-transition"));
            let mut tr = k.transition_ms;
            if secs_edit(ui, &mut tr, 5_000) {
                k.transition_ms = tr;
                changed = true;
            }
            ui.end_row();
        });
        ui.checkbox(&mut self.zoom_full_frame, t("ve-zoom-full-frame"));
        ui.weak(t("ve-zoom-hint"));
        ui.add_space(6.0);
        let delete = ui.button(t("ve-delete")).clicked();
        if changed {
            self.edit.sort_zoom();
            self.touch();
        }
        if delete {
            self.delete_selection();
        }
    }

    fn segment_list(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(t("ve-segments")).strong());
        let mut seek = None;
        let mut idx = 0;
        for it in &self.tl.items {
            if let ItemKind::Clip { src_start, src_end } = it.kind {
                idx += 1;
                let text = format!(
                    "{idx}. {} – {}  ({:.1} s)",
                    format_ms(src_start),
                    format_ms(src_end),
                    (src_end - src_start) as f32 / 1000.0
                );
                let on = (it.out_start..it.out_end()).contains(&self.playhead);
                if ui.selectable_label(on, text).on_hover_text(t("ve-segment-tip")).clicked() {
                    seek = Some(it.out_start);
                }
            }
        }
        let kept: u64 = self.edit.segments.iter().map(|s| s.len()).sum();
        ui.weak(tf(
            "ve-segments-summary",
            &[
                ("kept", &format_ms(kept)),
                ("total", &format_ms(self.source_duration())),
                ("out", &format_ms(self.tl.duration_ms)),
            ],
        ));
        let whole = self.edit.segments.len() == 1
            && self.edit.segments[0].start_ms == 0
            && self.edit.segments[0].end_ms >= self.source_duration();
        if ui.add_enabled(!whole, egui::Button::new(t("ve-restore-segments"))).clicked() {
            self.restore_segments();
        }
        if let Some(s) = seek {
            self.seek(s);
        }
    }

    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(src_w) = self.source.as_ref().map(|s| s.info.width) else { return };
        let Some(d) = self.export_dialog.as_mut() else { return };
        let mut open = true;
        let mut go = false;
        let title = if d.kind == ExportKind::Gif { t("ve-export-gif") } else { t("ve-export-mp4") };
        egui::Window::new(title)
            .id(egui::Id::new("ve-export-dialog"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("ve-export-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label(t("ve-export-width"));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.width).range(16..=src_w.max(16)).suffix(" px"));
                        for pct in [100u32, 75, 50] {
                            if ui.small_button(format!("{pct}%")).clicked() {
                                d.width = (src_w * pct / 100).max(16);
                            }
                        }
                    });
                    ui.end_row();
                    match d.kind {
                        ExportKind::Mp4 => {
                            ui.label(t("ve-export-fps"));
                            egui::ComboBox::from_id_salt("ve-fps")
                                .selected_text(if d.fps <= 0.0 { t("ve-fps-source") } else { format!("{}", d.fps) })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut d.fps, 0.0, t("ve-fps-source"));
                                    for f in [24.0, 25.0, 30.0, 50.0, 60.0] {
                                        ui.selectable_value(&mut d.fps, f, format!("{f}"));
                                    }
                                });
                            ui.end_row();
                            ui.label(t("ve-export-bitrate"));
                            ui.add(egui::Slider::new(&mut d.mbps, 1.0..=40.0).suffix(" Mbit/s"));
                            ui.end_row();
                        }
                        ExportKind::Gif => {
                            ui.label(t("ve-export-fps"));
                            ui.add(egui::Slider::new(&mut d.fps, 2.0..=30.0).step_by(1.0));
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut d.dither, t("ve-export-dither"));
                            ui.end_row();
                            ui.label("");
                            ui.checkbox(&mut d.looping, t("ve-export-loop"));
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    go = ui.add(egui::Button::new(RichText::new(t("ve-export-start")).strong())).clicked();
                    if ui.button(t("ve-cancel")).clicked() {
                        d.width = 0;
                    }
                });
            });
        let cancelled = self.export_dialog.as_ref().is_some_and(|d| d.width == 0);
        if !open || cancelled {
            self.export_dialog = None;
        } else if go {
            self.start_export();
        }
    }

    fn close_window(&mut self, ctx: &egui::Context) {
        if !self.confirm_close {
            return;
        }
        egui::Window::new(t("ve-unsaved-title"))
            .id(egui::Id::new("ve-close-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(t("ve-unsaved-body"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(t("ve-save")).clicked() {
                        self.confirm_close = false;
                        if self.save_project(false) {
                            self.close = true;
                        }
                    }
                    if ui.button(t("ve-dont-save")).clicked() {
                        self.confirm_close = false;
                        self.close = true;
                    }
                    if ui.button(t("ve-cancel")).clicked() {
                        self.confirm_close = false;
                    }
                });
            });
    }
}
