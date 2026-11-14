//! Direct Win32 control of the editor's top-level window. Used from other threads to
//! show the window while it is hidden (a hidden window does not run `update()`), and for
//! saving / restoring the placement in physical pixels.

use crate::common::win::{hwnd_from_isize, monitors};
use crate::common::Rect;
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, GetWindowRect, IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow, SetWindowPos,
    ShowWindowAsync, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOW, SW_SHOWMAXIMIZED, WINDOWPLACEMENT,
};

/// Smallest placement we restore; anything smaller is treated as corrupt.
const MIN_W: i32 = 400;
const MIN_H: i32 = 300;

/// Show (restoring if minimised) and bring to the front. Never blocks on the editor
/// thread: `ShowWindowAsync` posts instead of sending.
pub fn show_and_focus(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let h = hwnd_from_isize(hwnd);
    // SAFETY: the HWND belongs to our editor window; all calls tolerate a stale handle.
    unsafe {
        if !IsWindow(h).as_bool() {
            return;
        }
        let cmd = if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW };
        let _ = ShowWindowAsync(h, cmd);
        let _ = SetForegroundWindow(h);
    }
}

pub fn focus(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = SetForegroundWindow(hwnd_from_isize(hwnd));
        }
    }
}

pub fn is_visible(hwnd: isize) -> bool {
    // SAFETY: plain query.
    hwnd != 0 && unsafe { IsWindowVisible(hwnd_from_isize(hwnd)).as_bool() }
}

/// Current outer placement [x, y, w, h] in physical px. For a maximised or minimised
/// window this is the restored ("normal") rectangle.
pub fn placement(hwnd: isize) -> Option<[i32; 4]> {
    if hwnd == 0 {
        return None;
    }
    let h = hwnd_from_isize(hwnd);
    let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    // SAFETY: valid out pointers of the right size.
    unsafe {
        GetWindowPlacement(h, &mut wp).ok()?;
        let r = if IsIconic(h).as_bool() || wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 {
            // rcNormalPosition is in workspace coordinates; close enough for restoring.
            wp.rcNormalPosition
        } else {
            let mut r = RECT::default();
            GetWindowRect(h, &mut r).ok()?;
            r
        };
        let p = [r.left, r.top, r.right - r.left, r.bottom - r.top];
        (p[2] >= MIN_W && p[3] >= MIN_H).then_some(p)
    }
}

/// True if a saved placement is sane and at least partly on a connected monitor.
pub fn placement_is_valid(p: [i32; 4], screens: &[Rect]) -> bool {
    if p[2] < MIN_W || p[3] < MIN_H || p[2] > 32_000 || p[3] > 32_000 {
        return false;
    }
    let r = Rect::new(p[0], p[1], p[2], p[3]);
    // Require the title-bar area to be reachable on some monitor.
    let title = Rect::new(r.x, r.y, r.w, 40);
    screens.iter().any(|s| {
        let i = title.intersect(s);
        i.w >= 120 && i.h >= 20
    })
}

/// First run (no saved placement): make sure the default-sized window fits the work area
/// of its monitor (at high DPI the logical default can exceed it), centred.
pub fn fit_to_work_area(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let r = crate::common::win::window_bounds(hwnd_from_isize(hwnd));
    let Some(m) = crate::common::win::monitor_at(r.center()) else { return };
    let work = m.work;
    if r.w <= work.w && r.h <= work.h && r.intersect(&work) == r {
        return;
    }
    let w = r.w.min(work.w * 9 / 10);
    let h = r.h.min(work.h * 9 / 10);
    let (x, y) = (work.x + (work.w - w) / 2, work.y + (work.h - h) / 2);
    // SAFETY: plain call on our own (still hidden) window.
    unsafe {
        let _ = SetWindowPos(hwnd_from_isize(hwnd), None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Apply a saved placement. False if it was rejected (off-screen or corrupt).
pub fn restore_placement(hwnd: isize, p: [i32; 4]) -> bool {
    let screens: Vec<Rect> = monitors().into_iter().map(|m| m.work).collect();
    if hwnd == 0 || !placement_is_valid(p, &screens) {
        return false;
    }
    // SAFETY: plain call on our own (still hidden) window.
    unsafe { SetWindowPos(hwnd_from_isize(hwnd), None, p[0], p[1], p[2], p[3], SWP_NOZORDER | SWP_NOACTIVATE).is_ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_validation() {
        let screens = [Rect::new(0, 0, 1920, 1040), Rect::new(1920, 0, 2560, 1400)];
        assert!(placement_is_valid([100, 100, 1200, 800], &screens));
        assert!(placement_is_valid([2000, 50, 1200, 800], &screens));
        // Off every monitor.
        assert!(!placement_is_valid([5000, 100, 1200, 800], &screens));
        // Title bar above the top edge.
        assert!(!placement_is_valid([100, -500, 1200, 800], &screens));
        // Too small / absurd.
        assert!(!placement_is_valid([100, 100, 50, 50], &screens));
        assert!(!placement_is_valid([100, 100, 100_000, 800], &screens));
    }
}
