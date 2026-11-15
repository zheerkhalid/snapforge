//! Ruler calibration: physical pixel density per monitor, and the small dialog where the
//! user enters the screen's diagonal in inches.

use super::measure;
use super::wnd;
use crate::common::win::{self, hwnd_to_isize, pcwstr, wide, MonitorInfo};
use crate::common::Rect;
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, GetSysColorBrush, SetBkMode, SetTextColor, COLOR_WINDOW, HDC, HFONT, HORZSIZE,
    TRANSPARENT, VERTSIZE,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, SetForegroundWindow, SetWindowPos, ShowWindow, BN_CLICKED, HMENU,
    IDC_ARROW, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORSTATIC, WM_DPICHANGED, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

/// Settings key of a monitor: device name plus resolution.
pub fn monitor_key(m: &MonitorInfo) -> String {
    format!("{}@{}x{}", m.device, m.rect.w, m.rect.h)
}

/// Physical size in millimetres reported by the display driver (from the EDID).
pub fn physical_mm(m: &MonitorInfo) -> (i32, i32) {
    let dev = wide(&m.device);
    let drv = wide("DISPLAY");
    // SAFETY: a DC for the display device, deleted right after the two queries.
    unsafe {
        let dc = CreateDCW(pcwstr(&drv), pcwstr(&dev), None, None);
        if dc.is_invalid() {
            return (0, 0);
        }
        let r = (GetDeviceCaps(dc, HORZSIZE), GetDeviceCaps(dc, VERTSIZE));
        let _ = DeleteDC(dc);
        r
    }
}

/// Diagonal suggested for a monitor without a stored calibration.
pub fn detected_diagonal(m: &MonitorInfo) -> (f64, bool) {
    let (wmm, hmm) = physical_mm(m);
    match measure::diagonal_from_mm(wmm, hmm, m.rect.w as u32, m.rect.h as u32) {
        Some(d) => (d, true),
        None => (measure::diagonal_from_ppi(m.rect.w as u32, m.rect.h as u32, measure::DEFAULT_PPI), false),
    }
}

/// Pixels per inch for a monitor: stored calibration, else the detected size.
pub fn ppi_for(m: &MonitorInfo) -> f64 {
    let stored = crate::settings::get().tools.calibration_for(&monitor_key(m));
    let diag = stored.unwrap_or_else(|| detected_diagonal(m).0);
    measure::ppi_from_diagonal(m.rect.w as u32, m.rect.h as u32, diag)
}

// ---------------------------------------------------------------- dialog

const CLASS: &str = "SnapForge.Calibrate";
const ID_EDIT: u16 = 10;
const ID_RESET: u16 = 11;
const ID_CANCEL: u16 = 12;
const ID_OK: u16 = 1;
const CW: i32 = 380;
const CH: i32 = 168;

struct Dialog {
    hwnd: HWND,
    edit: HWND,
    key: String,
    font: HFONT,
    controls: Vec<(HWND, (i32, i32, i32, i32))>,
}

thread_local! {
    static STATE: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Dialog) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn hwnds() -> Vec<isize> {
    with(|d| vec![hwnd_to_isize(d.hwnd)]).unwrap_or_default()
}

/// Open the calibration dialog for monitor `m`, centred on it.
pub fn open(m: MonitorInfo) {
    close();
    if !wnd::register(CLASS, Some(proc), IDC_ARROW) {
        return;
    }
    let dpi = m.dpi;
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    let mut rc = RECT { left: 0, top: 0, right: win::scale(CW, dpi), bottom: win::scale(CH, dpi) };
    // SAFETY: valid RECT pointer.
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, ex, dpi);
    }
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let r = Rect::new(m.work.x + (m.work.w - w) / 2, m.work.y + (m.work.h - h) / 2, w, h);
    let Some(hwnd) = wnd::create(ex, CLASS, &crate::i18n::t("tools-cal-title"), style, r, HWND::default()) else {
        return;
    };
    win::exclude_from_capture(hwnd);
    let key = monitor_key(&m);
    let stored = crate::settings::get().tools.calibration_for(&key);
    let (detected, from_edid) = detected_diagonal(&m);
    let current = stored.unwrap_or(detected);
    let mut d = Dialog { hwnd, edit: HWND::default(), key, font: wnd::ui_font(9.0, dpi, false), controls: Vec::new() };
    let t = crate::i18n::t;
    d.child(
        "STATIC",
        &crate::i18n::tf("tools-cal-prompt", &[("monitor", &m.device.replace("\\\\.\\", ""))]),
        0x80,
        WINDOW_EX_STYLE(0),
        0,
        (16, 14, 348, 22),
    );
    d.edit =
        d.child("EDIT", &format!("{current:.1}"), WS_TABSTOP.0 | 0x80, WS_EX_CLIENTEDGE, ID_EDIT, (16, 42, 90, 26));
    d.child("STATIC", &t("tools-cal-inches"), 0x80 | 0x200, WINDOW_EX_STYLE(0), 0, (112, 42, 60, 26));
    let ppi = measure::ppi_from_diagonal(m.rect.w as u32, m.rect.h as u32, current);
    let src = if stored.is_some() {
        t("tools-cal-source-user")
    } else if from_edid {
        t("tools-cal-source-edid")
    } else {
        t("tools-cal-source-default")
    };
    let info = format!("{} \u{00D7} {} px \u{00B7} {:.1} PPI \u{00B7} {}", m.rect.w, m.rect.h, ppi, src);
    d.child("STATIC", &info, 0x80, WINDOW_EX_STYLE(0), 0, (16, 76, 348, 22));
    d.child(
        "STATIC",
        &crate::i18n::tf("tools-cal-detected", &[("size", &format!("{detected:.1}"))]),
        0x80,
        WINDOW_EX_STYLE(0),
        0,
        (16, 98, 348, 22),
    );
    d.child("BUTTON", &t("tools-cal-reset"), WS_TABSTOP.0, WINDOW_EX_STYLE(0), ID_RESET, (16, 126, 100, 30));
    d.child("BUTTON", &t("tools-ok"), WS_TABSTOP.0 | 1, WINDOW_EX_STYLE(0), ID_OK, (CW - 16 - 206, 126, 100, 30));
    d.child("BUTTON", &t("tools-cancel"), WS_TABSTOP.0, WINDOW_EX_STYLE(0), ID_CANCEL, (CW - 16 - 100, 126, 100, 30));
    d.layout(dpi);
    let edit = d.edit;
    STATE.with(|s| *s.borrow_mut() = Some(d));
    // SAFETY: our own window and control.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(edit);
        // EM_SETSEL 0..-1: select all so typing replaces the value.
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(edit, 0x00B1, WPARAM(0), LPARAM(-1));
    }
}

pub fn close() {
    let Some(d) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    // SAFETY: our own window; the font is deleted after its controls are gone.
    unsafe {
        let _ = DestroyWindow(d.hwnd);
    }
    wnd::delete_font(d.font);
}

impl Dialog {
    fn child(
        &mut self,
        class: &str,
        text: &str,
        style: u32,
        ex: WINDOW_EX_STYLE,
        id: u16,
        r: (i32, i32, i32, i32),
    ) -> HWND {
        let c = wide(class);
        let t = wide(text);
        // SAFETY: valid strings; the child belongs to our dialog window.
        let h = unsafe {
            CreateWindowExW(
                ex,
                pcwstr(&c),
                pcwstr(&t),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                0,
                0,
                1,
                1,
                self.hwnd,
                HMENU(id as usize as *mut _),
                wnd::hinstance(),
                None,
            )
        }
        .unwrap_or_default();
        if style & WS_TABSTOP.0 != 0 {
            wnd::dialog_keys(h);
        }
        self.controls.push((h, r));
        h
    }

    fn layout(&mut self, dpi: u32) {
        let s = |v: i32| win::scale(v, dpi);
        for &(h, (x, y, w, hh)) in &self.controls {
            wnd::set_font(h, self.font);
            // SAFETY: our own child windows.
            unsafe {
                let _ = SetWindowPos(h, None, s(x), s(y), s(w), s(hh), SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
}

/// Parse a diagonal in inches (accepts `27`, `27.0`, `27,5`, `27"`).
pub fn parse_diagonal(s: &str) -> Option<f64> {
    let t = s.trim().trim_end_matches(['"', '\u{201D}']).trim().replace(',', ".");
    let v: f64 = t.parse().ok()?;
    (4.0..=120.0).contains(&v).then_some(v)
}

fn apply(id: u16) {
    let Some((key, text)) = with(|d| (d.key.clone(), wnd::window_text(d.edit))) else {
        return;
    };
    match id {
        ID_OK => match parse_diagonal(&text) {
            Some(v) => crate::settings::update(|s| s.tools.set_calibration(&key, Some(v))),
            None => {
                // Keep the dialog open on bad input and select the text for retyping.
                with(|d| {
                    // SAFETY: our own edit control; EM_SETSEL 0..-1 selects all.
                    unsafe {
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(d.edit);
                        windows::Win32::UI::WindowsAndMessaging::SendMessageW(d.edit, 0x00B1, WPARAM(0), LPARAM(-1));
                    }
                });
                return;
            }
        },
        ID_RESET => crate::settings::update(|s| s.tools.set_calibration(&key, None)),
        _ => {}
    }
    close();
    super::ruler::calibration_changed();
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as u16;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            if code == BN_CLICKED || id == ID_OK {
                match id {
                    ID_OK | ID_RESET | ID_CANCEL => apply(id),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut _);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x0030_2A26));
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            // SAFETY: lParam points to the suggested window RECT.
            let r = &*(lp.0 as *const RECT);
            with(|d| {
                wnd::delete_font(std::mem::replace(&mut d.font, wnd::ui_font(9.0, dpi, false)));
                d.layout(dpi);
            });
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_parsing() {
        assert_eq!(parse_diagonal("27"), Some(27.0));
        assert_eq!(parse_diagonal(" 15,6 "), Some(15.6));
        assert_eq!(parse_diagonal("24\""), Some(24.0));
        assert_eq!(parse_diagonal("2"), None);
        assert_eq!(parse_diagonal("abc"), None);
        assert_eq!(parse_diagonal("500"), None);
    }

    #[test]
    fn key_includes_resolution() {
        let m = MonitorInfo {
            handle: 0,
            rect: Rect::new(0, 0, 2560, 1440),
            work: Rect::new(0, 0, 2560, 1400),
            dpi: 96,
            primary: true,
            device: "\\\\.\\DISPLAY1".into(),
        };
        assert_eq!(monitor_key(&m), "\\\\.\\DISPLAY1@2560x1440");
    }
}
