//! Small Win32 helpers shared across modules.

use super::geom::{Point, Rect};
use std::ffi::c_void;
use std::path::PathBuf;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, GetWindowRect, SetWindowDisplayAffinity, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WDA_EXCLUDEFROMCAPTURE,
};

/// UTF-16, NUL-terminated.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// HWNDs are raw pointers (not `Send`); pass them between threads as `isize`.
pub fn hwnd_to_isize(h: HWND) -> isize {
    h.0 as isize
}
pub fn hwnd_from_isize(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

pub fn rect_from_win(r: &RECT) -> Rect {
    Rect::from_ltrb(r.left, r.top, r.right, r.bottom)
}

pub fn rect_to_win(r: &Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

pub fn cursor_pos() -> Point {
    let mut p = POINT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    Point::new(p.x, p.y)
}

pub fn virtual_screen() -> Rect {
    // SAFETY: plain metric queries.
    unsafe {
        Rect::new(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub handle: isize,
    pub rect: Rect,
    pub work: Rect,
    pub dpi: u32,
    pub primary: bool,
    pub device: String,
}

pub fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: data is the &mut Vec passed below; callback runs synchronously.
        let v = &mut *(data.0 as *mut Vec<MonitorInfo>);
        if let Some(m) = monitor_info(h) {
            v.push(m);
        }
        BOOL(1)
    }
    let mut v: Vec<MonitorInfo> = Vec::new();
    // SAFETY: callback only touches `v` during the call.
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

pub fn monitor_info(h: HMONITOR) -> Option<MonitorInfo> {
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize is set accordingly.
    unsafe {
        if !GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        Some(MonitorInfo {
            handle: h.0 as isize,
            rect: rect_from_win(&mi.monitorInfo.rcMonitor),
            work: rect_from_win(&mi.monitorInfo.rcWork),
            dpi: dx,
            primary: mi.monitorInfo.dwFlags & 1 != 0,
            device: from_wide(&mi.szDevice),
        })
    }
}

pub fn monitor_at(p: Point) -> Option<MonitorInfo> {
    // SAFETY: plain query.
    let h = unsafe { MonitorFromPoint(POINT { x: p.x, y: p.y }, MONITOR_DEFAULTTONEAREST) };
    monitor_info(h)
}

pub fn dpi_for_window(hwnd: HWND) -> u32 {
    // SAFETY: plain query; returns 0 for invalid windows.
    let d = unsafe { GetDpiForWindow(hwnd) };
    if d == 0 {
        96
    } else {
        d
    }
}

pub fn scale(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

/// Visible window bounds (excludes the invisible resize border on Win10/11).
pub fn window_bounds(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    // SAFETY: valid out pointers of the right size.
    unsafe {
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            let _ = GetWindowRect(hwnd, &mut r);
        }
    }
    rect_from_win(&r)
}

/// Windows on other virtual desktops, or suspended UWP frames, are "cloaked".
pub fn is_cloaked(hwnd: HWND) -> bool {
    let mut v: u32 = 0;
    // SAFETY: valid out pointer.
    unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut v as *mut u32 as *mut c_void, 4).is_ok() && v != 0
    }
}

/// Keep our own UI (toolbar, countdown, recorder bar) out of every capture API.
pub fn exclude_from_capture(hwnd: HWND) {
    // SAFETY: plain call; fails harmlessly on old Windows.
    unsafe {
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    }
}

pub fn round_corners(hwnd: HWND) {
    let pref = DWMWCP_ROUND;
    // SAFETY: valid pointer and size.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const c_void,
            std::mem::size_of_val(&pref) as u32,
        );
    }
}

pub fn set_dark_titlebar(hwnd: HWND, dark: bool) {
    let v: BOOL = dark.into();
    // SAFETY: valid pointer and size.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &v as *const _ as *const c_void, 4);
    }
}

/// True when Windows apps are set to dark mode.
pub fn system_dark_mode() -> bool {
    let mut v: u32 = 1;
    let mut size: u32 = 4;
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let val = wide("AppsUseLightTheme");
    // SAFETY: valid buffers.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&key),
            pcwstr(&val),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && v == 0
}

pub fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p: PWSTR = SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s.map(PathBuf::from)
    }
}

pub fn desktop_dir() -> PathBuf {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Desktop).unwrap_or_else(|| PathBuf::from("."))
}

pub fn documents_dir() -> PathBuf {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Documents).unwrap_or_else(|| PathBuf::from("."))
}

pub fn pictures_dir() -> PathBuf {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Pictures).unwrap_or_else(documents_dir)
}

pub fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("SnapForge.exe"))
}

pub fn exe_dir() -> PathBuf {
    exe_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// Open a file or folder with its default handler (ShellExecute "open").
pub fn shell_open(path: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let p = wide(path);
    let verb = wide("open");
    // SAFETY: valid NUL-terminated strings.
    unsafe {
        ShellExecuteW(HWND::default(), pcwstr(&verb), pcwstr(&p), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Open Explorer with `path` selected.
pub fn reveal_in_explorer(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}
