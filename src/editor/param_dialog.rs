//! Schema-driven effect dialog: sliders / checkboxes / colour pickers / choices built
//! from an effect's [`ParamDef`]s, with a live preview computed on a downscaled proxy by
//! a debounced worker thread. Dense options sit under "More…".

use super::effects::{
    downscale_proxy, ApplyFn, EffectCtx, EffectDef, ParamDef, ParamKind, ParamValue, ParamValues, Scope,
};
use super::style;
use super::texture::{image_options, tile_to_color_image, TileRect};
use crate::common::Image;
use crate::i18n::t;
use eframe::egui::{self, vec2, Color32, Id, Key, Modifiers, RichText, TextureHandle};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest side of the preview proxy.
const PROXY_SIDE: u32 = 720;
/// Wait this long after the last change before recomputing the preview.
const DEBOUNCE: Duration = Duration::from_millis(120);

enum ToWorker {
    Preview(u64, ParamValues),
}

enum FromWorker {
    /// The proxy (the "original" preview) is ready.
    Proxy(Image),
    Preview(u64, Image),
}

/// Owns the preview thread; dropping it stops the thread.
struct PreviewWorker {
    tx: Sender<ToWorker>,
    rx: Receiver<FromWorker>,
}

impl PreviewWorker {
    fn start(source: Arc<Image>, apply: ApplyFn, background: [u8; 4], ctx: egui::Context) -> Option<Self> {
        let (tx, wrx) = channel::<ToWorker>();
        let (wtx, rx) = channel::<FromWorker>();
        let res = std::thread::Builder::new().name("editor-preview".into()).spawn(move || {
            let (proxy, scale) = downscale_proxy(&source, PROXY_SIDE);
            drop(source);
            if wtx.send(FromWorker::Proxy(proxy.clone())).is_err() {
                return;
            }
            ctx.request_repaint();
            while let Ok(mut msg) = wrx.recv() {
                // Only the newest request matters.
                while let Ok(newer) = wrx.try_recv() {
                    msg = newer;
                }
                let ToWorker::Preview(gen, values) = msg;
                let ectx = EffectCtx { scale, preview: true, background };
                let out = apply(&proxy, &values, &ectx);
                if wtx.send(FromWorker::Preview(gen, out)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        match res {
            Ok(_) => Some(Self { tx, rx }),
            Err(e) => {
                log::warn!("editor: cannot start the preview worker: {e}");
                None
            }
        }
    }
}

pub enum DialogOutcome {
    Open,
    Cancel,
    Apply(ParamValues),
}

pub struct EffectDialog {
    pub doc: u64,
    pub effect_id: &'static str,
    title: String,
    params: Vec<ParamDef>,
    placeholder: bool,
    on_selection: bool,
    pub values: ParamValues,
    defaults: ParamValues,
    show_more: bool,
    worker: Option<PreviewWorker>,
    original: Option<TextureHandle>,
    preview: Option<TextureHandle>,
    proxy_size: [f32; 2],
    generation: u64,
    shown_generation: u64,
    changed_at: Option<Instant>,
    compare: bool,
}

impl EffectDialog {
    /// `source`: what the effect will process (selection pixels or the whole image).
    pub fn new(
        ctx: &egui::Context,
        doc: u64,
        def: &EffectDef,
        source: Arc<Image>,
        on_selection: bool,
        background: [u8; 4],
    ) -> Self {
        let params = def.params.clone().unwrap_or_default();
        let values = ParamValues::defaults(&params);
        let worker = PreviewWorker::start(source, def.apply.clone(), background, ctx.clone());
        Self {
            doc,
            effect_id: def.id,
            title: def.label.clone(),
            defaults: values.clone(),
            values,
            params,
            placeholder: def.placeholder,
            on_selection: on_selection && def.scope == Scope::SelectionOrImage,
            show_more: false,
            worker,
            original: None,
            preview: None,
            proxy_size: [0.0, 0.0],
            generation: 1,
            shown_generation: 0,
            changed_at: Some(Instant::now() - DEBOUNCE),
            compare: false,
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(w) = &self.worker else { return };
        while let Ok(msg) = w.rx.try_recv() {
            match msg {
                FromWorker::Proxy(img) => {
                    self.proxy_size = [img.width as f32, img.height as f32];
                    self.original = Some(texture(ctx, "effect-original", &img));
                }
                FromWorker::Preview(gen, img) => {
                    if gen >= self.shown_generation {
                        self.shown_generation = gen;
                        self.preview = Some(texture(ctx, "effect-preview", &img));
                    }
                }
            }
        }
        // Debounced request.
        if let Some(at) = self.changed_at {
            let left = DEBOUNCE.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.changed_at = None;
                let _ = w.tx.send(ToWorker::Preview(self.generation, self.values.clone()));
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
        self.changed_at = Some(Instant::now());
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> DialogOutcome {
        self.poll(ctx);
        let mut outcome = DialogOutcome::Open;
        let resp = egui::Modal::new(Id::new(("effect-dialog", self.effect_id))).show(ctx, |ui| {
            ui.set_width(660.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&self.title).size(17.0).strong());
                if self.on_selection {
                    ui.label(
                        RichText::new(t("editor-effect-on-selection")).small().color(ui.visuals().weak_text_color()),
                    );
                }
            });
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    self.controls(ui);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| self.preview_ui(ui));
            });
            if self.placeholder {
                ui.add_space(6.0);
                ui.label(RichText::new(t("editor-effect-placeholder")).small().color(ui.visuals().warn_fg_color));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let has_params = !self.params.is_empty();
                if has_params && ui.button(t("editor-reset")).on_hover_text(t("editor-reset-tip")).clicked() {
                    self.values = self.defaults.clone();
                    self.changed();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let apply = ui
                        .add_enabled(
                            !self.placeholder,
                            style::primary_button(t("editor-apply")).min_size(vec2(88.0, 28.0)),
                        )
                        .on_hover_text(format!("{} (Enter)", t("editor-apply")))
                        .on_disabled_hover_text(t("editor-effect-placeholder"));
                    if apply.clicked() {
                        outcome = DialogOutcome::Apply(self.values.clone());
                    }
                    if ui
                        .add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0)))
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        outcome = DialogOutcome::Cancel;
                    }
                });
            });
        });
        if resp.should_close() && matches!(outcome, DialogOutcome::Open) {
            outcome = DialogOutcome::Cancel;
        }
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        if enter && !self.placeholder && matches!(outcome, DialogOutcome::Open) {
            outcome = DialogOutcome::Apply(self.values.clone());
        }
        outcome
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        if self.params.is_empty() {
            ui.label(RichText::new(t("editor-effect-no-params")).color(ui.visuals().weak_text_color()));
            return;
        }
        let params = self.params.clone();
        let mut changed = false;
        egui::Grid::new("effect-params").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
            for p in params.iter().filter(|p| !p.advanced) {
                changed |= self.param_row(ui, p);
            }
        });
        if params.iter().any(|p| p.advanced) {
            ui.add_space(4.0);
            let label = if self.show_more { t("editor-less") } else { t("editor-more") };
            if ui.add(egui::Button::new(label).frame(false)).clicked() {
                self.show_more = !self.show_more;
            }
            if self.show_more {
                egui::Grid::new("effect-params-more").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    for p in params.iter().filter(|p| p.advanced) {
                        changed |= self.param_row(ui, p);
                    }
                });
            }
        }
        if changed {
            self.changed();
        }
    }

    fn param_row(&mut self, ui: &mut egui::Ui, p: &ParamDef) -> bool {
        let label = ui.label(&p.label);
        if let Some(tip) = &p.tooltip {
            label.on_hover_text(tip);
        }
        let cur = self.values.0.get(p.id).cloned().unwrap_or_else(|| p.kind.default_value());
        let mut changed = false;
        let new = match (&p.kind, cur) {
            (ParamKind::Float { min, max, step, decimals, suffix, logarithmic, .. }, ParamValue::Float(mut v)) => {
                let s = egui::Slider::new(&mut v, *min..=*max)
                    .step_by(*step as f64)
                    .fixed_decimals(*decimals)
                    .suffix(*suffix)
                    .logarithmic(*logarithmic);
                changed = ui.add(s).changed();
                ParamValue::Float(v)
            }
            (ParamKind::Int { min, max, suffix, .. }, ParamValue::Int(mut v)) => {
                changed = ui.add(egui::Slider::new(&mut v, *min..=*max).suffix(*suffix)).changed();
                ParamValue::Int(v)
            }
            (ParamKind::Bool { .. }, ParamValue::Bool(mut v)) => {
                changed = ui.checkbox(&mut v, "").changed();
                ParamValue::Bool(v)
            }
            (ParamKind::Color { alpha, .. }, ParamValue::Color(c)) => {
                let mut c32 = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                let mode =
                    if *alpha { egui::color_picker::Alpha::OnlyBlend } else { egui::color_picker::Alpha::Opaque };
                changed = egui::color_picker::color_edit_button_srgba(ui, &mut c32, mode).changed();
                ParamValue::Color(c32.to_srgba_unmultiplied())
            }
            (ParamKind::Choice { options, .. }, ParamValue::Choice(mut v)) => {
                let shown = options.get(v).cloned().unwrap_or_default();
                egui::ComboBox::from_id_salt(("effect-choice", p.id)).selected_text(shown).show_ui(ui, |ui| {
                    for (i, o) in options.iter().enumerate() {
                        changed |= ui.selectable_value(&mut v, i, o).changed();
                    }
                });
                ParamValue::Choice(v)
            }
            (ParamKind::Text { multiline, .. }, ParamValue::Text(mut v)) => {
                let edit = if *multiline {
                    egui::TextEdit::multiline(&mut v).desired_rows(3).desired_width(200.0)
                } else {
                    egui::TextEdit::singleline(&mut v).desired_width(200.0)
                };
                changed = ui.add(edit).changed();
                ParamValue::Text(v)
            }
            (kind, _) => kind.default_value(),
        };
        ui.end_row();
        if changed {
            self.values.set(p.id, new);
        }
        changed
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let box_size = vec2(320.0, 240.0);
        let (rect, resp) = ui.allocate_exact_size(box_size, egui::Sense::click_and_drag());
        let pal_bg = ui.visuals().extreme_bg_color;
        ui.painter().rect_filled(rect, 6.0, pal_bg);
        let holding = resp.is_pointer_button_down_on() || self.compare;
        let tex = if holding { self.original.as_ref() } else { self.preview.as_ref().or(self.original.as_ref()) };
        match tex {
            Some(tex) if self.proxy_size[0] > 0.0 => {
                let s = (box_size.x / self.proxy_size[0]).min(box_size.y / self.proxy_size[1]).min(4.0);
                let r = egui::Rect::from_center_size(rect.center(), vec2(self.proxy_size[0], self.proxy_size[1]) * s);
                ui.painter().image(
                    tex.id(),
                    r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            _ => {
                ui.put(rect, egui::Spinner::new());
            }
        }
        let busy = self.shown_generation < self.generation && self.original.is_some();
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.compare, t("editor-show-original")).on_hover_text(t("editor-preview-hold"));
            if busy {
                ui.add(egui::Spinner::new().size(12.0));
            }
        });
    }
}

fn texture(ctx: &egui::Context, name: &str, img: &Image) -> TextureHandle {
    let r = TileRect { x: 0, y: 0, w: img.width, h: img.height };
    ctx.load_texture(name, tile_to_color_image(img, r), image_options())
}
