//! The eframe `App`: window lifetime (hide instead of close), panels, shortcuts and the
//! commands behind them.

use super::document::{push_recent, DocId, Document, SaveTarget, Tabs};
use super::jobs::{JobResult, Jobs};
use super::style::{self, Palette};
use super::{canvas, dialogs, texture, view, window, EditorMsg, LAST_IMAGE_PRESENTED_QPC};
use crate::app::{post, AppCommand};
use crate::common::time::{qpc_ms, qpc_now};
use crate::common::win;
use crate::i18n::{t, tf};
use crate::settings::{self, Destination};
use eframe::egui::{
    self, vec2, Align, Align2, Color32, CornerRadius, Id, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke,
    TextureHandle, ViewportCommand,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const MAX_RECENT: usize = 15;

/// Start eframe on the current (editor) thread; returns when the editor shuts down.
pub(super) fn run(rx: Receiver<EditorMsg>, t0: i64) -> Result<(), eframe::Error> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(t("editor-title"))
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([560.0, 380.0])
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoomCmd {
    In,
    Out,
    Actual,
    FitWindow,
    FitWidth,
    Set(u32),
}

/// Everything the UI can ask for; executed at the end of the frame.
#[derive(Debug, Clone, PartialEq)]
enum Action {
    Open,
    Save(DocId),
    SaveAs(DocId),
    Copy(DocId),
    Close(DocId),
    /// Close without asking (after "Don't Save").
    Discard(DocId),
    Activate(DocId),
    Zoom(ZoomCmd),
    OpenFolder,
    Continue(DocId, Destination),
    OpenRecent(String),
    ClearRecent,
    About,
    Hide,
}

enum Modal {
    ConfirmClose(DocId),
    About,
}

struct Toast {
    text: String,
    is_error: bool,
    until: Instant,
}

pub struct EditorApp {
    rx: Receiver<EditorMsg>,
    hwnd: isize,
    tabs: Tabs,
    jobs: Jobs,
    pal: Palette,
    checker: TextureHandle,
    about_icon: Option<TextureHandle>,
    actions: Vec<Action>,
    modal: Option<Modal>,
    toast: Option<Toast>,
    /// Our view of the window's visibility (hidden on close).
    visible: bool,
    /// Set on shutdown so the close request goes through.
    allow_close: bool,
    /// Files being decoded.
    loading: usize,
    /// Last canvas size (physical px), for zoom commands issued from the toolbar.
    view_px: [f32; 2],
    /// QPC of the open call whose image was painted last frame (stored next frame).
    present_pending: Option<i64>,
    /// Number of images presented so far (first one is the "cold" open).
    presented_count: u32,
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
        Self {
            rx,
            hwnd,
            tabs: Tabs::default(),
            jobs: Jobs::new(ctx.clone()),
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
                    self.tabs.push(doc);
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

    fn refresh_theme(&mut self, ctx: &egui::Context) {
        let dark = win::system_dark_mode();
        if dark != self.pal.dark {
            self.pal = style::apply(ctx, dark);
            self.checker = texture::checker_texture(ctx, dark);
            win::set_dark_titlebar(win::hwnd_from_isize(self.hwnd), dark);
        }
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut to_load = Vec::new();
        for p in paths {
            if let Some(id) = self.tabs.find_path(&p) {
                self.tabs.activate(id);
            } else if !to_load.contains(&p) {
                to_load.push(p);
            }
        }
        self.loading += to_load.len();
        self.jobs.load(to_load);
    }

    fn poll_jobs(&mut self) {
        for r in self.jobs.poll() {
            match r {
                JobResult::Loaded { path, result } => {
                    self.loading = self.loading.saturating_sub(1);
                    match result {
                        Ok(img) => {
                            remember_recent(&path);
                            let id = self.tabs.next_id();
                            self.tabs.push(Document::from_file(id, img, path));
                        }
                        Err(e) => {
                            let name = file_name(&path);
                            log::warn!("editor: open {} failed: {e}", path.display());
                            self.toast(tf("editor-open-failed", &[("name", &name), ("error", &e)]), true);
                        }
                    }
                }
                JobResult::Saved { doc, path, revision, close_after, result } => {
                    let Some(d) = self.tabs.get_mut(doc) else { continue };
                    d.saving = false;
                    match result {
                        Ok(()) => {
                            d.on_saved(path.clone(), revision);
                            let clean = !d.dirty;
                            remember_recent(&path);
                            self.toast(tf("notify-saved", &[("path", &path.display().to_string())]), false);
                            if close_after && clean {
                                self.tabs.close(doc);
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
            }
        }
    }

    fn toast(&mut self, text: String, is_error: bool) {
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
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide(ctx);
        }
    }

    // ------------------------------------------------------------------ input

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let active = self.tabs.active_id();
        let cmd = Modifiers::COMMAND;
        let mut acts = Vec::new();
        ctx.input_mut(|i| {
            if i.consume_key(cmd, Key::O) {
                acts.push(Action::Open);
            }
            if i.consume_key(cmd | Modifiers::SHIFT, Key::Tab) {
                acts.push(self.cycle_tab(-1));
            } else if i.consume_key(cmd, Key::Tab) {
                acts.push(self.cycle_tab(1));
            }
            let Some(id) = active else {
                if i.consume_key(cmd, Key::W) {
                    acts.push(Action::Hide);
                }
                return;
            };
            if i.consume_key(cmd, Key::S) {
                acts.push(Action::Save(id));
            }
            if i.consume_key(Modifiers::ALT, Key::S) {
                acts.push(Action::SaveAs(id));
            }
            if i.consume_key(cmd, Key::W) {
                acts.push(Action::Close(id));
            }
            if i.consume_key(cmd, Key::Num0) {
                acts.push(Action::Zoom(ZoomCmd::Actual));
            }
            if i.consume_key(cmd, Key::Plus) || i.consume_key(cmd, Key::Equals) {
                acts.push(Action::Zoom(ZoomCmd::In));
            }
            if i.consume_key(cmd, Key::Minus) {
                acts.push(Action::Zoom(ZoomCmd::Out));
            }
            if typing {
                return;
            }
            if i.events.iter().any(|e| matches!(e, egui::Event::Copy)) {
                acts.push(Action::Copy(id));
            }
            let none = Modifiers::NONE;
            if i.consume_key(none, Key::K) {
                acts.push(Action::Zoom(ZoomCmd::FitWidth));
            }
            if i.consume_key(none, Key::F) {
                acts.push(Action::Zoom(ZoomCmd::FitWindow));
            }
            if i.consume_key(none, Key::W) {
                acts.push(Action::OpenFolder);
            }
            if i.consume_key(none, Key::Plus) || i.consume_key(none, Key::Equals) {
                acts.push(Action::Zoom(ZoomCmd::In));
            }
            if i.consume_key(none, Key::Minus) {
                acts.push(Action::Zoom(ZoomCmd::Out));
            }
        });
        self.actions.extend(acts);
    }

    fn cycle_tab(&self, dir: isize) -> Action {
        let n = self.tabs.docs.len() as isize;
        if n == 0 {
            return Action::Activate(0);
        }
        let i = (self.tabs.active as isize + dir).rem_euclid(n) as usize;
        Action::Activate(self.tabs.docs[i].id)
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

    // ------------------------------------------------------------------ actions

    fn process_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            self.run_action(ctx, a);
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, a: Action) {
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
            Action::Save(id) => self.save(id, false, false),
            Action::SaveAs(id) => self.save(id, true, false),
            Action::Copy(id) => {
                if let Some(d) = self.tabs.get_mut(id) {
                    let img = d.image.clone();
                    self.jobs.copy(img);
                }
            }
            Action::Close(id) => {
                if self.tabs.get_mut(id).is_some_and(|d| d.dirty) {
                    self.tabs.activate(id);
                    self.modal = Some(Modal::ConfirmClose(id));
                } else {
                    self.tabs.close(id);
                }
            }
            Action::Discard(id) => {
                self.tabs.close(id);
            }
            Action::Activate(id) => self.tabs.activate(id),
            Action::Zoom(z) => self.zoom(z),
            Action::OpenFolder => match self.tabs.active().and_then(|d| d.path.clone()) {
                Some(p) => win::reveal_in_explorer(&p),
                None => {
                    let dir = settings::get().output_folder();
                    win::shell_open(&dir.to_string_lossy());
                }
            },
            Action::Continue(id, dest) => {
                if let Some(d) = self.tabs.get_mut(id) {
                    post(AppCommand::Deliver { image: (*d.image).clone(), dest });
                    self.tabs.close(id);
                    if self.tabs.is_empty() {
                        self.hide(ctx);
                    }
                }
            }
            Action::OpenRecent(p) => self.open_paths(vec![PathBuf::from(p)]),
            Action::ClearRecent => settings::update(|s| s.editor.recent_files.clear()),
            Action::About => self.modal = Some(Modal::About),
            Action::Hide => self.hide(ctx),
        }
    }

    /// Save (`as_new`: always ask for a file). Encoding and writing run on a worker.
    fn save(&mut self, id: DocId, as_new: bool, close_after: bool) {
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
        self.jobs.save(id, d.image.clone(), path, d.revision, close_after);
    }

    fn zoom(&mut self, z: ZoomCmd) {
        let view = self.view_px;
        let Some(d) = self.tabs.active_mut() else { return };
        let img = d.size();
        let v = &mut d.view;
        match z {
            ZoomCmd::In => v.set_zoom_centered(view::step_zoom(v.zoom, 1), img, view),
            ZoomCmd::Out => v.set_zoom_centered(view::step_zoom(v.zoom, -1), img, view),
            ZoomCmd::Actual => v.set_zoom_centered(1.0, img, view),
            ZoomCmd::Set(pct) => v.set_zoom_centered(pct as f32 / 100.0, img, view),
            ZoomCmd::FitWindow => v.fit_window(img, view),
            ZoomCmd::FitWidth => v.fit_width(img, view),
        }
    }

    // ------------------------------------------------------------------ panels

    fn top_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin::symmetric(8, 6));
        egui::TopBottomPanel::top("toolbar").frame(frame).show_separator_line(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Ghost buttons: no fill until hovered.
                ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                self.file_menu(ui);
                separator(ui, &self.pal);
                let active = self.tabs.active_id();
                let has = active.is_some();
                let id = active.unwrap_or(0);
                if tool_button(ui, "📂", &t("editor-open-button"), "Ctrl+O", true) {
                    self.actions.push(Action::Open);
                }
                if tool_button(ui, "💾", &t("editor-save"), "Ctrl+S", has) {
                    self.actions.push(Action::Save(id));
                }
                if tool_button(ui, "🖫", &t("editor-save-as-button"), "Alt+S", has) {
                    self.actions.push(Action::SaveAs(id));
                }
                if tool_button(ui, "📋", &t("editor-copy"), "Ctrl+C", has) {
                    self.actions.push(Action::Copy(id));
                }
                if tool_button(ui, "🗁", &t("editor-open-folder"), "W", true) {
                    self.actions.push(Action::OpenFolder);
                }
                separator(ui, &self.pal);
                self.zoom_controls(ui, has);

                let cont = self.tabs.active().and_then(|d| d.continue_to.map(|dest| (d.id, dest)));
                if let Some((id, dest)) = cont {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = tf("editor-continue", &[("dest", &t(dest.label_key()))]);
                        let btn = style::primary_button(format!("{label}  ➡")).min_size(vec2(0.0, 28.0));
                        if ui.add(btn).clicked() {
                            self.actions.push(Action::Continue(id, dest));
                        }
                    });
                }
            });
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button(t("editor-file"), |ui| {
            ui.set_min_width(220.0);
            let active = self.tabs.active_id();
            if menu_item(ui, &t("editor-open"), "Ctrl+O", true) {
                self.actions.push(Action::Open);
            }
            if menu_item(ui, &t("editor-save"), "Ctrl+S", active.is_some()) {
                self.actions.extend(active.map(Action::Save));
            }
            if menu_item(ui, &t("editor-save-as"), "Alt+S", active.is_some()) {
                self.actions.extend(active.map(Action::SaveAs));
            }
            ui.separator();
            ui.menu_button(t("editor-recent"), |ui| {
                ui.set_min_width(320.0);
                let recent = settings::get().editor.recent_files.clone();
                if recent.is_empty() {
                    ui.add_enabled(false, egui::Button::new(t("editor-recent-empty")));
                }
                for p in recent {
                    let name = file_name(Path::new(&p));
                    if ui.button(name).on_hover_text(&p).clicked() {
                        self.actions.push(Action::OpenRecent(p));
                        ui.close_menu();
                    }
                }
                ui.separator();
                if ui.button(t("editor-recent-clear")).clicked() {
                    self.actions.push(Action::ClearRecent);
                    ui.close_menu();
                }
            });
            ui.separator();
            if menu_item(ui, &t("editor-close-tab"), "Ctrl+W", active.is_some()) {
                self.actions.extend(active.map(Action::Close));
            }
            if menu_item(ui, &t("editor-about"), "", true) {
                self.actions.push(Action::About);
            }
        });
    }

    fn zoom_controls(&mut self, ui: &mut egui::Ui, has: bool) {
        let pct = self.tabs.active().map(|d| d.view.percent()).unwrap_or(100);
        if tool_icon(ui, "➖", &t("editor-zoom-out"), "−", has) {
            self.actions.push(Action::Zoom(ZoomCmd::Out));
        }
        ui.add_enabled_ui(has, |ui| {
            ui.menu_button(RichText::new(format!("{pct}%")).monospace(), |ui| {
                for p in [25, 50, 100, 200, 400, 800] {
                    if ui.button(format!("{p}%")).clicked() {
                        self.actions.push(Action::Zoom(ZoomCmd::Set(p)));
                        ui.close_menu();
                    }
                }
            });
        });
        if tool_icon(ui, "➕", &t("editor-zoom-in"), "+", has) {
            self.actions.push(Action::Zoom(ZoomCmd::In));
        }
        if tool_button(ui, "", "1:1", "Ctrl+0", has) {
            self.actions.push(Action::Zoom(ZoomCmd::Actual));
        }
        if tool_button(ui, "", &t("editor-zoom-fit"), "F", has) {
            self.actions.push(Action::Zoom(ZoomCmd::FitWindow));
        }
        if tool_button(ui, "", &t("editor-zoom-fit-width"), "K", has) {
            self.actions.push(Action::Zoom(ZoomCmd::FitWidth));
        }
    }

    fn tab_strip(&mut self, ctx: &egui::Context) {
        if self.tabs.is_empty() {
            return;
        }
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin { left: 8, right: 8, top: 0, bottom: 0 });
        egui::TopBottomPanel::top("tabs").frame(frame).show_separator_line(false).show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let active = self.tabs.active_id();
                        let tabs: Vec<(DocId, String, bool)> =
                            self.tabs.docs.iter().map(|d| (d.id, d.title.clone(), d.dirty)).collect();
                        for (id, title, dirty) in tabs {
                            self.tab(ui, id, &title, dirty, Some(id) == active);
                        }
                    });
                });
        });
    }

    fn tab(&mut self, ui: &mut egui::Ui, id: DocId, title: &str, dirty: bool, selected: bool) {
        let pal = self.pal;
        let font = egui::FontId::proportional(13.5);
        let text_color = if selected { ui.visuals().strong_text_color() } else { pal.muted_text };
        let shown: String = if title.chars().count() > 32 {
            format!("{}…", title.chars().take(31).collect::<String>())
        } else {
            title.to_string()
        };
        let galley = ui.painter().layout_no_wrap(shown, font, text_color);
        let (pad, close, h) = (12.0, 18.0, 32.0);
        let dot_w = if dirty { 12.0 } else { 0.0 };
        let w = pad + galley.size().x + dot_w + 8.0 + close + pad * 0.6;
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - pad * 0.6 - close / 2.0, rect.center().y),
            vec2(close, close),
        );
        let close_resp = ui.interact(close_rect, Id::new(("tab-close", id)), Sense::click());
        let resp = resp.on_hover_text(title);

        let fill = if selected {
            pal.tab_active
        } else if resp.hovered() || close_resp.hovered() {
            pal.tab_hover
        } else {
            Color32::TRANSPARENT
        };
        let radius = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };
        let p = ui.painter();
        p.rect_filled(rect, radius, fill);
        if selected {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 6.0, rect.top()),
                egui::pos2(rect.right() - 6.0, rect.top() + 2.5),
            );
            p.rect_filled(bar, CornerRadius::same(2), style::ACCENT);
        }
        let text_pos = egui::pos2(rect.left() + pad, rect.center().y - galley.size().y / 2.0);
        let text_w = galley.size().x;
        p.galley(text_pos, galley, text_color);
        if dirty {
            p.circle_filled(egui::pos2(text_pos.x + text_w + 8.0, rect.center().y), 3.5, pal.dirty);
        }
        if close_resp.hovered() {
            p.rect_filled(close_rect, CornerRadius::same(4), ui.visuals().widgets.hovered.weak_bg_fill);
        }
        if selected || resp.hovered() || close_resp.hovered() {
            let c = close_rect.center();
            let s = 4.0;
            let stroke = Stroke::new(
                1.4_f32,
                if close_resp.hovered() { ui.visuals().strong_text_color() } else { pal.muted_text },
            );
            p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
            p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
        }

        if close_resp.on_hover_text(format!("{} (Ctrl+W)", t("editor-close-tab"))).clicked() || resp.middle_clicked() {
            self.actions.push(Action::Close(id));
        } else if resp.clicked() {
            self.actions.push(Action::Activate(id));
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.bar).inner_margin(Margin::symmetric(12, 4));
        egui::TopBottomPanel::bottom("status").frame(frame).exact_height(28.0).show_separator_line(false).show(
            ctx,
            |ui| {
                ui.horizontal_centered(|ui| {
                    let muted = self.pal.muted_text;
                    if let Some(d) = self.tabs.active() {
                        ui.label(RichText::new(format!("{} × {} px", d.image.width, d.image.height)).color(muted));
                        ui.label(RichText::new("·").color(muted));
                        ui.label(RichText::new(format!("{}%", d.view.percent())).color(muted));
                        ui.label(RichText::new("·").color(muted));
                        match &d.path {
                            Some(p) => ui.label(RichText::new(p.display().to_string()).color(muted)),
                            None => ui.label(RichText::new(t("editor-unsaved")).color(muted).italics()),
                        };
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let saving = self.tabs.docs.iter().any(|d| d.saving);
                        if saving || self.loading > 0 {
                            let key = if saving { "editor-saving" } else { "editor-loading" };
                            ui.label(RichText::new(t(key)).color(muted));
                            ui.add(egui::Spinner::new().size(14.0));
                        }
                    });
                });
            },
        );
    }

    fn central(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::new().fill(self.pal.canvas);
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let ppp = ctx.pixels_per_point();
            self.view_px = [ui.available_width() * ppp, ui.available_height() * ppp];
            let space = ctx.input(|i| i.key_down(Key::Space)) && !ctx.wants_keyboard_input() && self.modal.is_none();
            let checker = self.checker.clone();
            if let Some(doc) = self.tabs.active_mut() {
                let out = canvas::show(ui, doc, &checker, space);
                if out.painted {
                    if let Some(q) = doc.opened_qpc.take() {
                        self.present_pending = Some(q);
                        ctx.request_repaint();
                    }
                }
            } else {
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
            ui.add_space((ui.available_height() / 2.0 - 90.0).max(16.0));
            ui.label(RichText::new("🖼").size(48.0).color(self.pal.muted_text));
            ui.add_space(8.0);
            ui.label(RichText::new(t("editor-empty-title")).size(20.0).strong());
            ui.label(RichText::new(t("editor-empty-hint")).color(self.pal.muted_text));
            ui.add_space(12.0);
            if ui.add(style::primary_button(format!("📂  {}", t("editor-open"))).min_size(vec2(140.0, 32.0))).clicked()
            {
                self.actions.push(Action::Open);
            }
        });
    }

    // ------------------------------------------------------------------ overlays

    fn modals(&mut self, ctx: &egui::Context) {
        match self.modal {
            Some(Modal::ConfirmClose(id)) => self.confirm_close(ctx, id),
            Some(Modal::About) => self.about(ctx),
            None => {}
        }
    }

    fn confirm_close(&mut self, ctx: &egui::Context, id: DocId) {
        let Some(title) = self.tabs.docs.iter().find(|d| d.id == id).map(|d| d.title.clone()) else {
            self.modal = None;
            return;
        };
        let mut choice = None;
        let resp = egui::Modal::new(Id::new("confirm-close")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new(t("editor-close-prompt-title")).size(17.0).strong());
            ui.add_space(4.0);
            ui.label(tf("editor-close-prompt", &[("name", &title)]));
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(style::primary_button(t("editor-save")).min_size(vec2(84.0, 28.0))).clicked() {
                    choice = Some(0);
                }
                if ui.add(egui::Button::new(t("editor-discard")).min_size(vec2(84.0, 28.0))).clicked() {
                    choice = Some(1);
                }
                if ui.add(egui::Button::new(t("editor-cancel")).min_size(vec2(84.0, 28.0))).clicked() {
                    choice = Some(2);
                }
            });
        });
        if resp.should_close() && choice.is_none() {
            choice = Some(2);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            choice = Some(0);
        }
        match choice {
            Some(0) => {
                self.modal = None;
                self.save(id, false, true);
            }
            Some(1) => {
                self.modal = None;
                self.actions.push(Action::Discard(id));
            }
            Some(_) => self.modal = None,
            None => {}
        }
    }

    fn about(&mut self, ctx: &egui::Context) {
        if self.about_icon.is_none() {
            self.about_icon = style::app_icon().map(|i| {
                let ci = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
                ctx.load_texture("about-icon", ci, egui::TextureOptions::LINEAR)
            });
        }
        let icon = self.about_icon.clone();
        let mut close = false;
        let resp = egui::Modal::new(Id::new("about")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.vertical_centered(|ui| {
                if let Some(icon) = &icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(64.0, 64.0)));
                }
                ui.add_space(4.0);
                ui.label(RichText::new(t("app-name")).size(22.0).strong());
                ui.label(
                    RichText::new(tf("editor-version", &[("version", env!("CARGO_PKG_VERSION"))]))
                        .color(self.pal.muted_text),
                );
                ui.add_space(6.0);
                ui.label(t("app-tagline"));
                ui.add_space(10.0);
                if ui.link(t("app-credit")).on_hover_text("https://zheer.dev").clicked() {
                    win::shell_open("https://zheer.dev");
                }
                ui.add_space(14.0);
                if ui.add(style::primary_button(t("editor-close")).min_size(vec2(96.0, 28.0))).clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.modal = None;
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
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
        self.poll_jobs();
        self.handle_close_request(ctx);
        if self.modal.is_none() {
            self.shortcuts(ctx);
            self.handle_drops(ctx);
        }
        self.top_bar(ctx);
        self.tab_strip(ctx);
        self.status_bar(ctx);
        self.central(ctx);
        self.modals(ctx);
        self.toasts(ctx);
        self.process_actions(ctx);
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

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// `settings.editor.last_folder` if it still exists, else the output folder.
fn default_folder() -> PathBuf {
    let s = settings::get();
    let last = s.editor.last_folder.trim();
    if !last.is_empty() && Path::new(last).is_dir() {
        PathBuf::from(last)
    } else {
        s.output_folder()
    }
}

fn remember_folder(dir: &Path) {
    let d = dir.to_string_lossy().into_owned();
    if settings::get().editor.last_folder != d {
        settings::update(|s| s.editor.last_folder = d);
    }
}

fn remember_recent(path: &Path) {
    let p = path.to_string_lossy().into_owned();
    settings::update(|s| push_recent(&mut s.editor.recent_files, &p, MAX_RECENT));
}

fn separator(ui: &mut egui::Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(9.0, 20.0), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0_f32, pal.border));
}

/// Toolbar button with icon + label and a tooltip that shows the shortcut.
fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str, shortcut: &str, enabled: bool) -> bool {
    let text = if icon.is_empty() { label.to_string() } else { format!("{icon}  {label}") };
    let tip = if shortcut.is_empty() { label.to_string() } else { format!("{label} ({shortcut})") };
    ui.add_enabled(enabled, egui::Button::new(text).min_size(vec2(0.0, 28.0))).on_hover_text(tip).clicked()
}

/// Icon-only toolbar button.
fn tool_icon(ui: &mut egui::Ui, icon: &str, label: &str, shortcut: &str, enabled: bool) -> bool {
    let tip = format!("{label} ({shortcut})");
    ui.add_enabled(enabled, egui::Button::new(icon).min_size(vec2(28.0, 28.0))).on_hover_text(tip).clicked()
}

fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let btn = egui::Button::new(label).shortcut_text(shortcut);
    let clicked = ui.add_enabled(enabled, btn).clicked();
    if clicked {
        ui.close_menu();
    }
    clicked
}
