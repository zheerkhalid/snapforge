//! Popup menus shared by the tray icon and the toolbar.
//!
//! Menus are tracked with `TPM_RETURNCMD`, so the chosen id is dispatched right here,
//! regardless of which window opened the menu.

use crate::app::{self, AppCommand, RecordTarget, ScreenTool, ToolWindow};
use crate::common::win::{pcwstr, wide};
use crate::i18n::{t, tf};
use crate::settings::{self, CaptureMode, Destination};
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, DestroyMenu, GetForegroundWindow, IsWindow, PostMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HMENU, MENU_ITEM_FLAGS, MF_BYCOMMAND, MF_CHECKED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, TPMPARAMS, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, TPM_VERTICAL,
    WM_NULL,
};

pub const DELAYS: [u32; 6] = [0, 1, 2, 3, 5, 10];

const ID_MODE: u32 = 100; // + mode index
const ID_REPEAT: u32 = 120;
const ID_DELAY: u32 = 200; // + delay index
const ID_DEST: u32 = 300; // + destination index
const ID_PREVIEW: u32 = 320;
const ID_AUTOCOPY: u32 = 321;
const ID_TIMESTAMP: u32 = 322;
const ID_AUTO_CAPTURE: u32 = 400;
const ID_TOOLBAR: u32 = 401;
const ID_EDITOR: u32 = 402;
const ID_FOLDER: u32 = 403;
const ID_SETTINGS: u32 = 404;
const ID_AUTOSTART: u32 = 405;
const ID_ABOUT: u32 = 406;
const ID_EXIT: u32 = 407;
const ID_EXTRA: u32 = 500; // + index in EXTRA

/// Recorder and tool entries: (fluent key, command factory, submenu index 0=record 1=tools).
/// (fluent key, command factory, submenu: 0 = record, 1 = tools)
type ExtraItem = (&'static str, fn() -> AppCommand, u8);

const EXTRA: &[ExtraItem] = &[
    ("menu-record-fullscreen", || AppCommand::Record(RecordTarget::FullScreen), 0),
    ("menu-record-window", || AppCommand::Record(RecordTarget::Window), 0),
    ("menu-record-region", || AppCommand::Record(RecordTarget::Region), 0),
    ("menu-record-fixed", || AppCommand::Record(RecordTarget::FixedRegion), 0),
    ("menu-record-pause", || AppCommand::TogglePauseRecording, 0),
    ("menu-record-stop", || AppCommand::StopRecording, 0),
    ("menu-record-schedule", || AppCommand::OpenTool(ToolWindow::ScheduleRecording), 0),
    ("menu-video-editor", || AppCommand::OpenVideoEditor(None), 0),
    ("menu-convert-mp4", || AppCommand::OpenTool(ToolWindow::ConvertToMp4), 0),
    ("menu-capture-text", || AppCommand::CaptureText, 1),
    ("menu-pin-capture", || AppCommand::PinCapture, 1),
    ("menu-pin-clipboard", || AppCommand::PinFromClipboard, 1),
    ("menu-color-picker", || AppCommand::ScreenTool(ScreenTool::ColorPicker), 1),
    ("menu-magnifier", || AppCommand::ScreenTool(ScreenTool::Magnifier), 1),
    ("menu-crosshair", || AppCommand::ScreenTool(ScreenTool::Crosshair), 1),
    ("menu-focus", || AppCommand::ScreenTool(ScreenTool::Focus), 1),
    ("menu-ruler", || AppCommand::ScreenTool(ScreenTool::Ruler), 1),
    ("menu-combine", || AppCommand::OpenTool(ToolWindow::CombineImages), 1),
    ("menu-to-pdf", || AppCommand::OpenTool(ToolWindow::ImagesToPdf), 1),
    ("menu-batch-print", || AppCommand::OpenTool(ToolWindow::BatchPrint), 1),
];

fn build_extra(group: u8) -> Option<Menu> {
    let m = Menu::new()?;
    for (i, (key, _, g)) in EXTRA.iter().enumerate() {
        if *g == group {
            m.item(ID_EXTRA + i as u32, &t(key), false);
        }
    }
    Some(m)
}

/// First id free for callers that add their own items (e.g. the toolbar's context menu).
pub const ID_USER: u32 = 1000;

/// An owned popup menu, destroyed on drop (submenus are destroyed with their parent).
pub struct Menu(pub HMENU);

impl Menu {
    pub fn new() -> Option<Menu> {
        // SAFETY: plain creation; destroyed in Drop.
        unsafe { CreatePopupMenu().ok().map(Menu) }
    }

    pub fn item(&self, id: u32, label: &str, checked: bool) {
        let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
        self.append(flags, id as usize, label);
    }

    /// Mark `checked` with a radio bullet within the id range `first..=last`.
    pub fn radio_group(&self, first: u32, last: u32, checked: u32) {
        // SAFETY: valid menu handle; ids were appended by this menu.
        unsafe {
            let _ = CheckMenuRadioItem(self.0, first, last, checked, MF_BYCOMMAND.0);
        }
    }

    pub fn separator(&self) {
        // SAFETY: valid menu handle.
        unsafe {
            let _ = AppendMenuW(self.0, MF_SEPARATOR, 0, None);
        }
    }

    /// Append `sub` as a submenu; ownership moves into this menu.
    pub fn submenu(&self, sub: Menu, label: &str) {
        let h = sub.0;
        std::mem::forget(sub);
        self.append(MF_POPUP | MF_STRING, h.0 as usize, label);
    }

    fn append(&self, flags: MENU_ITEM_FLAGS, id: usize, label: &str) {
        let w = wide(label);
        // SAFETY: valid menu handle and NUL-terminated label (copied by the call).
        unsafe {
            let _ = AppendMenuW(self.0, flags, id, pcwstr(&w));
        }
    }

    /// Show the menu at a screen point (keeping `exclude` uncovered) and return the chosen id (0 = none).
    pub fn track(&self, x: i32, y: i32, exclude: Option<RECT>) -> u32 {
        track(self.0, x, y, exclude)
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

fn track(menu: HMENU, x: i32, y: i32, exclude: Option<RECT>) -> u32 {
    let owner = super::owner_hwnd();
    if owner.is_invalid() {
        log::warn!("menu: no owner window");
        return 0;
    }
    // SAFETY: standard "SetForegroundWindow + TrackPopupMenu + WM_NULL" sequence so the
    // menu closes when the user clicks elsewhere; all handles are valid for the call.
    unsafe {
        let prev = GetForegroundWindow();
        let _ = SetForegroundWindow(owner);
        let mut flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let params = exclude.map(|r| TPMPARAMS { cbSize: std::mem::size_of::<TPMPARAMS>() as u32, rcExclude: r });
        if params.is_some() {
            flags |= TPM_VERTICAL;
        }
        let id = TrackPopupMenuEx(menu, flags.0, x, y, owner, params.as_ref().map(|p| p as *const TPMPARAMS));
        let _ = PostMessageW(owner, WM_NULL, WPARAM(0), LPARAM(0));
        // Give focus back to the window the user was working in, so "active window"
        // captures and typing continue where they were.
        if !prev.is_invalid() && prev != owner && IsWindow(prev).as_bool() {
            let _ = SetForegroundWindow(prev);
        }
        id.0 as u32
    }
}

fn hotkey_suffix(mode: CaptureMode) -> String {
    let text = crate::hotkeys::display(settings::get().hotkeys.for_mode(mode));
    if text.is_empty() {
        String::new()
    } else {
        format!("\t{text}")
    }
}

pub fn delay_label(secs: u32) -> String {
    if secs == 0 {
        t("delay-none")
    } else {
        tf("delay-secs", &[("secs", &secs.to_string())])
    }
}

pub fn build_delay() -> Option<Menu> {
    let m = Menu::new()?;
    let cur = settings::get().capture.delay_secs;
    let mut sel = None;
    for (i, &d) in DELAYS.iter().enumerate() {
        m.item(ID_DELAY + i as u32, &delay_label(d), false);
        if d == cur {
            sel = Some(i as u32);
        }
    }
    if let Some(i) = sel {
        m.radio_group(ID_DELAY, ID_DELAY + DELAYS.len() as u32 - 1, ID_DELAY + i);
    }
    Some(m)
}

pub fn build_output() -> Option<Menu> {
    let m = Menu::new()?;
    let (dest, preview, autocopy, stamp) = {
        let s = settings::get();
        (s.output.destination, s.output.preview_in_editor, s.output.auto_copy, s.output.timestamp)
    };
    for (i, d) in Destination::ALL.iter().enumerate() {
        m.item(ID_DEST + i as u32, &t(d.label_key()), false);
    }
    if let Some(i) = Destination::ALL.iter().position(|d| *d == dest) {
        m.radio_group(ID_DEST, ID_DEST + Destination::ALL.len() as u32 - 1, ID_DEST + i as u32);
    }
    m.separator();
    m.item(ID_PREVIEW, &t("opt-preview"), preview);
    m.item(ID_AUTOCOPY, &t("opt-autocopy"), autocopy);
    m.item(ID_TIMESTAMP, &t("opt-timestamp"), stamp);
    Some(m)
}

/// The full tray menu.
pub fn build_full() -> Option<Menu> {
    let m = Menu::new()?;
    for (i, mode) in CaptureMode::ALL.iter().enumerate() {
        m.item(ID_MODE + i as u32, &format!("{}{}", t(mode.label_key()), hotkey_suffix(*mode)), false);
    }
    let repeat = crate::hotkeys::display(&settings::get().hotkeys.repeat_last);
    let repeat_label = if repeat.is_empty() { t("repeat-last") } else { format!("{}\t{}", t("repeat-last"), repeat) };
    m.item(ID_REPEAT, &repeat_label, false);
    m.separator();
    if let Some(sub) = build_delay() {
        m.submenu(sub, &t("delay"));
    }
    if let Some(sub) = build_output() {
        m.submenu(sub, &t("output-to"));
    }
    let auto = settings::get().capture.auto_capture_enabled;
    m.item(ID_AUTO_CAPTURE, &t("auto-capture"), auto);
    m.separator();
    if let Some(sub) = build_extra(0) {
        m.submenu(sub, &t("menu-record"));
    }
    if let Some(sub) = build_extra(1) {
        m.submenu(sub, &t("menu-tools"));
    }
    m.separator();
    m.item(ID_TOOLBAR, &t("tray-show-toolbar"), crate::toolbar::is_visible());
    m.item(ID_EDITOR, &t("tray-open-editor"), false);
    m.item(ID_FOLDER, &t("tray-open-folder"), false);
    m.item(ID_SETTINGS, &t("tray-settings"), false);
    m.item(ID_AUTOSTART, &t("tray-start-with-windows"), super::autostart::sync_setting());
    m.separator();
    m.item(ID_ABOUT, &t("tray-about"), false);
    m.item(ID_EXIT, &t("tray-exit"), false);
    Some(m)
}

/// Execute a menu id built by this module. Returns false for unknown ids.
pub fn dispatch(id: u32) -> bool {
    let idx = |base: u32| (id - base) as usize;
    let cmd = match id {
        _ if (ID_MODE..ID_MODE + CaptureMode::ALL.len() as u32).contains(&id) => {
            AppCommand::Capture(CaptureMode::ALL[idx(ID_MODE)])
        }
        ID_REPEAT => AppCommand::RepeatLast,
        _ if (ID_DELAY..ID_DELAY + DELAYS.len() as u32).contains(&id) => AppCommand::SetDelay(DELAYS[idx(ID_DELAY)]),
        _ if (ID_DEST..ID_DEST + Destination::ALL.len() as u32).contains(&id) => {
            AppCommand::SetDestination(Destination::ALL[idx(ID_DEST)])
        }
        ID_PREVIEW => AppCommand::TogglePreview,
        ID_AUTOCOPY => AppCommand::ToggleAutoCopy,
        ID_TIMESTAMP => AppCommand::ToggleTimestamp,
        ID_AUTO_CAPTURE => AppCommand::ToggleAutoCapture,
        ID_TOOLBAR => AppCommand::ToggleToolbar,
        ID_EDITOR => AppCommand::OpenEditor,
        ID_FOLDER => AppCommand::OpenOutputFolder,
        ID_SETTINGS => AppCommand::OpenSettings,
        ID_AUTOSTART => {
            let on = !super::autostart::is_enabled();
            match super::autostart::set_enabled(on) {
                Ok(()) => {
                    settings::update(|s| s.general.start_with_windows = on);
                }
                Err(e) => {
                    log::error!("autostart: {e:#}");
                    app::notify(t("app-name"), tf("notify-error", &[("error", &format!("{e:#}"))]), true);
                }
            }
            return true;
        }
        ID_ABOUT => AppCommand::About,
        ID_EXIT => AppCommand::Exit,
        _ if (ID_EXTRA..ID_EXTRA + EXTRA.len() as u32).contains(&id) => (EXTRA[idx(ID_EXTRA)].1)(),
        _ => return false,
    };
    app::post(cmd);
    true
}

/// Show one of the shared menus at a screen point and dispatch the choice.
pub fn popup(menu: Option<Menu>, x: i32, y: i32, exclude: Option<RECT>) {
    if let Some(m) = menu {
        let id = m.track(x, y, exclude);
        drop(m);
        if id != 0 {
            dispatch(id);
        }
    }
}

pub fn show_full(x: i32, y: i32, exclude: Option<RECT>) {
    popup(build_full(), x, y, exclude);
}

pub fn show_output(x: i32, y: i32, exclude: Option<RECT>) {
    popup(build_output(), x, y, exclude);
}

pub fn show_delay(x: i32, y: i32, exclude: Option<RECT>) {
    popup(build_delay(), x, y, exclude);
}
