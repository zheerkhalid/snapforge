//! The command palette (Ctrl+K): fuzzy search over every registered command, run with
//! Enter. Arrow keys move the selection, Esc closes.

use super::commands::{Action, CmdState, CommandRegistry};
use super::style::Palette as Colors;
use crate::i18n::t;
use eframe::egui::{self, vec2, Align2, Color32, CornerRadius, Id, Key, Margin, Modifiers, RichText, Sense};

const MAX_RESULTS: usize = 12;

#[derive(Default)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    focus_requested: bool,
}

pub enum PaletteOutcome {
    Open,
    Close,
    Run(Action),
}

pub fn show(
    ctx: &egui::Context,
    st: &mut PaletteState,
    reg: &CommandRegistry,
    state: &CmdState,
    pal: &Colors,
) -> PaletteOutcome {
    let results = reg.search(&st.query, MAX_RESULTS);
    if st.selected >= results.len() {
        st.selected = results.len().saturating_sub(1);
    }
    let (up, down, enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if esc {
        return PaletteOutcome::Close;
    }
    if down && !results.is_empty() {
        st.selected = (st.selected + 1) % results.len();
    }
    if up && !results.is_empty() {
        st.selected = (st.selected + results.len() - 1) % results.len();
    }
    let mut outcome = PaletteOutcome::Open;
    if enter {
        if let Some(c) = results.get(st.selected) {
            if (c.enabled)(state) {
                return PaletteOutcome::Run(c.action.clone());
            }
        }
    }

    // Dim the editor behind.
    let screen = ctx.screen_rect();
    egui::Area::new(Id::new("palette-dim")).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(70));
        if resp.clicked() {
            outcome = PaletteOutcome::Close;
        }
    });
    egui::Area::new(Id::new("palette"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 72.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(10)).show(
                ui,
                |ui| {
                    ui.set_width(560.0);
                    let edit = egui::TextEdit::singleline(&mut st.query)
                        .hint_text(t("editor-palette-hint"))
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::proportional(16.0))
                        .margin(Margin::symmetric(8, 6));
                    let resp = ui.add(edit);
                    if !st.focus_requested {
                        resp.request_focus();
                        st.focus_requested = true;
                    }
                    if resp.changed() {
                        st.selected = 0;
                    }
                    ui.add_space(6.0);
                    if results.is_empty() {
                        ui.label(RichText::new(t("editor-palette-empty")).color(pal.muted_text));
                    }
                    for (i, c) in results.iter().enumerate() {
                        let enabled = (c.enabled)(state);
                        let selected = i == st.selected;
                        let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                        if row.hovered() && ui.ctx().input(|inp| inp.pointer.delta() != egui::Vec2::ZERO) {
                            st.selected = i;
                        }
                        if selected {
                            ui.painter().rect_filled(
                                rect,
                                6.0,
                                super::style::ACCENT.gamma_multiply(if pal.dark { 0.45 } else { 0.18 }),
                            );
                        }
                        let text_color = if enabled { ui.visuals().strong_text_color() } else { pal.muted_text };
                        let left = rect.left_center() + vec2(10.0, 0.0);
                        let icon = if c.icon.is_empty() { "•" } else { c.icon };
                        ui.painter().text(
                            left,
                            Align2::LEFT_CENTER,
                            icon,
                            egui::FontId::proportional(14.0),
                            pal.muted_text,
                        );
                        ui.painter().text(
                            left + vec2(24.0, 0.0),
                            Align2::LEFT_CENTER,
                            &c.label,
                            egui::FontId::proportional(14.0),
                            text_color,
                        );
                        let right = rect.right_center() - vec2(10.0, 0.0);
                        let sc = c.shortcut_text();
                        let cat = t(c.category.label_key());
                        let tail = if sc.is_empty() { cat } else { format!("{cat}   {sc}") };
                        ui.painter().text(
                            right,
                            Align2::RIGHT_CENTER,
                            tail,
                            egui::FontId::proportional(12.0),
                            pal.muted_text,
                        );
                        if row.clicked() && enabled {
                            outcome = PaletteOutcome::Run(c.action.clone());
                        }
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new(t("editor-palette-footer")).small().color(pal.muted_text));
                },
            );
        });
    outcome
}
