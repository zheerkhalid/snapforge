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

mod app;
mod canvas;
mod dialogs;
mod document;
mod jobs;
mod style;
mod texture;
mod view;
mod window;

use crate::common::time::qpc_now;
use crate::common::Image;
use crate::settings::Destination;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Bench support: QPC timestamp of the first frame presented after the most recent
/// [`open_image`] call.
pub static LAST_IMAGE_PRESENTED_QPC: AtomicI64 = AtomicI64::new(0);

/// Messages from any thread to the editor thread.
pub(crate) enum EditorMsg {
    Open { img: Image, title: String, path: Option<PathBuf>, continue_to: Option<Destination>, qpc: i64 },
    OpenFiles(Vec<PathBuf>),
    Show,
    Shutdown,
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
    send(EditorMsg::Open { img, title, path, continue_to, qpc }, true);
}

/// Open files from disk in tabs.
pub fn open_files(paths: Vec<PathBuf>) {
    send(EditorMsg::OpenFiles(paths), true);
}

/// Show the editor (empty if no tabs).
pub fn show() {
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
