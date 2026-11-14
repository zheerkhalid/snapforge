//! Application core: the command bus and the UI-thread controller.
//!
//! Every module (tray, toolbar, hotkeys, editor, CLI forwarding, worker threads) talks to
//! the app by calling [`post`] with an [`AppCommand`]. Commands are queued and executed on
//! the UI thread, in order.

use crate::common::{Image, Rect};
use crate::settings::{CaptureMode, Destination};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

/// Posted to the app's message window when the command queue is non-empty.
pub const WM_APP_COMMAND: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// Where a finished capture came from (used for "repeat last capture").
#[derive(Debug, Clone)]
pub enum CaptureSource {
    Screen(Rect),
    Polygon(Vec<crate::common::Point>),
    MultiRect(Vec<Rect>),
    Window(isize),
    Scrolling,
    Other,
}

/// A finished capture travelling to the output pipeline.
pub struct CaptureOutcome {
    pub image: Image,
    pub mode: CaptureMode,
    pub source: CaptureSource,
    /// Overrides (CLI / auto-capture); `None` = use settings.
    pub dest: Option<Destination>,
    pub out_path: Option<PathBuf>,
}

/// One-shot request with overrides (CLI automation, auto capture).
#[derive(Debug, Clone, Default)]
pub struct CaptureRequest {
    pub mode: CaptureMode,
    pub delay_secs: Option<u32>,
    pub dest: Option<Destination>,
    pub out_path: Option<PathBuf>,
    /// Exit the process after the output finished (standalone CLI run).
    pub exit_after: bool,
}

pub enum AppCommand {
    /// Start a capture with the current settings.
    Capture(CaptureMode),
    /// Start a capture with explicit overrides.
    CaptureWith(CaptureRequest),
    RepeatLast,
    ToggleAutoCapture,
    SetDestination(Destination),
    SetDelay(u32),
    ToggleAutoCopy,
    TogglePreview,
    ToggleTimestamp,
    ShowToolbar(bool),
    ToggleToolbar,
    OpenEditor,
    OpenSettings,
    OpenOutputFolder,
    About,
    /// Settings changed elsewhere: re-register hotkeys, refresh toolbar/tray.
    SettingsChanged,
    /// A capture finished on a worker thread (e.g. scrolling capture).
    Captured(Box<CaptureOutcome>),
    /// Send an image straight to a destination (editor "Continue to…", CLI).
    Deliver {
        image: Image,
        dest: Destination,
    },
    /// Show a tray notification.
    Notify {
        title: String,
        text: String,
        is_error: bool,
    },
    /// Command-line arguments forwarded from a second instance.
    ForwardedArgs(Vec<String>),
    Exit,
}

static QUEUE: Mutex<VecDeque<AppCommand>> = Mutex::new(VecDeque::new());
static APP_HWND: AtomicIsize = AtomicIsize::new(0);

/// Queue a command for the UI thread. Callable from any thread.
pub fn post(cmd: AppCommand) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push_back(cmd);
    }
    wake();
}

/// Convenience: post a notification.
pub fn notify(title: impl Into<String>, text: impl Into<String>, is_error: bool) {
    post(AppCommand::Notify { title: title.into(), text: text.into(), is_error });
}

fn wake() {
    let h = APP_HWND.load(Ordering::Acquire);
    if h != 0 {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        // SAFETY: posting to our own message window.
        unsafe {
            let _ = PostMessageW(crate::common::win::hwnd_from_isize(h), WM_APP_COMMAND, WPARAM(0), LPARAM(0));
        }
    }
}

pub(crate) fn set_app_hwnd(h: isize) {
    APP_HWND.store(h, Ordering::Release);
    wake();
}

pub fn app_hwnd() -> isize {
    APP_HWND.load(Ordering::Acquire)
}

pub(crate) fn take_commands() -> Vec<AppCommand> {
    QUEUE.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}
