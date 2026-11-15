//! The left vertical tool palette (every draw tool, grouped).

use super::keys::{self, GROUP_STARTS, ORDER};
use super::widgets::tool_icon;
use super::{tool_label, tool_tip};
use crate::draw::Tool;
use crate::editor::style::{Palette, ACCENT};
use crate::i18n::t;
use eframe::egui::{self, vec2, Color32, Margin, Sense, Stroke};

pub enum PaletteAction {
    Select(Tool),
    SetDefault(Tool),
}

/// Show the palette; `active` is the current draw tool (if any).
pub fn palette_panel(ctx: &egui::Context, pal: &Palette, active: Option<Tool>, enabled: bool) -> Option<PaletteAction> {
    let mut out = None;
    let default_tool = crate::settings::get().draw.default_tool;
    let frame = egui::Frame::new().fill(pal.bar).inner_margin(Margin { left: 5, right: 5, top: 6, bottom: 6 });
    egui::SidePanel::left("draw-palette").resizable(false).exact_width(46.0).frame(frame).show_separator_line(false).show(
        ctx,
        |ui| {
            egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(
                ui,
                |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                    for (i, &tool) in ORDER.iter().enumerate() {
                        if i > 0 && GROUP_STARTS.contains(&i) {
                            let (r, _) = ui.allocate_exact_size(vec2(36.0, 9.0), Sense::hover());
                            ui.painter().hline(r.x_range().shrink(5.0), r.center().y, Stroke::new(1.0_f32, pal.border));
                        }
                        let selected = active == Some(tool);
                        let (r, resp) = ui.allocate_exact_size(vec2(36.0, 32.0), Sense::click());
                        let resp = if enabled { resp } else { resp.on_hover_cursor(egui::CursorIcon::NotAllowed) };
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
                        tool_icon(p, egui::Rect::from_center_size(r.center(), vec2(20.0, 20.0)), tool, fg);
                        if tool == default_tool {
                            p.circle_filled(r.right_top() + vec2(-5.0, 5.0), 2.5, if selected { Color32::WHITE } else { ACCENT });
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
                },
            );
        },
    );
    out
}
