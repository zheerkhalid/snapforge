//! The context-sensitive properties bar: style of the active tool (for new objects) or of
//! the selected objects (group edits, one undo step per gesture), presets, alignment and
//! z-order.

use super::layer;
use super::widgets::{color_box, dash_preview, head_preview, tool_icon, PickFrom};
use super::{tool_label, ColorSlot, DrawState, Order};
use crate::draw::edit::{Align, StylePatch};
use crate::draw::tools::{apply_options, options_of, FlowMode};
use crate::draw::{
    ArrowHead, Color, CursorShape, Dash, HighlightShape, Kind, LineDecor, LineKind, MagnifierShape, Object, Outline,
    Pt, Shadow, ShapeKind, Side, StepShape, StickerShape, Tail, TextAlign, TextBox, TextFlow, Tool, ToolOptions,
};
use crate::editor::document::Document;
use crate::editor::style::{Palette, ACCENT};
use crate::i18n::t;
use crate::settings::{self, draw::Preset, draw::ToolStyle};
use eframe::egui::{self, vec2, Color32, RichText};

/// What a tool's properties offer.
#[derive(Default, Clone, Copy)]
struct Caps {
    stroke: bool,
    fill: bool,
    width: bool,
    dash: bool,
    heads: bool,
    font: bool,
    shadow: bool,
    outline: bool,
    opacity: bool,
}

fn caps(tool: Tool) -> Caps {
    use Tool::*;
    let line = matches!(tool, Line | Arrow | Curve | Polyline | Elbow);
    let shape = matches!(tool, Rect | Ellipse | Polygon | Bracket);
    let text = matches!(tool, Text | Callout);
    Caps {
        stroke: line || shape || text || matches!(tool, Highlighter | Magnifier | Step | Pencil),
        fill: matches!(
            tool,
            Rect | Ellipse | Polygon | Callout | Text | Highlighter | LineHighlighter | Step | Sticker | Cursor | Fill
        ),
        width: line || shape || text || matches!(tool, Highlighter | LineHighlighter | Magnifier | Step | Pencil),
        dash: line || shape || text,
        heads: line,
        font: text || tool == Step,
        shadow: line || shape || text || matches!(tool, Magnifier | Step | Emoji | Sticker | Image | Cursor),
        outline: line || text,
        opacity: !matches!(tool, Select | Eraser | Fill | Blur | Pixelate),
    }
}

const EMOJIS: [&str; 40] = [
    "😀", "😂", "😍", "😎", "🤔", "😮", "😢", "😡", "👍", "👎", "👏", "🙏", "👉", "👆", "👇", "👈", "✅", "❌", "⚠️",
    "❗", "❓", "💡", "⭐", "🔥", "❤️", "🎉", "📌", "📎", "🔒", "🔑", "🐞", "🚀", "⏰", "📷", "💬", "✏️", "📝", "🔍",
    "➡️", "⬅️",
];

/// The bar contents. `tool`: the active draw tool, if any. `hwnd` owns the screen picker.
pub fn properties_bar(
    ui: &mut egui::Ui,
    st: &mut DrawState,
    mut doc: Option<&mut Document>,
    tool: Option<Tool>,
    pal: &Palette,
    hwnd: isize,
) {
    let sel = doc.as_deref_mut().map(|d| st.selection(d)).unwrap_or_default();
    let targets: Vec<Object> = match doc.as_deref() {
        Some(d) => layer::layer(d)
            .map(|l| l.objects().iter().filter(|o| sel.contains(&o.id)).cloned().collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let (kind, mut proto) = match (targets.first(), tool) {
        (Some(o), _) => {
            let k = Tool::of(o);
            let base = st.style_for(k).options.clone();
            (k, ToolStyle { style: o.style.clone(), options: options_of(o, &base) })
        }
        (None, Some(t)) if t != Tool::Select => (t, st.style_for(t).clone()),
        _ => {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new(t("draw-select-hint")).color(pal.muted_text));
            });
            return;
        }
    };
    let orig = proto.clone();
    if let Some((slot, c)) = st.picked.take() {
        set_slot(&mut proto, slot, c);
    }
    let c = caps(kind);
    let mut pick: Option<(ColorSlot, PickFrom)> = None;
    let mut tail_toggle: Option<bool> = None;
    let mut action: Option<Act> = None;
    let muted = pal.muted_text;

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        // Header: tool icon and name.
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 24.0), egui::Sense::hover());
        tool_icon(ui.painter(), r.shrink(2.0), kind, ACCENT);
        let head = if targets.len() > 1 {
            format!(
                "{} · {}",
                tool_label(kind),
                crate::i18n::tf("draw-n-selected", &[("count", &targets.len().to_string())])
            )
        } else {
            tool_label(kind)
        };
        ui.label(RichText::new(head).strong());
        sep(ui, pal);

        let s = &mut proto.style;
        if c.stroke {
            let label = if matches!(kind, Tool::Text | Tool::Callout | Tool::Highlighter | Tool::Step) {
                t("draw-prop-border")
            } else {
                t("draw-prop-color")
            };
            color_slot(ui, "stroke", &mut s.color, &label, true, ColorSlot::Stroke, &mut pick);
        }
        if c.fill {
            let label = if kind == Tool::Highlighter || kind == Tool::LineHighlighter {
                t("draw-prop-highlight")
            } else {
                t("draw-prop-fill")
            };
            color_slot(ui, "fill", &mut s.fill, &label, true, ColorSlot::Fill, &mut pick);
        }
        if c.font {
            color_slot(ui, "text", &mut s.font.color, &t("draw-prop-text-color"), false, ColorSlot::Text, &mut pick);
        }
        if c.width {
            ui.label(RichText::new(t("draw-prop-width")).color(muted));
            ui.add(egui::DragValue::new(&mut s.width).range(0.0..=100.0).speed(0.2).max_decimals(1).suffix(" px"));
        }
        if c.dash {
            egui::ComboBox::from_id_salt("draw-dash").width(78.0).selected_text(dash_label(s.dash)).show_ui(ui, |ui| {
                for d in Dash::ALL {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(s.dash == d, dash_label(d)).clicked() {
                            s.dash = d;
                        }
                        dash_preview(ui, d, ui.visuals().strong_text_color());
                    });
                }
            });
        }
        if c.opacity {
            ui.label(RichText::new(t("draw-prop-opacity")).color(muted));
            let mut pct = (s.opacity * 100.0).round();
            if ui
                .add(egui::DragValue::new(&mut pct).range(5.0..=100.0).speed(0.5).max_decimals(0).suffix("%"))
                .changed()
            {
                s.opacity = pct / 100.0;
            }
        }
        if c.heads {
            sep(ui, pal);
            let o = &mut proto.options;
            head_combo(ui, "draw-head-start", &mut o.head_start, false);
            head_combo(ui, "draw-head-end", &mut o.head_end, true);
            ui.add(egui::DragValue::new(&mut o.head_size).range(0.3..=5.0).speed(0.02).max_decimals(1).prefix("×"))
                .on_hover_text(t("draw-prop-head-size"));
            egui::ComboBox::from_id_salt("draw-decor")
                .width(96.0)
                .selected_text(decor_label(o.decor))
                .show_ui(ui, |ui| {
                    for d in LineDecor::ALL {
                        ui.selectable_value(&mut o.decor, d, decor_label(d));
                    }
                })
                .response
                .on_hover_text(t("draw-prop-decor"));
            if matches!(o.decor, LineDecor::Gradient | LineDecor::Neon | LineDecor::Railroad) {
                color_slot(ui, "color2", &mut o.color2, &t("draw-prop-color2"), false, ColorSlot::Color2, &mut pick);
            }
            if kind == Tool::Curve && targets.is_empty() {
                egui::ComboBox::from_id_salt("draw-curve-kind")
                    .width(84.0)
                    .selected_text(curve_label(o.line_kind))
                    .show_ui(ui, |ui| {
                        for k in [LineKind::Quad, LineKind::Cubic, LineKind::Smooth] {
                            ui.selectable_value(&mut o.line_kind, k, curve_label(k));
                        }
                    });
            }
            if kind == Tool::Elbow {
                let mut v = o.line_kind == LineKind::ElbowV;
                if ui.toggle_value(&mut v, t("draw-elbow-vertical")).changed() {
                    o.line_kind = if v { LineKind::ElbowV } else { LineKind::ElbowH };
                }
            }
        }
        if c.font {
            sep(ui, pal);
            let s = &mut proto.style;
            font_combo(ui, st, &mut s.font.family, pal.dark);
            ui.add(egui::DragValue::new(&mut s.font.size).range(1.0..=200.0).speed(0.25).max_decimals(1).suffix(" pt"))
                .on_hover_text(t("draw-prop-font-size"));
            ui.toggle_value(&mut s.font.bold, RichText::new("B").strong()).on_hover_text(t("draw-prop-bold"));
            ui.toggle_value(&mut s.font.italic, RichText::new("I").italics()).on_hover_text(t("draw-prop-italic"));
            ui.toggle_value(&mut s.font.underline, RichText::new("U").underline())
                .on_hover_text(t("draw-prop-underline"));
            if kind != Tool::Step {
                for (a, g) in [(TextAlign::Left, "⬅"), (TextAlign::Center, "↔"), (TextAlign::Right, "➡")] {
                    if ui.selectable_label(s.font.align == a, g).on_hover_text(t("draw-prop-align")).clicked() {
                        s.font.align = a;
                    }
                }
            }
        }
        if c.shadow || c.outline {
            sep(ui, pal);
            let s = &mut proto.style;
            if c.shadow {
                let mut on = s.shadow.is_some();
                if ui.toggle_value(&mut on, t("draw-prop-shadow")).changed() {
                    s.shadow = on.then(Shadow::default);
                }
                if let Some(sh) = &mut s.shadow {
                    color_slot(
                        ui,
                        "shadow",
                        &mut sh.color,
                        &t("draw-prop-shadow"),
                        false,
                        ColorSlot::Shadow,
                        &mut pick,
                    );
                    ui.add(
                        egui::DragValue::new(&mut sh.blur).range(0.0..=40.0).speed(0.2).max_decimals(0).prefix("◌ "),
                    )
                    .on_hover_text(t("draw-shadow-blur"));
                }
            }
            if c.outline {
                let mut on = s.outline.is_some();
                if ui.toggle_value(&mut on, t("draw-prop-outline")).changed() {
                    s.outline = on.then(Outline::default);
                }
                if let Some(ol) = &mut s.outline {
                    color_slot(
                        ui,
                        "outline",
                        &mut ol.color,
                        &t("draw-prop-outline"),
                        false,
                        ColorSlot::Outline,
                        &mut pick,
                    );
                    ui.add(
                        egui::DragValue::new(&mut ol.width).range(0.5..=20.0).speed(0.1).max_decimals(1).suffix(" px"),
                    );
                }
            }
        }
        kind_options(ui, kind, &mut proto.options, &targets, &mut tail_toggle, pal);

        // Presets.
        sep(ui, pal);
        presets_menu(ui, kind, &mut proto);

        if !targets.is_empty() {
            sep(ui, pal);
            ui.menu_button(t("draw-align"), |ui| {
                ui.set_min_width(200.0);
                for (a, key) in [
                    (Align::Left, "draw-align-left"),
                    (Align::Right, "draw-align-right"),
                    (Align::Top, "draw-align-top"),
                    (Align::Bottom, "draw-align-bottom"),
                    (Align::CenterH, "draw-align-center-h"),
                    (Align::CenterV, "draw-align-center-v"),
                    (Align::SameWidth, "draw-same-width"),
                    (Align::SameHeight, "draw-same-height"),
                    (Align::SameSize, "draw-same-size"),
                ] {
                    if ui.add_enabled(targets.len() > 1, egui::Button::new(t(key))).clicked() {
                        action = Some(Act::Align(a));
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button(t("draw-order"), |ui| {
                ui.set_min_width(180.0);
                for (o, key) in [
                    (Order::Front, "draw-bring-to-front"),
                    (Order::Forward, "draw-bring-forward"),
                    (Order::Backward, "draw-send-backward"),
                    (Order::Back, "draw-send-to-back"),
                ] {
                    if ui.button(t(key)).clicked() {
                        action = Some(Act::Order(o));
                        ui.close_menu();
                    }
                }
            });
            if ui.button(t("draw-duplicate")).clicked() {
                action = Some(Act::Duplicate);
            }
            if ui.button(format!("🗑 {}", t("draw-delete"))).clicked() {
                action = Some(Act::Delete);
            }
        }
    });

    // Colour picks.
    if let Some((slot, from)) = pick {
        match from {
            PickFrom::Canvas => st.pick = Some(slot),
            PickFrom::Screen => {
                if let Some(c) = crate::editor::screen_pick::pick_screen_color(hwnd) {
                    set_slot(&mut proto, slot, c);
                }
            }
        }
    }

    let Some(doc) = doc else {
        if proto != orig {
            *st.style_for(kind) = proto;
        }
        remember_when_idle(ui, st, kind);
        return;
    };
    if proto != orig || tail_toggle.is_some() {
        if targets.is_empty() {
            *st.style_for(kind) = proto.clone();
        } else {
            let before = layer::layer(doc).map(|l| l.objects().to_vec()).unwrap_or_default();
            if st.prop_before.is_none() {
                st.prop_before = Some((doc.id, before));
            }
            if let Some(l) = layer::layer_mut(doc) {
                for o in l.objects_mut().iter_mut().filter(|o| sel.contains(&o.id)) {
                    apply_diff(o, &orig, &proto);
                    if let (Some(on), Kind::Text(t)) = (tail_toggle, &mut o.kind) {
                        t.tail = on.then(|| Tail {
                            tip: Pt::new(t.rect.x + t.rect.w * 0.2, t.rect.bottom() + 48.0),
                            width: 22.0,
                        });
                        if on && t.boxed == TextBox::None {
                            t.boxed = TextBox::Rounded;
                        }
                    }
                }
            }
            // New objects of this kind follow the edited style.
            *st.style_for(kind) = proto.clone();
        }
        for (a, b) in [
            (orig.style.color, proto.style.color),
            (orig.style.fill, proto.style.fill),
            (orig.style.font.color, proto.style.font.color),
        ] {
            if a != b && b.a > 0 {
                st.recent.push(b);
            }
        }
    }
    if let Some(a) = action {
        st.commit_props(doc);
        match a {
            Act::Align(a) => {
                st.align(doc, a);
            }
            Act::Order(o) => {
                st.order(doc, o);
            }
            Act::Duplicate => {
                st.duplicate(doc);
            }
            Act::Delete => {
                st.delete_selected(doc);
            }
        }
    }
    if !ui.input(|i| i.pointer.any_down()) {
        st.commit_props(doc);
    }
    remember_when_idle(ui, st, kind);
}

enum Act {
    Align(Align),
    Order(Order),
    Duplicate,
    Delete,
}

/// Persist the edited tool style and recent colours once the pointer is released.
fn remember_when_idle(ui: &egui::Ui, st: &mut DrawState, kind: Tool) {
    if ui.input(|i| i.pointer.any_down()) {
        return;
    }
    if !st.recent.is_empty() {
        let colors = std::mem::take(&mut st.recent);
        settings::update(|s| {
            for c in colors {
                s.draw.push_recent_color(c);
            }
        });
    }
    if st.styles.get(&kind).is_some_and(|ts| settings::get().draw.last_styles.get(kind.id()) != Some(ts)) {
        st.remember(kind);
    }
}

fn sep(ui: &mut egui::Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(9.0, 24.0), egui::Sense::hover());
    ui.painter().vline(r.center().x, (r.top() + 3.0)..=(r.bottom() - 3.0), egui::Stroke::new(1.0_f32, pal.border));
}

fn color_slot(
    ui: &mut egui::Ui,
    salt: &str,
    c: &mut Color,
    tip: &str,
    allow_none: bool,
    slot: ColorSlot,
    pick: &mut Option<(ColorSlot, PickFrom)>,
) {
    let (_, p) = color_box(ui, salt, c, tip, allow_none);
    if let Some(p) = p {
        *pick = Some((slot, p));
    }
}

fn set_slot(ts: &mut ToolStyle, slot: ColorSlot, c: Color) {
    let s = &mut ts.style;
    match slot {
        ColorSlot::Stroke => s.color = c,
        ColorSlot::Fill => s.fill = c,
        ColorSlot::Text => s.font.color = c,
        ColorSlot::Color2 => ts.options.color2 = c,
        ColorSlot::Outline => s.outline.get_or_insert_with(Outline::default).color = c,
        ColorSlot::Shadow => s.shadow.get_or_insert_with(Shadow::default).color = c,
    }
}

fn head_combo(ui: &mut egui::Ui, salt: &str, h: &mut ArrowHead, end: bool) {
    let color = ui.visuals().strong_text_color();
    let tip = if end { t("draw-prop-head-end") } else { t("draw-prop-head-start") };
    let r = egui::ComboBox::from_id_salt(salt).width(58.0).selected_text(" ").show_ui(ui, |ui| {
        for a in ArrowHead::ALL {
            let resp = ui.horizontal(|ui| {
                let sel = ui.selectable_label(*h == a, head_label(a));
                head_preview(ui, a, end, color);
                sel
            });
            if resp.inner.clicked() {
                *h = a;
            }
        }
    });
    // Preview of the current head on the combo button (left of its arrow).
    let rect = r.response.rect;
    let area =
        egui::Rect::from_min_max(rect.min + vec2(7.0, 5.0), egui::pos2(rect.right() - 20.0, rect.bottom() - 5.0));
    paint_head_mini(&ui.painter_at(rect), area, *h, end, color);
    r.response.on_hover_text(tip);
}

fn paint_head_mini(p: &egui::Painter, rect: egui::Rect, h: ArrowHead, end: bool, color: Color32) {
    let y = rect.center().y;
    let st = egui::Stroke::new(1.8_f32, color);
    let (tail, tip) = if end {
        (egui::pos2(rect.left(), y), egui::pos2(rect.right(), y))
    } else {
        (egui::pos2(rect.right(), y), egui::pos2(rect.left(), y))
    };
    p.line_segment([tail, tip], st);
    if h != ArrowHead::None {
        let d = if end { 1.0 } else { -1.0 };
        p.add(egui::Shape::convex_polygon(
            vec![tip, egui::pos2(tip.x - d * 7.0, y - 4.0), egui::pos2(tip.x - d * 7.0, y + 4.0)],
            color,
            egui::Stroke::NONE,
        ));
    }
}

fn font_combo(ui: &mut egui::Ui, st: &mut DrawState, family: &mut String, dark: bool) {
    let shown: String = family.chars().take(18).collect();
    // A fixed-size popup (a ComboBox keeps the size of its first, still-loading frame).
    let button =
        ui.add(egui::Button::new(format!("{shown}  ⏷")).min_size(vec2(150.0, 0.0))).on_hover_text(t("draw-prop-font"));
    let popup_id = ui.make_persistent_id("draw-font-popup");
    if button.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    egui::popup_below_widget(ui, popup_id, &button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_size(vec2(280.0, 360.0));
        let Some(list) = st.fonts.families() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t("editor-loading"));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let filter_id = egui::Id::new("draw-font-filter");
        let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id).unwrap_or_default());
        let r = ui.add(egui::TextEdit::singleline(&mut filter).hint_text(t("draw-font-search")).desired_width(240.0));
        if r.changed() {
            ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
        }
        let f = filter.to_lowercase();
        let items: Vec<&String> = list.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)).collect();
        let mut budget = 6u32;
        let row_h = 28.0;
        egui::ScrollArea::vertical().id_salt("draw-font-list").max_height(320.0).min_scrolled_height(320.0).show_rows(
            ui,
            row_h,
            items.len(),
            |ui, range| {
                for name in &items[range] {
                    let selected = family.eq_ignore_ascii_case(name);
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), egui::Sense::click());
                    if selected || resp.hovered() {
                        let fill = if selected {
                            ACCENT.gamma_multiply(0.25)
                        } else {
                            ui.visuals().widgets.hovered.weak_bg_fill
                        };
                        ui.painter().rect_filled(rect, 4.0, fill);
                    }
                    match st.fonts.preview(ui.ctx(), name, dark, &mut budget) {
                        Some(tex) => {
                            let ppp = ui.ctx().pixels_per_point();
                            let size = tex.size_vec2() / ppp;
                            let size = if size.y > row_h - 2.0 { size * ((row_h - 2.0) / size.y) } else { size };
                            let size = vec2(size.x.min(rect.width() - 12.0), size.y);
                            let r = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 6.0, rect.center().y - size.y / 2.0),
                                size,
                            );
                            let uv_w = (size.x / (tex.size_vec2().x / ppp)).min(1.0);
                            ui.painter().image(
                                tex.id(),
                                r,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_w, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        None => {
                            ui.painter().text(
                                egui::pos2(rect.left() + 6.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                name.as_str(),
                                egui::FontId::proportional(14.0),
                                ui.visuals().text_color(),
                            );
                            ui.ctx().request_repaint();
                        }
                    }
                    if resp.on_hover_text(name.as_str()).clicked() {
                        *family = name.to_string();
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            },
        );
    });
}

fn kind_options(
    ui: &mut egui::Ui,
    kind: Tool,
    o: &mut ToolOptions,
    targets: &[Object],
    tail_toggle: &mut Option<bool>,
    pal: &Palette,
) {
    let muted = pal.muted_text;
    let lbl = |ui: &mut egui::Ui, k: &str| {
        ui.label(RichText::new(t(k)).color(muted));
    };
    match kind {
        Tool::Text | Tool::Callout => {
            sep(ui, pal);
            egui::ComboBox::from_id_salt("draw-box")
                .width(90.0)
                .selected_text(box_label(o.text_box))
                .show_ui(ui, |ui| {
                    for b in [TextBox::None, TextBox::Rect, TextBox::Rounded, TextBox::Oval] {
                        ui.selectable_value(&mut o.text_box, b, box_label(b));
                    }
                })
                .response
                .on_hover_text(t("draw-prop-text-box"));
            egui::ComboBox::from_id_salt("draw-flow")
                .width(90.0)
                .selected_text(flow_label(o.text_flow))
                .show_ui(ui, |ui| {
                    for f in [FlowMode::Straight, FlowMode::Arc, FlowMode::Path] {
                        ui.selectable_value(&mut o.text_flow, f, flow_label(f));
                    }
                })
                .response
                .on_hover_text(t("draw-prop-text-flow"));
            ui.toggle_value(&mut o.wrap, t("draw-prop-wrap"));
            if o.text_box != TextBox::None {
                lbl(ui, "draw-prop-padding");
                ui.add(egui::DragValue::new(&mut o.padding).range(0.0..=80.0).speed(0.2).max_decimals(0));
            }
            if o.text_box == TextBox::Rounded {
                lbl(ui, "draw-prop-radius");
                ui.add(egui::DragValue::new(&mut o.radius).range(0.0..=80.0).speed(0.2).max_decimals(0));
            }
            let has_tail = targets.first().is_some_and(|x| matches!(&x.kind, Kind::Text(t) if t.tail.is_some()));
            if !targets.is_empty() {
                let mut on = has_tail;
                if ui.toggle_value(&mut on, t("draw-prop-tail")).changed() {
                    *tail_toggle = Some(on);
                }
            }
            if kind == Tool::Callout || has_tail {
                lbl(ui, "draw-prop-tail-width");
                ui.add(egui::DragValue::new(&mut o.tail_width).range(4.0..=200.0).speed(0.3).max_decimals(0));
            }
        }
        Tool::Rect => {
            sep(ui, pal);
            lbl(ui, "draw-prop-radius");
            ui.add(egui::DragValue::new(&mut o.radius).range(0.0..=200.0).speed(0.3).max_decimals(0));
        }
        Tool::Bracket => {
            sep(ui, pal);
            let mut square = o.shape == ShapeKind::SquareBracket;
            if ui.toggle_value(&mut square, "[ ]").on_hover_text(t("draw-bracket-square")).changed() {
                o.shape = if square { ShapeKind::SquareBracket } else { ShapeKind::CurlyBracket };
            }
            for (s, g) in [(Side::Left, "◀"), (Side::Right, "▶"), (Side::Top, "▲"), (Side::Bottom, "▼")] {
                if ui.selectable_label(o.facing == s, g).on_hover_text(t("draw-prop-facing")).clicked() {
                    o.facing = s;
                }
            }
        }
        Tool::Highlighter => {
            sep(ui, pal);
            for (s, key) in [
                (HighlightShape::Rect, "draw-shape-rect"),
                (HighlightShape::Rounded, "draw-shape-rounded"),
                (HighlightShape::Oval, "draw-shape-oval"),
            ] {
                if ui.selectable_label(o.highlight == s, t(key)).clicked() {
                    o.highlight = s;
                }
            }
            lbl(ui, "draw-prop-blur-outside");
            ui.add(egui::DragValue::new(&mut o.blur_outside).range(0.0..=40.0).speed(0.2).max_decimals(0));
            lbl(ui, "draw-prop-dim-outside");
            let mut pct = (o.dim_outside * 100.0).round();
            if ui.add(egui::DragValue::new(&mut pct).range(0.0..=95.0).speed(0.5).suffix("%")).changed() {
                o.dim_outside = pct / 100.0;
            }
        }
        Tool::Magnifier => {
            sep(ui, pal);
            lbl(ui, "draw-prop-zoom");
            ui.add(egui::DragValue::new(&mut o.zoom).range(1.0..=16.0).speed(0.05).max_decimals(1).prefix("×"));
            for (s, key) in [
                (MagnifierShape::Circle, "draw-shape-oval"),
                (MagnifierShape::Rect, "draw-shape-rect"),
                (MagnifierShape::Rounded, "draw-shape-rounded"),
            ] {
                if ui.selectable_label(o.magnifier == s, t(key)).clicked() {
                    o.magnifier = s;
                }
            }
            ui.toggle_value(&mut o.connector, t("draw-prop-connector"));
        }
        Tool::Step => {
            sep(ui, pal);
            for (s, g) in [(StepShape::Circle, "●"), (StepShape::Square, "■"), (StepShape::Teardrop, "💧")] {
                if ui.selectable_label(o.step_shape == s, g).on_hover_text(t("draw-prop-step-shape")).clicked() {
                    o.step_shape = s;
                }
            }
            ui.toggle_value(&mut o.step_letters, "A B C").on_hover_text(t("draw-prop-step-letters"));
            lbl(ui, "draw-prop-size");
            ui.add(egui::DragValue::new(&mut o.step_size).range(8.0..=400.0).speed(0.3).max_decimals(0).suffix(" px"));
        }
        Tool::Emoji => {
            sep(ui, pal);
            ui.menu_button(RichText::new(format!("{}  ▾", o.emoji)).size(17.0), |ui| {
                ui.set_max_width(300.0);
                ui.horizontal_wrapped(|ui| {
                    for e in EMOJIS {
                        if ui.add(egui::Button::new(RichText::new(e).size(20.0)).frame(false)).clicked() {
                            o.emoji = e.to_string();
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(t("draw-emoji-custom"));
                    ui.add(egui::TextEdit::singleline(&mut o.emoji).desired_width(60.0));
                });
            })
            .response
            .on_hover_text(t("draw-tip-emoji"));
        }
        Tool::Sticker => {
            sep(ui, pal);
            egui::ComboBox::from_id_salt("draw-sticker").width(120.0).selected_text(sticker_label(o.sticker)).show_ui(
                ui,
                |ui| {
                    for s in StickerShape::ALL {
                        ui.selectable_value(&mut o.sticker, s, sticker_label(s));
                    }
                },
            );
        }
        Tool::Cursor => {
            sep(ui, pal);
            egui::ComboBox::from_id_salt("draw-cursor").width(120.0).selected_text(cursor_label(o.cursor)).show_ui(
                ui,
                |ui| {
                    for s in CursorShape::ALL {
                        ui.selectable_value(&mut o.cursor, s, cursor_label(s));
                    }
                },
            );
        }
        Tool::Blur | Tool::Pixelate => {
            sep(ui, pal);
            lbl(ui, "draw-prop-strength");
            ui.add(
                egui::DragValue::new(&mut o.blur_strength).range(1.0..=80.0).speed(0.2).max_decimals(0).suffix(" px"),
            );
            ui.toggle_value(&mut o.blur_ellipse, t("draw-shape-oval"));
        }
        Tool::Eraser => {
            sep(ui, pal);
            lbl(ui, "draw-prop-radius-brush");
            ui.add(
                egui::DragValue::new(&mut o.eraser_radius).range(1.0..=500.0).speed(0.3).max_decimals(0).suffix(" px"),
            )
            .on_hover_text(t("draw-eraser-wheel"));
            let mut transparent = o.eraser_background.is_none();
            if ui.toggle_value(&mut transparent, t("draw-prop-transparent")).changed() {
                o.eraser_background = if transparent { None } else { Some(Color::WHITE) };
            }
            if let Some(bg) = &mut o.eraser_background {
                let _ = color_box(ui, "eraser-bg", bg, &t("draw-prop-background"), false);
            }
        }
        Tool::Fill => {
            sep(ui, pal);
            lbl(ui, "draw-prop-tolerance");
            let mut tol = o.fill_tolerance as i32;
            if ui.add(egui::DragValue::new(&mut tol).range(0..=255)).changed() {
                o.fill_tolerance = tol.clamp(0, 255) as u8;
            }
            ui.toggle_value(&mut o.fill_transparent, t("draw-prop-transparent"));
        }
        _ => {}
    }
}

fn presets_menu(ui: &mut egui::Ui, kind: Tool, proto: &mut ToolStyle) {
    let presets: Vec<Preset> = settings::get().draw.presets_for(kind).cloned().collect();
    let name_id = egui::Id::new("draw-preset-name");
    ui.menu_button(format!("★ {}", t("draw-presets")), |ui| {
        ui.set_min_width(220.0);
        if presets.is_empty() {
            ui.label(RichText::new(t("draw-presets-empty")).weak());
        }
        for p in &presets {
            ui.horizontal(|ui| {
                if ui.button(&p.name).on_hover_text(t("draw-preset-apply")).clicked() {
                    proto.style = p.style.clone();
                    proto.options = p.options.clone();
                    ui.close_menu();
                }
                if ui.small_button("✕").on_hover_text(t("draw-preset-delete")).clicked() {
                    let (tool, name) = (p.tool, p.name.clone());
                    settings::update(|s| {
                        s.draw.delete_preset(tool, &name);
                    });
                }
            });
        }
        ui.separator();
        let mut name: String = ui.data_mut(|d| d.get_temp(name_id).unwrap_or_default());
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(t("draw-preset-name")).desired_width(130.0));
            if r.changed() {
                ui.data_mut(|d| d.insert_temp(name_id, name.clone()));
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!name.trim().is_empty(), egui::Button::new(t("draw-save-preset"))).clicked() || enter)
                && !name.trim().is_empty()
            {
                let preset = Preset {
                    name: name.trim().to_string(),
                    tool: kind,
                    style: proto.style.clone(),
                    options: proto.options.clone(),
                };
                settings::update(|s| s.draw.save_preset(preset));
                ui.data_mut(|d| d.remove::<String>(name_id));
                ui.close_menu();
            }
        });
    })
    .response
    .on_hover_text(t("draw-presets-tip"));
}

/// Apply the fields that changed between `old` and `new` to one object.
fn apply_diff(o: &mut Object, old: &ToolStyle, new: &ToolStyle) {
    let (a, b) = (&old.style, &new.style);
    let mut p = StylePatch::default();
    macro_rules! diff {
        ($field:ident, $($path:tt)+) => {
            if a.$($path)+ != b.$($path)+ {
                p.$field = Some(b.$($path)+.clone());
            }
        };
    }
    diff!(color, color);
    diff!(fill, fill);
    diff!(width, width);
    diff!(opacity, opacity);
    diff!(dash, dash);
    diff!(shadow, shadow);
    diff!(outline, outline);
    diff!(font_family, font.family);
    diff!(font_size, font.size);
    diff!(bold, font.bold);
    diff!(italic, font.italic);
    diff!(underline, font.underline);
    diff!(align, font.align);
    diff!(text_color, font.color);
    p.apply(o);

    let (oa, ob) = (&old.options, &new.options);
    let mut merged = options_of(o, ob);
    macro_rules! merge {
        ($($f:ident),*) => { $( if oa.$f != ob.$f { merged.$f = ob.$f.clone(); } )* };
    }
    merge!(
        head_start,
        head_end,
        head_size,
        decor,
        color2,
        text_box,
        wrap,
        padding,
        radius,
        tail_width,
        facing,
        indent,
        blur_outside,
        dim_outside,
        magnifier,
        connector,
        step_shape,
        step_size,
        sticker,
        cursor,
        blur_ellipse,
        blur_strength,
        eraser_radius,
        eraser_background,
        fill_tolerance,
        fill_transparent
    );
    if oa.shape != ob.shape && matches!(ob.shape, ShapeKind::CurlyBracket | ShapeKind::SquareBracket) {
        if let Kind::Shape(s) = &mut o.kind {
            if matches!(s.kind, ShapeKind::CurlyBracket | ShapeKind::SquareBracket) {
                s.kind = ob.shape;
            }
        }
    }
    if oa.highlight != ob.highlight {
        if let Kind::Highlight(h) = &mut o.kind {
            if h.shape != HighlightShape::Line {
                h.shape = ob.highlight;
            }
        }
    }
    apply_options(o, &merged);
    if oa.zoom != ob.zoom {
        if let Kind::Magnifier(m) = &mut o.kind {
            let c = m.target.center();
            let (w, h) = (m.source.w * ob.zoom, m.source.h * ob.zoom);
            m.target = crate::draw::RectF::from_center(c, w, h);
        }
    }
    if oa.emoji != ob.emoji {
        if let Kind::Emoji(e) = &mut o.kind {
            e.text = ob.emoji.clone();
        }
    }
    if oa.step_letters != ob.step_letters {
        if let Kind::Step(s) = &mut o.kind {
            s.value = match (s.value, ob.step_letters) {
                (crate::draw::StepValue::Number(n), true) => crate::draw::StepValue::Letter(n.saturating_sub(1)),
                (crate::draw::StepValue::Letter(i), false) => crate::draw::StepValue::Number(i + 1),
                (v, _) => v,
            };
        }
    }
    if oa.text_flow != ob.text_flow {
        if let Kind::Text(t) = &mut o.kind {
            let r = t.rect;
            t.flow = match ob.text_flow {
                FlowMode::Straight => TextFlow::Straight,
                FlowMode::Arc => TextFlow::Arc { bend: 0.5 },
                FlowMode::Path => TextFlow::Path {
                    points: vec![
                        Pt::new(r.x, r.center().y),
                        Pt::new(r.center().x, r.y),
                        Pt::new(r.right(), r.center().y),
                    ],
                    smooth: true,
                },
            };
        }
    }
}

fn dash_label(d: Dash) -> String {
    t(match d {
        Dash::Solid => "draw-dash-solid",
        Dash::Dash => "draw-dash-dash",
        Dash::Dot => "draw-dash-dot",
        Dash::DashDot => "draw-dash-dash-dot",
        Dash::LongDash => "draw-dash-long",
    })
}

fn head_label(h: ArrowHead) -> String {
    t(match h {
        ArrowHead::None => "draw-head-none",
        ArrowHead::Triangle => "draw-head-triangle",
        ArrowHead::Open => "draw-head-open",
        ArrowHead::Stealth => "draw-head-stealth",
        ArrowHead::Diamond => "draw-head-diamond",
        ArrowHead::Circle => "draw-head-circle",
        ArrowHead::Bar => "draw-head-bar",
        ArrowHead::Fancy => "draw-head-fancy",
        ArrowHead::ThreeD => "draw-head-3d",
    })
}

fn decor_label(d: LineDecor) -> String {
    t(match d {
        LineDecor::Plain => "draw-decor-plain",
        LineDecor::Double => "draw-decor-double",
        LineDecor::Gradient => "draw-decor-gradient",
        LineDecor::Neon => "draw-decor-neon",
        LineDecor::Chalk => "draw-decor-chalk",
        LineDecor::Beads => "draw-decor-beads",
        LineDecor::Tapered => "draw-decor-tapered",
        LineDecor::Wavy => "draw-decor-wavy",
        LineDecor::Zigzag => "draw-decor-zigzag",
        LineDecor::Railroad => "draw-decor-railroad",
        LineDecor::Brush => "draw-decor-brush",
    })
}

fn curve_label(k: LineKind) -> String {
    t(match k {
        LineKind::Cubic => "draw-curve-cubic",
        LineKind::Smooth => "draw-curve-smooth",
        _ => "draw-curve-quad",
    })
}

fn box_label(b: TextBox) -> String {
    t(match b {
        TextBox::None => "draw-box-none",
        TextBox::Rect => "draw-shape-rect",
        TextBox::Rounded => "draw-shape-rounded",
        TextBox::Oval => "draw-shape-oval",
    })
}

fn flow_label(f: FlowMode) -> String {
    t(match f {
        FlowMode::Straight => "draw-flow-straight",
        FlowMode::Arc => "draw-flow-arc",
        FlowMode::Path => "draw-flow-path",
    })
}

fn sticker_label(s: StickerShape) -> String {
    t(match s {
        StickerShape::Arrow => "draw-sticker-arrow",
        StickerShape::DoubleArrow => "draw-sticker-double",
        StickerShape::CurvedArrow => "draw-sticker-curved",
        StickerShape::UTurnArrow => "draw-sticker-uturn",
        StickerShape::CornerArrow => "draw-sticker-corner",
        StickerShape::CircularArrow => "draw-sticker-circular",
    })
}

fn cursor_label(c: CursorShape) -> String {
    t(match c {
        CursorShape::Arrow => "draw-cursor-arrow",
        CursorShape::ArrowClick => "draw-cursor-arrow-click",
        CursorShape::LeftClick => "draw-cursor-left",
        CursorShape::RightClick => "draw-cursor-right",
        CursorShape::Hand => "draw-cursor-hand",
    })
}
