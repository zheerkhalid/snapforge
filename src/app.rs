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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordTarget {
    FullScreen,
    Window,
    Region,
    FixedRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenTool {
    ColorPicker,
    Magnifier,
    Crosshair,
    Focus,
    Ruler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolWindow {
    CombineImages,
    ImagesToPdf,
    BatchPrint,
    ConvertToMp4,
    ScheduleRecording,
    Settings,
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
    /// Show a tray notification. Clicking it opens `file` (a video in the video editor,
    /// anything else revealed in Explorer).
    Notify {
        title: String,
        text: String,
        is_error: bool,
        file: Option<PathBuf>,
    },
    /// Command-line arguments forwarded from a second instance.
    ForwardedArgs(Vec<String>),

    // ---- Phase 4: recorder
    /// Start a screen recording (region modes show a selection overlay first).
    Record(RecordTarget),
    StopRecording,
    TogglePauseRecording,
    /// Open the video editor, optionally with a file.
    OpenVideoEditor(Option<PathBuf>),

    // ---- Phase 5: pin, OCR, screen tools
    /// Select a region and recognize its text (OCR).
    CaptureText,
    /// Select a region and pin it to the screen.
    PinCapture,
    /// Pin an existing image (editor, clipboard).
    PinImage(Image),
    PinFromClipboard,
    ScreenTool(ScreenTool),

    // ---- Phase 2/4/6: tools and windows
    OpenTool(ToolWindow),
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
    post(AppCommand::Notify { title: title.into(), text: text.into(), is_error, file: None });
}

/// Post a notification about a saved file; clicking it opens the file (see [`AppCommand::Notify`]).
pub fn notify_file(title: impl Into<String>, text: impl Into<String>, file: PathBuf) {
    post(AppCommand::Notify { title: title.into(), text: text.into(), is_error: false, file: Some(file) });
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
