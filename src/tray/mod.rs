//! Notification-area (tray) icon and menu.
//!
//! CONTRACT: called on the UI thread. Owns its own hidden message window. Menu actions
//! are sent with `crate::app::post(AppCommand::…)`. Re-adds the icon when Explorer
//! restarts ("TaskbarCreated"). Uses the EXE's icon resource id 1.

pub mod autostart;
pub mod menu;

use crate::app::{self, AppCommand};
use crate::common::win::{pcwstr, wide};
use crate::i18n::{t, tf};
use crate::settings;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIIF_LARGE_ICON, NIIF_USER,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4, NOTIFY_ICON_MESSAGE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, LoadImageW,
    RegisterClassW, RegisterWindowMessageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, MSGFLT_ALLOW, SM_CXICON, SM_CXSMICON,
    WM_APP, WM_CONTEXTMENU, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

const CLASS: &str = "SnapForge.Tray";
const WM_TRAY: u32 = WM_APP + 10;
const TRAY_ID: u32 = 1;
/// NIN_SELECT | NINF_KEY
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

struct Tray {
    hwnd: HWND,
    icon: HICON,
    balloon_icon: HICON,
    taskbar_created: u32,
    added: bool,
    /// The file the last balloon was about, opened on NIN_BALLOONUSERCLICK.
    last_file: Option<PathBuf>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// The hidden window that owns the tray icon; also the owner of every popup menu.
pub fn owner_hwnd() -> HWND {
    TRAY.with(|t| t.try_borrow().ok().and_then(|t| t.as_ref().map(|t| t.hwnd))).unwrap_or_default()
}

fn load_app_icon(size: i32) -> HICON {
    // SAFETY: loads icon resource 1 from our own module; MAKEINTRESOURCE(1).
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return HICON::default() };
        match LoadImageW(hinst, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
            Ok(h) => HICON(h.0),
            Err(e) => {
                log::warn!("tray: icon resource not found: {e}");
                HICON::default()
            }
        }
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    let mut i = 0;
    for c in s.encode_utf16().take(n) {
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

fn shell(msg: NOTIFY_ICON_MESSAGE, d: &NOTIFYICONDATAW) -> bool {
    // SAFETY: `d` is a fully initialised NOTIFYICONDATAW with cbSize set.
    unsafe { Shell_NotifyIconW(msg, d).as_bool() }
}

fn add_icon(tr: &mut Tray) {
    let mut d = base_data(tr.hwnd);
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = tr.icon;
    copy_wide(&mut d.szTip, &tooltip());
    if !shell(NIM_ADD, &d) {
        // Explorer may not be ready yet (early autostart); TaskbarCreated will retry.
        log::warn!("tray: NIM_ADD failed");
        tr.added = false;
        return;
    }
    d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    if !shell(NIM_SETVERSION, &d) {
        log::warn!("tray: NIM_SETVERSION failed");
    }
    tr.added = true;
}

pub fn create() -> Result<()> {
    if !owner_hwnd().is_invalid() {
        return Ok(());
    }
    let class = wide(CLASS);
    // SAFETY: class registration and a hidden (never shown) top-level window, which
    // (unlike a message-only window) can become foreground for the popup-menu trick.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(WS_EX_TOOLWINDOW, pcwstr(&class), pcwstr(&class), WS_POPUP, 0, 0, 0, 0, None, None, hinst, None)
            .context("tray window")?
    };
    let tb = wide("TaskbarCreated");
    // SAFETY: plain calls with valid arguments.
    let (taskbar_created, small, large) = unsafe {
        let msg = RegisterWindowMessageW(pcwstr(&tb));
        let _ = ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, None);
        let dpi = GetDpiForSystem();
        (msg, GetSystemMetricsForDpi(SM_CXSMICON, dpi), GetSystemMetricsForDpi(SM_CXICON, dpi))
    };
    let mut tr = Tray {
        hwnd,
        icon: load_app_icon(small),
        balloon_icon: load_app_icon(large),
        taskbar_created,
        added: false,
        last_file: None,
    };
    add_icon(&mut tr);
    TRAY.with(|t| *t.borrow_mut() = Some(tr));
    // Mirror the real autostart state into the settings once at startup.
    autostart::sync_setting();
    Ok(())
}

/// Show a balloon/toast-style notification from the tray icon.
pub fn notify(title: &str, text: &str, is_error: bool) {
    notify_file(title, text, is_error, None);
}

/// [`notify`] about a file: clicking the balloon opens a video in the video editor and
/// reveals anything else in Explorer.
pub fn notify_file(title: &str, text: &str, is_error: bool, file: Option<PathBuf>) {
    if !is_error && !settings::get().general.show_notifications {
        return;
    }
    TRAY.with(|cell| {
        let Ok(mut g) = cell.try_borrow_mut() else { return };
        let Some(tr) = g.as_mut() else { return };
        if !tr.added {
            add_icon(tr);
        }
        let mut d = base_data(tr.hwnd);
        d.uFlags = NIF_INFO;
        copy_wide(&mut d.szInfoTitle, title);
        copy_wide(&mut d.szInfo, if text.is_empty() { " " } else { text });
        if is_error {
            d.dwInfoFlags = NIIF_ERROR;
        } else {
            d.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
            d.hBalloonIcon = tr.balloon_icon;
        }
        if !shell(NIM_MODIFY, &d) {
            log::warn!("tray: balloon failed: {title}: {text}");
        }
        tr.last_file = file;
    });
}

/// Settings changed (tooltip, checked menu states are read when the menu opens).
pub fn refresh() {
    TRAY.with(|cell| {
        let Ok(g) = cell.try_borrow() else { return };
        let Some(tr) = g.as_ref() else { return };
        if !tr.added {
            return;
        }
        let mut d = base_data(tr.hwnd);
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_wide(&mut d.szTip, &tooltip());
        shell(NIM_MODIFY, &d);
    });
}

pub fn remove() {
    let tr = TRAY.with(|t| t.borrow_mut().take());
    if let Some(tr) = tr {
        if tr.added {
            shell(NIM_DELETE, &base_data(tr.hwnd));
        }
        // SAFETY: we own these handles.
        unsafe {
            if !tr.icon.is_invalid() {
                let _ = DestroyIcon(tr.icon);
            }
            if !tr.balloon_icon.is_invalid() {
                let _ = DestroyIcon(tr.balloon_icon);
            }
            let _ = DestroyWindow(tr.hwnd);
        }
    }
}

/// A balloon about a file was clicked: open a video in the video editor, reveal anything
/// else (or its folder, when the file is gone).
fn on_balloon_click() {
    let file = TRAY.with(|t| t.try_borrow().ok().and_then(|t| t.as_ref().and_then(|t| t.last_file.clone())));
    let Some(p) = file else { return };
    if p.exists() {
        if crate::video_editor::accepts(&p) {
            app::post(AppCommand::OpenVideoEditor(Some(p)));
        } else {
            crate::common::win::reveal_in_explorer(&p);
        }
    } else if let Some(dir) = p.parent().filter(|d| d.exists()) {
        crate::common::win::shell_open(&dir.display().to_string());
    }
}

/// Tooltip text: names the region-capture hotkey when one is assigned.
fn tooltip() -> String {
    let parsed = crate::hotkeys::parse(settings::get().hotkeys.for_mode(settings::CaptureMode::Region));
    match parsed {
        Some(h) => tf("tray-tooltip", &[("hotkey", &crate::hotkeys::format(&h))]),
        None => t("tray-tooltip-plain"),
    }
}

fn signed_lo(v: usize) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}
fn signed_hi(v: usize) -> i32 {
    ((v >> 16) & 0xFFFF) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        // NOTIFYICON_VERSION_4: LOWORD(lParam) = event, wParam = anchor x/y.
        let event = (lp.0 as usize & 0xFFFF) as u32;
        match event {
            NIN_SELECT | NIN_KEYSELECT => app::post(AppCommand::ToggleToolbar),
            WM_CONTEXTMENU => {
                let (x, y) = (signed_lo(wp.0), signed_hi(wp.0));
                menu::show_full(x, y, None);
            }
            NIN_BALLOONUSERCLICK => on_balloon_click(),
            _ => {}
        }
        return LRESULT(0);
    }
    let tbc = TRAY.with(|t| t.try_borrow().ok().and_then(|t| t.as_ref().map(|t| t.taskbar_created)));
    if tbc.is_some_and(|m| m != 0 && m == msg) {
        log::info!("tray: taskbar re-created, re-adding icon");
        TRAY.with(|cell| {
            if let Ok(mut g) = cell.try_borrow_mut() {
                if let Some(tr) = g.as_mut() {
                    add_icon(tr);
                }
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
