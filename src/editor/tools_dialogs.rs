//! Tools ▸ Combine Images, Convert Images to PDF and Batch Print as egui windows inside the
//! editor. Decoding, combining, PDF writing and printing all run on worker threads; the
//! windows only show state and progress.
//!
//! Hosting (editor app):
//! ```ignore
//! dialogs.open(ToolKind::Combine, tools_context);   // from the ToolsHost / Tools menu
//! for ev in dialogs.ui(ctx, hwnd) { /* NewTab → push a document, Toast → show it */ }
//! ```

use super::effects::downscale_proxy;
use super::ext::ToolsContext;
use super::style;
use super::texture::{tile_to_color_image, TileRect};
use crate::common::Image;
use crate::compose::combine::{CombineAlign, CombineItem, CombineParams};
use crate::compose::pdf::{PageOrientation, PageSize, PdfOptions, PdfQuality, PdfSource};
use crate::effects::{Color, Orientation};
use crate::i18n::{t, tf};
use crate::settings::print::{PrintFit, PrintSettings};
use crate::settings::{self};
use eframe::egui::{self, vec2, Align2, Color32, Id, RichText, TextureHandle};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest side of an image in the combine preview.
const PREVIEW_SIDE: u32 = 600;
const DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    Combine,
    Pdf,
    BatchPrint,
}

pub enum ToolsEvent {
    NewTab { image: Image, title: String },
    Toast { text: String, error: bool },
}

/// The three tool windows; each can be open at the same time.
#[derive(Default)]
pub struct ToolsDialogs {
    combine: Option<CombineDlg>,
    pdf: Option<PdfDlg>,
    print: Option<PrintDlg>,
}

impl ToolsDialogs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or bring back) a tool window, seeded with the open tabs.
    pub fn open(&mut self, kind: ToolKind, ctx: ToolsContext) {
        match kind {
            ToolKind::Combine => {
                if self.combine.is_none() {
                    self.combine = Some(CombineDlg::new(&ctx));
                }
            }
            ToolKind::Pdf => {
                if self.pdf.is_none() {
                    self.pdf = Some(PdfDlg::new(&ctx));
                }
            }
            ToolKind::BatchPrint => {
                if self.print.is_none() {
                    self.print = Some(PrintDlg::new(&ctx));
                }
            }
        }
    }

    /// Call every frame; `owner_hwnd` owns file/print dialogs.
    pub fn ui(&mut self, ctx: &egui::Context, owner_hwnd: isize) -> Vec<ToolsEvent> {
        let mut events = Vec::new();
        if let Some(d) = self.combine.as_mut() {
            if !d.ui(ctx, owner_hwnd, &mut events) {
                self.combine = None;
            }
        }
        if let Some(d) = self.pdf.as_mut() {
            if !d.ui(ctx, owner_hwnd, &mut events) {
                self.pdf = None;
            }
        }
        if let Some(d) = self.print.as_mut() {
            if !d.ui(ctx, owner_hwnd, &mut events) {
                self.print = None;
            }
        }
        events
    }
}

// ---------------------------------------------------------------------- pure helpers

/// Move item `i` by `delta` places; false if it would leave the list.
pub fn move_item<T>(v: &mut [T], i: usize, delta: isize) -> bool {
    let j = i as isize + delta;
    if i >= v.len() || j < 0 || j as usize >= v.len() {
        return false;
    }
    v.swap(i, j as usize);
    true
}

/// One scale for every image so the largest fits `max_side` (relative sizes are kept).
pub fn preview_scale(sizes: &[(u32, u32)], max_side: u32) -> f32 {
    let longest = sizes.iter().map(|&(w, h)| w.max(h)).max().unwrap_or(0);
    if longest <= max_side || longest == 0 {
        1.0
    } else {
        max_side as f32 / longest as f32
    }
}

/// Combine parameters with every pixel distance scaled for a preview.
pub fn scale_combine_params(p: &CombineParams, s: f32) -> CombineParams {
    if (s - 1.0).abs() < 1e-6 {
        return p.clone();
    }
    let px = |v: u32| (v as f32 * s).round() as u32;
    let mut q = p.clone();
    q.spacing = px(p.spacing);
    q.margin = px(p.margin);
    q.shadow_params.offset_x = (p.shadow_params.offset_x as f32 * s).round() as i32;
    q.shadow_params.offset_y = (p.shadow_params.offset_y as f32 * s).round() as i32;
    q.shadow_params.blur = p.shadow_params.blur * s;
    q.label_font.size = px(p.label_font.size).max(6);
    q
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

fn texture(ctx: &egui::Context, name: &str, img: &Image) -> TextureHandle {
    let r = TileRect { x: 0, y: 0, w: img.width, h: img.height };
    ctx.load_texture(name, tile_to_color_image(img, r), egui::TextureOptions::LINEAR)
}

fn to32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn from32(c: Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::rgba(r, g, b, a)
}

fn color_row(ui: &mut egui::Ui, label: &str, c: &mut Color, alpha: bool) -> bool {
    ui.label(label);
    let mut c32 = to32(*c);
    let mode = if alpha { egui::color_picker::Alpha::OnlyBlend } else { egui::color_picker::Alpha::Opaque };
    let changed = egui::color_picker::color_edit_button_srgba(ui, &mut c32, mode).changed();
    if changed {
        *c = from32(c32);
    }
    ui.end_row();
    changed
}

fn u32_row(ui: &mut egui::Ui, label: &str, v: &mut u32, max: u32, suffix: &str) -> bool {
    ui.label(label);
    let r = ui.add(egui::DragValue::new(v).range(0..=max).suffix(suffix)).changed();
    ui.end_row();
    r
}

fn f32_row(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) -> bool {
    ui.label(label);
    let r = ui.add(egui::DragValue::new(v).range(range).speed(0.5).suffix(suffix)).changed();
    ui.end_row();
    r
}

fn choice_row<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, label: &str, v: &mut T, opts: &[(T, String)]) -> bool {
    ui.label(label);
    let mut changed = false;
    let shown = opts.iter().find(|(o, _)| o == v).map(|(_, l)| l.clone()).unwrap_or_default();
    egui::ComboBox::from_id_salt(("tools-choice", id)).selected_text(shown).width(180.0).show_ui(ui, |ui| {
        for (o, l) in opts {
            changed |= ui.selectable_value(v, *o, l).changed();
        }
    });
    ui.end_row();
    changed
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(("tools-grid", id)).num_columns(2).spacing(vec2(10.0, 7.0)).show(ui, add);
}

fn title_of(key: &str) -> String {
    t(key).trim_end_matches('…').trim_end_matches("...").to_string()
}

/// A centred, closable tool window. Returns false when closed with the title-bar button.
fn tool_window(ctx: &egui::Context, id: &str, title: &str, width: f32, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut open = true;
    let center = ctx.screen_rect().center();
    egui::Window::new(RichText::new(title).strong())
        .id(Id::new(("tools-window", id)))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(width)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(center)
        .show(ctx, add);
    open
}

/// Right-aligned primary + cancel buttons. Returns (run, cancel).
fn action_row(ui: &mut egui::Ui, run_label: &str, run_enabled: bool) -> (bool, bool) {
    let mut run = false;
    let mut cancel = false;
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add_enabled(run_enabled, style::primary_button(run_label).min_size(vec2(96.0, 28.0))).clicked() {
            run = true;
        }
        if ui.add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0))).clicked() {
            cancel = true;
        }
    });
    (run, cancel)
}

/// Run `f` on a worker thread; the result arrives on the returned channel (and wakes egui).
fn spawn<T: Send + 'static>(name: &str, ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    let res = std::thread::Builder::new().name(name.into()).spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    if let Err(e) = res {
        log::error!("tools: cannot start {name}: {e}");
    }
    rx
}

// ---------------------------------------------------------------------- input list

enum Source {
    /// An open tab (already decoded).
    Memory,
    File(PathBuf),
}

struct Input {
    id: u64,
    label: String,
    src: Source,
    /// Decoded pixels (open tabs, or files once the loader finished).
    img: Option<Arc<Image>>,
    error: Option<String>,
}

impl Input {
    fn path(&self) -> Option<&Path> {
        match &self.src {
            Source::File(p) => Some(p),
            Source::Memory => None,
        }
    }
}

/// The list of images a tool works on: reorder, remove, add files. Optionally decodes files
/// in the background (combine needs the pixels for the preview).
struct InputList {
    items: Vec<Input>,
    next_id: u64,
    preload: bool,
    tx: Sender<(u64, Result<Image, String>)>,
    rx: Receiver<(u64, Result<Image, String>)>,
    /// Bumped whenever the list changes.
    version: u64,
}

impl InputList {
    fn new(preload: bool) -> Self {
        let (tx, rx) = channel();
        Self { items: Vec::new(), next_id: 1, preload, tx, rx, version: 0 }
    }

    fn add_memory(&mut self, title: &str, img: Arc<Image>) {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Input { id, label: title.to_string(), src: Source::Memory, img: Some(img), error: None });
        self.version += 1;
    }

    fn add_files(&mut self, ctx: Option<&egui::Context>, paths: Vec<PathBuf>) {
        let mut to_load = Vec::new();
        for p in paths {
            if self.items.iter().any(|i| i.path().is_some_and(|q| q.as_os_str().eq_ignore_ascii_case(p.as_os_str()))) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.items.push(Input { id, label: file_name(&p), src: Source::File(p.clone()), img: None, error: None });
            to_load.push((id, p));
        }
        self.version += 1;
        if let (true, false, Some(ctx)) = (self.preload, to_load.is_empty(), ctx) {
            let tx = self.tx.clone();
            let ctx = ctx.clone();
            let res = std::thread::Builder::new().name("tools-load".into()).spawn(move || {
                for (id, p) in to_load {
                    let r = crate::output::save::load_image(&p).map_err(|e| format!("{e:#}"));
                    if tx.send((id, r)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
            if let Err(e) = res {
                log::error!("tools: cannot start the loader: {e}");
            }
        }
    }

    fn poll(&mut self) {
        while let Ok((id, r)) = self.rx.try_recv() {
            if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                match r {
                    Ok(img) => it.img = Some(Arc::new(img)),
                    Err(e) => it.error = Some(e),
                }
                self.version += 1;
            }
        }
    }

    fn loading(&self) -> bool {
        self.preload && self.items.iter().any(|i| i.img.is_none() && i.error.is_none())
    }

    fn files(&self) -> Vec<PathBuf> {
        self.items.iter().filter_map(|i| i.path().map(Path::to_path_buf)).collect()
    }

    fn browse(&mut self, ctx: &egui::Context, owner: isize) {
        let folder = super::app::default_folder();
        let files =
            super::dialogs::open_files(owner, Some(&folder), &t("editor-filter-images"), &t("editor-filter-all"));
        if let Some(dir) = files.first().and_then(|p| p.parent()) {
            super::app::remember_folder(dir);
        }
        if !files.is_empty() {
            self.add_files(Some(ctx), files);
        }
    }

    /// The list with per-row buttons. Returns true if it changed.
    fn ui(&mut self, ui: &mut egui::Ui, owner: isize) -> bool {
        let before = self.version;
        ui.horizontal(|ui| {
            ui.label(RichText::new(tf("tools-files-count", &[("count", &self.items.len().to_string())])).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(format!("➕  {}", t("tools-add-files"))).clicked() {
                    let ctx = ui.ctx().clone();
                    self.browse(&ctx, owner);
                }
            });
        });
        let weak = ui.visuals().weak_text_color();
        let mut action: Option<(usize, isize)> = None;
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                    if self.items.is_empty() {
                        ui.label(RichText::new(t("tools-no-files")).color(weak));
                    }
                    let n = self.items.len();
                    for (i, it) in self.items.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let icon = if it.path().is_some() { "🗋" } else { "🖼" };
                            let mut text = RichText::new(format!("{icon}  {}", it.label));
                            if it.error.is_some() {
                                text = text.color(ui.visuals().error_fg_color);
                            }
                            let resp = ui.add(egui::Label::new(text).truncate());
                            match (&it.error, it.path()) {
                                (Some(e), _) => {
                                    resp.on_hover_text(e);
                                }
                                (None, Some(p)) => {
                                    resp.on_hover_text(p.display().to_string());
                                }
                                _ => {}
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if ui.small_button("✖").on_hover_text(t("tools-remove")).clicked() {
                                    action = Some((i, 0));
                                }
                                if ui
                                    .add_enabled(i + 1 < n, egui::Button::new("⏷").small())
                                    .on_hover_text(t("tools-move-down"))
                                    .clicked()
                                {
                                    action = Some((i, 1));
                                }
                                if ui
                                    .add_enabled(i > 0, egui::Button::new("⏶").small())
                                    .on_hover_text(t("tools-move-up"))
                                    .clicked()
                                {
                                    action = Some((i, -1));
                                }
                                if self.preload && it.img.is_none() && it.error.is_none() {
                                    ui.add(egui::Spinner::new().size(12.0));
                                }
                            });
                        });
                    }
                });
            });
        match action {
            Some((i, 0)) if i < self.items.len() => {
                self.items.remove(i);
                self.version += 1;
            }
            Some((i, d)) if move_item(&mut self.items, i, d) => self.version += 1,
            _ => {}
        }
        self.version != before
    }
}

// ---------------------------------------------------------------------- combine

type PreviewReq = (u64, Vec<(Arc<Image>, String)>, CombineParams);

struct CombineDlg {
    list: InputList,
    params: CombineParams,
    more: bool,
    preview_tx: Option<Sender<PreviewReq>>,
    preview_rx: Option<Receiver<(u64, Image)>>,
    preview: Option<(TextureHandle, [f32; 2])>,
    generation: u64,
    shown: u64,
    changed_at: Option<Instant>,
    /// Version of the list / params the last preview request was built from.
    requested: Option<(u64, CombineParams)>,
    running: Option<Receiver<Image>>,
}

impl CombineDlg {
    fn new(ctx: &ToolsContext) -> Self {
        let mut list = InputList::new(true);
        for (title, img) in &ctx.images {
            list.add_memory(title, img.clone());
        }
        Self {
            list,
            params: settings::get().print.last_combine.clone(),
            more: false,
            preview_tx: None,
            preview_rx: None,
            preview: None,
            generation: 0,
            shown: 0,
            changed_at: Some(Instant::now() - DEBOUNCE),
            requested: None,
            running: None,
        }
    }

    fn ensure_worker(&mut self, ctx: &egui::Context) {
        if self.preview_tx.is_some() {
            return;
        }
        let (tx, wrx) = channel::<PreviewReq>();
        let (wtx, rx) = channel::<(u64, Image)>();
        let ctx = ctx.clone();
        let res = std::thread::Builder::new().name("tools-combine-preview".into()).spawn(move || {
            while let Ok(mut req) = wrx.recv() {
                while let Ok(newer) = wrx.try_recv() {
                    req = newer;
                }
                let (gen, items, params) = req;
                let sizes: Vec<(u32, u32)> = items.iter().map(|(i, _)| (i.width, i.height)).collect();
                let s = preview_scale(&sizes, PREVIEW_SIDE);
                let proxies: Vec<CombineItem> = items
                    .iter()
                    .map(|(img, label)| {
                        let side = ((img.width.max(img.height) as f32 * s).round() as u32).max(1);
                        CombineItem { image: downscale_proxy(img, side).0, label: label.clone() }
                    })
                    .collect();
                let out = crate::compose::combine_images(&proxies, &scale_combine_params(&params, s));
                if wtx.send((gen, out)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        match res {
            Ok(_) => {
                self.preview_tx = Some(tx);
                self.preview_rx = Some(rx);
            }
            Err(e) => log::warn!("tools: cannot start the combine preview: {e}"),
        }
    }

    fn items(&self) -> Vec<(Arc<Image>, String)> {
        self.list.items.iter().filter_map(|i| i.img.clone().map(|img| (img, i.label.clone()))).collect()
    }

    fn poll(&mut self, ctx: &egui::Context, events: &mut Vec<ToolsEvent>) {
        self.list.poll();
        if let Some(rx) = &self.preview_rx {
            while let Ok((gen, img)) = rx.try_recv() {
                if gen >= self.shown {
                    self.shown = gen;
                    self.preview = (!img.is_empty())
                        .then(|| (texture(ctx, "tools-combine-preview", &img), [img.width as f32, img.height as f32]));
                }
            }
        }
        if let Some(rx) = &self.running {
            if let Ok(img) = rx.try_recv() {
                self.running = None;
                if img.is_empty() {
                    events.push(ToolsEvent::Toast { text: t("tools-nothing"), error: true });
                } else {
                    events.push(ToolsEvent::NewTab { image: img, title: title_of("tool-combine") });
                }
            }
        }
        // Debounced preview request whenever the list or the parameters changed.
        let key = (self.list.version, self.params.clone());
        if self.requested.as_ref() != Some(&key) {
            if self.changed_at.is_none() {
                self.changed_at = Some(Instant::now());
            }
            let at = self.changed_at.unwrap_or_else(Instant::now);
            let left = DEBOUNCE.saturating_sub(at.elapsed());
            if left.is_zero() {
                self.changed_at = None;
                self.requested = Some(key);
                self.generation += 1;
                let items = self.items();
                if items.is_empty() {
                    self.preview = None;
                    self.shown = self.generation;
                } else {
                    self.ensure_worker(ctx);
                    if let Some(tx) = &self.preview_tx {
                        let _ = tx.send((self.generation, items, self.params.clone()));
                    }
                }
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn ui(&mut self, ctx: &egui::Context, owner: isize, events: &mut Vec<ToolsEvent>) -> bool {
        self.poll(ctx, events);
        let mut keep = true;
        let open = tool_window(ctx, "combine", &title_of("tool-combine"), 760.0, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(330.0);
                    self.list.ui(ui, owner);
                    ui.add_space(6.0);
                    self.params_ui(ui);
                });
                ui.add_space(10.0);
                ui.vertical(|ui| self.preview_ui(ui));
            });
            let busy = self.running.is_some();
            let ready = !self.items().is_empty() && !self.list.loading() && !busy;
            let label = if busy { t("tools-working") } else { t("tools-combine-run") };
            let (run, cancel) = action_row(ui, &label, ready);
            if run {
                let items: Vec<CombineItem> =
                    self.items().into_iter().map(|(img, label)| CombineItem { image: (*img).clone(), label }).collect();
                let params = self.params.clone();
                settings::update(|s| s.print.last_combine = params.clone());
                self.running =
                    Some(spawn("tools-combine", ui.ctx(), move || crate::compose::combine_images(&items, &params)));
            }
            if cancel {
                keep = false;
            }
        });
        keep && open
    }

    fn params_ui(&mut self, ui: &mut egui::Ui) {
        let p = &mut self.params;
        grid(ui, "combine", |ui| {
            choice_row(
                ui,
                "dir",
                &t("tool-combine-direction"),
                &mut p.direction,
                &[
                    (Orientation::Horizontal, t("tool-combine-side-by-side")),
                    (Orientation::Vertical, t("tool-combine-stacked")),
                ],
            );
            choice_row(
                ui,
                "align",
                &t("tool-combine-align"),
                &mut p.align,
                &[
                    (CombineAlign::Start, t("tool-combine-align-start")),
                    (CombineAlign::Center, t("tool-combine-align-center")),
                    (CombineAlign::End, t("tool-combine-align-end")),
                ],
            );
            u32_row(ui, &t("tool-combine-spacing"), &mut p.spacing, 2000, " px");
            u32_row(ui, &t("tool-combine-margin"), &mut p.margin, 2000, " px");
            color_row(ui, &t("fx-param-background"), &mut p.background, true);
            ui.label(t("tool-combine-shadow"));
            ui.checkbox(&mut p.shadow, "");
            ui.end_row();
            ui.label(t("tool-combine-labels"));
            ui.checkbox(&mut p.labels, "");
            ui.end_row();
        });
        let label = if self.more { t("editor-less") } else { t("editor-more") };
        if ui.add(egui::Button::new(label).frame(false)).clicked() {
            self.more = !self.more;
        }
        if self.more {
            let p = &mut self.params;
            grid(ui, "combine-more", |ui| {
                ui.add_enabled_ui(p.shadow, |ui| {
                    ui.label(t("fx-shadow-offset-x"));
                });
                ui.add_enabled(p.shadow, egui::DragValue::new(&mut p.shadow_params.offset_x).range(-200..=200));
                ui.end_row();
                ui.add_enabled_ui(p.shadow, |ui| {
                    ui.label(t("fx-shadow-offset-y"));
                });
                ui.add_enabled(p.shadow, egui::DragValue::new(&mut p.shadow_params.offset_y).range(-200..=200));
                ui.end_row();
                ui.add_enabled_ui(p.shadow, |ui| {
                    ui.label(t("fx-shadow-blur"));
                });
                ui.add_enabled(p.shadow, egui::DragValue::new(&mut p.shadow_params.blur).range(0.0..=200.0).speed(0.5));
                ui.end_row();
                ui.add_enabled_ui(p.shadow, |ui| {
                    ui.label(t("fx-shadow-opacity"));
                });
                ui.add_enabled(p.shadow, egui::Slider::new(&mut p.shadow_params.opacity, 0..=100));
                ui.end_row();
                color_row(ui, &t("fx-shadow-color"), &mut p.shadow_params.color, false);
                u32_row(ui, &t("tools-label-size"), &mut p.label_font.size, 200, " px");
                color_row(ui, &t("tools-label-color"), &mut p.label_color, true);
                ui.label(t("fx-caption-bold"));
                ui.checkbox(&mut p.label_font.bold, "");
                ui.end_row();
            });
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let box_size = vec2(380.0, 330.0);
        let (rect, _) = ui.allocate_exact_size(box_size, egui::Sense::hover());
        let bg = ui.visuals().extreme_bg_color;
        ui.painter().rect_filled(rect, 8.0, bg);
        match &self.preview {
            Some((tex, size)) if size[0] > 0.0 && size[1] > 0.0 => {
                let avail = rect.shrink(10.0).size();
                let s = (avail.x / size[0]).min(avail.y / size[1]).min(2.0);
                let r = egui::Rect::from_center_size(rect.center(), vec2(size[0], size[1]) * s);
                // Checkerboard behind transparent backgrounds.
                let cell = 8.0;
                let painter = ui.painter_at(r);
                let (light, dark) = if ui.visuals().dark_mode {
                    (Color32::from_gray(70), Color32::from_gray(55))
                } else {
                    (Color32::WHITE, Color32::from_gray(220))
                };
                painter.rect_filled(r, 0.0, light);
                let (nx, ny) = ((r.width() / cell).ceil() as i32, (r.height() / cell).ceil() as i32);
                if nx * ny < 20_000 {
                    for y in 0..ny {
                        for x in (y % 2..nx).step_by(2) {
                            let c = egui::Rect::from_min_size(
                                r.min + vec2(x as f32 * cell, y as f32 * cell),
                                vec2(cell, cell),
                            );
                            painter.rect_filled(c, 0.0, dark);
                        }
                    }
                }
                ui.painter().image(
                    tex.id(),
                    r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            _ => {
                let weak = ui.visuals().weak_text_color();
                if self.list.items.is_empty() {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        t("tools-no-files"),
                        egui::FontId::proportional(13.0),
                        weak,
                    );
                } else {
                    ui.put(rect, egui::Spinner::new());
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("tools-preview")).small().color(ui.visuals().weak_text_color()));
            if self.shown < self.generation || self.changed_at.is_some() {
                ui.add(egui::Spinner::new().size(11.0));
            }
        });
    }
}

// ---------------------------------------------------------------------- PDF

enum PdfMsg {
    Progress(usize, usize),
    Done(Result<usize, String>),
}

struct PdfDlg {
    list: InputList,
    opts: PdfOptions,
    output: String,
    rx: Option<Receiver<PdfMsg>>,
    progress: (usize, usize),
    error: Option<String>,
}

impl PdfDlg {
    fn new(ctx: &ToolsContext) -> Self {
        let mut list = InputList::new(false);
        for (title, img) in &ctx.images {
            list.add_memory(title, img.clone());
        }
        // Open tabs that are saved files are already in `images`; extra files come from `files`.
        let output = super::app::default_folder().join("Images.pdf").to_string_lossy().into_owned();
        Self { list, opts: settings::get().print.last_pdf.clone(), output, rx: None, progress: (0, 0), error: None }
    }

    fn poll(&mut self, events: &mut Vec<ToolsEvent>) {
        let Some(rx) = &self.rx else { return };
        while let Ok(m) = rx.try_recv() {
            match m {
                PdfMsg::Progress(n, total) => self.progress = (n, total),
                PdfMsg::Done(r) => {
                    self.rx = None;
                    match r {
                        Ok(pages) => {
                            let text =
                                tf("tool-pdf-done", &[("path", self.output.as_str()), ("pages", &pages.to_string())]);
                            events.push(ToolsEvent::Toast { text, error: false });
                            self.error = None;
                        }
                        Err(e) => {
                            events.push(ToolsEvent::Toast { text: tf("tools-failed", &[("error", &e)]), error: true });
                            self.error = Some(e);
                        }
                    }
                    return;
                }
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        enum Job {
            Mem(Arc<Image>),
            File(PathBuf),
        }
        let jobs: Vec<Job> = self
            .list
            .items
            .iter()
            .map(|i| match (&i.src, &i.img) {
                (Source::File(p), _) => Job::File(p.clone()),
                (Source::Memory, Some(img)) => Job::Mem(img.clone()),
                (Source::Memory, None) => Job::File(PathBuf::new()),
            })
            .collect();
        let opts = self.opts.clone();
        settings::update(|s| s.print.last_pdf = opts.clone());
        let out = PathBuf::from(self.output.trim());
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.progress = (0, jobs.len());
        self.error = None;
        let res = std::thread::Builder::new().name("tools-pdf".into()).spawn(move || {
            let total = jobs.len();
            let mut sources = Vec::with_capacity(total);
            for (n, j) in jobs.into_iter().enumerate() {
                let _ = tx.send(PdfMsg::Progress(n, total));
                ctx.request_repaint();
                match j {
                    Job::Mem(img) => sources.push(PdfSource::Image((*img).clone())),
                    Job::File(p) => match crate::output::save::load_image(&p) {
                        Ok(img) => sources.push(PdfSource::Image(img)),
                        Err(e) => log::warn!("pdf: skipping {}: {e:#}", p.display()),
                    },
                }
            }
            let _ = tx.send(PdfMsg::Progress(total, total));
            let r = crate::compose::images_to_pdf(&sources, &opts, &out).map_err(|e| format!("{e:#}"));
            let _ = tx.send(PdfMsg::Done(r));
            ctx.request_repaint();
        });
        match res {
            Ok(_) => self.rx = Some(rx),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn ui(&mut self, ctx: &egui::Context, owner: isize, events: &mut Vec<ToolsEvent>) -> bool {
        self.poll(events);
        let mut keep = true;
        let open = tool_window(ctx, "pdf", &title_of("editor-tool-pdf"), 460.0, |ui| {
            let busy = self.rx.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                self.list.ui(ui, owner);
                ui.add_space(6.0);
                self.options_ui(ui, owner);
            });
            if busy {
                let (n, total) = self.progress;
                let f = if total == 0 { 0.0 } else { n as f32 / total as f32 };
                let text = if n >= total {
                    t("tools-pdf-writing")
                } else {
                    tf("tools-pdf-progress", &[("n", &(n + 1).to_string()), ("total", &total.to_string())])
                };
                ui.add_space(6.0);
                ui.add(egui::ProgressBar::new(f).text(text).animate(true));
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
            }
            let ready = !busy && !self.list.items.is_empty() && !self.output.trim().is_empty();
            let label = if busy { t("tools-working") } else { t("tools-pdf-run") };
            let (run, cancel) = action_row(ui, &label, ready);
            if run {
                self.start(ui.ctx());
            }
            if cancel {
                keep = false;
            }
        });
        keep && open
    }

    fn options_ui(&mut self, ui: &mut egui::Ui, owner: isize) {
        let o = &mut self.opts;
        grid(ui, "pdf", |ui| {
            let sizes: Vec<(PageSize, String)> = PageSize::ALL.iter().map(|s| (*s, t(s.label_key()))).collect();
            choice_row(ui, "size", &t("pdf-page-size"), &mut o.page_size, &sizes);
            if o.page_size == PageSize::Custom {
                f32_row(ui, &t("pdf-custom-width"), &mut o.custom_width_mm, 10.0..=5000.0, " mm");
                f32_row(ui, &t("pdf-custom-height"), &mut o.custom_height_mm, 10.0..=5000.0, " mm");
            }
            choice_row(
                ui,
                "orient",
                &t("pdf-orientation"),
                &mut o.orientation,
                &[
                    (PageOrientation::Auto, t("pdf-orientation-auto")),
                    (PageOrientation::Portrait, t("pdf-orientation-portrait")),
                    (PageOrientation::Landscape, t("pdf-orientation-landscape")),
                ],
            );
            f32_row(ui, &t("pdf-margin"), &mut o.margin_mm, 0.0..=100.0, " mm");
            choice_row(
                ui,
                "quality",
                &t("pdf-quality"),
                &mut o.quality,
                &[(PdfQuality::Jpeg, t("pdf-quality-jpeg")), (PdfQuality::Lossless, t("pdf-quality-lossless"))],
            );
            if o.quality == PdfQuality::Jpeg {
                ui.label(t("pdf-jpeg-quality"));
                ui.add(egui::Slider::new(&mut o.jpeg_quality, 10..=100));
                ui.end_row();
            }
            ui.label(t("pdf-split-long"));
            ui.checkbox(&mut o.split_long_images, "");
            ui.end_row();
            ui.label(t("pdf-title"));
            ui.add(egui::TextEdit::singleline(&mut o.title).desired_width(200.0));
            ui.end_row();
            ui.label(t("tools-pdf-output"));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.output).desired_width(200.0));
                if ui.button(t("editor-browse")).clicked() {
                    let cur = PathBuf::from(self.output.trim());
                    let folder = cur
                        .parent()
                        .filter(|p| p.is_dir())
                        .map(Path::to_path_buf)
                        .unwrap_or_else(super::app::default_folder);
                    let name = cur.file_name().map(|n| n.to_string_lossy().into_owned());
                    let name = name.unwrap_or_else(|| "Images.pdf".into());
                    if let Some(p) = super::dialogs::save_file_with(
                        owner,
                        Some(&folder),
                        &name,
                        &[("PDF (*.pdf)".into(), "*.pdf".into())],
                        "pdf",
                    ) {
                        self.output = p.to_string_lossy().into_owned();
                    }
                }
            });
            ui.end_row();
        });
    }
}

// ---------------------------------------------------------------------- batch print

struct PrintDlg {
    list: InputList,
    opts: PrintSettings,
    rx: Option<Receiver<Result<bool, String>>>,
    count: usize,
    error: Option<String>,
}

impl PrintDlg {
    fn new(ctx: &ToolsContext) -> Self {
        let mut list = InputList::new(false);
        list.add_files(None, ctx.files.clone());
        Self { list, opts: settings::get().print.clone(), rx: None, count: 0, error: None }
    }

    fn poll(&mut self, events: &mut Vec<ToolsEvent>) {
        let Some(rx) = &self.rx else { return };
        if let Ok(r) = rx.try_recv() {
            self.rx = None;
            match r {
                Ok(true) => events.push(ToolsEvent::Toast {
                    text: tf("tool-batch-print-done", &[("count", &self.count.to_string())]),
                    error: false,
                }),
                Ok(false) => {}
                Err(e) => {
                    events.push(ToolsEvent::Toast { text: tf("tools-failed", &[("error", &e)]), error: true });
                    self.error = Some(e);
                }
            }
        }
    }

    fn ui(&mut self, ctx: &egui::Context, owner: isize, events: &mut Vec<ToolsEvent>) -> bool {
        self.poll(events);
        let mut keep = true;
        let open = tool_window(ctx, "print", &title_of("editor-tool-batch-print"), 460.0, |ui| {
            let busy = self.rx.is_some();
            ui.add_enabled_ui(!busy, |ui| {
                self.list.ui(ui, owner);
                ui.add_space(6.0);
                self.options_ui(ui);
            });
            if busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(t("tools-working"));
                });
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
            }
            let ready = !busy && !self.list.items.is_empty();
            let (run, cancel) = action_row(ui, &t("tools-print-run"), ready);
            if run {
                let paths = self.list.files();
                let opts = self.opts.clone();
                self.count = paths.len();
                self.error = None;
                settings::update(|s| {
                    let keep_pdf = s.print.last_pdf.clone();
                    let keep_combine = s.print.last_combine.clone();
                    s.print = PrintSettings { last_pdf: keep_pdf, last_combine: keep_combine, ..opts.clone() };
                });
                self.rx = Some(spawn("tools-batch-print", ui.ctx(), move || {
                    crate::compose::batch_print(&paths, &opts, owner).map_err(|e| format!("{e:#}"))
                }));
            }
            if cancel {
                keep = false;
            }
        });
        keep && open
    }

    fn options_ui(&mut self, ui: &mut egui::Ui) {
        let first = self.list.files().into_iter().next();
        let total = self.list.items.len().max(1);
        let o = &mut self.opts;
        grid(ui, "print", |ui| {
            let fits: Vec<(PrintFit, String)> = PrintFit::ALL.iter().map(|f| (*f, t(f.label_key()))).collect();
            choice_row(ui, "fit", &t("print-fit"), &mut o.fit, &fits);
            ui.label(t("print-margins"));
            ui.horizontal(|ui| {
                for (v, key) in [
                    (&mut o.margins.left, "fx-side-left"),
                    (&mut o.margins.top, "fx-side-top"),
                    (&mut o.margins.right, "fx-side-right"),
                    (&mut o.margins.bottom, "fx-side-bottom"),
                ] {
                    ui.add(egui::DragValue::new(v).range(0.0..=100.0).speed(0.2).max_decimals(1)).on_hover_text(t(key));
                }
            });
            ui.end_row();
            for (label, text) in [(t("print-header"), &mut o.header), (t("print-footer"), &mut o.footer)] {
                ui.label(label);
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(text).desired_width(240.0));
                    let preview = match &first {
                        Some(p) if !text.trim().is_empty() => {
                            crate::compose::batch::template_preview(text, p, 1, total)
                        }
                        _ => String::new(),
                    };
                    if !preview.is_empty() {
                        ui.label(
                            RichText::new(tf("tools-template-preview", &[("text", &preview)]))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
                ui.end_row();
            }
            ui.label("");
            ui.label(
                RichText::new(tf("print-template-help", &[("tokens", "{file} {path} {n} {total} {date} {time}")]))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.end_row();
            f32_row(ui, &t("print-header-footer-size"), &mut o.header_footer_pt, 5.0..=48.0, " pt");
            ui.label(t("print-avoid-split"));
            ui.checkbox(&mut o.avoid_splitting_lines, "");
            ui.end_row();
            ui.label(t("print-center"));
            ui.checkbox(&mut o.center, "");
            ui.end_row();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_item_bounds() {
        let mut v = vec![1, 2, 3];
        assert!(move_item(&mut v, 0, 1));
        assert_eq!(v, [2, 1, 3]);
        assert!(move_item(&mut v, 2, -1));
        assert_eq!(v, [2, 3, 1]);
        assert!(!move_item(&mut v, 0, -1));
        assert!(!move_item(&mut v, 2, 1));
        assert!(!move_item(&mut v, 7, 1));
        assert_eq!(v, [2, 3, 1]);
    }

    #[test]
    fn preview_scale_keeps_relative_sizes() {
        assert_eq!(preview_scale(&[], 600), 1.0);
        assert_eq!(preview_scale(&[(300, 200)], 600), 1.0);
        let s = preview_scale(&[(1200, 400), (600, 2400)], 600);
        assert!((s - 0.25).abs() < 1e-6);
    }

    #[test]
    fn scaled_params() {
        let p = CombineParams { spacing: 40, margin: 20, ..Default::default() };
        let q = scale_combine_params(&p, 0.5);
        assert_eq!((q.spacing, q.margin), (20, 10));
        assert!(q.label_font.size >= 6);
        assert_eq!(scale_combine_params(&p, 1.0), p);
    }

    #[test]
    fn combine_preview_matches_layout_order() {
        let mut list = InputList::new(false);
        list.add_memory("a", Arc::new(Image::filled(4, 2, [0, 0, 255, 255])));
        list.add_memory("b", Arc::new(Image::filled(2, 2, [0, 255, 0, 255])));
        assert!(move_item(&mut list.items, 1, -1));
        let labels: Vec<&str> = list.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["b", "a"]);
        assert!(!list.loading());
        assert!(list.files().is_empty());
    }
}
