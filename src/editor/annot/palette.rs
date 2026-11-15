//! The left vertical tool palette (every draw tool, grouped). It switches to two columns
//! when the window is too short for one.

use super::keys::{self, GROUP_STARTS, ORDER};
use super::widgets::tool_icon;
use super::{tool_label, tool_tip};
use crate::draw::Tool;
use crate::editor::style::{Palette, ACCENT};
use crate::i18n::t;
use eframe::egui::{self, pos2, vec2, Color32, Margin, Rect, Sense, Stroke};

pub enum PaletteAction {
    Select(Tool),
    SetDefault(Tool),
}

const BTN: egui::Vec2 = vec2(36.0, 32.0);
const GAP: f32 = 2.0;
const SEP: f32 = 9.0;

/// Button rectangles (relative to the content origin) and separator y positions.
fn layout(cols: usize) -> (Vec<Rect>, Vec<f32>, egui::Vec2) {
    let mut rects = Vec::with_capacity(ORDER.len());
    let mut seps = Vec::new();
    let mut y = 0.0;
    let mut col = 0;
    for i in 0..ORDER.len() {
        if i > 0 && GROUP_STARTS.contains(&i) {
            if col != 0 {
                y += BTN.y + GAP;
                col = 0;
            }
            seps.push(y + SEP / 2.0);
            y += SEP;
        }
        rects.push(Rect::from_min_size(pos2(col as f32 * (BTN.x + GAP), y), BTN));
        col += 1;
        if col == cols {
            col = 0;
            y += BTN.y + GAP;
        }
    }
    if col != 0 {
        y += BTN.y + GAP;
    }
    (rects, seps, vec2(cols as f32 * (BTN.x + GAP) - GAP, y))
}

/// Show the palette; `active` is the current draw tool (if any).
pub fn palette_panel(ctx: &egui::Context, pal: &Palette, active: Option<Tool>, enabled: bool) -> Option<PaletteAction> {
    let mut out = None;
    let default_tool = crate::settings::get().draw.default_tool;
    let avail = ctx.available_rect().height() - 12.0;
    let cols = if layout(1).2.y > avail { 2 } else { 1 };
    let (rects, seps, size) = layout(cols);
    let frame = egui::Frame::new().fill(pal.bar).inner_margin(Margin { left: 5, right: 5, top: 6, bottom: 6 });
    egui::SidePanel::left("draw-palette")
        .resizable(false)
        .exact_width(size.x + 10.0)
        .frame(frame)
        .show_separator_line(false)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
                    let o = area.min.to_vec2();
                    for y in seps {
                        ui.painter().hline(
                            (area.left() + 5.0)..=(area.right() - 5.0),
                            area.top() + y,
                            Stroke::new(1.0_f32, pal.border),
                        );
                    }
                    for (&tool, rel) in ORDER.iter().zip(rects) {
                        let r = rel.translate(o);
                        let resp = ui.interact(r, egui::Id::new(("draw-palette-tool", tool.id())), Sense::click());
                        let resp = if enabled { resp } else { resp.on_hover_cursor(egui::CursorIcon::NotAllowed) };
                        let selected = active == Some(tool);
                        let p = ui.painter();
                        let bg = if selected {
                            ACCENT
                        } else if resp.hovered() && enabled {
                            ui.visuals().widgets.hovered.weak_bg_fill
                        } else {
                            Color32::TRANSPARENT
                        };
                        p.rect_filled(r, 7.0, bg);
                        let fg = if selected {
                            Color32::WHITE
                        } else if enabled {
                            ui.visuals().strong_text_color()
                        } else {
                            pal.muted_text.gamma_multiply(0.6)
                        };
                        tool_icon(p, Rect::from_center_size(r.center(), vec2(20.0, 20.0)), tool, fg);
                        if tool == default_tool {
                            p.circle_filled(
                                r.right_top() + vec2(-5.0, 5.0),
                                2.5,
                                if selected { Color32::WHITE } else { ACCENT },
                            );
                        }
                        let key = keys::shortcut(tool).map(|k| format!(" ({k})")).unwrap_or_default();
                        let resp = resp.on_hover_ui(|ui| {
                            ui.label(egui::RichText::new(format!("{}{key}", tool_label(tool))).strong());
                            ui.label(tool_tip(tool));
                        });
                        if resp.clicked() && enabled {
                            out = Some(PaletteAction::Select(tool));
                        }
                        resp.context_menu(|ui| {
                            let label = if tool == default_tool {
                                format!("✔ {}", t("draw-default-tool"))
                            } else {
                                t("draw-set-default-tool")
                            };
                            if ui.button(label).clicked() {
                                out = Some(PaletteAction::SetDefault(tool));
                                ui.close_menu();
                            }
                        });
                    }
                });
        });
    out
}
