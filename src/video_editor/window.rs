//! The video editor window: session state, loading, undo, playback and actions. The panels are
//! drawn by `ui_panels.rs`, `ui_preview.rs` and `ui_timeline.rs` (all `impl Session`).

use super::analysis::{self, Analysis, Pcm};
use super::audio_out::AudioPlayer;
use super::decoder::Decoder;
use super::dialogs::{self, Filter};
use super::export::{self, ExportJob, ExportRequest, Format, Outcome};
use super::gif_export::GifOptions;
use super::mf::{self, MediaInfo};
use super::model::{Anchor, EditState, TitleCard, ZoomKey};
use super::project::{self, ProjectFile, SourceInfo};
use super::timeline::{self, Pos, Timeline};
use super::zoom;
use crate::common::Image;
use crate::draw::{ObjectId, Tool};
use crate::i18n::{t, tf};
use eframe::egui::{self, Key, TextureHandle, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

pub(super) const VIEWPORT_ID: &str = "snapforge-video-editor";
const DEFAULT_SIZE: [f32; 2] = [1280.0, 820.0];
const MIN_SIZE: [f32; 2] = [900.0, 600.0];

pub(super) fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of(VIEWPORT_ID)
}

/// Repaint the editor viewport from any thread.
pub(super) fn waker(ctx: &egui::Context) -> impl Fn() + Send + Clone + 'static {
    let c = ctx.clone();
    move || c.request_repaint_of(viewport_id())
}

/// Owner window for modal dialogs: the active window of the UI thread (ours, while a button in
/// it is being handled).
pub(super) fn owner_hwnd() -> isize {
    // SAFETY: trivial query for the calling thread.
    let h = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() };
    crate::common::win::hwnd_to_isize(h)
}

pub(super) struct Source {
    pub path: PathBuf,
    pub info: MediaInfo,
}

struct Loaded {
    video: PathBuf,
    info: MediaInfo,
    project: Option<(PathBuf, ProjectFile)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Selection {
    None,
    Annotation(ObjectId),
    Title(u64),
    Zoom(usize),
}

pub(super) struct PlayClock {
    pub start: Instant,
    pub from: u64,
}

impl PlayClock {
    pub fn now(&self) -> u64 {
        self.from + self.start.elapsed().as_millis() as u64
    }
}

pub(super) struct Status {
    pub text: String,
    pub error: bool,
    pub at: Instant,
    pub reveal: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportKind {
    Mp4,
    Gif,
}

pub(super) struct ExportDialog {
    pub kind: ExportKind,
    pub width: u32,
    pub fps: f32,
    /// MP4: 0 = source frame rate.
    pub mbps: f32,
    pub dither: bool,
    pub looping: bool,
}

pub(super) struct RunningExport {
    pub job: ExportJob,
    pub kind: ExportKind,
    pub started: Instant,
}

pub(super) enum StillAction {
    Save(PathBuf),
    OpenInEditor,
}

/// Drag in progress on the preview.
pub(super) enum CanvasDrag {
    Create { start: crate::draw::Pt, cur: crate::draw::Pt },
    Move { id: ObjectId, snapshot: crate::draw::Object, start: crate::draw::Pt },
    Handle { id: ObjectId, handle: crate::draw::HandleId, snapshot: crate::draw::Object, start: crate::draw::Pt },
    ZoomPan { index: usize, snapshot: ZoomKey, start: egui::Pos2 },
}

/// Drag in progress on the timeline.
#[derive(Clone)]
pub(super) enum TlDrag {
    Scrub,
    TrimStart { orig: Vec<super::model::Segment>, x0: f32 },
    TrimEnd { orig: Vec<super::model::Segment>, x0: f32 },
    MoveAnn { id: ObjectId, orig: crate::draw::Timing, x0: f32 },
    ResizeAnnStart { id: ObjectId, orig: crate::draw::Timing, x0: f32 },
    ResizeAnnEnd { id: ObjectId, orig: crate::draw::Timing, x0: f32 },
    MoveZoom { index: usize, orig: u64, x0: f32 },
}

pub(super) struct Session {
    pub source: Option<Source>,
    loading: Option<Receiver<Result<Loaded, String>>>,
    pub loading_name: Option<String>,
    pub project_path: Option<PathBuf>,
    pub edit: EditState,
    pub saved: EditState,
    pub undo: Vec<EditState>,
    pub redo: Vec<EditState>,
    pending: Option<EditState>,
    frame_start: EditState,
    pub rev: u64,
    pub tl: Timeline,

    pub decoder: Option<Decoder>,
    analysis: Option<Analysis>,
    pub thumbs: Vec<(u64, TextureHandle)>,
    pub thumbs_done: bool,
    pub peaks: Option<Vec<u8>>,
    pub pcm: Option<Arc<Pcm>>,
    pub audio_progress: Option<f32>,
    pub no_audio: bool,

    // Preview caches.
    pub frame_tex: Option<TextureHandle>,
    pub frame_key: Option<(u64, u64)>,
    pub frame_bgra: Option<(u64, Arc<Image>)>,
    pub overlay_tex: Option<TextureHandle>,
    pub overlay_key: Option<super::ui_preview::OverlayKey>,
    pub title_tex: Option<(TitleCard, [u32; 2], TextureHandle)>,

    // Playback.
    pub playhead: u64,
    pub play: Option<PlayClock>,
    audio: Option<AudioPlayer>,
    pub mark_in: Option<u64>,
    pub mark_out: Option<u64>,

    // Timeline view.
    pub px_per_sec: f32,
    pub tl_scroll: f32,
    pub tl_scroll_to: Option<f32>,
    pub tl_drag: Option<TlDrag>,
    pub tl_fit: bool,

    // Annotation editing.
    pub tool: Tool,
    pub selection: Selection,
    pub canvas_drag: Option<CanvasDrag>,
    pub zoom_full_frame: bool,

    // Jobs and dialogs.
    pub export: Option<RunningExport>,
    pub export_dialog: Option<ExportDialog>,
    still: Option<Receiver<Result<String, String>>>,
    pub status: Option<Status>,
    pub confirm_close: bool,
    pub close: bool,
    pub builder: ViewportBuilder,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Session {
    pub fn new() -> Self {
        let builder = ViewportBuilder::default()
            .with_title(t("ve-window-title"))
            .with_inner_size(DEFAULT_SIZE)
            .with_min_inner_size(MIN_SIZE)
            .with_drag_and_drop(true);
        Self {
            source: None,
            loading: None,
            loading_name: None,
            project_path: None,
            edit: EditState::default(),
            saved: EditState::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            frame_start: EditState::default(),
            rev: 0,
            tl: Timeline::default(),
            decoder: None,
            analysis: None,
            thumbs: Vec::new(),
            thumbs_done: false,
            peaks: None,
            pcm: None,
            audio_progress: None,
            no_audio: false,
            frame_tex: None,
            frame_key: None,
            frame_bgra: None,
            overlay_tex: None,
            overlay_key: None,
            title_tex: None,
            playhead: 0,
            play: None,
            audio: None,
            mark_in: None,
            mark_out: None,
            px_per_sec: 80.0,
            tl_scroll: 0.0,
            tl_scroll_to: None,
            tl_drag: None,
            tl_fit: false,
            tool: Tool::Select,
            selection: Selection::None,
            canvas_drag: None,
            zoom_full_frame: true,
            export: None,
            export_dialog: None,
            still: None,
            status: None,
            confirm_close: false,
            close: false,
            builder,
            wake: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.source.is_some() && (self.edit != self.saved || self.pending.is_some())
    }

    pub fn duration(&self) -> u64 {
        self.tl.duration_ms
    }

    pub fn source_duration(&self) -> u64 {
        self.source.as_ref().map(|s| s.info.duration_ms).unwrap_or(0)
    }

    pub fn frame_ms(&self) -> u64 {
        self.source.as_ref().map(|s| (1000.0 / s.info.frame_rate.fps()).round().max(1.0) as u64).unwrap_or(33)
    }

    pub fn set_status(&mut self, text: String, error: bool) {
        if error {
            log::warn!("video editor: {text}");
        }
        self.status = Some(Status { text, error, at: Instant::now(), reveal: None });
    }

    fn wake_fn(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.wake.clone().unwrap_or_else(|| Arc::new(|| {}))
    }

    // -----------------------------------------------------------------------------------------
    // Opening
    // -----------------------------------------------------------------------------------------

    /// Open a video or `.sfvid` project (probing runs on a worker thread).
    pub fn open_path(&mut self, path: PathBuf) {
        self.stop_playback();
        let (tx, rx) = channel();
        let wake = self.wake_fn();
        self.loading_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let r = std::thread::Builder::new().name("video-open".into()).spawn(move || {
            let res = (|| -> anyhow::Result<Loaded> {
                let _mf = mf::init()?;
                if dialogs::is_project(&path) {
                    let p = project::load(&path)?;
                    let video = p.resolve_source(&path);
                    let info = mf::probe(&video)?;
                    Ok(Loaded { video, info, project: Some((path, p)) })
                } else {
                    let info = mf::probe(&path)?;
                    Ok(Loaded { video: path, info, project: None })
                }
            })();
            let _ = tx.send(res.map_err(|e| format!("{e:#}")));
            wake();
        });
        match r {
            Ok(_) => self.loading = Some(rx),
            Err(e) => self.set_status(format!("{e}"), true),
        }
    }

    fn finish_loading(&mut self, ctx: &egui::Context, l: Loaded) {
        if l.info.duration_ms == 0 {
            self.set_status(t("ve-error-no-duration"), true);
            return;
        }
        let (edit, project_path) = match l.project {
            Some((pp, p)) => {
                let mut e = p.edit;
                if e.segments.is_empty() {
                    e.segments = EditState::new(l.info.duration_ms).segments;
                }
                (e, Some(pp))
            }
            None => (EditState::new(l.info.duration_ms), None),
        };
        let wake = waker(ctx);
        self.decoder = Some(Decoder::start(l.video.clone(), edit.segments.clone(), wake.clone()));
        self.analysis = Some(Analysis::start(l.video.clone(), l.info.duration_ms, wake));
        ctx.send_viewport_cmd_to(
            viewport_id(),
            ViewportCommand::Title(tf(
                "ve-window-title-file",
                &[("name", &l.video.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())],
            )),
        );
        self.source = Some(Source { path: l.video, info: l.info });
        self.project_path = project_path;
        self.saved = edit.clone();
        self.edit = edit;
        self.undo.clear();
        self.redo.clear();
        self.pending = None;
        self.thumbs.clear();
        self.thumbs_done = false;
        self.peaks = None;
        self.pcm = None;
        self.audio_progress = Some(0.0);
        self.no_audio = false;
        self.frame_tex = None;
        self.frame_key = None;
        self.frame_bgra = None;
        self.overlay_key = None;
        self.title_tex = None;
        self.playhead = 0;
        self.mark_in = None;
        self.mark_out = None;
        self.selection = Selection::None;
        self.tool = Tool::Select;
        self.tl_scroll_to = Some(0.0);
        self.tl_fit = true;
        self.changed();
    }

    fn poll_workers(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.loading {
            if let Ok(res) = rx.try_recv() {
                self.loading = None;
                self.loading_name = None;
                match res {
                    Ok(l) => self.finish_loading(ctx, l),
                    Err(e) => self.set_status(tf("ve-error-open", &[("error", &e)]), true),
                }
            }
        }
        let msgs: Vec<analysis::Msg> = self.analysis.as_ref().map(|a| a.rx.try_iter().collect()).unwrap_or_default();
        for m in msgs {
            match m {
                analysis::Msg::Thumb { time_ms, image } => {
                    let tex = ctx.load_texture(format!("ve-thumb-{time_ms}"), image, egui::TextureOptions::LINEAR);
                    self.thumbs.push((time_ms, tex));
                    self.thumbs.sort_by_key(|(t, _)| *t);
                }
                analysis::Msg::ThumbsDone => self.thumbs_done = true,
                analysis::Msg::Waveform { peaks, pcm } => {
                    self.peaks = Some(peaks);
                    self.pcm = pcm;
                    self.audio_progress = None;
                }
                analysis::Msg::NoAudio => {
                    self.no_audio = true;
                    self.audio_progress = None;
                }
                analysis::Msg::AudioProgress(p) => self.audio_progress = Some(p),
                analysis::Msg::Error(e) => {
                    log::warn!("video editor: analysis: {e}");
                    self.audio_progress = None;
                }
            }
        }
        if let Some(e) = self.decoder.as_ref().and_then(|d| d.error()) {
            if self.status.as_ref().map_or(true, |s| s.text != e) && self.frame_tex.is_none() {
                self.set_status(e, true);
            }
        }
        if let Some(job) = &self.export {
            if let Some(res) = job.job.poll() {
                let kind = job.kind;
                let secs = job.started.elapsed().as_secs_f32();
                self.export = None;
                match res {
                    Ok(Outcome::Done(p)) => {
                        let key = if kind == ExportKind::Gif { "ve-exported-gif" } else { "ve-exported-mp4" };
                        let mut s = Status {
                            text: tf(key, &[("path", &p.display().to_string()), ("secs", &format!("{secs:.1}"))]),
                            error: false,
                            at: Instant::now(),
                            reveal: None,
                        };
                        s.reveal = Some(p);
                        self.status = Some(s);
                    }
                    Ok(Outcome::Cancelled) => self.set_status(t("ve-export-cancelled"), false),
                    Err(e) => self.set_status(tf("ve-error-export", &[("error", &e)]), true),
                }
            }
        }
        if let Some(rx) = &self.still {
            if let Ok(res) = rx.try_recv() {
                self.still = None;
                match res {
                    Ok(msg) => self.set_status(msg, false),
                    Err(e) => self.set_status(tf("ve-error-frame", &[("error", &e)]), true),
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Edits and undo
    // -----------------------------------------------------------------------------------------

    /// Rebuild derived state after `edit` changed.
    pub fn changed(&mut self) {
        timeline::normalize(&mut self.edit.segments);
        self.rev += 1;
        self.tl = timeline::build(&self.edit.segments, &self.edit.titles);
        if let Some(d) = &self.decoder {
            d.set_segments(&self.edit.segments);
        }
        self.playhead = self.playhead.min(self.tl.duration_ms.saturating_sub(1));
        if let Selection::Zoom(i) = self.selection {
            if i >= self.edit.zoom.len() {
                self.selection = Selection::None;
            }
        }
    }

    /// Record that `edit` was modified this frame (coalesced into one undo step until the
    /// pointer is released and no text field has focus).
    pub fn touch(&mut self) {
        if self.pending.is_none() {
            self.pending = Some(self.frame_start.clone());
        }
        self.changed();
    }

    /// Close the pending undo step now (for one-shot actions).
    pub fn commit(&mut self) {
        self.touch();
        self.flush_undo();
    }

    fn flush_undo(&mut self) {
        if let Some(p) = self.pending.take() {
            if p != self.edit {
                self.undo.push(p);
                self.redo.clear();
            }
        }
        self.frame_start = self.edit.clone();
    }

    pub fn undo(&mut self) {
        self.flush_undo();
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.edit, prev));
            self.after_history();
        }
    }

    pub fn redo(&mut self) {
        self.flush_undo();
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.edit, next));
            self.after_history();
        }
    }

    fn after_history(&mut self) {
        self.changed();
        self.frame_start = self.edit.clone();
        let valid = match self.selection {
            Selection::Annotation(id) => self.edit.annotations.iter().any(|o| o.id == id),
            Selection::Title(id) => self.edit.titles.iter().any(|c| c.id == id),
            Selection::Zoom(i) => i < self.edit.zoom.len(),
            Selection::None => true,
        };
        if !valid {
            self.selection = Selection::None;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Playback
    // -----------------------------------------------------------------------------------------

    pub fn is_playing(&self) -> bool {
        self.play.is_some()
    }

    pub fn toggle_play(&mut self) {
        if self.play.is_some() {
            self.stop_playback();
            return;
        }
        if self.source.is_none() || self.tl.duration_ms == 0 {
            return;
        }
        if self.playhead + 50 >= self.tl.duration_ms {
            self.playhead = 0;
        }
        let start = Instant::now();
        self.play = Some(PlayClock { start, from: self.playhead });
        self.audio = self.pcm.clone().map(|pcm| AudioPlayer::start(pcm, self.tl.clone(), self.playhead, start));
    }

    pub fn stop_playback(&mut self) {
        if let Some(p) = self.play.take() {
            self.playhead = p.now().min(self.tl.duration_ms.saturating_sub(1));
        }
        self.audio = None;
    }

    /// Move the playhead (stops playback).
    pub fn seek(&mut self, out_ms: u64) {
        let was = self.is_playing();
        self.stop_playback();
        self.playhead = out_ms.min(self.tl.duration_ms.saturating_sub(1));
        if was {
            self.toggle_play();
        }
    }

    pub fn step(&mut self, delta_ms: i64) {
        self.stop_playback();
        let p = (self.playhead as i64 + delta_ms).clamp(0, self.tl.duration_ms.saturating_sub(1) as i64);
        self.playhead = p as u64;
    }

    /// The source time under the playhead, if it is on video.
    pub fn playhead_src(&self) -> Option<u64> {
        match self.tl.locate(self.playhead) {
            Pos::Clip { src_ms, .. } => Some(src_ms),
            _ => None,
        }
    }

    fn tick_playback(&mut self, ctx: &egui::Context) {
        if let Some(p) = &self.play {
            let now = p.now();
            if now >= self.tl.duration_ms {
                self.play = None;
                self.audio = None;
                self.playhead = self.tl.duration_ms.saturating_sub(1);
            } else {
                self.playhead = now;
                ctx.request_repaint();
            }
        }
        if let (Some(d), Some(src)) = (&self.decoder, self.playhead_src()) {
            d.set_target(src, self.play.is_some());
        }
    }

    // -----------------------------------------------------------------------------------------
    // Edit actions
    // -----------------------------------------------------------------------------------------

    pub fn mark_in(&mut self) {
        self.mark_in = Some(self.playhead);
        if self.mark_out.is_some_and(|o| o <= self.playhead) {
            self.mark_out = None;
        }
    }

    pub fn mark_out(&mut self) {
        self.mark_out = Some(self.playhead);
        if self.mark_in.is_some_and(|i| i >= self.playhead) {
            self.mark_in = None;
        }
    }

    /// The marked range, if both ends are set (output ms).
    pub fn marked(&self) -> Option<(u64, u64)> {
        match (self.mark_in, self.mark_out) {
            (Some(a), Some(b)) if b > a => Some((a, b)),
            _ => None,
        }
    }

    pub fn cut_marked(&mut self) {
        let Some((a, b)) = self.marked() else {
            self.set_status(t("ve-cut-need-marks"), false);
            return;
        };
        self.stop_playback();
        if timeline::cut(&mut self.edit.segments, &mut self.edit.titles, a, b) {
            self.commit();
            self.mark_in = None;
            self.mark_out = None;
            self.playhead = a.min(self.tl.duration_ms.saturating_sub(1));
            if let Selection::Title(id) = self.selection {
                if !self.edit.titles.iter().any(|c| c.id == id) {
                    self.selection = Selection::None;
                }
            }
        } else {
            self.set_status(t("ve-cut-nothing"), false);
        }
    }

    pub fn restore_segments(&mut self) {
        self.edit.segments = EditState::new(self.source_duration()).segments;
        self.commit();
    }

    pub fn delete_selection(&mut self) {
        match self.selection {
            Selection::Annotation(id) => self.edit.annotations.retain(|o| o.id != id),
            Selection::Title(id) => self.edit.titles.retain(|c| c.id != id),
            Selection::Zoom(i) if i < self.edit.zoom.len() => {
                self.edit.zoom.remove(i);
            }
            _ => return,
        }
        self.selection = Selection::None;
        self.commit();
    }

    pub fn add_title(&mut self, at: Anchor) {
        if self.source.is_none() {
            return;
        }
        let anchor = match at {
            Anchor::Source(_) => match self.tl.locate(self.playhead) {
                Pos::Clip { src_ms, .. } => Anchor::Source(src_ms),
                Pos::Title { index, .. } => self.edit.titles.get(index).map(|c| c.anchor).unwrap_or(Anchor::Start),
                Pos::End => Anchor::End,
            },
            a => a,
        };
        let id = self.edit.next_title_id();
        self.edit.titles.push(TitleCard { id, anchor, title: t("ve-title-default"), ..TitleCard::default() });
        self.commit();
        self.selection = Selection::Title(id);
        if let Some(start) = self.tl.title_start(self.edit.titles.len() - 1) {
            self.seek(start);
        }
    }

    pub fn add_zoom(&mut self, scale: f32) {
        let Some(src) = self.playhead_src() else {
            self.set_status(t("ve-zoom-need-video"), false);
            return;
        };
        let cur = zoom::view_at(&self.edit.zoom, src);
        let (cx, cy) = if scale > 1.0 && cur.is_identity() { (0.5, 0.5) } else { (cur.cx, cur.cy) };
        let key = ZoomKey { time_ms: src, cx, cy, scale, ..ZoomKey::default() };
        self.edit.zoom.retain(|k| k.time_ms != src);
        self.edit.zoom.push(key);
        self.edit.sort_zoom();
        self.commit();
        self.selection =
            self.edit.zoom.iter().position(|k| k.time_ms == src).map(Selection::Zoom).unwrap_or(Selection::None);
    }

    // -----------------------------------------------------------------------------------------
    // Files
    // -----------------------------------------------------------------------------------------

    pub fn open_dialog(&mut self) {
        let folder = self.source.as_ref().and_then(|s| s.path.parent().map(Path::to_path_buf));
        let filters = [
            Filter::new(t("ve-filter-media"), &["mp4", "m4v", "wmv", "mov", "avi", "mkv", project::EXTENSION]),
            Filter::new(t("ve-filter-video"), &dialogs::VIDEO_EXTENSIONS),
            Filter::new(t("ve-filter-project"), &[project::EXTENSION]),
        ];
        if let Some(p) = dialogs::open_file(owner_hwnd(), folder.as_deref(), &filters) {
            self.open_path(p);
        }
    }

    fn stem(&self) -> String {
        self.source
            .as_ref()
            .and_then(|s| s.path.file_stem().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "video".into())
    }

    fn folder(&self) -> Option<PathBuf> {
        self.source.as_ref().and_then(|s| s.path.parent().map(Path::to_path_buf))
    }

    /// Save the project (asks for a path the first time or with `save_as`).
    pub fn save_project(&mut self, save_as: bool) -> bool {
        let Some((src_path, src_info)) = self.source.as_ref().map(|s| (s.path.clone(), s.info)) else { return false };
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let filter = Filter::new(t("ve-filter-project"), &[project::EXTENSION]);
                let name = format!("{}.{}", self.stem(), project::EXTENSION);
                match dialogs::save_file(owner_hwnd(), self.folder().as_deref(), &name, &filter, project::EXTENSION) {
                    Some(p) => p,
                    None => return false,
                }
            }
        };
        self.flush_undo();
        let info = SourceInfo {
            duration_ms: src_info.duration_ms,
            width: src_info.width,
            height: src_info.height,
            fps: src_info.frame_rate.fps(),
        };
        let file = ProjectFile::new(src_path, info, self.edit.clone());
        match project::save(&path, &file) {
            Ok(()) => {
                self.saved = self.edit.clone();
                self.set_status(tf("ve-project-saved", &[("path", &path.display().to_string())]), false);
                self.project_path = Some(path);
                true
            }
            Err(e) => {
                self.set_status(tf("ve-error-save", &[("error", &format!("{e:#}"))]), true);
                false
            }
        }
    }

    pub fn open_export_dialog(&mut self, kind: ExportKind) {
        let Some(src) = &self.source else { return };
        let w = src.info.width;
        self.export_dialog = Some(ExportDialog {
            kind,
            width: if kind == ExportKind::Gif { w.min(640) } else { w },
            fps: if kind == ExportKind::Gif { 12.0 } else { 0.0 },
            mbps: (mf::WriterConfig::default_bitrate(w, src.info.height, src.info.frame_rate.fps()) as f32 / 1e6)
                .round(),
            dither: true,
            looping: true,
        });
    }

    /// Ask for the output path and start the export described by the dialog.
    pub fn start_export(&mut self) {
        let (Some(d), Some(src)) = (&self.export_dialog, &self.source) else { return };
        let (filter, ext) = match d.kind {
            ExportKind::Mp4 => (Filter::new(t("ve-filter-mp4"), &["mp4"]), "mp4"),
            ExportKind::Gif => (Filter::new(t("ve-filter-gif"), &["gif"]), "gif"),
        };
        let name = format!("{} edited.{ext}", self.stem());
        let Some(path) = dialogs::save_file(owner_hwnd(), self.folder().as_deref(), &name, &filter, ext) else {
            return;
        };
        if path == src.path {
            self.set_status(t("ve-error-overwrite-source"), true);
            return;
        }
        let format = match d.kind {
            ExportKind::Mp4 => Format::Mp4 { width: d.width, fps: d.fps as f64, bitrate: (d.mbps * 1e6) as u32 },
            ExportKind::Gif => {
                Format::Gif { width: d.width, opts: GifOptions { fps: d.fps, dither: d.dither, looping: d.looping } }
            }
        };
        let (kind, source) = (d.kind, src.path.clone());
        self.flush_undo();
        let req = ExportRequest { source, edit: self.edit.clone(), format, path, hardware: true };
        let wake = self.wake_fn();
        let job = export::spawn(req, move || wake());
        self.export = Some(RunningExport { job, kind, started: Instant::now() });
        self.export_dialog = None;
        self.stop_playback();
    }

    /// Render the frame under the playhead on a worker thread and save it or send it to the
    /// image editor.
    pub fn save_frame(&mut self, open_in_editor: bool) {
        let Some(src) = &self.source else { return };
        if self.still.is_some() {
            return;
        }
        let action = if open_in_editor {
            StillAction::OpenInEditor
        } else {
            let filter = Filter::new("PNG", &["png"]);
            let name = format!("{} {}.png", self.stem(), timeline::format_ms(self.playhead).replace([':', '.'], "-"));
            match dialogs::save_file(owner_hwnd(), self.folder().as_deref(), &name, &filter, "png") {
                Some(p) => StillAction::Save(p),
                None => return,
            }
        };
        let (source, edit, at, stem) = (src.path.clone(), self.edit.clone(), self.playhead, self.stem());
        let (tx, rx) = channel();
        let wake = self.wake_fn();
        let r = std::thread::Builder::new().name("video-still".into()).spawn(move || {
            let res =
                export::render_still(&source, &edit, at).map_err(|e| format!("{e:#}")).and_then(|img| match action {
                    StillAction::Save(p) => crate::output::save::save_image(&img, &p)
                        .map(|_| tf("ve-frame-saved", &[("path", &p.display().to_string())]))
                        .map_err(|e| format!("{e:#}")),
                    StillAction::OpenInEditor => {
                        let title = format!("{stem} {}", timeline::format_ms(at));
                        crate::editor::open_image(img, title, None, None);
                        Ok(t("ve-frame-sent"))
                    }
                });
            let _ = tx.send(res);
            wake();
        });
        match r {
            Ok(_) => self.still = Some(rx),
            Err(e) => self.set_status(format!("{e}"), true),
        }
    }

    pub fn still_busy(&self) -> bool {
        self.still.is_some()
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    // -----------------------------------------------------------------------------------------
    // Frame
    // -----------------------------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        // Consumed so a focused button or slider doesn't also act on them.
        let none = egui::Modifiers::NONE;
        let cmd = egui::Modifiers::COMMAND;
        let (space, left, right, shift, i, o, del, esc, home, end, redo, undo, save, open) = ctx.input_mut(|inp| {
            let redo = inp.consume_key(cmd | egui::Modifiers::SHIFT, Key::Z) || inp.consume_key(cmd, Key::Y);
            (
                inp.consume_key(none, Key::Space),
                inp.consume_key(none, Key::ArrowLeft),
                inp.consume_key(none, Key::ArrowRight),
                inp.modifiers.shift,
                inp.consume_key(none, Key::I),
                inp.consume_key(none, Key::O),
                inp.consume_key(none, Key::Delete),
                inp.consume_key(none, Key::Escape),
                inp.consume_key(none, Key::Home),
                inp.consume_key(none, Key::End),
                redo,
                inp.consume_key(cmd, Key::Z),
                inp.consume_key(cmd, Key::S),
                inp.consume_key(cmd, Key::O),
            )
        });
        if open {
            self.open_dialog();
        }
        if self.source.is_none() {
            return;
        }
        if space {
            self.toggle_play();
        }
        let step = if shift { 1000 } else { self.frame_ms() as i64 };
        if left {
            self.step(-step);
        }
        if right {
            self.step(step);
        }
        if home {
            self.seek(0);
        }
        if end {
            self.seek(self.tl.duration_ms);
        }
        if i {
            self.mark_in();
        }
        if o {
            self.mark_out();
        }
        if del {
            if self.marked().is_some() {
                self.cut_marked();
            } else {
                self.delete_selection();
            }
        }
        if esc {
            if self.mark_in.is_some() || self.mark_out.is_some() {
                self.mark_in = None;
                self.mark_out = None;
            } else {
                self.selection = Selection::None;
                self.tool = Tool::Select;
            }
        }
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
        if save {
            self.save_project(false);
        }
    }

    pub fn ensure_wake(&mut self, ctx: &egui::Context) {
        if self.wake.is_none() {
            let w = waker(ctx);
            self.wake = Some(Arc::new(w));
        }
    }

    pub fn frame(&mut self, ctx: &egui::Context) {
        self.ensure_wake(ctx);
        if self.pending.is_none() {
            self.frame_start = self.edit.clone();
        }
        self.poll_workers(ctx);
        // Files dropped on the window.
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            if dialogs::is_video(&p) || dialogs::is_project(&p) {
                self.open_path(p);
            }
        }
        self.tick_playback(ctx);
        self.shortcuts(ctx);
        self.panels(ctx);
        // Close the coalesced undo step once the gesture is over.
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input();
        if self.pending.is_some() && !busy {
            self.flush_undo();
        }
        if self.loading.is_some() || self.export.is_some() || self.still.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(s) = &self.status {
            let left = 6.0 - s.at.elapsed().as_secs_f32();
            if left <= 0.0 && s.reveal.is_none() {
                self.status = None;
            } else if left > 0.0 {
                ctx.request_repaint_after(std::time::Duration::from_secs_f32(left.max(0.05)));
            }
        }
    }

    /// Close requested (title bar or menu). Asks when there are unsaved changes.
    pub fn request_close(&mut self) {
        if self.is_dirty() {
            self.confirm_close = true;
        } else {
            self.close = true;
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_playback();
        if let Some(e) = &self.export {
            e.job.progress.cancel();
        }
        self.decoder = None;
        self.analysis = None;
    }
}

// ---------------------------------------------------------------------------------------------
// Hosting
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct Inner {
    pub session: Option<Session>,
    pub raise: bool,
    pub pending_open: Option<PathBuf>,
}

impl Inner {
    pub fn frame(&mut self, ctx: &egui::Context, class: ViewportClass) {
        let Some(s) = self.session.as_mut() else {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            return;
        };
        s.ensure_wake(ctx);
        if let Some(p) = self.pending_open.take() {
            s.open_path(p);
        }
        if class == ViewportClass::Embedded {
            let mut open = true;
            egui::Window::new(t("ve-window-title"))
                .id(egui::Id::new(VIEWPORT_ID))
                .open(&mut open)
                .default_size(DEFAULT_SIZE)
                .show(ctx, |ui| {
                    ui.label(t("ve-needs-viewports"));
                });
            if !open {
                s.close = true;
            }
        } else {
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                s.request_close();
            }
            s.frame(ctx);
        }
        if s.close {
            s.shutdown();
            self.session = None;
            if class != ViewportClass::Embedded {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            }
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }
}
