//! Editor look: light/dark visuals following the system theme, accent colour, app icon.

use eframe::egui::{self, vec2, Color32, CornerRadius, Margin, Stroke, Visuals};
use std::sync::Arc;

pub const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x6B, 0xFF);
const DIRTY_LIGHT: Color32 = Color32::from_rgb(0xE0, 0x8A, 0x00);
const DIRTY_DARK: Color32 = Color32::from_rgb(0xFF, 0xB0, 0x40);

/// Colours not covered by `egui::Visuals`.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub canvas: Color32,
    pub bar: Color32,
    pub tab_active: Color32,
    pub tab_hover: Color32,
    pub border: Color32,
    pub muted_text: Color32,
    pub dirty: Color32,
    pub toast_bg: Color32,
    pub error: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                canvas: Color32::from_rgb(0x17, 0x18, 0x1C),
                bar: Color32::from_rgb(0x22, 0x23, 0x29),
                tab_active: Color32::from_rgb(0x30, 0x32, 0x3A),
                tab_hover: Color32::from_rgb(0x2A, 0x2C, 0x33),
                border: Color32::from_rgb(0x34, 0x36, 0x3E),
                muted_text: Color32::from_rgb(0x9A, 0x9E, 0xAA),
                dirty: DIRTY_DARK,
                toast_bg: Color32::from_rgb(0x2E, 0x30, 0x38),
                error: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            }
        } else {
            Self {
                dark,
                canvas: Color32::from_rgb(0xE8, 0xEA, 0xEF),
                bar: Color32::from_rgb(0xF7, 0xF8, 0xFA),
                tab_active: Color32::WHITE,
                tab_hover: Color32::from_rgb(0xEC, 0xEE, 0xF3),
                border: Color32::from_rgb(0xDA, 0xDD, 0xE4),
                muted_text: Color32::from_rgb(0x6B, 0x70, 0x7C),
                dirty: DIRTY_LIGHT,
                toast_bg: Color32::from_rgb(0x26, 0x28, 0x30),
                error: Color32::from_rgb(0xD9, 0x3A, 0x3A),
            }
        }
    }
}

pub fn apply(ctx: &egui::Context, dark: bool) -> Palette {
    let pal = Palette::new(dark);
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.selection.bg_fill = if dark { ACCENT.gamma_multiply(0.75) } else { ACCENT.gamma_multiply(0.85) };
    v.selection.stroke = Stroke::new(1.0_f32, if dark { Color32::WHITE } else { ACCENT });
    v.hyperlink_color = if dark { Color32::from_rgb(0x8E, 0xA0, 0xFF) } else { ACCENT };
    v.panel_fill = pal.bar;
    v.window_fill = if dark { Color32::from_rgb(0x26, 0x27, 0x2E) } else { Color32::WHITE };
    v.extreme_bg_color = pal.canvas;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0_f32, pal.border);
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(60) };
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(45) };
    let (inactive, hovered, active) = if dark {
        (Color32::from_rgb(0x2C, 0x2E, 0x36), Color32::from_rgb(0x38, 0x3B, 0x45), Color32::from_rgb(0x43, 0x46, 0x52))
    } else {
        (Color32::from_rgb(0xEC, 0xEE, 0xF3), Color32::from_rgb(0xE0, 0xE4, 0xEC), Color32::from_rgb(0xD3, 0xD8, 0xE3))
    };
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.border);
    v.widgets.inactive.weak_bg_fill = inactive;
    v.widgets.inactive.bg_fill = inactive;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.weak_bg_fill = hovered;
    v.widgets.hovered.bg_fill = hovered;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.expansion = 0.0;
    v.widgets.active.weak_bg_fill = active;
    v.widgets.active.bg_fill = active;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.expansion = 0.0;
    v.widgets.open.weak_bg_fill = hovered;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = vec2(6.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.window_margin = Margin::same(16);
        s.spacing.interact_size.y = 26.0;
    });
    pal
}

/// The accent "primary" button colour set for `Continue to …`.
pub fn primary_button(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    let text: egui::WidgetText = text.into();
    egui::Button::new(text.color(Color32::WHITE).strong()).fill(ACCENT).corner_radius(CornerRadius::same(6))
}

/// App icon for the title bar / taskbar, decoded from the embedded PNG at 64 px.
pub fn app_icon() -> Option<Arc<egui::IconData>> {
    const PNG: &[u8] = include_bytes!("../../docs/assets/snapforge-icon.png");
    let img = match image::load_from_memory_with_format(PNG, image::ImageFormat::Png) {
        Ok(i) => i.into_rgba8(),
        Err(e) => {
            log::warn!("editor: icon decode failed: {e}");
            return None;
        }
    };
    let img = if img.width() > 64 {
        image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let (width, height) = img.dimensions();
    Some(Arc::new(egui::IconData { rgba: img.into_raw(), width, height }))
}
