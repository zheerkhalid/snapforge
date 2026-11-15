//! Effects that need the canvas, where a parameter schema is not enough:
//!
//! * Make Color Transparent – click the colour on the canvas; tolerance, mode and smooth
//!   edges; the affected area is previewed over the image.
//! * Spotlight – drag the bright rectangle on the canvas.
//! * Remove / Insert Strip – drag across the canvas to define the band.
//! * Watermark – choose an image, then move / resize its placement rectangle.
//! * Clone Stamp – a canvas tool: Alt+click sets the source, then paint.
//!
//! A session owns a side panel (parameters, Apply / Cancel), takes canvas input like a
//! tool and previews on a downscaled proxy computed by a worker thread. Apply returns a
//! closure the app runs on its effect worker (one undo step).

use super::document::{DocId, Document};
use super::effects::{downscale_proxy, EffectCtx, EffectDef, EffectRegistry, MenuSlot, ParamValues};
use super::ext::{CanvasTool, ToolInput};
use super::selection::CanvasMap;
use super::style::{self, ACCENT};
use super::texture::{image_options, tile_to_color_image, TileRect};
use crate::common::{Image, Point, Rect};
use crate::effects::{
    self, Color, Orientation, SpotShape, SpotlightParams, StripParams, TransparencyMode, TransparencyParams,
    WatermarkParams,
};
use crate::i18n::t;
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Stroke, TextureHandle};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Effects opened as a canvas session instead of a schema dialog.
pub const INTERACTIVE: [&str; 5] =
    ["image.transparency", "effects.spotlight", "image.remove_strip", "image.insert_strip", "effects.watermark"];
/// Shows a result instead of changing the image.
pub const COUNT_COLORS: &str = "image.count_colors";

fn label(key: &str) -> String {
    t(key).trim_end_matches('…').to_string()
}

/// Menu entries for the interactive effects (their apply is never called directly).
pub fn install(r: &mut EffectRegistry) {
    let mut add = |id: &'static str, key: &str, menu, group, dialog: bool| {
        let mut d = EffectDef::new(id, label(key), menu, group, |img: &Image, _: &ParamValues, _: &EffectCtx| img.clone())
            .whole_image();
        if dialog {
            // An empty schema: the menu shows an ellipsis.
            d.params = Some(Vec::new());
        }
        r.register(d);
    };
    add("image.transparency", "fx-make-transparent", MenuSlot::Image, "color", true);
    add(COUNT_COLORS, "fx-count-colors", MenuSlot::Image, "color", false);
    add("image.remove_strip", "fx-remove-strip", MenuSlot::Image, "strip", true);
    add("image.insert_strip", "fx-insert-strip", MenuSlot::Image, "strip", true);
    add("effects.spotlight", "fx-spotlight", MenuSlot::Effects, "decorate", true);
    add("effects.watermark", "fx-watermark", MenuSlot::Effects, "text", true);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxKind {
    Transparency,
    Spotlight,
    RemoveStrip,
    InsertStrip,
    Watermark,
}

impl FxKind {
    pub fn from_id(id: &str) -> Option<FxKind> {
        Some(match id {
            "image.transparency" => FxKind::Transparency,
            "effects.spotlight" => FxKind::Spotlight,
            "image.remove_strip" => FxKind::RemoveStrip,
            "image.insert_strip" => FxKind::InsertStrip,
            "effects.watermark" => FxKind::Watermark,
            _ => return None,
        })
    }
    fn title_key(self) -> &'static str {
        match self {
            FxKind::Transparency => "fx-make-transparent",
            FxKind::Spotlight => "fx-spotlight",
            FxKind::RemoveStrip => "fx-remove-strip",
            FxKind::InsertStrip => "fx-insert-strip",
            FxKind::Watermark => "fx-watermark",
        }
    }
    fn hint_key(self) -> &'static str {
        match self {
            FxKind::Transparency => "fx-session-hint-transparent",
            FxKind::Spotlight => "fx-session-hint-spotlight",
            FxKind::RemoveStrip | FxKind::InsertStrip => "fx-session-hint-strip",
            FxKind::Watermark => "fx-session-hint-watermark",
        }
    }
}

type PreviewFn = Box<dyn FnOnce(&Image, f32) -> Image + Send>;

/// Proxy preview worker: the latest request wins.
struct Preview {
    tx: Sender<(u64, PreviewFn)>,
    rx: Receiver<(u64, Image)>,
    gen: u64,
    shown: u64,
    tex: Option<TextureHandle>,
}

impl Preview {
    fn start(source: Arc<Image>, ctx: egui::Context) -> Option<Self> {
        let (tx, wrx) = channel::<(u64, PreviewFn)>();
        let (wtx, rx) = channel();
        std::thread::Builder::new()
            .name("editor-fx-preview".into())
            .spawn(move || {
                let (proxy, scale) = downscale_proxy(&source, 1400);
                drop(source);
                while let Ok(mut msg) = wrx.recv() {
                    while let Ok(newer) = wrx.try_recv() {
                        msg = newer;
                    }
                    let (gen, f) = msg;
                    let out = f(&proxy, scale);
                    if wtx.send((gen, out)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .map_err(|e| log::warn!("editor: cannot start the effect preview: {e}"))
            .ok()?;
        Some(Self { tx, rx, gen: 0, shown: 0, tex: None })
    }

    fn request(&mut self, f: PreviewFn) {
        self.gen += 1;
        let _ = self.tx.send((self.gen, f));
    }

    fn clear(&mut self) {
        self.gen += 1;
        self.shown = self.gen;
        self.tex = None;
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((g, img)) = self.rx.try_recv() {
            if g >= self.shown && g == self.gen {
                self.shown = g;
                let ci = tile_to_color_image(&img, TileRect { x: 0, y: 0, w: img.width, h: img.height });
                self.tex = Some(ctx.load_texture("fx-session-preview", ci, image_options()));
            }
        }
    }

    fn busy(&self) -> bool {
        self.shown < self.gen
    }
}

enum SDrag {
    None,
    Rect { start: [f64; 2] },
    MoveMark { grab: [f64; 2], orig: [f32; 4] },
    ResizeMark { orig: [f32; 4], grab: [f64; 2] },
}

pub enum SessionOutcome {
    Open,
    Cancel,
    Apply { name: String, run: Arc<dyn Fn(&Image) -> Image + Send + Sync> },
}

pub struct FxSession {
    pub doc: DocId,
    pub kind: FxKind,
    size: (u32, u32),
    tp: TransparencyParams,
    tp_picked: bool,
    show_mask: bool,
    spot: SpotlightParams,
    spot_set: bool,
    strip: StripParams,
    strip_set: bool,
    mark: Option<Arc<Image>>,
    mark_path: Option<PathBuf>,
    mark_rx: Option<Receiver<Result<Image, String>>>,
    mark_tex: Option<TextureHandle>,
    /// Placement [x, y, w, h] in image pixels.
    mark_rect: [f32; 4],
    mark_opacity: u8,
    mark_tile: bool,
    mark_spacing: u32,
    drag: SDrag,
    preview: Option<Preview>,
    changed: bool,
    cursor: CursorIcon,
    error: Option<String>,
}

impl FxSession {
    pub fn new(ctx: &egui::Context, kind: FxKind, doc: &Document) -> Self {
        let (w, h) = (doc.image.width, doc.image.height);
        let sel = doc.selection.as_ref().map(|s| s.bounds);
        let spot_rect = sel.unwrap_or(Rect::new(w as i32 / 4, h as i32 / 4, (w as i32 / 2).max(1), (h as i32 / 2).max(1)));
        let mut strip = StripParams { position: h / 3, size: (h / 10).max(1), ..Default::default() };
        if let Some(r) = sel {
            strip.position = r.y.max(0) as u32;
            strip.size = r.h.max(1) as u32;
        }
        Self {
            doc: doc.id,
            kind,
            size: (w, h),
            tp: TransparencyParams::default(),
            tp_picked: false,
            show_mask: true,
            spot: SpotlightParams { rect: spot_rect, ..Default::default() },
            spot_set: true,
            strip,
            strip_set: sel.is_some(),
            mark: None,
            mark_path: None,
            mark_rx: None,
            mark_tex: None,
            mark_rect: [0.0; 4],
            mark_opacity: 60,
            mark_tile: false,
            mark_spacing: 60,
            drag: SDrag::None,
            preview: Preview::start(doc.image.clone(), ctx.clone()),
            changed: true,
            cursor: CursorIcon::Crosshair,
            error: None,
        }
    }

    fn mark_hit(&self, p: [f64; 2], scale: f32) -> Option<bool> {
        self.mark.as_ref()?;
        let [x, y, w, h] = self.mark_rect;
        let (px, py) = (p[0] as f32, p[1] as f32);
        let corner = 9.0 / scale.max(1e-3);
        if (px - (x + w)).abs() <= corner && (py - (y + h)).abs() <= corner {
            return Some(true);
        }
        (px >= x && px <= x + w && py >= y && py <= y + h).then_some(false)
    }

    fn refresh_preview(&mut self) {
        if !self.changed {
            return;
        }
        self.changed = false;
        let Some(pv) = self.preview.as_mut() else { return };
        match self.kind {
            FxKind::Transparency if self.tp_picked => {
                let p = self.tp.clone();
                let mask_only = self.show_mask;
                pv.request(Box::new(move |img, s| {
                    let mut p = p;
                    p.seed = Point::new((p.seed.x as f32 * s) as i32, (p.seed.y as f32 * s) as i32);
                    let mask = effects::transparency_mask(img, &p);
                    if mask_only {
                        effects::mask_preview(img, &mask, Color::rgba(255, 0, 200, 150))
                    } else {
                        effects::checkerboard(&effects::apply_removal(img, &mask, p.color, p.smooth_edges), 6)
                    }
                }));
            }
            FxKind::Spotlight if self.spot_set => {
                let p = self.spot.clone();
                pv.request(Box::new(move |img, s| {
                    let mut p = p;
                    p.rect = Rect::new(
                        (p.rect.x as f32 * s) as i32,
                        (p.rect.y as f32 * s) as i32,
                        ((p.rect.w as f32 * s) as i32).max(1),
                        ((p.rect.h as f32 * s) as i32).max(1),
                    );
                    p.feather = (p.feather as f32 * s).round() as u32;
                    effects::spotlight(img, &p)
                }));
            }
            FxKind::Watermark if self.mark_tile => {
                if let Some(mark) = self.mark.clone() {
                    let params = self.watermark_params(&mark);
                    pv.request(Box::new(move |img, s| {
                        let mut p = params;
                        p.scale *= s;
                        p.spacing = (p.spacing as f32 * s) as u32;
                        effects::watermark(img, &mark, &p)
                    }));
                }
            }
            _ => pv.clear(),
        }
    }

    fn watermark_params(&self, mark: &Image) -> WatermarkParams {
        WatermarkParams {
            scale: (self.mark_rect[2] / mark.width.max(1) as f32 * 100.0).clamp(1.0, 1000.0),
            opacity: self.mark_opacity,
            tile: self.mark_tile,
            spacing: self.mark_spacing,
            ..Default::default()
        }
    }

    fn poll_mark(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.mark_rx else { return };
        let Ok(res) = rx.try_recv() else { return };
        self.mark_rx = None;
        match res {
            Ok(img) => {
                let (w, h) = (self.size.0 as f32, self.size.1 as f32);
                let s = ((w * 0.3) / img.width.max(1) as f32).min(1.0);
                let (mw, mh) = (img.width as f32 * s, img.height as f32 * s);
                let margin = (w.min(h) * 0.03).max(4.0);
                self.mark_rect = [(w - mw - margin).max(0.0), (h - mh - margin).max(0.0), mw.max(1.0), mh.max(1.0)];
                let ci = tile_to_color_image(&img, TileRect { x: 0, y: 0, w: img.width, h: img.height });
                self.mark_tex = Some(ctx.load_texture("fx-watermark", ci, image_options()));
                self.mark = Some(Arc::new(img));
                self.error = None;
                self.changed = true;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The side panel. `hwnd` owns the file dialog.
    pub fn panel(&mut self, ctx: &egui::Context, hwnd: isize) -> SessionOutcome {
        self.poll_mark(ctx);
        self.refresh_preview();
        if let Some(pv) = self.preview.as_mut() {
            pv.poll(ctx);
        }
        let mut out = SessionOutcome::Open;
        let mut changed = false;
        egui::Window::new(label(self.kind.title_key()))
            .id(egui::Id::new("fx-session"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-18.0, 150.0))
            .collapsible(false)
            .resizable(false)
            .default_width(290.0)
            .show(ctx, |ui| {
                ui.set_width(290.0);
                ui.label(egui::RichText::new(t(self.kind.hint_key())).color(ui.visuals().weak_text_color()));
                ui.add_space(6.0);
                egui::Grid::new("fx-session-grid").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
                    changed |= self.controls(ui, hwnd);
                });
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if self.preview.as_ref().is_some_and(|p| p.busy()) {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = self.ready();
                        if ui
                            .add_enabled(ready, style::primary_button(t("editor-apply")).min_size(vec2(88.0, 28.0)))
                            .on_hover_text("Enter")
                            .clicked()
                        {
                            out = self.apply();
                        }
                        if ui.add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0))).on_hover_text("Esc").clicked() {
                            out = SessionOutcome::Cancel;
                        }
                    });
                });
            });
        if changed {
            self.changed = true;
        }
        if matches!(out, SessionOutcome::Open) && !ctx.wants_keyboard_input() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                out = SessionOutcome::Cancel;
            } else if self.ready() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                out = self.apply();
            }
        }
        out
    }

    fn ready(&self) -> bool {
        match self.kind {
            FxKind::Transparency => self.tp_picked,
            FxKind::Spotlight => self.spot_set,
            FxKind::RemoveStrip | FxKind::InsertStrip => self.strip_set && self.strip.size > 0,
            FxKind::Watermark => self.mark.is_some(),
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, hwnd: isize) -> bool {
        let mut ch = false;
        match self.kind {
            FxKind::Transparency => {
                ui.label(t("fx-transparent-color"));
                ui.horizontal(|ui| {
                    let c = self.tp.color;
                    let (r, _) = ui.allocate_exact_size(vec2(36.0, 20.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 4.0, Color32::from_rgb(c.r, c.g, c.b));
                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0_f32, ui.visuals().weak_text_color()), egui::StrokeKind::Inside);
                    let txt = if self.tp_picked { format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b) } else { t("fx-transparent-pick") };
                    ui.label(txt);
                });
                ui.end_row();
                ui.label(t("fx-transparent-tolerance"));
                let mut tol = self.tp.tolerance as i32;
                if ui.add(egui::Slider::new(&mut tol, 0..=100).suffix("%")).changed() {
                    self.tp.tolerance = tol as u8;
                    ch = true;
                }
                ui.end_row();
                ui.label(t("fx-transparent-mode"));
                egui::ComboBox::from_id_salt("fx-tp-mode")
                    .selected_text(match self.tp.mode {
                        TransparencyMode::Contiguous => t("fx-transparent-contiguous"),
                        TransparencyMode::Global => t("fx-transparent-global"),
                    })
                    .show_ui(ui, |ui| {
                        ch |= ui.selectable_value(&mut self.tp.mode, TransparencyMode::Contiguous, t("fx-transparent-contiguous")).changed();
                        ch |= ui.selectable_value(&mut self.tp.mode, TransparencyMode::Global, t("fx-transparent-global")).changed();
                    });
                ui.end_row();
                ui.label("");
                ch |= ui.checkbox(&mut self.tp.smooth_edges, t("fx-transparent-smooth")).changed();
                ui.end_row();
                ui.label("");
                ch |= ui.checkbox(&mut self.show_mask, t("fx-transparent-show-mask")).changed();
                ui.end_row();
            }
            FxKind::Spotlight => {
                let p = &mut self.spot;
                ui.label(t("fx-spotlight-shape"));
                ui.horizontal(|ui| {
                    ch |= ui.selectable_value(&mut p.shape, SpotShape::Ellipse, t("fx-spotlight-ellipse")).changed();
                    ch |= ui.selectable_value(&mut p.shape, SpotShape::Rectangle, t("fx-spotlight-rectangle")).changed();
                });
                ui.end_row();
                ui.label(t("fx-spotlight-dim"));
                ch |= ui.add(egui::Slider::new(&mut p.dim, 0..=100).suffix("%")).changed();
                ui.end_row();
                ui.label(t("fx-spotlight-feather"));
                ch |= ui.add(egui::Slider::new(&mut p.feather, 0..=200).suffix(" px")).changed();
                ui.end_row();
                ui.label(t("fx-param-color"));
                let mut c = Color32::from_rgb(p.color.r, p.color.g, p.color.b);
                if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                    p.color = Color::rgb(c.r(), c.g(), c.b());
                    ch = true;
                }
                ui.end_row();
                ui.label("");
                ch |= ui.checkbox(&mut p.transparent_outside, t("fx-spotlight-transparent")).changed();
                ui.end_row();
                ui.label(t("fx-param-size"));
                ui.label(format!("{} × {} px", p.rect.w, p.rect.h));
                ui.end_row();
            }
            FxKind::RemoveStrip | FxKind::InsertStrip => {
                ui.label(t("fx-strip-orientation"));
                ui.horizontal(|ui| {
                    let before = self.strip.orientation;
                    ui.selectable_value(&mut self.strip.orientation, Orientation::Horizontal, t("fx-orientation-horizontal"));
                    ui.selectable_value(&mut self.strip.orientation, Orientation::Vertical, t("fx-orientation-vertical"));
                    if before != self.strip.orientation {
                        self.strip_set = false;
                        ch = true;
                    }
                });
                ui.end_row();
                let max = match self.strip.orientation {
                    Orientation::Horizontal => self.size.1,
                    Orientation::Vertical => self.size.0,
                };
                ui.label(t("fx-strip-position"));
                ch |= ui.add(egui::DragValue::new(&mut self.strip.position).range(0..=max.saturating_sub(1)).suffix(" px")).changed();
                ui.end_row();
                ui.label(t("fx-strip-size"));
                if ui.add(egui::DragValue::new(&mut self.strip.size).range(1..=max.max(1) * 2).suffix(" px")).changed() {
                    self.strip_set = true;
                    ch = true;
                }
                ui.end_row();
                if self.kind == FxKind::InsertStrip {
                    ui.label(t("fx-strip-color"));
                    let c = self.strip.color;
                    let mut cc = Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a);
                    if egui::color_picker::color_edit_button_srgba(ui, &mut cc, egui::color_picker::Alpha::OnlyBlend).changed() {
                        let [r, g, b, a] = cc.to_srgba_unmultiplied();
                        self.strip.color = Color::rgba(r, g, b, a);
                    }
                    ui.end_row();
                }
            }
            FxKind::Watermark => {
                ui.label(t("fx-watermark-image"));
                ui.horizontal(|ui| {
                    if ui.button(t("editor-browse")).clicked() {
                        let files = super::dialogs::open_files(hwnd, None, &t("editor-filter-images"), &t("editor-filter-all"));
                        if let Some(p) = files.into_iter().next() {
                            let (tx, rx) = channel();
                            let path = p.clone();
                            let _ = std::thread::Builder::new().name("editor-watermark-load".into()).spawn(move || {
                                let _ = tx.send(crate::output::save::load_image(&path).map_err(|e| format!("{e:#}")));
                            });
                            self.mark_rx = Some(rx);
                            self.mark_path = Some(p);
                        }
                    }
                    if self.mark_rx.is_some() {
                        ui.spinner();
                    } else if let Some(p) = &self.mark_path {
                        ui.label(super::app::file_name(p));
                    }
                });
                ui.end_row();
                ui.label(t("fx-param-opacity"));
                ch |= ui.add(egui::Slider::new(&mut self.mark_opacity, 1..=100).suffix("%")).changed();
                ui.end_row();
                ui.label("");
                ch |= ui.checkbox(&mut self.mark_tile, t("fx-watermark-tile")).changed();
                ui.end_row();
                if self.mark_tile {
                    ui.label(t("fx-watermark-spacing"));
                    ch |= ui.add(egui::Slider::new(&mut self.mark_spacing, 0..=400).suffix(" px")).changed();
                    ui.end_row();
                }
                if self.mark.is_some() {
                    ui.label(t("fx-param-size"));
                    ui.label(format!("{:.0} × {:.0} px", self.mark_rect[2], self.mark_rect[3]));
                    ui.end_row();
                }
            }
        }
        ch
    }

    fn apply(&self) -> SessionOutcome {
        let name = label(self.kind.title_key());
        let run: Arc<dyn Fn(&Image) -> Image + Send + Sync> = match self.kind {
            FxKind::Transparency => {
                let p = self.tp.clone();
                Arc::new(move |img| effects::make_transparent(img, &p))
            }
            FxKind::Spotlight => {
                let p = self.spot.clone();
                Arc::new(move |img| effects::spotlight(img, &p))
            }
            FxKind::RemoveStrip => {
                let p = self.strip.clone();
                Arc::new(move |img| effects::remove_strip(img, &p))
            }
            FxKind::InsertStrip => {
                let p = self.strip.clone();
                Arc::new(move |img| effects::insert_strip(img, &p))
            }
            FxKind::Watermark => {
                let Some(mark) = self.mark.clone() else { return SessionOutcome::Open };
                if self.mark_tile {
                    let p = self.watermark_params(&mark);
                    Arc::new(move |img| effects::watermark(img, &mark, &p))
                } else {
                    let [x, y, w, h] = self.mark_rect;
                    let op = (self.mark_opacity as u32 * 255 / 100) as u8;
                    Arc::new(move |img| {
                        let scaled = effects::resize_to(&mark, w.round().max(1.0) as u32, h.round().max(1.0) as u32, effects::ResampleFilter::Lanczos3);
                        let mut out = img.clone();
                        effects::composite(&mut out, &scaled, x.round() as i32, y.round() as i32, op);
                        out
                    })
                }
            }
        };
        SessionOutcome::Apply { name, run }
    }
}

impl CanvasTool for FxSession {
    fn id(&self) -> &str {
        "fx.session"
    }
    fn label(&self) -> String {
        label(self.kind.title_key())
    }
    fn icon(&self) -> &str {
        "✨"
    }
    fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    fn on_input(&mut self, doc: &mut Document, input: &ToolInput) {
        let Some(p) = input.pos else { return };
        let scale = input.map.scale;
        let (w, h) = (doc.image.width as i32, doc.image.height as i32);
        let px = (p[0].floor() as i32).clamp(0, (w - 1).max(0));
        let py = (p[1].floor() as i32).clamp(0, (h - 1).max(0));
        self.cursor = match self.kind {
            FxKind::Watermark => match self.mark_hit(p, scale) {
                Some(true) => CursorIcon::ResizeNwSe,
                Some(false) => CursorIcon::Move,
                None => CursorIcon::Default,
            },
            FxKind::RemoveStrip | FxKind::InsertStrip => match self.strip.orientation {
                Orientation::Horizontal => CursorIcon::ResizeVertical,
                Orientation::Vertical => CursorIcon::ResizeHorizontal,
            },
            _ => CursorIcon::Crosshair,
        };
        if input.pressed {
            match self.kind {
                FxKind::Transparency => {
                    let c = Color::from_premul(doc.image.pixel(px as u32, py as u32));
                    self.tp.color = Color::rgb(c.r, c.g, c.b);
                    self.tp.seed = Point::new(px, py);
                    self.tp_picked = true;
                    self.changed = true;
                }
                FxKind::Watermark => {
                    self.drag = match self.mark_hit(p, scale) {
                        Some(true) => SDrag::ResizeMark { orig: self.mark_rect, grab: p },
                        Some(false) => SDrag::MoveMark { grab: p, orig: self.mark_rect },
                        None => SDrag::None,
                    };
                }
                _ => self.drag = SDrag::Rect { start: p },
            }
        }
        if input.down || input.released {
            match self.drag {
                SDrag::Rect { start } => {
                    let a = (start[0].round() as i32, start[1].round() as i32);
                    let b = (p[0].round() as i32, p[1].round() as i32);
                    let r = Rect::from_ltrb(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
                        .intersect(&Rect::new(0, 0, w, h));
                    match self.kind {
                        FxKind::Spotlight if r.w > 1 && r.h > 1 => {
                            self.spot.rect = r;
                            self.spot_set = true;
                            self.changed = true;
                        }
                        FxKind::RemoveStrip | FxKind::InsertStrip => {
                            let (pos, size) = match self.strip.orientation {
                                Orientation::Horizontal => (r.y, r.h),
                                Orientation::Vertical => (r.x, r.w),
                            };
                            if size > 0 {
                                self.strip.position = pos.max(0) as u32;
                                self.strip.size = size as u32;
                                self.strip_set = true;
                            }
                        }
                        _ => {}
                    }
                }
                SDrag::MoveMark { grab, orig } => {
                    self.mark_rect[0] = orig[0] + (p[0] - grab[0]) as f32;
                    self.mark_rect[1] = orig[1] + (p[1] - grab[1]) as f32;
                    self.changed |= self.mark_tile;
                }
                SDrag::ResizeMark { orig, grab } => {
                    let aspect = orig[3] / orig[2].max(1e-3);
                    let nw = (orig[2] + (p[0] - grab[0]) as f32).max(4.0);
                    self.mark_rect[2] = nw;
                    self.mark_rect[3] = (nw * aspect).max(1.0);
                    self.changed |= self.mark_tile;
                }
                SDrag::None => {}
            }
        }
        if input.released {
            self.drag = SDrag::None;
        }
    }

    fn paint(&mut self, doc: &Document, painter: &egui::Painter, map: &CanvasMap) {
        let to = |x: f32, y: f32| {
            let s = map.to_screen([x as f64, y as f64]);
            pos2(s[0], s[1])
        };
        let (w, h) = (doc.image.width as f32, doc.image.height as f32);
        let img_rect = egui::Rect::from_min_max(to(0.0, 0.0), to(w, h));
        let uv = egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        if let Some(tex) = self.preview.as_ref().and_then(|p| p.tex.as_ref()) {
            painter.image(tex.id(), img_rect, uv, Color32::WHITE);
        }
        let accent = Stroke::new(1.5_f32, ACCENT);
        match self.kind {
            FxKind::Spotlight => {
                let r = self.spot.rect;
                let sr = egui::Rect::from_min_max(to(r.x as f32, r.y as f32), to(r.right() as f32, r.bottom() as f32));
                painter.rect_stroke(sr, 0.0, accent, egui::StrokeKind::Outside);
            }
            FxKind::RemoveStrip | FxKind::InsertStrip if self.strip_set => {
                let (pos, size) = (self.strip.position as f32, self.strip.size as f32);
                let band = match self.strip.orientation {
                    Orientation::Horizontal => egui::Rect::from_min_max(to(0.0, pos), to(w, pos + size)),
                    Orientation::Vertical => egui::Rect::from_min_max(to(pos, 0.0), to(pos + size, h)),
                };
                let tint = if self.kind == FxKind::RemoveStrip {
                    Color32::from_rgba_unmultiplied(230, 60, 60, 70)
                } else {
                    Color32::from_rgba_unmultiplied(60, 200, 110, 70)
                };
                painter.rect_filled(band, 0.0, tint);
                painter.rect_stroke(band, 0.0, accent, egui::StrokeKind::Inside);
            }
            FxKind::Watermark => {
                if let (Some(tex), false) = (&self.mark_tex, self.mark_tile) {
                    let [x, y, mw, mh] = self.mark_rect;
                    let r = egui::Rect::from_min_max(to(x, y), to(x + mw, y + mh));
                    let a = (self.mark_opacity as u32 * 255 / 100) as u8;
                    painter.image(tex.id(), r, uv, Color32::from_white_alpha(a));
                    painter.rect_stroke(r, 0.0, accent, egui::StrokeKind::Outside);
                    let c = r.right_bottom();
                    painter.rect_filled(egui::Rect::from_center_size(c, vec2(9.0, 9.0)), 1.5, Color32::WHITE);
                    painter.rect_stroke(egui::Rect::from_center_size(c, vec2(9.0, 9.0)), 1.5, accent, egui::StrokeKind::Inside);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------- clone

/// Clone stamp: Alt+click sets the source, then paint; one undo step per stroke.
pub struct CloneTool {
    src: Option<Point>,
    offset: Option<(i32, i32)>,
    last: Option<[f64; 2]>,
    hover: Option<[f64; 2]>,
    radius: u32,
    hardness: f32,
    painting: bool,
}

impl CloneTool {
    pub fn new() -> Self {
        Self { src: None, offset: None, last: None, hover: None, radius: 16, hardness: 0.6, painting: false }
    }

    fn stamp(&self, doc: &mut Document, at: [f64; 2]) {
        let Some((dx, dy)) = self.offset else { return };
        let dst = Point::new(at[0].floor() as i32, at[1].floor() as i32);
        let src = Point::new(dst.x + dx, dst.y + dy);
        let r = self.radius as i32;
        effects::clone_stamp(doc.live_pixels(), src, dst, self.radius, self.hardness);
        doc.mark_region_dirty(Rect::new(dst.x - r - 1, dst.y - r - 1, 2 * r + 3, 2 * r + 3));
    }
}

impl CanvasTool for CloneTool {
    fn id(&self) -> &str {
        "fx.clone"
    }
    fn label(&self) -> String {
        t("fx-clone-tool")
    }
    fn icon(&self) -> &str {
        "⎘"
    }
    fn cursor(&self) -> CursorIcon {
        CursorIcon::None
    }
    fn wants_wheel(&self) -> bool {
        true
    }

    fn on_input(&mut self, doc: &mut Document, input: &ToolInput) {
        self.hover = input.pos.filter(|_| input.hovered);
        if input.scroll != 0.0 {
            let r = self.radius as f32 * if input.scroll > 0.0 { 1.15 } else { 1.0 / 1.15 };
            self.radius = (r.round() as u32).clamp(1, 400);
        }
        let Some(p) = input.pos else { return };
        if input.pressed && input.modifiers.alt {
            self.src = Some(Point::new(p[0].floor() as i32, p[1].floor() as i32));
            self.offset = None;
            return;
        }
        if input.pressed {
            if let Some(s) = self.src {
                if self.offset.is_none() {
                    self.offset = Some((s.x - p[0].floor() as i32, s.y - p[1].floor() as i32));
                }
                self.painting = true;
                self.stamp(doc, p);
                self.last = Some(p);
            }
        } else if input.down && self.painting {
            if let Some(l) = self.last {
                let step = (self.radius as f64 / 3.0).max(1.0);
                let d = ((p[0] - l[0]).powi(2) + (p[1] - l[1]).powi(2)).sqrt();
                if d >= step {
                    let n = (d / step).ceil() as usize;
                    for i in 1..=n {
                        let k = i as f64 / n as f64;
                        self.stamp(doc, [l[0] + (p[0] - l[0]) * k, l[1] + (p[1] - l[1]) * k]);
                    }
                    self.last = Some(p);
                }
            }
        }
        if input.released && self.painting {
            self.painting = false;
            self.last = None;
            doc.end_live_edit(&t("fx-clone-tool"));
        }
    }

    fn paint(&mut self, _doc: &Document, painter: &egui::Painter, map: &CanvasMap) {
        let to = |p: [f64; 2]| {
            let s = map.to_screen(p);
            pos2(s[0], s[1])
        };
        let rr = (self.radius as f32 * map.scale).max(2.0);
        if let Some(h) = self.hover {
            let c = to(h);
            painter.circle_stroke(c, rr, Stroke::new(2.5_f32, Color32::from_black_alpha(140)));
            painter.circle_stroke(c, rr, Stroke::new(1.0_f32, Color32::WHITE));
            let src = match (self.offset, self.src) {
                (Some((dx, dy)), _) => Some([h[0] + dx as f64, h[1] + dy as f64]),
                (None, Some(s)) => Some([s.x as f64 + 0.5, s.y as f64 + 0.5]),
                _ => None,
            };
            match src {
                Some(s) => {
                    let c = to(s);
                    painter.circle_stroke(c, rr, Stroke::new(1.0_f32, ACCENT));
                    painter.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], Stroke::new(1.0_f32, ACCENT));
                    painter.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], Stroke::new(1.0_f32, ACCENT));
                }
                None => {
                    painter.text(
                        c + vec2(rr + 8.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        t("fx-clone-hint"),
                        egui::FontId::proportional(12.0),
                        ACCENT,
                    );
                }
            }
        }
    }

    fn options_ui(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(t("fx-clone-radius"));
                ui.add(egui::Slider::new(&mut self.radius, 1..=200).suffix(" px"));
                ui.label(t("fx-clone-hardness"));
                ui.add(egui::Slider::new(&mut self.hardness, 0.0..=1.0).fixed_decimals(2));
            });
            ui.label(egui::RichText::new(t("fx-clone-hint")).size(10.5).weak());
        });
    }

    fn deactivate(&mut self, doc: &mut Document) {
        if self.painting {
            self.painting = false;
            doc.end_live_edit(&t("fx-clone-tool"));
        }
    }
}
