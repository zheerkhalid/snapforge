//! Small widgets for the draw UI: colour boxes with swatches / eye dropper / screen pick,
//! the font list with live previews, and vector tool icons.

use super::super::style::ACCENT;
use super::super::texture::{image_options, tile_to_color_image, TileRect};
use crate::common::Image;
use crate::draw::{self, ArrowHead, Color, Dash, Tool};
use crate::i18n::t;
use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Sense, Stroke, TextureHandle};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub fn c32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

pub fn from_c32(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

/// Where a colour pick should come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickFrom {
    Canvas,
    Screen,
}

/// Paint a checkerboard (for colours with alpha).
fn checker(painter: &egui::Painter, r: Rect) {
    painter.rect_filled(r, 3.0, Color32::from_gray(250));
    let s = 5.0;
    let mut y = r.top();
    let mut row = 0;
    while y < r.bottom() {
        let mut x = r.left() + if row % 2 == 0 { 0.0 } else { s };
        while x < r.right() {
            let cell = Rect::from_min_max(pos2(x, y), pos2((x + s).min(r.right()), (y + s).min(r.bottom())));
            painter.rect_filled(cell, 0.0, Color32::from_gray(200));
            x += 2.0 * s;
        }
        y += s;
        row += 1;
    }
}

fn swatch(ui: &mut egui::Ui, c: Color, size: f32, selected: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let p = ui.painter();
    if c.a < 255 {
        checker(p, r);
    }
    p.rect_filled(r, 3.0, c32(c));
    let stroke = if selected || resp.hovered() {
        Stroke::new(2.0_f32, ACCENT)
    } else {
        Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    p.rect_stroke(r, 3.0, stroke, egui::StrokeKind::Inside);
    resp.on_hover_text(c.to_hex())
}

/// A colour box: click opens swatches, a picker, hex entry, eye dropper and screen pick;
/// right-click picks from the screen directly. Returns (changed, pick request).
pub fn color_box(
    ui: &mut egui::Ui,
    salt: &str,
    color: &mut Color,
    tip: &str,
    allow_none: bool,
) -> (bool, Option<PickFrom>) {
    let mut changed = false;
    let mut pick = None;
    let (r, resp) = ui.allocate_exact_size(vec2(30.0, 24.0), Sense::click());
    let p = ui.painter();
    let inner = r.shrink(3.0);
    p.rect_filled(r, 5.0, ui.visuals().widgets.inactive.bg_fill);
    if color.a < 255 {
        checker(p, inner);
    }
    p.rect_filled(inner, 3.0, c32(*color));
    if color.a == 0 {
        p.line_segment([inner.left_bottom(), inner.right_top()], Stroke::new(1.5_f32, Color32::from_rgb(220, 50, 50)));
    }
    p.rect_stroke(
        r,
        5.0,
        Stroke::new(1.0_f32, if resp.hovered() { ACCENT } else { ui.visuals().widgets.noninteractive.bg_stroke.color }),
        egui::StrokeKind::Inside,
    );
    let resp = resp.on_hover_text(format!("{tip}\n{}", t("draw-color-tip")));
    let popup_id = ui.make_persistent_id(("draw-color-popup", salt));
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    if resp.secondary_clicked() {
        pick = Some(PickFrom::Screen);
    }
    let presets = crate::settings::get().draw.preset_colors.clone();
    let recent = crate::settings::get().draw.recent_colors.clone();
    egui::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(236.0);
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for row in presets.chunks(8) {
            ui.horizontal(|ui| {
                for c in row {
                    if swatch(ui, *c, 24.0, *c == *color).clicked() {
                        *color = *c;
                        changed = true;
                    }
                }
            });
        }
        if !recent.is_empty() {
            ui.label(egui::RichText::new(t("draw-recent-colors")).small().weak());
            ui.horizontal_wrapped(|ui| {
                for c in recent.iter().take(16) {
                    if swatch(ui, *c, 18.0, *c == *color).clicked() {
                        *color = *c;
                        changed = true;
                    }
                }
            });
        }
        ui.separator();
        let mut cc = c32(*color);
        if egui::color_picker::color_picker_color32(ui, &mut cc, egui::color_picker::Alpha::OnlyBlend) {
            *color = from_c32(cc);
            changed = true;
        }
        ui.horizontal(|ui| {
            let mut hex = color.to_hex();
            let r = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(84.0).font(egui::TextStyle::Monospace));
            if r.changed() {
                if let Some(c) = Color::from_hex(&hex) {
                    *color = c;
                    changed = true;
                }
            }
            if ui.button("💧").on_hover_text(t("draw-eyedropper")).clicked() {
                pick = Some(PickFrom::Canvas);
                ui.close_menu();
            }
            if ui.button("🖥").on_hover_text(t("draw-screen-pick")).clicked() {
                pick = Some(PickFrom::Screen);
                ui.close_menu();
            }
            if allow_none && ui.button("⊘").on_hover_text(t("draw-no-color")).clicked() {
                *color = color.with_alpha(0);
                changed = true;
            }
        });
    });
    if pick.is_some() {
        ui.memory_mut(|m| m.close_popup());
    }
    (changed, pick)
}

// ------------------------------------------------------------------ fonts

/// The system font families (loaded on a worker) and their preview textures.
#[derive(Default)]
pub struct Fonts {
    list: Arc<Mutex<Option<Vec<String>>>>,
    started: bool,
    previews: HashMap<(String, bool), TextureHandle>,
}

impl Fonts {
    /// The families once loaded (starts loading on first call).
    pub fn families(&mut self) -> Option<Vec<String>> {
        if !self.started {
            self.started = true;
            let slot = self.list.clone();
            let _ = std::thread::Builder::new().name("draw-fonts".into()).spawn(move || {
                let mut v = draw::dwrite::font_families();
                v.sort_by_key(|s| s.to_lowercase());
                v.dedup();
                if let Ok(mut g) = slot.lock() {
                    *g = Some(v);
                }
            });
        }
        self.list.lock().ok().and_then(|g| g.clone())
    }

    /// The family name rendered in its own font (DirectWrite), cached. `budget` limits
    /// how many new previews are rendered this frame.
    pub fn preview(&mut self, ctx: &egui::Context, family: &str, dark: bool, budget: &mut u32) -> Option<TextureHandle> {
        let key = (family.to_string(), dark);
        if let Some(t) = self.previews.get(&key) {
            return Some(t.clone());
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let img = render_label(family, family, 15.0, if dark { Color::rgb(235, 236, 240) } else { Color::rgb(30, 32, 38) }, ctx.pixels_per_point())?;
        let ci = tile_to_color_image(&img, TileRect { x: 0, y: 0, w: img.width, h: img.height });
        let tex = ctx.load_texture(format!("font-{family}"), ci, image_options());
        self.previews.insert(key, tex.clone());
        Some(tex)
    }
}

/// Render `text` in `family` at `pt` points with DirectWrite (through a draw text object).
fn render_label(text: &str, family: &str, pt: f32, color: Color, scale: f32) -> Option<Image> {
    let mut style = draw::Style { width: 0.0, ..Default::default() };
    style.font.family = family.to_string();
    style.font.size = pt;
    style.font.color = color;
    let obj = draw::Object::new(
        1,
        draw::Kind::Text(draw::Text {
            rect: draw::RectF::new(0.0, 0.0, 10.0, 10.0),
            text: text.to_string(),
            wrap: false,
            padding: 0.0,
            ..Default::default()
        }),
        style,
    );
    let base = Image::new(1, 1);
    let (img, _) = draw::render_object(&base, &obj, scale)?;
    (!img.is_empty()).then_some(img)
}

// ------------------------------------------------------------------ previews in combos

/// A short line with the dash pattern (combo entries).
pub fn dash_preview(ui: &mut egui::Ui, d: Dash, color: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(64.0, 14.0), Sense::hover());
    let y = r.center().y;
    let (a, b) = (pos2(r.left() + 2.0, y), pos2(r.right() - 2.0, y));
    let st = Stroke::new(2.0_f32, color);
    let pat: &[f32] = match d {
        Dash::Solid => &[],
        Dash::Dash => &[8.0, 4.0],
        Dash::Dot => &[2.0, 3.0],
        Dash::DashDot => &[8.0, 3.0, 2.0, 3.0],
        Dash::LongDash => &[14.0, 5.0],
    };
    if pat.is_empty() {
        ui.painter().line_segment([a, b], st);
    } else {
        let mut x = a.x;
        let mut i = 0;
        while x < b.x {
            let len = pat[i % pat.len()];
            if i % 2 == 0 {
                ui.painter().line_segment([pos2(x, y), pos2((x + len).min(b.x), y)], st);
            }
            x += len;
            i += 1;
        }
    }
    resp
}

pub fn head_preview(ui: &mut egui::Ui, h: ArrowHead, at_end: bool, color: Color32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(44.0, 14.0), Sense::hover());
    let y = r.center().y;
    let (tail, tip) =
        if at_end { (pos2(r.left() + 2.0, y), pos2(r.right() - 3.0, y)) } else { (pos2(r.right() - 2.0, y), pos2(r.left() + 3.0, y)) };
    let st = Stroke::new(2.0_f32, color);
    ui.painter().line_segment([tail, tip], st);
    let dir = if at_end { 1.0 } else { -1.0 };
    let back = |d: f32| pos2(tip.x - dir * d, y);
    let p = ui.painter();
    match h {
        ArrowHead::None => {}
        ArrowHead::Triangle | ArrowHead::ThreeD | ArrowHead::Fancy => {
            p.add(egui::Shape::convex_polygon(
                vec![tip, pos2(tip.x - dir * 10.0, y - 5.0), pos2(tip.x - dir * 10.0, y + 5.0)],
                color,
                Stroke::NONE,
            ));
        }
        ArrowHead::Stealth => {
            p.add(egui::Shape::convex_polygon(
                vec![tip, pos2(tip.x - dir * 11.0, y - 5.0), back(7.0)],
                color,
                Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![tip, back(7.0), pos2(tip.x - dir * 11.0, y + 5.0)],
                color,
                Stroke::NONE,
            ));
        }
        ArrowHead::Open => {
            p.line_segment([tip, pos2(tip.x - dir * 9.0, y - 5.0)], st);
            p.line_segment([tip, pos2(tip.x - dir * 9.0, y + 5.0)], st);
        }
        ArrowHead::Diamond => {
            p.add(egui::Shape::convex_polygon(
                vec![tip, pos2(tip.x - dir * 6.0, y - 4.5), back(12.0), pos2(tip.x - dir * 6.0, y + 4.5)],
                color,
                Stroke::NONE,
            ));
        }
        ArrowHead::Circle => {
            p.circle_filled(back(4.5), 4.5, color);
        }
        ArrowHead::Bar => {
            p.line_segment([pos2(tip.x, y - 6.0), pos2(tip.x, y + 6.0)], st);
        }
    }
    resp
}

// ------------------------------------------------------------------ tool icons

/// Vector icon for a draw tool, drawn into `r` with `c`.
pub fn tool_icon(p: &egui::Painter, r: Rect, tool: Tool, c: Color32) {
    let s = r.width().min(r.height());
    let o = r.center() - vec2(s / 2.0, s / 2.0);
    // Coordinates in a 0..20 box.
    let q = |x: f32, y: f32| -> Pos2 { o + vec2(x, y) * (s / 20.0) };
    let st = Stroke::new(1.6_f32, c);
    let thin = Stroke::new(1.2_f32, c);
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], st);
    };
    let poly = |pts: Vec<Pos2>, closed: bool| {
        if closed {
            p.add(egui::Shape::closed_line(pts, st));
        } else {
            p.add(egui::Shape::line(pts, st));
        }
    };
    let arrow_head = |tip: Pos2, from: Pos2, size: f32| {
        let d = (tip - from).normalized();
        let n = vec2(-d.y, d.x);
        let k = size * s / 20.0;
        p.add(egui::Shape::convex_polygon(vec![tip, tip - d * k + n * k * 0.55, tip - d * k - n * k * 0.55], c, Stroke::NONE));
    };
    let text = |pos: Pos2, txt: &str, size: f32| {
        p.text(pos, egui::Align2::CENTER_CENTER, txt, egui::FontId::proportional(size * s / 20.0), c);
    };
    match tool {
        Tool::Select => {
            p.add(egui::Shape::convex_polygon(
                vec![q(5.0, 3.0), q(15.0, 11.5), q(10.5, 12.0), q(13.0, 17.0), q(11.0, 18.0), q(8.5, 13.0), q(5.0, 16.0)],
                c,
                Stroke::NONE,
            ));
        }
        Tool::Text => text(q(10.0, 10.5), "T", 17.0),
        Tool::Callout => {
            p.rect_stroke(Rect::from_min_max(q(2.5, 3.5), q(17.5, 13.0)), 3.0 * s / 20.0, st, egui::StrokeKind::Middle);
            poly(vec![q(6.0, 13.0), q(4.5, 17.5), q(10.0, 13.0)], false);
            line(q(5.5, 7.0), q(14.5, 7.0));
            p.line_segment([q(5.5, 9.8), q(11.5, 9.8)], thin);
        }
        Tool::Line => line(q(3.5, 16.5), q(16.5, 3.5)),
        Tool::Arrow => {
            line(q(3.5, 16.5), q(14.0, 6.0));
            arrow_head(q(17.0, 3.0), q(3.5, 16.5), 7.0);
        }
        Tool::Curve => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0;
                    let x = 3.0 + 14.0 * t;
                    let y = 10.0 - 6.0 * (t * std::f32::consts::TAU).sin();
                    q(x, y)
                })
                .collect();
            poly(pts, false);
        }
        Tool::Polyline => poly(vec![q(3.0, 16.0), q(7.0, 6.0), q(12.0, 13.0), q(17.0, 4.0)], false),
        Tool::Elbow => {
            poly(vec![q(3.5, 5.0), q(14.0, 5.0), q(14.0, 13.0)], false);
            arrow_head(q(14.0, 17.5), q(14.0, 5.0), 6.0);
        }
        Tool::Rect => {
            p.rect_stroke(Rect::from_min_max(q(3.0, 5.0), q(17.0, 15.0)), 1.0, st, egui::StrokeKind::Middle);
        }
        Tool::Ellipse => {
            let pts: Vec<Pos2> = (0..32)
                .map(|i| {
                    let a = i as f32 / 32.0 * std::f32::consts::TAU;
                    q(10.0 + 7.5 * a.cos(), 10.0 + 5.5 * a.sin())
                })
                .collect();
            poly(pts, true);
        }
        Tool::Polygon => poly(vec![q(10.0, 3.0), q(17.0, 8.5), q(14.5, 16.5), q(5.5, 16.5), q(3.0, 8.5)], true),
        Tool::Bracket => {
            poly(vec![q(12.0, 3.0), q(9.5, 3.5), q(9.5, 8.5), q(7.0, 10.0), q(9.5, 11.5), q(9.5, 16.5), q(12.0, 17.0)], false)
        }
        Tool::Highlighter => {
            p.rect_filled(Rect::from_min_max(q(2.5, 7.0), q(17.5, 13.0)), 2.0, Color32::from_rgba_unmultiplied(255, 221, 0, 170));
            p.line_segment([q(4.0, 10.0), q(16.0, 10.0)], thin);
        }
        Tool::LineHighlighter => {
            p.add(egui::Shape::line(
                vec![q(3.0, 14.0), q(7.0, 9.0), q(11.0, 12.0), q(16.0, 6.0)],
                Stroke::new(4.0 * s / 20.0, Color32::from_rgba_unmultiplied(255, 221, 0, 190)),
            ));
            p.add(egui::Shape::line(vec![q(3.0, 14.0), q(7.0, 9.0), q(11.0, 12.0), q(16.0, 6.0)], thin));
        }
        Tool::Magnifier => {
            p.circle_stroke(q(8.5, 8.5), 5.5 * s / 20.0, st);
            p.line_segment([q(12.5, 12.5), q(17.0, 17.0)], Stroke::new(2.6_f32, c));
            p.line_segment([q(6.0, 8.5), q(11.0, 8.5)], thin);
            p.line_segment([q(8.5, 6.0), q(8.5, 11.0)], thin);
        }
        Tool::Step => {
            p.circle_filled(q(10.0, 10.0), 7.5 * s / 20.0, c);
            let inv = if c.r() as u32 + c.g() as u32 + c.b() as u32 > 380 { Color32::BLACK } else { Color32::WHITE };
            p.text(q(10.0, 10.3), egui::Align2::CENTER_CENTER, "1", egui::FontId::proportional(10.5 * s / 20.0), inv);
        }
        Tool::Emoji => {
            p.circle_stroke(q(10.0, 10.0), 7.0 * s / 20.0, st);
            p.circle_filled(q(7.5, 8.0), 1.1 * s / 20.0, c);
            p.circle_filled(q(12.5, 8.0), 1.1 * s / 20.0, c);
            poly(vec![q(6.5, 11.5), q(8.5, 13.6), q(11.5, 13.6), q(13.5, 11.5)], false);
        }
        Tool::Sticker => {
            p.add(egui::Shape::convex_polygon(
                vec![q(3.0, 8.0), q(11.0, 8.0), q(11.0, 4.0), q(17.5, 10.0), q(11.0, 16.0), q(11.0, 12.0), q(3.0, 12.0)],
                c.gamma_multiply(0.35),
                st,
            ));
        }
        Tool::Image => {
            p.rect_stroke(Rect::from_min_max(q(2.5, 4.0), q(17.5, 16.0)), 2.0, st, egui::StrokeKind::Middle);
            poly(vec![q(3.5, 15.0), q(8.0, 9.5), q(11.5, 13.0), q(13.5, 11.0), q(16.5, 15.0)], false);
            p.circle_filled(q(13.5, 7.5), 1.4 * s / 20.0, c);
        }
        Tool::Cursor => {
            p.add(egui::Shape::convex_polygon(
                vec![q(6.0, 2.5), q(6.0, 15.5), q(9.0, 12.5), q(11.5, 17.5), q(13.5, 16.5), q(11.0, 11.5), q(15.0, 11.5)],
                c.gamma_multiply(0.25),
                st,
            ));
        }
        Tool::Blur => {
            for (i, a) in [0.25f32, 0.45, 0.7, 0.45, 0.25].iter().enumerate() {
                let x = 4.0 + i as f32 * 3.0;
                p.rect_filled(Rect::from_min_max(q(x, 4.0), q(x + 3.0, 16.0)), 0.0, c.gamma_multiply(*a));
            }
        }
        Tool::Pixelate => {
            for y in 0..3 {
                for x in 0..3 {
                    let a = if (x + y) % 2 == 0 { 0.85 } else { 0.35 };
                    let (fx, fy) = (4.0 + x as f32 * 4.0, 4.0 + y as f32 * 4.0);
                    p.rect_filled(Rect::from_min_max(q(fx, fy), q(fx + 4.0, fy + 4.0)), 0.0, c.gamma_multiply(a));
                }
            }
        }
        Tool::Eraser => {
            p.add(egui::Shape::convex_polygon(vec![q(3.0, 12.0), q(10.0, 5.0), q(17.0, 12.0), q(12.0, 17.0), q(8.0, 17.0)], c.gamma_multiply(0.3), st));
            line(q(6.5, 8.5), q(13.5, 15.5));
        }
        Tool::Fill => {
            p.add(egui::Shape::closed_line(vec![q(4.0, 9.0), q(10.0, 3.0), q(16.0, 9.0), q(10.0, 15.0)], st));
            p.circle_filled(q(16.5, 14.5), 1.8 * s / 20.0, c);
            p.add(egui::Shape::convex_polygon(vec![q(16.5, 11.0), q(18.0, 14.0), q(15.0, 14.0)], c, Stroke::NONE));
        }
        Tool::Pencil => {
            poly(vec![q(4.0, 16.0), q(5.0, 12.0), q(14.0, 3.0), q(17.0, 6.0), q(8.0, 15.0)], true);
            line(q(12.0, 5.0), q(15.0, 8.0));
        }
    }
}
