//! Image editor (egui/eframe, glow backend) on its own thread.
//!
//! CONTRACT:
//! * The editor thread and window are created lazily on first use, hidden (not destroyed)
//!   when closed, and reused, so the second open is instant.
//! * All functions are callable from any thread and never block.
//!
//! Layout:
//! * `mod.rs`      – public contract, editor thread lifetime, message channel
//! * `app.rs`      – the eframe `App`: panels, shortcuts, commands
//! * `document.rs` – `Document` (image, path, dirty, title) and `Tabs`
//! * `view.rs`     – zoom / pan math (physical pixels)
//! * `canvas.rs`   – painting the image (tiles, checkerboard) and canvas input
//! * `texture.rs`  – BGRA → RGBA swizzle, texture tiling
//! * `dialogs.rs`  – IFileOpenDialog / IFileSaveDialog
//! * `jobs.rs`     – background decode / save / clipboard
//! * `style.rs`    – visuals, accent colour, app icon
//! * `window.rs`   – Win32 show / placement helpers
//!
//! Phase 2:
//! * `history.rs`      – undo / redo steps with memory budget and background compression
//! * `selection.rs`    – selections, screen ↔ image mapping, floating pixels, edge resize
//! * `commands.rs`     – the command registry (menus, toolbar, shortcuts, palette), fuzzy matcher
//! * `effects.rs`      – the effect table and parameter schema ([`EffectRegistry`])
//! * `param_dialog.rs` – schema-driven effect dialog with debounced proxy preview
//! * `ext.rs`          – extension traits: [`CanvasTool`], [`DocOverlay`], [`ToolsHost`]
//! * `menus.rs`        – menu bar, ribbon toolbar, workspace strip
//! * `modals.rs`       – dialogs (properties, delete, sequence save, editors, scan…)
//! * `palette.rs`      – the command palette (Ctrl+K)
//! * `fileops.rs`      – Recycle Bin, file properties, sequence names, file watcher
//! * `scan.rs`, `scan_post.rs` – WIA 2.0 acquisition and scan post-processing
//!
//! EXTENSION (integration pass): [`register_effects`], [`register_canvas_tools`],
//! [`set_tools_host`]; effects edit pixels through `Document::apply_image_edit`,
//! `apply_region_edit`, `commit_image` and `commit_region` (one undo step each).

mod annot;
mod app;
mod canvas;
mod commands;
mod dialogs;
mod document;
mod effects;
mod ext;
mod fileops;
mod fx_install;
mod fx_session;
mod history;
#[cfg(test)]
mod integration_tests;
mod jobs;
mod menus;
mod modals;
mod palette;
mod param_dialog;
mod scan;
mod scan_post;
mod screen_pick;
mod selection;
mod style;
mod texture;
mod tools_dialogs;
mod view;
mod window;

// The extension surface for the integration pass (effects, draw, tools); unused until then.
#[allow(unused_imports)]
pub use document::Document;
#[allow(unused_imports)]
pub use effects::{
    downscale_proxy, ApplyFn, EffectCtx, EffectDef, EffectRegistry, MenuSlot, ParamDef, ParamKind, ParamValue,
    ParamValues, Scope,
};
#[allow(unused_imports)]
pub use ext::{CanvasTool, DocOverlay, ToolInput, ToolsContext, ToolsHost};
#[allow(unused_imports)]
pub use history::{Budget, StepKind};
#[allow(unused_imports)]
pub use selection::{CanvasMap, Selection};

use crate::common::time::qpc_now;
use crate::common::Image;
use crate::settings::Destination;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Installs effects into the table (called on the editor thread, in registration order).
pub type EffectInstaller = fn(&mut EffectRegistry);
/// Creates the Phase 3 canvas tools.
pub type ToolFactory = fn() -> Vec<Box<dyn CanvasTool>>;

struct Extensions {
    effects: Vec<EffectInstaller>,
    tools: Vec<ToolFactory>,
    host: Option<Arc<dyn ToolsHost>>,
}

static EXT: Mutex<Extensions> = Mutex::new(Extensions { effects: Vec::new(), tools: Vec::new(), host: None });
/// Bumped on every registration so a running editor rebuilds its menus.
static EXT_GEN: AtomicU64 = AtomicU64::new(1);

fn ext_changed() {
    EXT_GEN.fetch_add(1, Ordering::AcqRel);
    let ctx = SHARED.ctx.lock().ok().and_then(|c| c.clone());
    if let Some(ctx) = ctx {
        ctx.request_repaint();
    }
}

/// Register effects for the Image / Effects menus. `f` runs on the editor thread
/// whenever the table is (re)built; an effect replaces the placeholder with its id.
pub fn register_effects(f: EffectInstaller) {
    if let Ok(mut e) = EXT.lock() {
        e.effects.push(f);
    }
    ext_changed();
}

/// Register canvas tools (Phase 3 draw tools). They appear on the ribbon, in the Tools
/// menu and in the command palette.
pub fn register_canvas_tools(f: ToolFactory) {
    if let Ok(mut e) = EXT.lock() {
        e.tools.push(f);
    }
    ext_changed();
}

/// Replace the handler of Tools ▸ Combine Images / Convert Images to PDF / Batch Print.
pub fn set_tools_host(h: Arc<dyn ToolsHost>) {
    if let Ok(mut e) = EXT.lock() {
        e.host = Some(h);
    }
    ext_changed();
}

pub(crate) fn ext_generation() -> u64 {
    EXT_GEN.load(Ordering::Acquire)
}

/// The effect table with every registered installer applied.
pub(crate) fn build_effects() -> EffectRegistry {
    let mut r = EffectRegistry::with_placeholders();
    let installers = EXT.lock().map(|e| e.effects.clone()).unwrap_or_default();
    for f in installers {
        f(&mut r);
    }
    r
}

pub(crate) fn build_tools() -> Vec<Box<dyn CanvasTool>> {
    let factories = EXT.lock().map(|e| e.tools.clone()).unwrap_or_default();
    factories.into_iter().flat_map(|f| f()).collect()
}

pub(crate) fn tools_host() -> Arc<dyn ToolsHost> {
    EXT.lock().ok().and_then(|e| e.host.clone()).unwrap_or_else(|| Arc::new(ext::DefaultToolsHost))
}

/// Bench support: QPC timestamp of the first frame presented after the most recent
/// [`open_image`] call.
pub static LAST_IMAGE_PRESENTED_QPC: AtomicI64 = AtomicI64::new(0);

/// Messages from any thread to the editor thread.
pub(crate) enum EditorMsg {
    Open {
        img: Image,
        title: String,
        path: Option<PathBuf>,
        continue_to: Option<Destination>,
        qpc: i64,
        /// Start with the default draw tool (the "Draw" destination).
        draw: bool,
    },
    OpenFiles(Vec<PathBuf>),
    Show,
    Shutdown,
    /// The Settings window (on a tab); the editor window itself stays as it is.
    OpenSettings(Option<crate::settings_ui::Tab>),
    /// A Tools dialog, with the open images when it came from the editor.
    OpenTool(tools_dialogs::ToolKind, Option<ToolsContext>),
    /// HOOK(video-editor): open the video editor window (see [`open_video_editor`]).
    OpenVideo(Option<PathBuf>),
}

/// Built-in registrations (effects, draw tools, clone stamp, tools host), once per process.
fn register_builtins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        register_effects(fx_install::install);
        register_effects(fx_session::install);
        register_canvas_tools(annot::draw_tools);
        register_canvas_tools(|| vec![Box::new(fx_session::CloneTool::new())]);
        let has_host = EXT.lock().map(|e| e.host.is_some()).unwrap_or(false);
        if !has_host {
            set_tools_host(Arc::new(EditorToolsHost));
        }
    });
}

/// Tools ▸ Combine / PDF / Batch Print open the editor's own dialogs.
struct EditorToolsHost;

impl ToolsHost for EditorToolsHost {
    fn combine_images(&self, ctx: ToolsContext) {
        send(EditorMsg::OpenTool(tools_dialogs::ToolKind::Combine, Some(ctx)), true);
    }
    fn images_to_pdf(&self, ctx: ToolsContext) {
        send(EditorMsg::OpenTool(tools_dialogs::ToolKind::Pdf, Some(ctx)), true);
    }
    fn batch_print(&self, ctx: ToolsContext) {
        send(EditorMsg::OpenTool(tools_dialogs::ToolKind::BatchPrint, Some(ctx)), true);
    }
}

/// The editor window should start hidden (the Settings window was asked for first).
static START_HIDDEN: AtomicBool = AtomicBool::new(false);

pub(crate) fn start_hidden() -> bool {
    START_HIDDEN.load(Ordering::Acquire)
}

struct Shared {
    /// `Some` while the editor thread is alive. Messages sent before the egui context
    /// exists simply wait in the channel and are drained by the first frame.
    tx: Mutex<Option<Sender<EditorMsg>>>,
    ctx: Mutex<Option<egui::Context>>,
    hwnd: AtomicIsize,
    running: AtomicBool,
    exited: Mutex<bool>,
    exited_cv: Condvar,
}

static SHARED: Shared = Shared {
    tx: Mutex::new(None),
    ctx: Mutex::new(None),
    hwnd: AtomicIsize::new(0),
    running: AtomicBool::new(false),
    exited: Mutex::new(false),
    exited_cv: Condvar::new(),
};

/// Called by the app once the egui context and window exist.
pub(crate) fn register_window(ctx: egui::Context, hwnd: isize) {
    SHARED.hwnd.store(hwnd, Ordering::Release);
    if let Ok(mut c) = SHARED.ctx.lock() {
        *c = Some(ctx);
    }
}

pub(crate) fn editor_hwnd() -> isize {
    SHARED.hwnd.load(Ordering::Acquire)
}

fn spawn_thread() -> Option<Sender<EditorMsg>> {
    register_builtins();
    let (tx, rx) = channel();
    if let Ok(mut e) = SHARED.exited.lock() {
        *e = false;
    }
    SHARED.running.store(true, Ordering::Release);
    let res = std::thread::Builder::new().name("editor".into()).spawn(move || {
        let t0 = qpc_now();
        if let Err(e) = app::run(rx, t0) {
            log::error!("editor: event loop failed: {e}");
        }
        log::info!("editor: thread exited");
        SHARED.running.store(false, Ordering::Release);
        SHARED.hwnd.store(0, Ordering::Release);
        if let Ok(mut c) = SHARED.ctx.lock() {
            *c = None;
        }
        if let Ok(mut t) = SHARED.tx.lock() {
            *t = None;
        }
        if let Ok(mut e) = SHARED.exited.lock() {
            *e = true;
        }
        SHARED.exited_cv.notify_all();
    });
    match res {
        Ok(_) => Some(tx),
        Err(e) => {
            log::error!("editor: cannot start thread: {e}");
            SHARED.running.store(false, Ordering::Release);
            None
        }
    }
}

/// Queue `msg`, starting the editor thread if needed, then wake (and optionally show)
/// the window.
fn send(msg: EditorMsg, show: bool) {
    {
        let Ok(mut guard) = SHARED.tx.lock() else { return };
        if guard.is_none() {
            *guard = spawn_thread();
        }
        if let Some(tx) = guard.as_ref() {
            if tx.send(msg).is_err() {
                log::warn!("editor: thread is gone; message dropped");
                return;
            }
        }
    }
    wake(show);
}

fn wake(show: bool) {
    let ctx = SHARED.ctx.lock().ok().and_then(|c| c.clone());
    if let Some(ctx) = ctx {
        if show {
            // A hidden window gets no paint messages, so update() would never run to
            // drain the channel: show it directly from this thread.
            window::show_and_focus(editor_hwnd());
        }
        ctx.request_repaint();
    }
    // No context yet: the first frame drains the channel and eframe shows the window.
}

/// Open an image in a new tab and bring the editor to the front.
/// `continue_to`: when previewing, show a "Continue to <dest>" button that sends the
/// (possibly edited) image on via `AppCommand::Deliver`.
pub fn open_image(img: Image, title: String, path: Option<PathBuf>, continue_to: Option<Destination>) {
    let qpc = qpc_now();
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::Open { img, title, path, continue_to, qpc, draw: false }, true);
}

/// Open an image with the draw tools active (the "Draw" destination): the default draw
/// tool (Settings ▸ Draw, or right-click a palette tool) is selected.
pub fn open_image_draw(img: Image, title: String) {
    let qpc = qpc_now();
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::Open { img, title, path: None, continue_to: None, qpc, draw: true }, true);
}

/// Open the Settings window. Starts the editor thread if needed; the editor's main window
/// stays hidden if it was (the Settings window is its own viewport).
pub fn open_settings() {
    open_settings_tab(None);
}

/// Settings ▸ About.
pub fn open_about() {
    open_settings_tab(Some(crate::settings_ui::Tab::About));
}

fn open_settings_tab(tab: Option<crate::settings_ui::Tab>) {
    if !is_running() {
        START_HIDDEN.store(true, Ordering::Release);
    }
    send(EditorMsg::OpenSettings(tab), false);
    // A hidden editor window runs no frames: pump one off-screen.
    window::show_ghost(editor_hwnd());
}

/// Tools ▸ Combine Images from outside the editor (tray, toolbar).
pub fn open_combine_images() {
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::OpenTool(tools_dialogs::ToolKind::Combine, None), true);
}

/// Tools ▸ Convert Images to PDF from outside the editor.
pub fn open_images_to_pdf() {
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::OpenTool(tools_dialogs::ToolKind::Pdf, None), true);
}

/// Tools ▸ Batch Print from outside the editor.
pub fn open_batch_print() {
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::OpenTool(tools_dialogs::ToolKind::BatchPrint, None), true);
}

/// HOOK(video-editor): open the video editor (a `VideoEditorWindow` viewport hosted by the
/// editor app, like the Settings window: `open(Option<PathBuf>)`, `ui(ctx)`, `is_open()`).
/// The final integration pass fills in `app::VideoEditorHost`.
#[allow(dead_code)]
pub fn open_video_editor(path: Option<PathBuf>) {
    if !is_running() {
        START_HIDDEN.store(true, Ordering::Release);
    }
    send(EditorMsg::OpenVideo(path), false);
    window::show_ghost(editor_hwnd());
}

/// Open files from disk in tabs.
pub fn open_files(paths: Vec<PathBuf>) {
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::OpenFiles(paths), true);
}

/// Show the editor (empty if no tabs).
pub fn show() {
    START_HIDDEN.store(false, Ordering::Release);
    send(EditorMsg::Show, true);
}

pub fn is_running() -> bool {
    SHARED.running.load(Ordering::Acquire)
}

/// Ask the editor thread to exit (app shutdown). Waits briefly for a clean exit (window
/// placement saved, GL context destroyed) but never hangs: the process may simply exit.
pub fn shutdown() {
    if !is_running() {
        return;
    }
    let sent = SHARED.tx.lock().ok().and_then(|t| t.as_ref().map(|tx| tx.send(EditorMsg::Shutdown).is_ok()));
    if sent != Some(true) {
        return;
    }
    wake(false);
    if !window::is_visible(editor_hwnd()) {
        // A hidden window runs no frames, so it cannot process the request; its placement
        // was saved when it was hidden. Let process exit tear it down.
        return;
    }
    let Ok(guard) = SHARED.exited.lock() else { return };
    let res = SHARED.exited_cv.wait_timeout_while(guard, Duration::from_millis(1500), |done| !*done);
    if matches!(res, Ok((_, t)) if t.timed_out()) {
        log::warn!("editor: did not exit in time; leaving it to process exit");
    }
}
