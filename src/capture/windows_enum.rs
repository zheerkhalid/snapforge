//! Top-level window snapshot in z-order, for the window/object overlay.

use super::WindowEntry;
use crate::common::win;
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetLayeredWindowAttributes, GetWindowLongW, GetWindowTextW, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// Classes that are always kept even when they look like click-through junk.
const POPUP_CLASSES: &[&str] = &["#32768", "tooltips_class32", "Xaml_WindowedPopupClass"];
/// The shell desktop background, and menu drop shadows.
const SKIP_CLASSES: &[&str] = &["Progman", "WorkerW", "SysShadow"];

pub fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: valid buffer; returns the number of characters copied.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: valid buffer. For windows of other processes this reads the cached caption
    // and never sends a message, so it cannot hang on an unresponsive app.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Decide whether a top-level window belongs in the snapshot; returns its entry if so.
fn entry_for(hwnd: HWND, exclude: &[isize]) -> Option<WindowEntry> {
    let h = win::hwnd_to_isize(hwnd);
    if exclude.contains(&h) {
        return None;
    }
    // SAFETY: plain queries on a window handle from EnumWindows (stale handles just fail).
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
    }
    if win::is_cloaked(hwnd) {
        return None;
    }
    let class = window_class(hwnd);
    if SKIP_CLASSES.contains(&class.as_str()) {
        return None;
    }
    let rect = win::window_bounds(hwnd);
    if rect.w <= 1 || rect.h <= 1 {
        return None;
    }
    let popup = POPUP_CLASSES.contains(&class.as_str());
    // SAFETY: plain style query.
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & WS_EX_LAYERED.0 != 0 && !popup {
        // Click-through layered windows are overlays (game bars, screen-dimmers…).
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        // Fully transparent layered windows are invisible.
        let mut alpha = 255u8;
        let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
        let mut key = COLORREF(0);
        // SAFETY: valid out pointers; fails for windows that use UpdateLayeredWindow.
        let ok =
            unsafe { GetLayeredWindowAttributes(hwnd, Some(&mut key), Some(&mut alpha), Some(&mut flags)) }.is_ok();
        if ok && flags.0 & LWA_ALPHA.0 != 0 && alpha == 0 {
            return None;
        }
    }
    Some(WindowEntry { hwnd: h, rect, title: window_title(hwnd), class })
}

pub fn snapshot(exclude: &[isize]) -> Vec<WindowEntry> {
    struct Ctx<'a> {
        exclude: &'a [isize],
        out: Vec<WindowEntry>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Ctx passed below; the callback runs synchronously.
        let ctx = &mut *(lp.0 as *mut Ctx);
        if let Some(e) = entry_for(hwnd, ctx.exclude) {
            ctx.out.push(e);
        }
        BOOL(1)
    }
    let mut ctx = Ctx { exclude, out: Vec::with_capacity(64) };
    // SAFETY: the callback only touches `ctx` during the call. EnumWindows enumerates
    // top-level windows in z-order, topmost first.
    unsafe {
        if let Err(e) = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)) {
            log::warn!("EnumWindows failed: {e}");
        }
    }
    ctx.out
}
