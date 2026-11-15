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

/// Size and position of a window of size `w`×`h` on `work`, keeping a margin of 4% (at
/// least 16 px) on every side, centred. Pure, so it is tested for every DPI.
pub fn fitted_rect(w: i32, h: i32, work: Rect) -> Rect {
    let mx = (work.w * 4 / 100).max(16);
    let my = (work.h * 4 / 100).max(16);
    let w = w.min(work.w - 2 * mx).max(MIN_W.min(work.w));
    let h = h.min(work.h - 2 * my).max(MIN_H.min(work.h));
    Rect::new(work.x + (work.w - w) / 2, work.y + (work.h - h) / 2, w, h)
}

/// First run (no saved placement): fit the default-sized window into the work area of
/// its monitor with margins (at 125%+ DPI the logical default exceeds a 1080p screen),
/// centred.
pub fn fit_to_work_area(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let r = crate::common::win::window_bounds(hwnd_from_isize(hwnd));
    let Some(m) = crate::common::win::monitor_at(r.center()) else { return };
    let f = fitted_rect(r.w, r.h, m.work);
    let (x, y, w, h) = (f.x, f.y, f.w, f.h);
    // SAFETY: plain call on our own (still hidden) window.
    unsafe {
        let _ = SetWindowPos(hwnd_from_isize(hwnd), None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Apply a saved placement. False if it was rejected (off-screen or corrupt). A placement
/// larger than its monitor's work area (DPI changed since) is shrunk to fit.
pub fn restore_placement(hwnd: isize, p: [i32; 4]) -> bool {
    let mons = monitors();
    let screens: Vec<Rect> = mons.iter().map(|m| m.work).collect();
    if hwnd == 0 || !placement_is_valid(p, &screens) {
        return false;
    }
    let r = Rect::new(p[0], p[1], p[2], p[3]);
    let p = match mons.iter().map(|m| m.work).find(|w| w.contains(r.center())) {
        Some(w) if r.w > w.w || r.h > w.h => {
            let f = fitted_rect(r.w, r.h, w);
            [f.x, f.y, f.w, f.h]
        }
        _ => p,
    };
    // SAFETY: plain call on our own (still hidden) window.
    unsafe { SetWindowPos(hwnd_from_isize(hwnd), None, p[0], p[1], p[2], p[3], SWP_NOZORDER | SWP_NOACTIVATE).is_ok() }
}

/// Give the top-level window of this process titled `title` the app icon (big and small).
/// eframe only sets the small icon on child viewports, so the taskbar showed its default.
pub fn set_icon_by_title(title: &str) -> bool {
    use crate::common::win::{pcwstr, wide};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetSystemMetrics, GetWindowThreadProcessId, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL,
        IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON, WM_SETICON,
    };
    let w = wide(title);
    // SAFETY: plain Win32 queries; the icon handles come from our own module's resources
    // (resource id 1, see build.rs) and are shared for the process lifetime.
    unsafe {
        let Ok(h) = FindWindowW(PCWSTR::null(), pcwstr(&w)) else { return false };
        if h.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != std::process::id() {
            return false;
        }
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return false };
        let res = PCWSTR(1 as *const u16);
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let size = GetSystemMetrics(metric);
            if let Ok(icon) = LoadImageW(module, res, IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
                SendMessageW(h, WM_SETICON, WPARAM(kind as usize), LPARAM(icon.0 as isize));
            }
        }
        true
    }
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

    #[test]
    fn first_run_size_fits_every_dpi() {
        // 1280×820 logical at 100…200% on a 1920×1080 screen (taskbar 48 px).
        let work = Rect::new(0, 0, 1920, 1032);
        for scale in [100, 125, 150, 175, 200] {
            let (w, h) = (1280 * scale / 100 + 16, 820 * scale / 100 + 39);
            let f = fitted_rect(w, h, work);
            assert!(f.x >= 16 && f.y >= 16, "{scale}%: {f:?}");
            assert!(f.right() <= work.right() - 16 && f.bottom() <= work.bottom() - 16, "{scale}%: {f:?}");
        }
        // Small enough windows keep their size, centred.
        let f = fitted_rect(800, 600, work);
        assert_eq!((f.w, f.h), (800, 600));
        assert_eq!(f.x, (1920 - 800) / 2);
        // A monitor not at the origin.
        let f = fitted_rect(5000, 5000, Rect::new(-1280, 100, 1280, 984));
        assert!(f.x >= -1280 + 16 && f.right() <= -16 && f.y >= 116);
    }
}
