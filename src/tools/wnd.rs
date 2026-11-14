//! Win32 window plumbing for the tools: class registration, creation, input helpers,
//! popup menus and small native-control helpers for the dialogs.

use crate::common::win::{pcwstr, wide};
use crate::common::{Point, Rect};
use std::cell::RefCell;
use std::collections::HashSet;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, GetParent, LoadCursorW, PostMessageW, RegisterClassW,
    SendMessageW, SetForegroundWindow, TrackPopupMenu, HCURSOR, HMENU, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_KEYDOWN,
    WM_SETFONT, WNDCLASSW, WNDPROC,
};

pub fn hinstance() -> HINSTANCE {
    // SAFETY: plain query for our own module.
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

thread_local! {
    static CLASSES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Register a window class once per thread (all tools live on the UI thread).
pub fn register(class: &str, proc: WNDPROC, cursor: PCWSTR) -> bool {
    if CLASSES.with(|c| c.borrow().contains(class)) {
        return true;
    }
    let name = wide(class);
    // SAFETY: valid NUL-terminated class name kept alive across the call; the system
    // copies it. The cursor is a shared system cursor.
    let ok = unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: hinstance(),
            lpszClassName: pcwstr(&name),
            hCursor: LoadCursorW(None, cursor).unwrap_or(HCURSOR::default()),
            ..Default::default()
        };
        RegisterClassW(&wc) != 0
    };
    if ok {
        CLASSES.with(|c| c.borrow_mut().insert(class.to_string()));
    } else {
        log::error!("tools: RegisterClassW({class}) failed");
    }
    ok
}

pub fn create(
    ex: WINDOW_EX_STYLE,
    class: &str,
    title: &str,
    style: WINDOW_STYLE,
    r: Rect,
    parent: HWND,
) -> Option<HWND> {
    let c = wide(class);
    let t = wide(title);
    // SAFETY: valid strings; the class was registered by `register`.
    let res = unsafe {
        CreateWindowExW(ex, pcwstr(&c), pcwstr(&t), style, r.x, r.y, r.w, r.h, parent, None, hinstance(), None)
    };
    match res {
        Ok(h) => Some(h),
        Err(e) => {
            log::error!("tools: CreateWindowExW({class}) failed: {e}");
            None
        }
    }
}

/// Signed client coordinates from a mouse message's LPARAM.
pub fn point_from_lparam(lp: LPARAM) -> Point {
    Point::new((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

/// Wheel delta in notches (positive = away from the user).
pub fn wheel_notches(wp: WPARAM) -> i32 {
    let d = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
    notches(d)
}

/// Raw wheel delta to whole notches; partial (high-resolution) deltas count as one.
pub fn notches(d: i32) -> i32 {
    if d.abs() < 120 {
        d.signum()
    } else {
        d / 120
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

pub fn mods() -> Mods {
    // SAFETY: plain key state queries.
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

/// A Segoe UI font `pt` points high at `dpi` (caller deletes it).
pub fn ui_font(pt: f32, dpi: u32, bold: bool) -> HFONT {
    let h = -((pt * dpi as f32 / 72.0).round() as i32);
    let face = wide("Segoe UI");
    // SAFETY: valid face name; CLEARTYPE_QUALITY (5) for native controls.
    unsafe { CreateFontW(h, 0, 0, 0, if bold { 600 } else { 400 }, 0, 0, 0, 1, 0, 0, 5, 0, pcwstr(&face)) }
}

pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        // SAFETY: a font we created; callers make sure no window still uses it.
        unsafe {
            let _ = DeleteObject(f);
        }
    }
}

pub fn set_font(hwnd: HWND, f: HFONT) {
    // SAFETY: WM_SETFONT with redraw.
    unsafe {
        SendMessageW(hwnd, WM_SETFONT, WPARAM(f.0 as usize), LPARAM(1));
    }
}

/// A popup menu entry.
pub enum MenuItem {
    Item { id: u32, text: String, checked: bool, enabled: bool },
    Sub { text: String, items: Vec<MenuItem> },
    Separator,
}

impl MenuItem {
    pub fn item(id: u32, text: impl Into<String>) -> Self {
        MenuItem::Item { id, text: text.into(), checked: false, enabled: true }
    }
    pub fn check(id: u32, text: impl Into<String>, checked: bool) -> Self {
        MenuItem::Item { id, text: text.into(), checked, enabled: true }
    }
}

fn build_menu(items: &[MenuItem]) -> Option<HMENU> {
    // SAFETY: the menu is filled here and destroyed by the caller (submenus with it).
    unsafe {
        let m = CreatePopupMenu().ok()?;
        for it in items {
            match it {
                MenuItem::Item { id, text, checked, enabled } => {
                    let w = wide(text);
                    let mut f = MF_STRING;
                    if *checked {
                        f |= MF_CHECKED;
                    }
                    if !*enabled {
                        f |= MF_GRAYED;
                    }
                    let _ = AppendMenuW(m, f, *id as usize, pcwstr(&w));
                }
                MenuItem::Sub { text, items } => {
                    if let Some(sub) = build_menu(items) {
                        let w = wide(text);
                        let _ = AppendMenuW(m, MF_POPUP, sub.0 as usize, pcwstr(&w));
                    }
                }
                MenuItem::Separator => {
                    let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }
        }
        Some(m)
    }
}

/// Show a popup menu at `at` (screen) owned by `owner`; returns the chosen id (0 = none).
pub fn popup_menu(owner: HWND, at: Point, items: &[MenuItem]) -> u32 {
    let Some(m) = build_menu(items) else {
        return 0;
    };
    // SAFETY: modal menu tracking on our own window; the menu is destroyed afterwards.
    unsafe {
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, at.x, at.y, 0, owner, None);
        let _ = DestroyMenu(m);
        r.0 as u32
    }
}

/// Subclass a dialog child so Esc closes its parent and Tab moves focus (there is no
/// `IsDialogMessage` in the app's message loop). Enter posts `WM_COMMAND(IDOK)`.
pub fn dialog_keys(child: HWND) {
    // SAFETY: plain subclass registration; the proc only forwards messages.
    unsafe {
        let _ = SetWindowSubclass(child, Some(dialog_key_proc), 1, 0);
    }
}

pub const IDOK: usize = 1;

unsafe extern "system" fn dialog_key_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN, VK_TAB};
    use windows::Win32::UI::WindowsAndMessaging::{GetNextDlgTabItem, WM_COMMAND};
    let parent = GetParent(hwnd).unwrap_or_default();
    match msg {
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            let _ = PostMessageW(parent, WM_CLOSE, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_TAB.0 as usize => {
            if let Ok(next) = GetNextDlgTabItem(parent, hwnd, mods().shift) {
                SetFocus(next).ok();
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_RETURN.0 as usize => {
            let _ = PostMessageW(parent, WM_COMMAND, WPARAM(IDOK), LPARAM(0));
            return LRESULT(0);
        }
        // Swallow the matching characters so edit controls do not beep.
        WM_CHAR if matches!(wp.0, 0x09 | 0x0D | 0x1B) => return LRESULT(0),
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// Text of a window (edit control contents).
pub fn window_text(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    // SAFETY: buffer sized from GetWindowTextLengthW plus the terminator.
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..got.min(n)])
    }
}

pub fn set_window_text(hwnd: HWND, s: &str) {
    use windows::Win32::UI::WindowsAndMessaging::SetWindowTextW;
    let w = wide(s);
    // SAFETY: valid NUL-terminated string.
    unsafe {
        let _ = SetWindowTextW(hwnd, pcwstr(&w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_and_lparam_decoding() {
        let lp = LPARAM(((-5i16 as u16 as isize) << 16) | (-3i16 as u16 as isize));
        assert_eq!(point_from_lparam(lp), Point::new(-3, -5));
        let wp = |d: i16| WPARAM(((d as u16) as usize) << 16);
        assert_eq!(wheel_notches(wp(120)), 1);
        assert_eq!(wheel_notches(wp(-240)), -2);
        assert_eq!(wheel_notches(wp(30)), 1, "high-resolution wheels still count");
        assert_eq!(wheel_notches(wp(-30)), -1);
        assert_eq!(wheel_notches(wp(0)), 0);
    }
}
