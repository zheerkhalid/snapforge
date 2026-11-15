//! The eframe `App`: window lifetime (hide instead of close), message and job handling,
//! shortcut dispatch through the command registry, and the actions behind commands.
//! Panels live in `menus.rs`, dialogs in `modals.rs`.

use super::canvas::{self, CanvasCtx, CanvasState};
use super::commands::{Action, CmdState, CommandRegistry, ToolId, ZoomCmd};
use super::document::{push_recent, DocId, Document, SaveTarget, Tabs};
use super::effects::{EffectRegistry, Scope};
use super::ext::{CanvasTool, ToolsContext};
use super::fileops::{self, FileWatcher};
use super::history::{Budget, MB};
use super::jobs::{EffectTarget, JobQueue, JobResult, Jobs};
use super::modals::Modal;
use super::palette::{self, PaletteOutcome, PaletteState};
use super::param_dialog::{DialogOutcome, EffectDialog};
use super::selection::Selection;
use super::style::{self, Palette};
use super::{dialogs, texture, view, window, EditorMsg, LAST_IMAGE_PRESENTED_QPC};
use crate::app::{post, AppCommand};
use crate::common::time::{qpc_ms, qpc_now};
use crate::common::{win, Image};
use crate::i18n::{t, tf};
use crate::settings::{self, Destination};
use eframe::egui::{
    self, vec2, Align2, Color32, CornerRadius, Id, Key, Margin, RichText, Stroke, TextureHandle, ViewportCommand,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const MAX_RECENT: usize = 15;

/// Start eframe on the current (editor) thread; returns when the editor shuts down.
pub(super) fn run(rx: Receiver<EditorMsg>, t0: i64) -> Result<(), eframe::Error> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(t("editor-title"))
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([640.0, 420.0])
        .with_drag_and_drop(true);
    if let Some(icon) = style::app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        vsync: true,
        centered: true,
        persist_window: false,
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::windows::EventLoopBuilderExtWindows;
            b.with_any_thread(true);
        })),
        ..Default::default()
    };
    eframe::run_native("SnapForge Editor", options, Box::new(move |cc| Ok(Box::new(EditorApp::new(cc, rx, t0)))))
}

pub(super) struct Toast {
    pub text: String,
    pub is_error: bool,
    pub until: Instant,
}

/// A file handed to an external editor and watched for changes.
pub(super) struct ExternalEdit {
    pub doc: DocId,
    pub path: PathBuf,
    pub editor: String,
    _watcher: FileWatcher,
}

/// Unique-colour count of the current selection, computed on a worker.
#[derive(Default)]
pub(super) struct SelStats {
    /// (doc, selection version, revision) the count belongs to.
    pub key: Option<(DocId, u64, u64)>,
    pub since: Option<Instant>,
    pub requested: bool,
    pub colors: Option<u64>,
}

pub struct EditorApp {
    pub(super) rx: Receiver<EditorMsg>,
    pub(super) hwnd: isize,
    pub(super) tabs: Tabs,
    pub(super) queue: JobQueue,
    pub(super) jobs: Jobs,
    pub(super) pal: Palette,
    pub(super) checker: TextureHandle,
    pub(super) about_icon: Option<TextureHandle>,
    pub(super) actions: Vec<Action>,
    pub(super) modal: Option<Modal>,
    pub(super) toast: Option<Toast>,
    /// Our view of the window's visibility (hidden on close).
    pub(super) visible: bool,
    /// Set on shutdown so the close request goes through.
    pub(super) allow_close: bool,
    /// Files being decoded.
    pub(super) loading: usize,
    /// Last canvas size (physical px), for zoom commands issued from the toolbar.
    pub(super) view_px: [f32; 2],
    /// QPC of the open call whose image was painted last frame (stored next frame).
    pub(super) present_pending: Option<i64>,
    /// Number of images presented so far (first one is the "cold" open).
    pub(super) presented_count: u32,
    pub(super) registry: CommandRegistry,
    /// What the registry was built from (rebuilt when it changes).
    pub(super) registry_key: (u64, Vec<String>, Vec<String>),
    pub(super) effects: EffectRegistry,
    pub(super) ext_tools: Vec<Box<dyn CanvasTool>>,
    pub(super) tool: ToolId,
    pub(super) canvas: CanvasState,
    pub(super) palette: Option<PaletteState>,
    pub(super) effect_dialog: Option<EffectDialog>,
    /// Recent files marked with Ctrl+click (open several at once).
    pub(super) recent_marks: Vec<String>,
    pub(super) stats: SelStats,
    pub(super) external: Vec<ExternalEdit>,
    pub(super) pending_reloads: Vec<(DocId, PathBuf)>,
    pub(super) scanning: bool,
    pub(super) printing: bool,
    /// Document whose effect is being applied on a worker.
    pub(super) applying: Option<DocId>,
    /// Remaining undo (negative: redo) steps while compressed steps decode.
    pub(super) pending_steps: Option<(DocId, isize)>,
    pub(super) paste_key_down: bool,
    pub(super) cmd_state: CmdState,
    pub(super) hover_px: Option<(i32, i32)>,
    /// Where the open effect dialog's result goes.
    pub(super) pending_effect_target: Option<EffectTarget>,
}

impl EditorApp {
    fn new(cc: &eframe::CreationContext<'_>, rx: Receiver<EditorMsg>, t0: i64) -> Self {
        let ctx = cc.egui_ctx.clone();
        let hwnd = hwnd_of(cc);
        let dark = win::system_dark_mode();
        let pal = style::apply(&ctx, dark);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        if hwnd != 0 {
            let h = win::hwnd_from_isize(hwnd);
            win::set_dark_titlebar(h, dark);
            let saved = settings::get().editor.window;
            match saved {
                Some(p) if window::restore_placement(hwnd, p) => {}
                _ => window::fit_to_work_area(hwnd),
            }
        }
        super::register_window(ctx.clone(), hwnd);
        log::info!("editor: window + GL ready {:.1} ms after thread start", qpc_ms(t0, qpc_now()));
        let queue = JobQueue::new(ctx.clone());
        let jobs = queue.jobs.clone();
        let mut app = Self {
            rx,
            hwnd,
            tabs: Tabs::default(),
            jobs,
            queue,
            pal,
            checker: texture::checker_texture(&ctx, dark),
            about_icon: None,
            actions: Vec::new(),
            modal: None,
            toast: None,
            visible: true,
            allow_close: false,
            loading: 0,
            view_px: [1280.0, 720.0],
            present_pending: None,
            presented_count: 0,
            registry: CommandRegistry::default(),
            registry_key: (0, Vec::new(), Vec::new()),
            effects: EffectRegistry::default(),
            ext_tools: Vec::new(),
            tool: ToolId::SelectRect,
            canvas: CanvasState::default(),
            palette: None,
            effect_dialog: None,
            recent_marks: Vec::new(),
            stats: SelStats::default(),
            external: Vec::new(),
            pending_reloads: Vec::new(),
            scanning: false,
            printing: false,
            applying: None,
            pending_steps: None,
            paste_key_down: false,
            cmd_state: CmdState::default(),
            hover_px: None,
            pending_effect_target: None,
        };
        app.apply_workspace_filter();
        app.restore_workspaces();
        app
    }

    // ------------------------------------------------------------------ workspaces

    pub(super) fn workspaces_on() -> bool {
        settings::get().editor_ext.workspaces_enabled
    }

    /// The active workspace id (validated against the list).
    pub(super) fn active_ws() -> u64 {
        let s = settings::get();
        let e = &s.editor_ext;
        if e.workspaces.iter().any(|w| w.id == e.active_workspace) {
            e.active_workspace
        } else {
            e.workspaces.first().map(|w| w.id).unwrap_or(1)
        }
    }

    pub(super) fn apply_workspace_filter(&mut self) {
        let f = Self::workspaces_on().then(Self::active_ws);
        self.tabs.set_filter(f);
    }

    fn restore_workspaces(&mut self) {
        let (on, restore, list) = {
            let s = settings::get();
            (s.editor_ext.workspaces_enabled, s.editor_ext.restore_workspaces, s.editor_ext.workspaces.clone())
        };
        if !on || !restore {
            return;
        }
        for ws in list {
            let paths: Vec<PathBuf> = ws.open_files.iter().map(PathBuf::from).filter(|p| p.is_file()).collect();
            self.loading += paths.len();
            self.jobs.load(paths, Some(ws.id));
        }
    }

    /// Remember each workspace's open files (only when they changed).
    fn persist_workspaces(&mut self) {
        if self.loading > 0 || !Self::workspaces_on() {
            return;
        }
        let current: Vec<(u64, Vec<String>)> = {
            let s = settings::get();
            s.editor_ext
                .workspaces
                .iter()
                .map(|w| {
                    let files = self
                        .tabs
                        .docs
                        .iter()
                        .filter(|d| d.workspace == w.id)
                        .filter_map(|d| d.path.as_ref().map(|p| p.to_string_lossy().into_owned()))
                        .collect();
                    (w.id, files)
                })
                .collect()
        };
        let differs = {
            let s = settings::get();
            s.editor_ext.workspaces.iter().zip(&current).any(|(w, (_, files))| &w.open_files != files)
        };
        if differs {
            settings::update(|s| {
                for w in s.editor_ext.workspaces.iter_mut() {
                    if let Some((_, f)) = current.iter().find(|(id, _)| *id == w.id) {
                        w.open_files = f.clone();
                    }
                }
            });
        }
    }

    // ------------------------------------------------------------------ messages

    fn drain_messages(&mut self, ctx: &egui::Context) {
        let mut show = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                EditorMsg::Open { img, title, path, continue_to, qpc } => {
                    let id = self.tabs.next_id();
                    let mut doc = Document::new(id, img, title, path, continue_to);
                    doc.opened_qpc = Some(qpc);
                    self.push_doc(doc, None);
                    show = true;
                }
                EditorMsg::OpenFiles(paths) => {
                    self.open_paths(paths);
                    show = true;
                }
                EditorMsg::Show => show = true,
                EditorMsg::Shutdown => {
                    if self.visible {
                        self.save_placement();
                    }
                    self.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        if show && !self.allow_close {
            if !self.visible {
                // Shown from another thread with ShowWindow; keep winit's state in sync.
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                self.visible = true;
                self.refresh_theme(ctx);
            }
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            window::focus(self.hwnd);
        }
    }

    /// Add a tab (to `workspace`, else the active workspace) and activate it.
    pub(super) fn push_doc(&mut self, mut doc: Document, workspace: Option<u64>) {
        let (raw, total) = {
            let s = settings::get();
            (s.editor_ext.undo_raw_mb.max(16) as usize, s.editor_ext.undo_total_mb.max(64) as usize)
        };
        doc.set_history_budget(Budget { raw: raw * MB, total: total.max(raw) * MB });
        doc.set_waker(self.jobs.waker());
        doc.workspace = workspace.unwrap_or_else(Self::active_ws);
        self.tabs.push(doc);
    }

    fn refresh_theme(&mut self, ctx: &egui::Context) {
        let dark = win::system_dark_mode();
        if dark != self.pal.dark {
            self.pal = style::apply(ctx, dark);
            self.checker = texture::checker_texture(ctx, dark);
            win::set_dark_titlebar(win::hwnd_from_isize(self.hwnd), dark);
        }
    }

    pub(super) fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut to_load = Vec::new();
        for p in paths {
            if let Some(id) = self.tabs.find_path(&p) {
                self.show_doc(id);
            } else if !to_load.contains(&p) {
                to_load.push(p);
            }
        }
        self.loading += to_load.len();
        self.jobs.load(to_load, None);
    }

    /// Activate a tab, switching to its workspace if needed.
    pub(super) fn show_doc(&mut self, id: DocId) {
        if let Some(ws) = self.tabs.docs.iter().find(|d| d.id == id).map(|d| d.workspace) {
            if Self::workspaces_on() && ws != Self::active_ws() {
                settings::update(|s| s.editor_ext.active_workspace = ws);
                self.apply_workspace_filter();
            }
        }
        self.tabs.activate(id);
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        for d in self.tabs.docs.iter_mut() {
            d.poll();
        }
        self.continue_pending_steps();
        for r in self.queue.poll() {
            self.on_job(ctx, r);
        }
    }

    fn on_job(&mut self, _ctx: &egui::Context, r: JobResult) {
        match r {
            JobResult::Loaded { path, workspace, result } => {
                self.loading = self.loading.saturating_sub(1);
                match result {
                    Ok(img) => {
                        remember_recent(&path);
                        let id = self.tabs.next_id();
                        self.push_doc(Document::from_file(id, img, path), workspace);
                        if workspace.is_some() {
                            // Restored workspaces: keep the visible one's first tab active.
                            self.apply_workspace_filter();
                        }
                    }
                    Err(e) => {
                        let name = file_name(&path);
                        log::warn!("editor: open {} failed: {e}", path.display());
                        self.toast(tf("editor-open-failed", &[("name", &name), ("error", &e)]), true);
                    }
                }
            }
            JobResult::Saved { doc, path, revision, close_after, result } => {
                let Some(d) = self.tabs.get_mut(doc) else { return };
                d.saving = false;
                match result {
                    Ok(()) => {
                        d.on_saved(path.clone(), revision);
                        let clean = !d.dirty;
                        remember_recent(&path);
                        self.toast(tf("notify-saved", &[("path", &path.display().to_string())]), false);
                        if close_after && clean {
                            self.close_doc(doc);
                        }
                    }
                    Err(e) => {
                        log::warn!("editor: save {} failed: {e}", path.display());
                        self.toast(tf("editor-save-failed", &[("error", &e)]), true);
                    }
                }
            }
            JobResult::Copied(result) => match result {
                Ok(()) => self.toast(t("notify-copied"), false),
                Err(e) => {
                    log::warn!("editor: copy failed: {e}");
                    self.toast(tf("editor-copy-failed", &[("error", &e)]), true);
                }
            },
            JobResult::Pasted { image, new_tab } => self.on_pasted(image, new_tab),
            JobResult::Deleted { doc, path, result } => match result {
                Ok(()) => {
                    let p = path.to_string_lossy().into_owned();
                    settings::update(|s| s.editor.recent_files.retain(|r| !r.eq_ignore_ascii_case(&p)));
                    self.close_doc(doc);
                    self.toast(tf("editor-deleted", &[("name", &file_name(&path))]), false);
                }
                Err(e) => self.toast(tf("editor-delete-failed", &[("error", &e)]), true),
            },
            JobResult::Props(doc, props) => {
                if let Some(Modal::Properties(id, slot)) = &mut self.modal {
                    if *id == doc {
                        *slot = Some(props);
                    }
                }
            }
            JobResult::Colors { doc, key, count } => {
                if self.stats.key == Some((doc, key.0, key.1)) {
                    self.stats.colors = Some(count);
                }
            }
            JobResult::EffectApplied { doc, name, target, result } => {
                self.applying = None;
                let Some(d) = self.tabs.get_mut(doc) else { return };
                let ok = match target {
                    EffectTarget::Whole => d.commit_image(&name, result),
                    EffectTarget::Region(r, mask) => {
                        if (result.width as i32, result.height as i32) == (r.w, r.h) {
                            d.commit_region(&name, r, &result, mask.as_deref().map(|m| m.as_slice()))
                        } else {
                            false
                        }
                    }
                };
                if !ok {
                    self.toast(tf("editor-effect-nochange", &[("name", &name)]), false);
                }
            }
            JobResult::EditReady { doc, editor, path, result } => self.on_edit_ready(doc, editor, path, result),
            JobResult::ExternalChanged { doc, path } => {
                if !self.pending_reloads.iter().any(|(d, _)| *d == doc) {
                    self.pending_reloads.push((doc, path));
                }
            }
            JobResult::Reloaded { doc, path, result } => match result {
                Ok(img) => {
                    let editor =
                        self.external.iter().find(|e| e.doc == doc).map(|e| e.editor.clone()).unwrap_or_default();
                    if let Some(d) = self.tabs.get_mut(doc) {
                        d.deselect();
                        d.commit_image(&tf("editor-step-reload", &[("editor", &editor)]), img);
                        if d.path.as_deref() == Some(path.as_path()) {
                            let rev = d.revision;
                            d.on_saved(path, rev);
                        }
                    }
                }
                Err(e) => self.toast(tf("editor-open-failed", &[("name", &file_name(&path)), ("error", &e)]), true),
            },
            JobResult::Scanned(res) => match res {
                Ok((img, title)) => {
                    let id = self.tabs.next_id();
                    self.push_doc(Document::new(id, img, title, None, None), None);
                }
                Err(super::scan::ScanError::NoDevice) => self.modal = Some(Modal::NoScanner),
                Err(e) => self.toast(tf("editor-scan-failed", &[("error", &e.to_string())]), true),
            },
            JobResult::ScanFinished => self.scanning = false,
            JobResult::Printed(r) => {
                self.printing = false;
                if let Err(e) = r {
                    self.toast(tf("editor-print-failed", &[("error", &e)]), true);
                }
            }
        }
    }

    pub(super) fn toast(&mut self, text: String, is_error: bool) {
        let secs = if is_error { 6 } else { 3 };
        self.toast = Some(Toast { text, is_error, until: Instant::now() + Duration::from_secs(secs) });
    }

    // ------------------------------------------------------------------ window

    fn save_placement(&self) {
        if let Some(p) = window::placement(self.hwnd) {
            if settings::get().editor.window != Some(p) {
                settings::update(|s| s.editor.window = Some(p));
            }
        }
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.save_placement();
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.visible = false;
        self.modal = None;
        self.palette = None;
        self.effect_dialog = None;
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }
    }

    // ------------------------------------------------------------------ commands

    /// Rebuild the command registry when effects, tools, recent files or external
    /// editors changed.
    fn refresh_registry(&mut self) {
        let gen = super::ext_generation();
        let (recent, editors) = {
            let s = settings::get();
            (
                s.editor.recent_files.clone(),
                s.editor.external_editors.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
            )
        };
        if self.registry_key.0 != gen {
            self.effects = super::build_effects();
            self.ext_tools = super::build_tools();
        }
        let key = (gen, recent, editors);
        if self.registry_key != key {
            self.registry = super::menus::build_registry(&self.effects, &self.ext_tools, &key.1, &key.2);
            for (a, b) in self.registry.conflicts() {
                log::warn!("editor: commands {a} and {b} share a shortcut");
            }
            self.registry_key = key;
        }
    }

    fn compute_cmd_state(&mut self) {
        let d = self.tabs.active();
        let busy = d.is_some_and(|d| d.busy()) || self.applying.is_some();
        self.cmd_state = CmdState {
            has_doc: d.is_some(),
            has_selection: d.is_some_and(|d| d.selection.is_some()),
            can_undo: d.is_some_and(|d| d.can_undo()) && !busy,
            can_redo: d.is_some_and(|d| d.can_redo()) && !busy,
            has_path: d.is_some_and(|d| d.path.is_some()),
            any_dirty: self.tabs.visible().any(|d| d.dirty),
            multiple_docs: self.tabs.visible().count() > 1,
            busy,
            can_continue: d.is_some_and(|d| d.continue_to.is_some()),
            workspaces: Self::workspaces_on(),
        };
    }

    /// Keyboard shortcuts → commands (exact modifier match), plus the clipboard keys
    /// that egui turns into events.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let mut acts = Vec::new();
        let reg = &self.registry;
        let state = self.cmd_state;
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { return true };
                let ctrl = modifiers.ctrl || modifiers.command;
                if typing
                    && (!ctrl && !modifiers.alt || matches!(key, Key::Z | Key::Y | Key::A | Key::C | Key::X | Key::V))
                {
                    return true;
                }
                if *key == Key::Escape && !ctrl && !modifiers.alt && !modifiers.shift {
                    acts.push(Action::Deselect);
                    return false;
                }
                match reg.for_key(*key, *modifiers) {
                    Some(c) if (c.enabled)(&state) => {
                        acts.push(c.action.clone());
                        false
                    }
                    Some(_) => false,
                    None => true,
                }
            });
            if !typing {
                for e in &i.events {
                    match e {
                        egui::Event::Copy => acts.push(Action::Copy),
                        egui::Event::Cut => acts.push(Action::Cut),
                        egui::Event::Paste(_) => acts.push(Action::Paste),
                        _ => {}
                    }
                }
            }
        });
        // egui only reports Ctrl+V when the clipboard holds text; watch the key itself
        // so image pastes work too.
        let v_down = key_down(0x56) && (key_down(0x11)) && ctx.input(|i| i.focused);
        if v_down && !self.paste_key_down && !typing {
            let shift = key_down(0x10);
            let target = if shift { Action::PasteNewTab } else { Action::Paste };
            acts.retain(|a| *a != Action::Paste);
            acts.push(target);
        }
        self.paste_key_down = v_down;
        acts.dedup();
        self.actions.extend(acts);
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            let (ok, skipped): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| dialogs::is_supported_image(p));
            if let Some(p) = skipped.first() {
                let name = file_name(p);
                self.toast(tf("editor-open-failed", &[("name", &name), ("error", &t("editor-unsupported"))]), true);
            }
            self.open_paths(ok);
        }
    }

    fn process_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            self.run_action(ctx, a);
        }
    }

    pub(super) fn run_action(&mut self, ctx: &egui::Context, a: Action) {
        let active = self.tabs.active_id();
        match a {
            Action::Open => {
                let folder = default_folder();
                let files =
                    dialogs::open_files(self.hwnd, Some(&folder), &t("editor-filter-images"), &t("editor-filter-all"));
                if let Some(dir) = files.first().and_then(|p| p.parent()) {
                    remember_folder(dir);
                }
                self.open_paths(files);
            }
            Action::Save => {
                if let Some(id) = active {
                    self.save(id, false, false);
                }
            }
            Action::SaveDoc(id) => self.save(id, false, false),
            Action::SaveAs => {
                if let Some(id) = active {
                    self.save(id, true, false);
                }
            }
            Action::SaveAll => {
                let ids: Vec<DocId> = self.tabs.visible().filter(|d| d.dirty && !d.saving).map(|d| d.id).collect();
                for id in ids {
                    self.save(id, false, false);
                }
            }
            Action::SaveAllSequence => self.modal = Some(Modal::sequence()),
            Action::CloseTab => match active {
                Some(id) => self.request_close(id),
                None => self.hide(ctx),
            },
            Action::Close(id) => self.request_close(id),
            Action::Discard(id) => self.close_doc(id),
            Action::CloseSaved => {
                let ids: Vec<DocId> = self.tabs.visible().filter(|d| !d.dirty && !d.saving).map(|d| d.id).collect();
                let n = ids.len();
                for id in ids {
                    self.close_doc(id);
                }
                self.toast(tf("editor-closed-saved", &[("count", &n.to_string())]), false);
            }
            Action::DeleteFile => {
                if let Some(d) = self.tabs.active() {
                    if let Some(p) = &d.path {
                        self.modal = Some(Modal::ConfirmDelete(d.id, p.clone()));
                    }
                }
            }
            Action::Properties => {
                if let Some(d) = self.tabs.active() {
                    self.jobs.props(d.id, d.path.clone(), d.image.clone());
                    self.modal = Some(Modal::Properties(d.id, None));
                }
            }
            Action::OpenFolder => match self.tabs.active().and_then(|d| d.path.clone()) {
                Some(p) => win::reveal_in_explorer(&p),
                None => {
                    let dir = settings::get().output_folder();
                    win::shell_open(&dir.to_string_lossy());
                }
            },
            Action::OpenRecent(paths) => {
                self.recent_marks.clear();
                let (exist, missing): (Vec<PathBuf>, Vec<PathBuf>) =
                    paths.into_iter().map(PathBuf::from).partition(|p| p.is_file());
                if let Some(p) = missing.first() {
                    self.toast(
                        tf("editor-open-failed", &[("name", &file_name(p)), ("error", &t("editor-missing"))]),
                        true,
                    );
                    let gone: Vec<String> = missing.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    settings::update(|s| {
                        s.editor.recent_files.retain(|r| !gone.iter().any(|g| g.eq_ignore_ascii_case(r)))
                    });
                }
                self.open_paths(exist);
            }
            Action::ClearRecent => {
                self.recent_marks.clear();
                settings::update(|s| s.editor.recent_files.clear());
            }
            Action::EditWith(i) => self.edit_with(i),
            Action::ConfigureEditors => {
                self.modal = Some(Modal::Editors(settings::get().editor.external_editors.clone()))
            }
            Action::Scan => self.modal = Some(Modal::Scan(settings::get().editor_ext.scan.clone())),
            Action::Print => {
                if let Some(d) = self.tabs.active() {
                    if !self.printing {
                        self.printing = true;
                        self.jobs.print(d.composited(), self.hwnd, d.title.clone());
                    }
                }
            }
            Action::Continue => {
                if let Some(d) = self.tabs.active() {
                    if let Some(dest) = d.continue_to {
                        let id = d.id;
                        self.continue_to(ctx, id, dest);
                    }
                }
            }
            Action::Hide => self.hide(ctx),
            Action::Undo => self.step_history(1),
            Action::Redo => self.step_history(-1),
            Action::UndoMany(n) => self.step_history(n as isize),
            Action::RedoMany(n) => self.step_history(-(n as isize)),
            Action::Copy => {
                if let Some(d) = self.tabs.active() {
                    self.jobs.copy(Arc::new(d.selection_or_image()));
                }
            }
            Action::Cut => {
                if let Some(d) = self.tabs.active() {
                    if d.selection.is_some() {
                        self.jobs.copy(Arc::new(d.selection_or_image()));
                        self.delete_selection("Cut");
                    }
                }
            }
            Action::Paste => self.jobs.paste(false),
            Action::PasteNewTab => self.jobs.paste(true),
            Action::SelectAll => {
                if let Some(d) = self.tabs.active_mut() {
                    d.deselect();
                    let b = d.image.bounds();
                    d.set_selection(Selection::rect(b));
                }
            }
            Action::Deselect => {
                if let Some(d) = self.tabs.active_mut() {
                    if d.floating.is_some() {
                        d.cancel_floating();
                    } else {
                        d.deselect();
                    }
                }
            }
            Action::DeleteSelection => self.delete_selection("Delete"),
            Action::CropToSelection => {
                if let Some(d) = self.tabs.active_mut() {
                    d.merge_floating();
                    if let Some(sel) = d.selection.clone() {
                        if d.apply_image_edit("Crop", |img| sel.extract(img)) {
                            d.set_selection(None);
                        }
                    }
                }
            }
            Action::SendToNewTab => {
                if let Some(d) = self.tabs.active() {
                    let img = d.selection_or_image();
                    let title = tf("editor-copy-of", &[("name", &d.title)]);
                    let id = self.tabs.next_id();
                    self.push_doc(Document::new(id, img, title, None, None), None);
                }
            }
            Action::PinToScreen => {
                if let Some(d) = self.tabs.active() {
                    post(AppCommand::PinImage(d.selection_or_image()));
                }
            }
            Action::Tool(tool) => self.set_tool(tool),
            Action::Effect(id) => self.start_effect(ctx, &id),
            Action::CombineImages => super::tools_host().combine_images(self.tools_context()),
            Action::ImagesToPdf => super::tools_host().images_to_pdf(self.tools_context()),
            Action::BatchPrint => super::tools_host().batch_print(self.tools_context()),
            Action::Palette => {
                self.palette = if self.palette.is_some() { None } else { Some(PaletteState::default()) };
            }
            Action::Zoom(z) => self.zoom(z),
            Action::Activate(id) => self.tabs.activate(id),
            Action::NextTab | Action::PrevTab => {
                let ids: Vec<DocId> = self.tabs.visible().map(|d| d.id).collect();
                if let (Some(cur), false) = (active, ids.is_empty()) {
                    let i = ids.iter().position(|&x| x == cur).unwrap_or(0) as isize;
                    let dir = if a == Action::NextTab { 1 } else { -1 };
                    let n = ids.len() as isize;
                    self.tabs.activate(ids[(i + dir).rem_euclid(n) as usize]);
                }
            }
            Action::ToggleWorkspaces => {
                let on = !Self::workspaces_on();
                settings::update(|s| {
                    s.editor_ext.workspaces_enabled = on;
                    if s.editor_ext.workspaces.is_empty() {
                        s.editor_ext.workspaces.push(Default::default());
                    }
                });
                // Tabs opened while workspaces were off join the active one.
                let ws = Self::active_ws();
                let known: Vec<u64> = settings::get().editor_ext.workspaces.iter().map(|w| w.id).collect();
                for d in self.tabs.docs.iter_mut().filter(|d| !known.contains(&d.workspace)) {
                    d.workspace = ws;
                }
                self.apply_workspace_filter();
            }
            Action::NewWorkspace => {
                let id = settings::update(|s| {
                    let id = s.editor_ext.workspaces.iter().map(|w| w.id).max().unwrap_or(0) + 1;
                    let n = s.editor_ext.workspaces.len() + 1;
                    s.editor_ext.workspaces.push(crate::settings::editor_ext::Workspace {
                        id,
                        name: format!("{} {n}", t("editor-workspace")),
                        ..Default::default()
                    });
                    s.editor_ext.active_workspace = id;
                    s.editor_ext.workspaces_enabled = true;
                    id
                });
                log::debug!("editor: workspace {id} created");
                self.apply_workspace_filter();
            }
            Action::SwitchWorkspace(id) => {
                settings::update(|s| s.editor_ext.active_workspace = id);
                self.apply_workspace_filter();
            }
            Action::CloseWorkspace(id) => self.close_workspace(id),
            Action::RenameWorkspace(id) => {
                let name = settings::get().editor_ext.workspaces.iter().find(|w| w.id == id).map(|w| w.name.clone());
                if let Some(name) = name {
                    self.modal = Some(Modal::RenameWorkspace(id, name));
                }
            }
            Action::About => self.modal = Some(Modal::About),
            Action::Shortcuts => self.modal = Some(Modal::Shortcuts),
        }
    }

    fn continue_to(&mut self, ctx: &egui::Context, id: DocId, dest: Destination) {
        if let Some(d) = self.tabs.get_mut(id) {
            post(AppCommand::Deliver { image: (*d.composited()).clone(), dest });
            self.close_doc(id);
            if self.tabs.is_empty() {
                self.hide(ctx);
            }
        }
    }

    fn request_close(&mut self, id: DocId) {
        if self.tabs.get_mut(id).is_some_and(|d| d.dirty) {
            self.show_doc(id);
            self.modal = Some(Modal::ConfirmClose(id));
        } else {
            self.close_doc(id);
        }
    }

    pub(super) fn close_doc(&mut self, id: DocId) {
        if self.tabs.active_id() == Some(id) {
            self.deactivate_tool();
        }
        self.external.retain(|e| e.doc != id);
        self.tabs.close(id);
    }

    fn close_workspace(&mut self, id: DocId) {
        let others: Vec<u64> =
            settings::get().editor_ext.workspaces.iter().map(|w| w.id).filter(|&w| w != id).collect();
        let Some(&target) = others.first() else { return };
        // Tabs move to the first remaining workspace rather than being closed unasked.
        for d in self.tabs.docs.iter_mut().filter(|d| d.workspace == id) {
            d.workspace = target;
        }
        settings::update(|s| {
            s.editor_ext.workspaces.retain(|w| w.id != id);
            if s.editor_ext.active_workspace == id {
                s.editor_ext.active_workspace = target;
            }
        });
        self.apply_workspace_filter();
    }

    fn tools_context(&self) -> ToolsContext {
        let mut docs: Vec<&Document> = self.tabs.visible().collect();
        if let Some(a) = self.tabs.active_id() {
            docs.sort_by_key(|d| d.id != a);
        }
        ToolsContext {
            files: docs.iter().filter(|d| !d.dirty).filter_map(|d| d.path.clone()).collect(),
            images: docs.iter().map(|d| (d.title.clone(), d.composited())).collect(),
            owner_hwnd: self.hwnd,
        }
    }

    fn set_tool(&mut self, tool: ToolId) {
        if tool != self.tool {
            self.deactivate_tool();
            self.tool = tool;
        }
    }

    fn deactivate_tool(&mut self) {
        if let ToolId::External(id) = &self.tool {
            let id = id.clone();
            if let (Some(t), Some(d)) = (self.ext_tools.iter_mut().find(|t| t.id() == id), self.tabs.active_mut()) {
                t.deactivate(d);
            }
        }
    }

    /// Undo (`n > 0`) or redo (`n < 0`) several steps; compressed steps decode on a
    /// worker, the rest continues once they are back.
    fn step_history(&mut self, n: isize) {
        let Some(id) = self.tabs.active_id() else { return };
        if self.applying.is_some() {
            return;
        }
        self.pending_steps = Some((id, n));
        self.continue_pending_steps();
    }

    fn continue_pending_steps(&mut self) {
        let Some((id, mut n)) = self.pending_steps else { return };
        let Some(d) = self.tabs.get_mut(id) else {
            self.pending_steps = None;
            return;
        };
        while n != 0 && !d.busy() {
            let ok = if n > 0 { d.undo() } else { d.redo() };
            if !ok {
                n = 0;
                break;
            }
            n -= n.signum();
        }
        self.pending_steps = (n != 0).then_some((id, n));
    }

    fn delete_selection(&mut self, name: &str) {
        let bg = background_color();
        let Some(d) = self.tabs.active_mut() else { return };
        d.merge_floating();
        let Some(sel) = d.selection.clone() else { return };
        let b = sel.bounds;
        d.apply_region_edit(name, b, |reg| {
            let local = sel.translated(-b.x, -b.y);
            local.fill(reg, bg);
        });
        d.set_selection(None);
    }

    fn on_pasted(&mut self, image: Option<Image>, new_tab: bool) {
        let Some(img) = image.filter(|i| !i.is_empty()) else {
            self.toast(t("editor-paste-empty"), true);
            return;
        };
        let view = self.view_px;
        match self.tabs.active_mut() {
            Some(d) if !new_tab => {
                // Top-left of the visible part of the image.
                let p = d.view.screen_to_image([0.0, 0.0]);
                let x = (p[0].max(0.0).floor() as i32).min(d.image.width as i32 - 1).max(0);
                let y = (p[1].max(0.0).floor() as i32).min(d.image.height as i32 - 1).max(0);
                let _ = view;
                d.paste_floating(img, x, y);
                if matches!(self.tool, ToolId::External(_) | ToolId::Pan) {
                    self.set_tool(ToolId::SelectRect);
                }
            }
            _ => {
                let id = self.tabs.next_id();
                self.push_doc(Document::new(id, img, t("editor-pasted"), None, None), None);
            }
        }
    }

    fn start_effect(&mut self, ctx: &egui::Context, id: &str) {
        let Some(def) = self.effects.get(id).cloned() else { return };
        if self.applying.is_some() {
            return;
        }
        let bg = background_color();
        let Some(d) = self.tabs.active_mut() else { return };
        if d.busy() {
            return;
        }
        d.merge_floating();
        let (source, target) = match (&d.selection, def.scope) {
            (Some(sel), Scope::SelectionOrImage) => {
                (Arc::new(d.image.crop(sel.bounds)), EffectTarget::Region(sel.bounds, sel.mask.clone()))
            }
            _ => (d.image.clone(), EffectTarget::Whole),
        };
        let doc_id = d.id;
        if def.params.is_some() {
            let on_sel = matches!(target, EffectTarget::Region(..));
            self.effect_dialog = Some(EffectDialog::new(ctx, doc_id, &def, source, on_sel, bg));
            self.pending_effect_target = Some(target);
        } else if def.placeholder {
            self.toast(tf("editor-effect-unavailable", &[("name", &def.label)]), false);
        } else {
            self.applying = Some(doc_id);
            self.jobs.effect(doc_id, def.label.clone(), source, target, def.apply.clone(), Default::default(), bg);
        }
    }

    fn effect_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dlg) = self.effect_dialog.as_mut() else { return };
        match dlg.ui(ctx) {
            DialogOutcome::Open => {}
            DialogOutcome::Cancel => {
                self.effect_dialog = None;
                self.pending_effect_target = None;
            }
            DialogOutcome::Apply(values) => {
                let (doc, id) = (dlg.doc, dlg.effect_id);
                self.effect_dialog = None;
                let target = self.pending_effect_target.take();
                let (Some(def), Some(target)) = (self.effects.get(id).cloned(), target) else { return };
                let Some(d) = self.tabs.get_mut(doc) else { return };
                let source = match &target {
                    EffectTarget::Region(r, _) => Arc::new(d.image.crop(*r)),
                    EffectTarget::Whole => d.image.clone(),
                };
                self.applying = Some(doc);
                self.jobs.effect(doc, def.label.clone(), source, target, def.apply.clone(), values, background_color());
            }
        }
    }

    /// Save (`as_new`: always ask for a file). Encoding and writing run on a worker.
    pub(super) fn save(&mut self, id: DocId, as_new: bool, close_after: bool) {
        let hwnd = self.hwnd;
        let Some(d) = self.tabs.get_mut(id) else { return };
        if d.saving {
            return;
        }
        let target = if as_new { SaveTarget::Prompt } else { d.save_target() };
        let path = match target {
            SaveTarget::Path(p, _) => p,
            SaveTarget::Prompt => {
                let fmt = d
                    .path
                    .as_deref()
                    .and_then(super::document::format_for_path)
                    .unwrap_or_else(|| settings::get().output.format);
                let folder =
                    d.path.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(default_folder);
                let Some((p, _fmt)) = dialogs::save_file(hwnd, Some(&folder), &d.suggested_name(fmt), fmt) else {
                    return;
                };
                if let Some(dir) = p.parent() {
                    remember_folder(dir);
                }
                p
            }
        };
        d.saving = true;
        let img = d.composited();
        self.jobs.save(id, img, path, d.revision, close_after);
    }

    fn edit_with(&mut self, idx: usize) {
        let Some(editor) = settings::get().editor.external_editors.get(idx).cloned() else { return };
        let Some(d) = self.tabs.active() else { return };
        let direct = d.path.as_ref().filter(|p| !d.dirty && d.floating.is_none() && d.overlay.is_none() && p.is_file());
        match direct {
            Some(p) => self.jobs.prepare_edit(d.id, idx, None, p.clone()),
            None => {
                let name = format!("{}-{}.png", fileops::sanitize_name(&d.title.replace('.', "_")), d.id);
                let path = fileops::temp_dir("edit").join(name);
                self.jobs.prepare_edit(d.id, idx, Some(d.composited()), path);
            }
        }
        log::info!("editor: editing with {}", editor.name);
    }

    fn on_edit_ready(&mut self, doc: DocId, editor: usize, path: PathBuf, result: Result<(), String>) {
        if let Err(e) = result {
            self.toast(tf("editor-save-failed", &[("error", &e)]), true);
            return;
        }
        let Some(ed) = settings::get().editor.external_editors.get(editor).cloned() else { return };
        if let Err(e) = crate::output::external::open_with(&ed.path, &path) {
            self.toast(tf("editor-edit-with-failed", &[("name", &ed.name), ("error", &format!("{e:#}"))]), true);
            return;
        }
        self.external.retain(|e| e.doc != doc);
        let jobs = self.jobs.clone();
        let p = path.clone();
        let watcher =
            FileWatcher::start(path.clone(), move || jobs.send(JobResult::ExternalChanged { doc, path: p.clone() }));
        self.external.push(ExternalEdit { doc, path, editor: ed.name.clone(), _watcher: watcher });
        self.toast(tf("editor-edit-with-started", &[("name", &ed.name)]), false);
    }

    fn zoom(&mut self, z: ZoomCmd) {
        let view = self.view_px;
        let Some(d) = self.tabs.active_mut() else { return };
        let img = d.size();
        let sel = d.selection.as_ref().map(|s| s.bounds);
        let v = &mut d.view;
        match z {
            ZoomCmd::In => v.set_zoom_centered(view::step_zoom(v.zoom, 1), img, view),
            ZoomCmd::Out => v.set_zoom_centered(view::step_zoom(v.zoom, -1), img, view),
            ZoomCmd::Actual => v.set_zoom_centered(1.0, img, view),
            ZoomCmd::Set(pct) => v.set_zoom_centered(pct as f32 / 100.0, img, view),
            ZoomCmd::FitWindow => v.fit_window(img, view),
            ZoomCmd::FitWidth => v.fit_width(img, view),
            ZoomCmd::Selection => match sel {
                Some(r) => v.zoom_to_rect([r.x as f32, r.y as f32, r.w as f32, r.h as f32], img, view),
                None => v.fit_window(img, view),
            },
        }
    }

    // ------------------------------------------------------------------ canvas

    fn central(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.canvas);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let ppp = ctx.pixels_per_point();
            self.view_px = [ui.available_width() * ppp, ui.available_height() * ppp];
            let blocked = self.modal.is_some() || self.palette.is_some() || self.effect_dialog.is_some();
            let space = ctx.input(|i| i.key_down(Key::Space)) && !ctx.wants_keyboard_input() && !blocked;
            let checker = self.checker.clone();
            let bg = background_color();
            let tool = self.tool.clone();
            let external = match &tool {
                ToolId::External(id) => self.ext_tools.iter_mut().find(|t| t.id() == id),
                _ => None,
            };
            if let Some(doc) = self.tabs.active_mut() {
                let cx = CanvasCtx { tool: &tool, external, space_down: space, background: bg, checker: &checker };
                let out = canvas::show(ui, doc, &mut self.canvas, cx);
                self.hover_px = out.hover_px;
                if out.painted {
                    if let Some(q) = doc.opened_qpc.take() {
                        self.present_pending = Some(q);
                        ctx.request_repaint();
                    }
                }
            } else {
                self.hover_px = None;
                self.empty_state(ui);
            }
        });
        // Drop hint while files are dragged over the window.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let rect = ctx.screen_rect().shrink(12.0);
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("drop-hint")));
            p.rect_filled(rect, CornerRadius::same(12), style::ACCENT.gamma_multiply(0.12));
            p.rect_stroke(rect, CornerRadius::same(12), Stroke::new(2.0_f32, style::ACCENT), egui::StrokeKind::Inside);
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                t("editor-drop-hint"),
                egui::FontId::proportional(22.0),
                style::ACCENT,
            );
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 110.0).max(16.0));
            ui.label(RichText::new("🖼").size(48.0).color(self.pal.muted_text));
            ui.add_space(8.0);
            ui.label(RichText::new(t("editor-empty-title")).size(20.0).strong());
            ui.label(RichText::new(t("editor-empty-hint")).color(self.pal.muted_text));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let w = 150.0 * 2.0 + ui.spacing().item_spacing.x;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                if ui
                    .add(style::primary_button(format!("📂  {}", t("editor-open"))).min_size(vec2(150.0, 32.0)))
                    .clicked()
                {
                    self.actions.push(Action::Open);
                }
                if ui
                    .add(egui::Button::new(format!("📋  {}", t("editor-paste-new-tab"))).min_size(vec2(150.0, 32.0)))
                    .clicked()
                {
                    self.actions.push(Action::PasteNewTab);
                }
            });
            ui.add_space(10.0);
            ui.label(RichText::new(t("editor-palette-tip")).small().color(self.pal.muted_text));
        });
    }

    /// Recompute the selection's unique colours after it settles.
    fn update_stats(&mut self, ctx: &egui::Context) {
        let dragging = self.canvas.is_dragging();
        let Some(d) = self.tabs.active() else {
            self.stats = SelStats::default();
            return;
        };
        let Some(sel) = &d.selection else {
            self.stats = SelStats::default();
            return;
        };
        let key = (d.id, d.selection_version, d.revision);
        if self.stats.key != Some(key) {
            self.stats = SelStats { key: Some(key), since: Some(Instant::now()), requested: false, colors: None };
        }
        let settled = self.stats.since.is_some_and(|s| s.elapsed() >= Duration::from_millis(150));
        if !self.stats.requested && settled && !dragging {
            self.stats.requested = true;
            let src = match &d.floating {
                Some(f) => {
                    let mut img = (*d.image).clone();
                    f.stamp(&mut img);
                    Arc::new(img)
                }
                None => d.image.clone(),
            };
            self.jobs.colors(d.id, (key.1, key.2), src, Some(sel.clone()));
        } else if !self.stats.requested {
            if let Some(s) = self.stats.since {
                ctx.request_repaint_after(Duration::from_millis(150).saturating_sub(s.elapsed()));
            }
        }
    }

    // ------------------------------------------------------------------ overlays

    pub(super) fn toasts(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else { return };
        let now = Instant::now();
        if now >= toast.until {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(toast.until - now);
        let (text, is_error) = (toast.text.clone(), toast.is_error);
        let pal = self.pal;
        egui::Area::new(Id::new("toast"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -44.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(pal.toast_bg)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(16, 10))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(70) })
                    .show(ui, |ui| {
                        ui.set_max_width(560.0);
                        ui.horizontal(|ui| {
                            let (icon, color) = if is_error {
                                ("⚠", pal.error)
                            } else {
                                ("✔", Color32::from_rgb(0x5C, 0xD6, 0x8A))
                            };
                            ui.label(RichText::new(icon).color(color));
                            ui.add(egui::Label::new(RichText::new(text).color(Color32::WHITE)).wrap());
                        });
                    });
            });
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(st) = self.palette.as_mut() else { return };
        match palette::show(ctx, st, &self.registry, &self.cmd_state, &self.pal) {
            PaletteOutcome::Open => {}
            PaletteOutcome::Close => self.palette = None,
            PaletteOutcome::Run(a) => {
                self.palette = None;
                self.actions.push(a);
            }
        }
    }

    fn bench_presented(&mut self) {
        if let Some(t0) = self.present_pending.take() {
            let now = qpc_now();
            LAST_IMAGE_PRESENTED_QPC.store(now, Ordering::Release);
            let kind = if self.presented_count == 0 { "cold" } else { "warm" };
            self.presented_count += 1;
            log::info!("editor: image presented {:.1} ms after open_image ({kind})", qpc_ms(t0, now));
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.bench_presented();
        self.drain_messages(ctx);
        self.poll_jobs(ctx);
        self.refresh_registry();
        self.compute_cmd_state();
        self.handle_close_request(ctx);
        let overlay_open = self.modal.is_some() || self.palette.is_some() || self.effect_dialog.is_some();
        if !overlay_open {
            self.shortcuts(ctx);
            self.handle_drops(ctx);
            if let Some((doc, path)) = self.pending_reloads.pop() {
                let editor = self.external.iter().find(|e| e.doc == doc).map(|e| e.editor.clone()).unwrap_or_default();
                if self.tabs.docs.iter().any(|d| d.id == doc) {
                    self.modal = Some(Modal::Reload { doc, path, editor });
                }
            }
        }
        self.menu_bar(ctx);
        self.ribbon(ctx);
        self.workspace_strip(ctx);
        self.tab_strip(ctx);
        self.status_bar(ctx);
        self.central(ctx);
        self.update_stats(ctx);
        self.modals(ctx);
        self.effect_dialog_ui(ctx);
        self.palette_ui(ctx);
        self.toasts(ctx);
        self.process_actions(ctx);
        self.persist_workspaces();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.pal.canvas.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        log::debug!("editor: on_exit");
    }
}

// ---------------------------------------------------------------------- helpers

fn hwnd_of(cc: &eframe::CreationContext<'_>) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        other => {
            log::warn!("editor: no Win32 window handle ({other:?})");
            0
        }
    }
}

/// True while a virtual key is held (state as of the message being processed).
fn key_down(vk: i32) -> bool {
    // SAFETY: plain query of this thread's keyboard state.
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(vk) < 0 }
}

pub(super) fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Premultiplied BGRA of the configured background (straight RGBA in settings).
pub(super) fn background_color() -> [u8; 4] {
    let c = settings::get().editor_ext.background;
    let a = c[3] as u32;
    let pm = |v: u8| ((v as u32 * a + 127) / 255) as u8;
    [pm(c[2]), pm(c[1]), pm(c[0]), c[3]]
}

/// The workspace's (or global) last folder if it still exists, else the output folder.
pub(super) fn default_folder() -> PathBuf {
    let s = settings::get();
    let ws_folder = s
        .editor_ext
        .workspaces_enabled
        .then(|| s.editor_ext.workspaces.iter().find(|w| w.id == s.editor_ext.active_workspace))
        .flatten()
        .map(|w| w.last_folder.trim().to_string())
        .filter(|f| !f.is_empty() && Path::new(f).is_dir());
    if let Some(f) = ws_folder {
        return PathBuf::from(f);
    }
    let last = s.editor.last_folder.trim();
    if !last.is_empty() && Path::new(last).is_dir() {
        PathBuf::from(last)
    } else {
        s.output_folder()
    }
}

pub(super) fn remember_folder(dir: &Path) {
    let d = dir.to_string_lossy().into_owned();
    let (global_same, ws_same) = {
        let s = settings::get();
        let ws = s.editor_ext.workspaces.iter().find(|w| w.id == s.editor_ext.active_workspace);
        (s.editor.last_folder == d, !s.editor_ext.workspaces_enabled || ws.is_none_or_same(&d))
    };
    if !global_same || !ws_same {
        settings::update(|s| {
            s.editor.last_folder = d.clone();
            if s.editor_ext.workspaces_enabled {
                let active = s.editor_ext.active_workspace;
                if let Some(w) = s.editor_ext.workspaces.iter_mut().find(|w| w.id == active) {
                    w.last_folder = d.clone();
                }
            }
        });
    }
}

trait SameFolder {
    fn is_none_or_same(&self, d: &str) -> bool;
}

impl SameFolder for Option<&crate::settings::editor_ext::Workspace> {
    fn is_none_or_same(&self, d: &str) -> bool {
        self.map_or(true, |w| w.last_folder == d)
    }
}

pub(super) fn remember_recent(path: &Path) {
    let p = path.to_string_lossy().into_owned();
    settings::update(|s| push_recent(&mut s.editor.recent_files, &p, MAX_RECENT));
}
