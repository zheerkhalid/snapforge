//! Tools ▸ Convert to MP4 and Record ▸ Schedule Recording.
//!
//! Each is a small native window (a *deferred* egui viewport of the editor app, like the
//! Settings window), so opening one from the tray does not show the image editor. The host
//! calls [`MediaTools::ui`] on every frame; the windows repaint on their own once open.
//! Conversion runs on a worker thread with a progress bar and Cancel.

use super::dialogs;
use super::style;
use crate::app::{self, AppCommand};
use crate::common::time::DateTime;
use crate::common::win;
use crate::i18n::{t, tf};
use crate::recorder::schedule;
use crate::recorder::{AudioFormat, ConvertOptions, Resize};
use crate::settings::recorder::{ScheduleSettings, ScheduleTarget, VideoQuality};
use eframe::egui::{self, vec2, RichText, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Convert,
    Schedule,
}

/// The two windows, each hosted in its own viewport.
pub struct MediaTools {
    convert: Hosted<ConvertDlg>,
    schedule: Hosted<ScheduleDlg>,
}

impl MediaTools {
    pub fn new() -> Self {
        Self { convert: Hosted::default(), schedule: Hosted::default() }
    }

    pub fn open(&mut self, kind: MediaKind) {
        match kind {
            MediaKind::Convert => self.convert.open(),
            MediaKind::Schedule => self.schedule.open(),
        }
    }

    /// Call on every frame of the host app.
    pub fn ui(&mut self, ctx: &egui::Context) {
        self.convert.ui(ctx);
        self.schedule.ui(ctx);
    }
}

// ---------------------------------------------------------------------- hosting

/// A tool window shown in its own viewport.
trait ToolWin: Send + 'static {
    const ID: &'static str;
    const SIZE: [f32; 2];
    fn title() -> String;
    fn new(ctx: &egui::Context) -> Self;
    /// Draw the contents; returns false when the window should close.
    fn contents(&mut self, ui: &mut egui::Ui) -> bool;
    /// The window is closing (title bar, Close button or Esc).
    fn closing(&mut self) {}
}

struct HostState<T> {
    win: Option<T>,
    raise: bool,
    /// Open requested; the window is created on the next host frame (needs the context).
    pending: bool,
    icon_done: bool,
    builder: Option<ViewportBuilder>,
}

impl<T> Default for HostState<T> {
    fn default() -> Self {
        Self { win: None, raise: false, pending: false, icon_done: false, builder: None }
    }
}

struct Hosted<T> {
    inner: Arc<Mutex<HostState<T>>>,
}

impl<T> Default for Hosted<T> {
    fn default() -> Self {
        Self { inner: Arc::new(Mutex::new(HostState::default())) }
    }
}

fn viewport_id(id: &str) -> ViewportId {
    ViewportId::from_hash_of(id)
}

impl<T: ToolWin> Hosted<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, HostState<T>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open(&self) {
        let mut g = self.lock();
        if g.win.is_none() {
            g.pending = true;
            g.icon_done = false;
        }
        g.raise = true;
    }

    fn ui(&mut self, ctx: &egui::Context) {
        let id = viewport_id(T::ID);
        let (raise, icon, builder) = {
            let mut g = self.lock();
            if std::mem::take(&mut g.pending) && g.win.is_none() {
                g.win = Some(T::new(ctx));
                let mut b = ViewportBuilder::default()
                    .with_title(T::title())
                    .with_inner_size(T::SIZE)
                    .with_min_inner_size([T::SIZE[0] * 0.8, T::SIZE[1] * 0.6]);
                if let Some(icon) = style::app_icon() {
                    b = b.with_icon(icon);
                }
                g.builder = Some(b);
            }
            let (Some(_), Some(builder)) = (g.win.as_ref(), g.builder.clone()) else { return };
            (std::mem::take(&mut g.raise), !g.icon_done, builder)
        };
        if raise {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
        }
        let inner = self.inner.clone();
        ctx.show_viewport_deferred(id, builder, move |ctx, class| {
            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
            frame(&mut *g, ctx, class);
        });
        if icon {
            // eframe sets only the small icon on child viewports (see DECISIONS #92).
            let done = super::window::set_icon_by_title(&T::title());
            self.lock().icon_done = done;
            if !done {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
    }
}

fn frame<T: ToolWin>(g: &mut HostState<T>, ctx: &egui::Context, class: ViewportClass) {
    let Some(w) = g.win.as_mut() else {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        return;
    };
    let mut keep = true;
    if class == ViewportClass::Embedded {
        let mut open = true;
        egui::Window::new(T::title())
            .id(egui::Id::new(T::ID))
            .open(&mut open)
            .collapsible(false)
            .default_size(T::SIZE)
            .show(ctx, |ui| keep = w.contents(ui));
        keep &= open;
    } else {
        if ctx.input(|i| i.viewport().close_requested()) {
            keep = false;
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !w.contents(ui) {
                    keep = false;
                }
            });
        });
    }
    if !keep {
        w.closing();
        g.win = None;
        if class != ViewportClass::Embedded {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        // Let the host run a frame so egui drops the viewport.
        ctx.request_repaint_of(ViewportId::ROOT);
    }
}

/// The active window of this thread (the tool window while it has focus): owner of the
/// file dialogs.
fn owner_hwnd() -> isize {
    // SAFETY: trivial query for the calling thread.
    let h = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() };
    win::hwnd_to_isize(h)
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

/// Right-aligned primary + secondary buttons. Returns (primary, secondary) clicks.
fn buttons(ui: &mut egui::Ui, primary: &str, primary_enabled: bool, secondary: &str) -> (bool, bool) {
    let (mut a, mut b) = (false, false);
    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        a = ui.add_enabled(primary_enabled, style::primary_button(primary).min_size(vec2(110.0, 28.0))).clicked();
        b = ui.add(egui::Button::new(secondary).min_size(vec2(90.0, 28.0))).clicked();
    });
    (a, b)
}

fn error_label(ui: &mut egui::Ui, text: &str) {
    ui.colored_label(ui.visuals().error_fg_color, text);
}

// ---------------------------------------------------------------------- Convert to MP4

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Mp4,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeMode {
    Original,
    Percent,
    Custom,
}

/// Source facts shown in the dialog (and used to keep the aspect ratio).
#[derive(Debug, Clone, Copy)]
struct SourceInfo {
    width: u32,
    height: u32,
    fps: f64,
    duration_ms: u64,
    has_audio: bool,
}

struct Job {
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(), String>>,
    output: PathBuf,
    started: Instant,
}

enum Outcome {
    Done(PathBuf, Duration),
    Failed(String),
    Cancelled,
}

const AUDIO_KBPS: [u32; 5] = [96, 128, 192, 256, 320];

struct ConvertDlg {
    ctx: egui::Context,
    mode: Mode,
    input: String,
    output: String,
    /// The output path was typed or picked (no longer derived from the input).
    output_custom: bool,
    info: Option<Result<SourceInfo, String>>,
    probing: Option<Receiver<Result<SourceInfo, String>>>,
    /// The input `info` belongs to.
    probed_input: String,
    size_mode: SizeMode,
    percent: u32,
    width: u32,
    height: u32,
    keep_aspect: bool,
    /// Output frame rate; 0 = same as the source.
    fps: u32,
    quality: VideoQuality,
    custom_kbps: u32,
    audio_format: AudioFormat,
    audio_kbps: u32,
    job: Option<Job>,
    outcome: Option<Outcome>,
    error: Option<String>,
}

impl ToolWin for ConvertDlg {
    const ID: &'static str = "snapforge-convert-mp4";
    const SIZE: [f32; 2] = [560.0, 560.0];

    fn title() -> String {
        t("convert-title")
    }

    fn new(ctx: &egui::Context) -> Self {
        let audio_kbps = crate::settings::get().recorder.audio_bitrate_kbps;
        Self {
            ctx: ctx.clone(),
            mode: Mode::Mp4,
            input: String::new(),
            output: String::new(),
            output_custom: false,
            info: None,
            probing: None,
            probed_input: String::new(),
            size_mode: SizeMode::Original,
            percent: 50,
            width: 1280,
            height: 720,
            keep_aspect: true,
            fps: 0,
            quality: VideoQuality::Medium,
            custom_kbps: 4000,
            audio_format: AudioFormat::Mp3,
            audio_kbps: AUDIO_KBPS.iter().copied().find(|&k| k >= audio_kbps).unwrap_or(192),
            job: None,
            outcome: None,
            error: None,
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> bool {
        self.poll();
        let busy = self.job.is_some();
        let mut keep = true;
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                let before = self.mode;
                ui.radio_value(&mut self.mode, Mode::Mp4, t("convert-mode-mp4"));
                ui.radio_value(&mut self.mode, Mode::Audio, t("convert-mode-audio"));
                if self.mode != before {
                    self.derive_output();
                }
            });
            self.files_ui(ui);
            match self.mode {
                Mode::Mp4 => self.video_ui(ui),
                Mode::Audio => self.audio_ui(ui),
            }
        });
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            error_label(ui, e);
        }
        self.progress_ui(ui);
        let run_label = match self.mode {
            Mode::Mp4 => t("convert-run"),
            Mode::Audio => t("convert-run-audio"),
        };
        let second = if busy { t("editor-cancel") } else { t("media-close") };
        let (run, other) = buttons(ui, &run_label, !busy && !self.input.trim().is_empty(), &second);
        if run {
            self.start();
        }
        if other {
            if let Some(j) = &self.job {
                j.cancel.store(true, Ordering::Release);
            } else {
                keep = false;
            }
        }
        keep
    }

    fn closing(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Release);
        }
    }
}

impl ConvertDlg {
    fn repaint(&self) -> impl Fn() + Send + 'static {
        let ctx = self.ctx.clone();
        move || ctx.request_repaint_of(viewport_id(Self::ID))
    }

    fn files_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, &t("convert-files"));
        egui::Grid::new("convert-files").num_columns(3).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label(t("convert-input"));
            let r = ui.add(egui::TextEdit::singleline(&mut self.input).desired_width(330.0));
            if r.lost_focus() && self.input.trim() != self.probed_input {
                self.input_changed();
            }
            if ui.button(t("media-browse")).clicked() {
                self.pick_input();
            }
            ui.end_row();
            ui.label(t("convert-output"));
            if ui.add(egui::TextEdit::singleline(&mut self.output).desired_width(330.0)).changed() {
                self.output_custom = true;
            }
            if ui.button(t("media-browse")).clicked() {
                self.pick_output();
            }
            ui.end_row();
        });
        match &self.info {
            Some(Ok(i)) => {
                let secs = i.duration_ms / 1000;
                let fps = format!("{:.3}", i.fps);
                let mut text = tf(
                    "convert-source-info",
                    &[
                        ("width", &i.width.to_string()),
                        ("height", &i.height.to_string()),
                        ("fps", fps.trim_end_matches('0').trim_end_matches('.')),
                        ("duration", &format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)),
                    ],
                );
                if !i.has_audio {
                    text.push_str(" · ");
                    text.push_str(&t("convert-no-audio"));
                }
                ui.weak(text);
            }
            Some(Err(e)) => error_label(ui, &tf("convert-probe-failed", &[("error", e)])),
            None if self.probing.is_some() => {
                ui.weak(t("convert-reading"));
            }
            None => {}
        }
    }

    fn video_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, &t("convert-video"));
        let src = self.source_size();
        egui::Grid::new("convert-video").num_columns(2).spacing(vec2(10.0, 7.0)).show(ui, |ui| {
            ui.label(t("convert-size"));
            ui.vertical(|ui| {
                ui.radio_value(&mut self.size_mode, SizeMode::Original, t("convert-size-original"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.size_mode, SizeMode::Percent, t("convert-size-percent"));
                    ui.add_enabled(
                        self.size_mode == SizeMode::Percent,
                        egui::DragValue::new(&mut self.percent).range(1..=400).suffix(" %"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.size_mode, SizeMode::Custom, t("convert-size-custom"));
                    let on = self.size_mode == SizeMode::Custom;
                    let max = crate::recorder::convert::MAX_SIDE;
                    let w = ui.add_enabled(on, egui::DragValue::new(&mut self.width).range(2..=max).suffix(" px"));
                    ui.label("×");
                    let h = ui.add_enabled(on, egui::DragValue::new(&mut self.height).range(2..=max).suffix(" px"));
                    if let (true, Some((sw, sh))) = (self.keep_aspect, src) {
                        if w.changed() {
                            self.height = scale_side(self.width, sh, sw);
                        } else if h.changed() {
                            self.width = scale_side(self.height, sw, sh);
                        }
                    }
                });
                ui.add_enabled(
                    self.size_mode == SizeMode::Custom && src.is_some(),
                    egui::Checkbox::new(&mut self.keep_aspect, t("convert-keep-aspect")),
                );
                if let Some((sw, sh)) = src {
                    let (w, h) = self.resize().apply(sw, sh);
                    ui.weak(tf("convert-size-result", &[("width", &w.to_string()), ("height", &h.to_string())]));
                }
            });
            ui.end_row();

            ui.label(t("convert-fps"));
            let label = |f: u32| if f == 0 { t("convert-fps-same") } else { format!("{f} fps") };
            egui::ComboBox::from_id_salt("convert-fps").selected_text(label(self.fps)).width(180.0).show_ui(ui, |ui| {
                for f in std::iter::once(0).chain(crate::settings::recorder::FPS_CHOICES) {
                    ui.selectable_value(&mut self.fps, f, label(f));
                }
            });
            ui.end_row();

            ui.label(t("convert-quality"));
            ui.horizontal(|ui| {
                let q = |v: VideoQuality| match v {
                    VideoQuality::Low => t("convert-quality-low"),
                    VideoQuality::Medium => t("convert-quality-medium"),
                    VideoQuality::High => t("convert-quality-high"),
                    VideoQuality::Custom => t("convert-quality-custom"),
                };
                egui::ComboBox::from_id_salt("convert-quality").selected_text(q(self.quality)).width(180.0).show_ui(
                    ui,
                    |ui| {
                        for v in VideoQuality::ALL {
                            ui.selectable_value(&mut self.quality, v, q(v));
                        }
                    },
                );
                if self.quality == VideoQuality::Custom {
                    ui.add(egui::DragValue::new(&mut self.custom_kbps).range(100..=200_000).suffix(" kbit/s"));
                }
            });
            ui.end_row();

            ui.label(t("convert-audio-bitrate"));
            kbps_combo(ui, "convert-audio-kbps", &mut self.audio_kbps);
            ui.end_row();
        });
        if let Some((sw, sh)) = src {
            let opts = self.options();
            let (w, h) = opts.resize.apply(sw, sh);
            let fps = opts.fps.unwrap_or_else(|| self.source_fps().unwrap_or(30));
            let kbps = opts.bitrate_for(w, h, fps) / 1000;
            ui.weak(tf("convert-bitrate-result", &[("kbps", &kbps.to_string())]));
        }
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, &t("convert-audio"));
        egui::Grid::new("convert-audio").num_columns(2).spacing(vec2(10.0, 7.0)).show(ui, |ui| {
            ui.label(t("convert-audio-format"));
            ui.horizontal(|ui| {
                let before = self.audio_format;
                ui.radio_value(&mut self.audio_format, AudioFormat::Mp3, "MP3");
                ui.radio_value(&mut self.audio_format, AudioFormat::M4a, "M4A (AAC)");
                if before != self.audio_format {
                    self.derive_output();
                }
            });
            ui.end_row();
            ui.label(t("convert-audio-bitrate"));
            kbps_combo(ui, "convert-extract-kbps", &mut self.audio_kbps);
            ui.end_row();
        });
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if let Some(j) = &self.job {
            let p = f32::from_bits(j.progress.load(Ordering::Acquire)).clamp(0.0, 1.0);
            ui.add(egui::ProgressBar::new(p).show_percentage().animate(true));
            let secs = j.started.elapsed().as_secs();
            let text = if j.cancel.load(Ordering::Acquire) {
                t("convert-cancelling")
            } else {
                tf("convert-running", &[("elapsed", &format!("{}:{:02}", secs / 60, secs % 60))])
            };
            ui.weak(text);
            return;
        }
        match &self.outcome {
            Some(Outcome::Done(path, took)) => {
                ui.label(tf(
                    "convert-done",
                    &[("path", &path.display().to_string()), ("secs", &format!("{:.1}", took.as_secs_f32()))],
                ));
                ui.horizontal(|ui| {
                    if crate::video_editor::accepts(path) && ui.button(t("convert-open-video-editor")).clicked() {
                        app::post(AppCommand::OpenVideoEditor(Some(path.clone())));
                    }
                    if ui.button(t("convert-show-folder")).clicked() {
                        win::reveal_in_explorer(path);
                    }
                });
            }
            Some(Outcome::Failed(e)) => error_label(ui, &tf("convert-failed", &[("error", e)])),
            Some(Outcome::Cancelled) => {
                ui.weak(t("convert-cancelled"));
            }
            None => {}
        }
    }

    fn source_size(&self) -> Option<(u32, u32)> {
        match &self.info {
            Some(Ok(i)) if i.width > 0 && i.height > 0 => Some((i.width, i.height)),
            _ => None,
        }
    }

    fn source_fps(&self) -> Option<u32> {
        match &self.info {
            Some(Ok(i)) if i.fps > 0.0 => Some(i.fps.round() as u32),
            _ => None,
        }
    }

    fn resize(&self) -> Resize {
        match self.size_mode {
            SizeMode::Original => Resize::Original,
            SizeMode::Percent => Resize::Percent(self.percent),
            SizeMode::Custom => Resize::Size { width: self.width, height: self.height },
        }
    }

    fn options(&self) -> ConvertOptions {
        ConvertOptions {
            video_bitrate: (self.quality == VideoQuality::Custom).then_some(self.custom_kbps.saturating_mul(1000)),
            quality: self.quality,
            audio_kbps: self.audio_kbps,
            encoder: crate::settings::get().recorder.encoder,
            resize: self.resize(),
            fps: (self.fps > 0).then_some(self.fps),
        }
    }

    fn extension(&self) -> &'static str {
        match (self.mode, self.audio_format) {
            (Mode::Mp4, _) => "mp4",
            (Mode::Audio, AudioFormat::Mp3) => "mp3",
            (Mode::Audio, AudioFormat::M4a) => "m4a",
        }
    }

    fn pick_input(&mut self) {
        let folder = Path::new(self.input.trim()).parent().map(Path::to_path_buf).filter(|p| p.is_dir());
        let folder = folder.unwrap_or_else(|| crate::settings::get().recorder.output_folder());
        let pattern = crate::video_editor::VIDEO_EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>();
        let items = [(t("convert-filter-video"), pattern.join(";")), (t("editor-filter-all"), "*.*".to_string())];
        if let Some(p) = dialogs::open_files_with(owner_hwnd(), Some(&folder), &items, false).into_iter().next() {
            self.input = p.display().to_string();
            self.input_changed();
        }
    }

    fn pick_output(&mut self) {
        let ext = self.extension();
        let current = PathBuf::from(self.output.trim());
        let folder = current.parent().map(Path::to_path_buf).filter(|p| p.is_dir());
        let name = current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let items = [(format!("{} (*.{ext})", ext.to_uppercase()), format!("*.{ext}"))];
        if let Some(p) = dialogs::save_file_with(owner_hwnd(), folder.as_deref(), &name, &items, ext) {
            self.output = p.display().to_string();
            self.output_custom = true;
        }
    }

    fn input_changed(&mut self) {
        let input = self.input.trim().to_string();
        self.probed_input = input.clone();
        self.error = None;
        self.outcome = None;
        self.derive_output();
        if input.is_empty() || !Path::new(&input).is_file() {
            self.info = None;
            self.probing = None;
            return;
        }
        let (tx, rx) = channel();
        let repaint = self.repaint();
        let spawned = std::thread::Builder::new().name("convert-probe".into()).spawn(move || {
            let r = (|| -> anyhow::Result<SourceInfo> {
                let _mf = crate::video_editor::mf::init()?;
                let i = crate::video_editor::mf::probe(Path::new(&input))?;
                Ok(SourceInfo {
                    width: i.width,
                    height: i.height,
                    fps: i.frame_rate.fps(),
                    duration_ms: i.duration_ms,
                    has_audio: i.has_audio,
                })
            })();
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
            repaint();
        });
        match spawned {
            Ok(_) => {
                self.info = None;
                self.probing = Some(rx);
            }
            Err(e) => log::warn!("convert: cannot start the probe: {e}"),
        }
    }

    /// Output next to the input, unless the user chose one.
    fn derive_output(&mut self) {
        let input = PathBuf::from(self.input.trim());
        if self.output_custom {
            // Keep the chosen folder and name, but follow the mode's extension.
            if !self.output.trim().is_empty() {
                self.output = PathBuf::from(self.output.trim()).with_extension(self.extension()).display().to_string();
            }
            return;
        }
        if self.input.trim().is_empty() {
            self.output.clear();
            return;
        }
        self.output = default_output(&input, self.extension()).display().to_string();
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.probing {
            match rx.try_recv() {
                Ok(r) => {
                    if let Ok(i) = &r {
                        let (w, h) = (i.width & !1, i.height & !1);
                        if w > 0 && h > 0 {
                            self.width = w;
                            self.height = h;
                        }
                    }
                    self.info = Some(r);
                    self.probing = None;
                }
                Err(TryRecvError::Disconnected) => self.probing = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let Some(j) = &self.job else { return };
        let r = match j.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                // Keep the elapsed time ticking.
                self.ctx.request_repaint_after_for(Duration::from_millis(500), viewport_id(Self::ID));
                return;
            }
            Err(TryRecvError::Disconnected) => Err(t("convert-worker-died")),
        };
        let (cancelled, output, took) = (j.cancel.load(Ordering::Acquire), j.output.clone(), j.started.elapsed());
        self.job = None;
        self.outcome = Some(match r {
            Ok(()) => Outcome::Done(output, took),
            Err(_) if cancelled => Outcome::Cancelled,
            Err(e) => Outcome::Failed(e),
        });
    }

    fn start(&mut self) {
        self.error = None;
        self.outcome = None;
        let input = PathBuf::from(self.input.trim());
        if !input.is_file() {
            self.error = Some(t("convert-no-input"));
            return;
        }
        if self.output.trim().is_empty() {
            self.derive_output();
        }
        let output = PathBuf::from(self.output.trim()).with_extension(self.extension());
        if same_file(&input, &output) {
            self.error = Some(t("convert-same-file"));
            return;
        }
        if let Some(dir) = output.parent().filter(|d| !d.as_os_str().is_empty()) {
            if let Err(e) = std::fs::create_dir_all(dir) {
                self.error = Some(tf("convert-failed", &[("error", &e.to_string())]));
                return;
            }
        }
        self.output = output.display().to_string();
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (mode, format, kbps, opts) = (self.mode, self.audio_format, self.audio_kbps, self.options());
        let (p, c, out) = (progress.clone(), cancel.clone(), output.clone());
        let repaint = self.repaint();
        let spawned = std::thread::Builder::new().name("convert".into()).spawn(move || {
            let mut last = -1.0f32;
            let mut cb = |f: f32| {
                p.store(f.to_bits(), Ordering::Release);
                if f - last >= 0.002 || f >= 1.0 {
                    last = f;
                    repaint();
                }
                !c.load(Ordering::Acquire)
            };
            let r = match mode {
                Mode::Mp4 => crate::recorder::convert_to_mp4(&input, &out, &opts, &mut cb),
                Mode::Audio => crate::recorder::extract_audio(&input, &out, format, kbps, &mut cb),
            };
            if let Err(e) = &r {
                log::warn!("convert: {} failed: {e:#}", input.display());
            }
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
            repaint();
        });
        match spawned {
            Ok(_) => self.job = Some(Job { progress, cancel, rx, output, started: Instant::now() }),
            Err(e) => self.error = Some(tf("convert-failed", &[("error", &e.to_string())])),
        }
    }
}

fn kbps_combo(ui: &mut egui::Ui, id: &str, v: &mut u32) {
    egui::ComboBox::from_id_salt(id).selected_text(format!("{v} kbit/s")).width(180.0).show_ui(ui, |ui| {
        for k in AUDIO_KBPS {
            ui.selectable_value(v, k, format!("{k} kbit/s"));
        }
    });
}

/// `side` scaled by `num / den`, rounded to an even number (at least 2).
fn scale_side(side: u32, num: u32, den: u32) -> u32 {
    if den == 0 {
        return side;
    }
    let v = (side as u64 * num as u64 + den as u64 / 2) / den as u64;
    ((v as u32).max(2) + 1) & !1
}

/// `<stem>.<ext>` next to the input, or `<stem>-converted.<ext>` when that is the input.
fn default_output(input: &Path, ext: &str) -> PathBuf {
    let plain = input.with_extension(ext);
    if !same_file(input, &plain) {
        return plain;
    }
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "video".into());
    input.with_file_name(format!("{stem}-converted.{ext}"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

// ---------------------------------------------------------------------- Schedule Recording

struct ScheduleDlg {
    start_date: String,
    start_time: String,
    end_date: String,
    end_time: String,
    target: ScheduleTarget,
    region: [i32; 4],
    window_title: String,
    windows: Vec<String>,
    sleep_after: bool,
    status: Option<String>,
}

impl ToolWin for ScheduleDlg {
    const ID: &'static str = "snapforge-schedule-recording";
    const SIZE: [f32; 2] = [520.0, 540.0];

    fn title() -> String {
        t("schedule-title")
    }

    fn new(_ctx: &egui::Context) -> Self {
        let current = crate::settings::get().recorder.schedule.clone();
        let now = schedule::key_of(&DateTime::now_local());
        let (start, end) = match (current.enabled, schedule::parse(&current.start), schedule::parse(&current.end)) {
            (true, Some(a), Some(b)) => (a, b),
            _ => {
                // Next round five minutes, for one hour.
                let a = schedule::add_minutes((now.0, now.1, now.2, now.3, now.4 - now.4 % 5), 10);
                (a, schedule::add_minutes(a, 60))
            }
        };
        let (start_date, start_time) = split_key(start);
        let (end_date, end_time) = split_key(end);
        let region = if current.enabled { current.region } else { primary_monitor() };
        Self {
            start_date,
            start_time,
            end_date,
            end_time,
            target: current.target,
            region,
            window_title: current.window_title.clone(),
            windows: window_titles(),
            sleep_after: current.sleep_after,
            status: None,
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) -> bool {
        let current = crate::settings::get().recorder.schedule.clone();
        section(ui, &t("schedule-current"));
        if current.enabled {
            ui.horizontal_wrapped(|ui| {
                ui.label(tf(
                    "schedule-current-text",
                    &[("start", &current.start), ("end", &current.end), ("target", &target_text(&current))],
                ));
                if ui.button(t("schedule-cancel")).clicked() {
                    schedule::cancel();
                    self.status = Some(t("schedule-cancelled"));
                }
            });
        } else {
            ui.weak(t("schedule-none"));
        }

        section(ui, &t("schedule-when"));
        egui::Grid::new("schedule-when").num_columns(3).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label(t("schedule-start"));
            date_time(ui, &mut self.start_date, &mut self.start_time);
            ui.end_row();
            ui.label(t("schedule-end"));
            date_time(ui, &mut self.end_date, &mut self.end_time);
            ui.end_row();
        });
        ui.horizontal(|ui| {
            ui.weak(t("schedule-duration"));
            for mins in [15u32, 30, 60, 120] {
                if ui.small_button(format_minutes(mins)).clicked() {
                    if let Some(a) = schedule::parse(&format!("{} {}", self.start_date, self.start_time)) {
                        let (date, time) = split_key(schedule::add_minutes(a, mins));
                        self.end_date = date;
                        self.end_time = time;
                    }
                }
            }
        });

        section(ui, &t("schedule-target"));
        ui.radio_value(&mut self.target, ScheduleTarget::FullScreen, t("schedule-target-full"));
        ui.radio_value(&mut self.target, ScheduleTarget::Region, t("schedule-target-region"));
        if self.target == ScheduleTarget::Region {
            ui.indent("schedule-region", |ui| {
                ui.horizontal(|ui| {
                    let [x, y, w, h] = &mut self.region;
                    ui.label("X");
                    ui.add(egui::DragValue::new(x));
                    ui.label("Y");
                    ui.add(egui::DragValue::new(y));
                    ui.label(t("schedule-width"));
                    ui.add(egui::DragValue::new(w).range(schedule::MIN_REGION..=16384));
                    ui.label(t("schedule-height"));
                    ui.add(egui::DragValue::new(h).range(schedule::MIN_REGION..=16384));
                });
                if ui.small_button(t("schedule-primary-monitor")).clicked() {
                    self.region = primary_monitor();
                }
            });
        }
        ui.radio_value(&mut self.target, ScheduleTarget::Window, t("schedule-target-window"));
        if self.target == ScheduleTarget::Window {
            ui.indent("schedule-window", |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.window_title)
                            .hint_text(t("schedule-window-hint"))
                            .desired_width(260.0),
                    );
                    egui::ComboBox::from_id_salt("schedule-windows").selected_text(t("schedule-pick-window")).show_ui(
                        ui,
                        |ui| {
                            for w in &self.windows {
                                if ui.selectable_label(*w == self.window_title, w).clicked() {
                                    self.window_title = w.clone();
                                }
                            }
                        },
                    );
                    if ui.small_button("⟳").on_hover_text(t("schedule-refresh-windows")).clicked() {
                        self.windows = window_titles();
                    }
                });
                ui.weak(t("schedule-window-note"));
            });
        }

        ui.add_space(6.0);
        ui.checkbox(&mut self.sleep_after, t("schedule-sleep-after"));
        ui.weak(t("schedule-settings-note"));

        let s = self.to_settings();
        let problem = schedule::validate(&s);
        ui.add_space(6.0);
        if let Some(key) = problem {
            error_label(ui, &t(key));
        } else if let Some(st) = &self.status {
            ui.label(st);
        }
        let (set, close) = buttons(ui, &t("schedule-set"), problem.is_none(), &t("media-close"));
        if set {
            schedule::set(s.clone());
            log::info!("recorder: scheduled {}..{}", s.start, s.end);
            self.status = Some(tf("schedule-done", &[("start", &s.start), ("end", &s.end)]));
        }
        !close
    }
}

impl ScheduleDlg {
    fn to_settings(&self) -> ScheduleSettings {
        let join = |d: &str, t: &str| format!("{} {}", d.trim(), t.trim());
        ScheduleSettings {
            enabled: true,
            start: normalize(&join(&self.start_date, &self.start_time)),
            end: normalize(&join(&self.end_date, &self.end_time)),
            target: self.target,
            region: self.region,
            window_title: self.window_title.trim().to_string(),
            sleep_after: self.sleep_after,
        }
    }
}

/// Re-format a parsable time as `YYYY-MM-DD HH:MM` (zero-padded); anything else unchanged.
fn normalize(s: &str) -> String {
    schedule::parse(s).map(schedule::format_key).unwrap_or_else(|| s.to_string())
}

fn split_key(k: (u16, u8, u8, u8, u8)) -> (String, String) {
    (format!("{:04}-{:02}-{:02}", k.0, k.1, k.2), format!("{:02}:{:02}", k.3, k.4))
}

fn date_time(ui: &mut egui::Ui, date: &mut String, time: &mut String) {
    ui.add(egui::TextEdit::singleline(date).hint_text("YYYY-MM-DD").desired_width(110.0));
    ui.add(egui::TextEdit::singleline(time).hint_text("HH:MM").desired_width(60.0));
}

fn format_minutes(m: u32) -> String {
    if m >= 60 && m % 60 == 0 {
        tf("schedule-hours", &[("n", &(m / 60).to_string())])
    } else {
        tf("schedule-minutes", &[("n", &m.to_string())])
    }
}

fn target_text(s: &ScheduleSettings) -> String {
    match s.target {
        ScheduleTarget::FullScreen => t("schedule-target-full"),
        ScheduleTarget::Region => {
            let [x, y, w, h] = s.region;
            let n = |v: i32| v.to_string();
            tf("schedule-region-text", &[("x", &n(x)), ("y", &n(y)), ("width", &n(w)), ("height", &n(h))])
        }
        ScheduleTarget::Window => tf("schedule-window-text", &[("title", &s.window_title)]),
    }
}

fn primary_monitor() -> [i32; 4] {
    let r = win::monitors().into_iter().find(|m| m.primary).map(|m| m.rect).unwrap_or_else(win::virtual_screen);
    [r.x, r.y, r.w, r.h]
}

/// Titles of the visible top-level windows (ours excluded), without duplicates.
fn window_titles() -> Vec<String> {
    let pid = std::process::id();
    let mut out: Vec<String> = Vec::new();
    for w in crate::capture::snapshot_windows(&[]) {
        let title = w.title.trim();
        let shell = matches!(w.class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW");
        if title.is_empty() || shell || window_pid(w.hwnd) == pid || out.iter().any(|t| t == title) {
            continue;
        }
        out.push(title.to_string());
    }
    out
}

fn window_pid(hwnd: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain query; a stale handle just yields 0.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(win::hwnd_from_isize(hwnd), Some(&mut pid));
    }
    pid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names() {
        assert_eq!(default_output(Path::new(r"C:\v\a.wmv"), "mp4"), PathBuf::from(r"C:\v\a.mp4"));
        assert_eq!(default_output(Path::new(r"C:\v\a.MP4"), "mp4"), PathBuf::from(r"C:\v\a-converted.mp4"));
        assert_eq!(default_output(Path::new(r"C:\v\a.mp4"), "mp3"), PathBuf::from(r"C:\v\a.mp3"));
    }

    #[test]
    fn aspect_sides_are_even() {
        assert_eq!(scale_side(1280, 1080, 1920), 720);
        assert_eq!(scale_side(1000, 1080, 1920), 564);
        assert_eq!(scale_side(1, 1, 1), 2);
        assert_eq!(scale_side(640, 1, 0), 640);
    }

    #[test]
    fn schedule_fields_round_trip() {
        assert_eq!(split_key((2026, 1, 2, 3, 4)), ("2026-01-02".to_string(), "03:04".to_string()));
        assert_eq!(normalize("2026-1-2 3:04"), "2026-01-02 03:04");
        assert_eq!(normalize("nonsense"), "nonsense");
    }
}
