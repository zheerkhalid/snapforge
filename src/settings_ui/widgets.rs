//! Look and feel plus small reusable widgets for the Settings window.

use crate::i18n::t;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Response, RichText, Sense, Stroke, TextStyle, Ui,
    Vec2, WidgetInfo, WidgetType,
};

pub const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x6B, 0xFF);

/// Colors for one theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub nav: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub error: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Palette {
                dark,
                bg: Color32::from_rgb(0x1B, 0x1C, 0x21),
                nav: Color32::from_rgb(0x16, 0x17, 0x1B),
                card: Color32::from_rgb(0x24, 0x26, 0x2D),
                border: Color32::from_rgb(0x33, 0x36, 0x40),
                text: Color32::from_rgb(0xE6, 0xE8, 0xEE),
                weak: Color32::from_rgb(0x9A, 0xA0, 0xAE),
                accent_text: Color32::from_rgb(0x8D, 0xA0, 0xFF),
                ok: Color32::from_rgb(0x5C, 0xC8, 0x8A),
                warn: Color32::from_rgb(0xF0, 0xB4, 0x4C),
                error: Color32::from_rgb(0xFF, 0x7A, 0x7A),
            }
        } else {
            Palette {
                dark,
                bg: Color32::from_rgb(0xF5, 0xF6, 0xFA),
                nav: Color32::from_rgb(0xEC, 0xEE, 0xF4),
                card: Color32::WHITE,
                border: Color32::from_rgb(0xDD, 0xE0, 0xE8),
                text: Color32::from_rgb(0x1F, 0x23, 0x2B),
                weak: Color32::from_rgb(0x66, 0x6D, 0x7A),
                accent_text: Color32::from_rgb(0x34, 0x4F, 0xE0),
                ok: Color32::from_rgb(0x1E, 0x8E, 0x4E),
                warn: Color32::from_rgb(0xB0, 0x6A, 0x00),
                error: Color32::from_rgb(0xC6, 0x28, 0x28),
            }
        }
    }
}

/// The window's style, derived from the host's (fonts stay the same).
pub fn style(base: &egui::Style, p: &Palette) -> egui::Style {
    let mut s = base.clone();
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = if p.dark { Color32::from_rgb(0x18, 0x19, 0x1E) } else { Color32::WHITE };
    v.faint_bg_color = p.nav;
    v.hyperlink_color = p.accent_text;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        // Small radius keeps check boxes square (a large one makes them look like radio buttons).
        w.corner_radius = CornerRadius::same(4);
        w.fg_stroke.color = p.text;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    let control_border = if p.dark { Color32::from_rgb(0x55, 0x5A, 0x68) } else { Color32::from_rgb(0xA8, 0xAE, 0xBC) };
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, control_border);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.bg_stroke = Stroke::new(1.5_f32, ACCENT);
    v.widgets.inactive.weak_bg_fill = if p.dark { Color32::from_rgb(0x2E, 0x31, 0x3A) } else { p.nav };
    v.widgets.inactive.bg_fill = v.widgets.inactive.weak_bg_fill;
    s.visuals = v;
    s.spacing.item_spacing = Vec2::new(8.0, 8.0);
    s.spacing.button_padding = Vec2::new(10.0, 4.0);
    s.spacing.interact_size.y = 24.0;
    s.spacing.combo_width = 220.0;
    s.text_styles.insert(TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional));
    s.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    s.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
    s.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    s
}

/// A titled card (rounded, bordered group).
pub fn card<R>(ui: &mut Ui, p: &Palette, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).strong().size(15.0));
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

/// The collapsible "More…" area with power-user options.
pub fn more(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    egui::CollapsingHeader::new(RichText::new(t("settings-more")).strong())
        .id_salt(("settings-more", id))
        .default_open(false)
        .show(ui, add);
}

/// A two-column form grid.
pub fn form(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(("settings-form", id))
        .num_columns(2)
        .spacing([18.0, 10.0])
        .min_col_width(170.0)
        .show(ui, |ui| add(ui));
}

/// One labelled form row: the label is linked to the control for screen readers.
pub fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    let l = ui.label(label);
    let r = ui.horizontal(|ui| add(ui)).inner.labelled_by(l.id);
    ui.end_row();
    r
}

/// Small grey explanatory text.
pub fn hint(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.label(RichText::new(text).small().color(p.weak))
}

/// Left-navigation entry: full-width, left-aligned, accent background when selected.
pub fn nav_item(ui: &mut Ui, p: &Palette, selected: bool, text: &str) -> Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, text));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            ACCENT
        } else if resp.hovered() || resp.has_focus() {
            if p.dark {
                Color32::from_rgb(0x2A, 0x2D, 0x36)
            } else {
                Color32::from_rgb(0xDF, 0xE3, 0xEE)
            }
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, CornerRadius::same(6), fill);
        if resp.has_focus() {
            painter.rect_stroke(
                rect.shrink(1.0),
                CornerRadius::same(6),
                Stroke::new(1.5_f32, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        let color = if selected { Color32::WHITE } else { p.text };
        painter.text(
            rect.left_center() + Vec2::new(12.0, 0.0),
            egui::Align2::LEFT_CENTER,
            text,
            FontId::proportional(14.0),
            color,
        );
    }
    resp
}

/// Accent-filled primary button.
pub fn primary_button(text: &str, min_width: f32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(Color32::WHITE))
        .fill(ACCENT)
        .min_size(Vec2::new(min_width, 28.0))
}

/// Hotkey capture button: shows the combination, or a prompt while capturing.
pub fn hotkey_button(ui: &mut Ui, p: &Palette, text: &str, capturing: bool, label: &str) -> Response {
    let shown = if capturing {
        RichText::new(t("settings-hotkey-press")).color(p.accent_text).italics()
    } else if text.trim().is_empty() {
        RichText::new(t("settings-hotkey-none")).color(p.weak)
    } else {
        RichText::new(crate::hotkeys::display(text)).monospace()
    };
    let mut b = egui::Button::new(shown).min_size(Vec2::new(210.0, 26.0));
    if capturing {
        b = b.stroke(Stroke::new(1.5_f32, ACCENT));
    }
    let r = ui.add(b);
    let label = label.to_string();
    let value = crate::hotkeys::display(text);
    r.widget_info(move || WidgetInfo::labeled(WidgetType::Button, true, format!("{label}: {value}")));
    r
}

/// Clickable link that opens in the default browser.
pub fn link(ui: &mut Ui, text: &str, url: &str) -> Response {
    let color = ui.visuals().hyperlink_color;
    let r = ui.link(RichText::new(text).color(color).underline()).on_hover_text(url);
    if r.clicked() {
        crate::common::win::shell_open(url);
    }
    r
}

/// Width of a button with this text in the current style (for right-aligned groups that
/// keep left-to-right Tab order).
pub fn button_width(ui: &Ui, text: &str, min: f32) -> f32 {
    let font = TextStyle::Button.resolve(ui.style());
    let w = ui.fonts(|f| f.layout_no_wrap(text.to_string(), font, Color32::WHITE).size().x);
    (w + 2.0 * ui.spacing().button_padding.x).max(min)
}
