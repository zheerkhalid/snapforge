//! Color fine-tune panel shown after a pick: RGB/HSV sliders with spin boxes, a hex
//! box, format/prefix/suffix, copy buttons and the recent-colors swatches.
//! Native Win32 controls (comctl32 v6 visual styles), DPI-aware via WM_DPICHANGED.

use super::color::{self, Rgb};
use super::gfx::{self, col, Dib};
use super::wnd::{self, MenuItem};
use crate::common::win::{self, hwnd_to_isize, pcwstr, wide};
use crate::common::{Point, Rect};
use crate::settings::tools::ColorFormat;
use std::cell::RefCell;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, GetSysColorBrush, InvalidateRect, SetBkMode, SetTextColor, COLOR_WINDOW, HDC,
    PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_BAR_CLASSES, ICC_UPDOWN_CLASS, INITCOMMONCONTROLSEX, TBM_SETPAGESIZE, TBM_SETPOS,
    TBM_SETRANGE, TBS_NOTICKS, TRACKBAR_CLASSW, UDM_SETBUDDY, UDM_SETPOS32, UDM_SETRANGE32, UDS_SETBUDDYINT,
    UPDOWN_CLASSW,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetClientRect, KillTimer, LoadCursorW, SendMessageW, SetCursor, SetForegroundWindow,
    SetTimer, SetWindowPos, ShowWindow, BN_CLICKED, CBN_SELCHANGE, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, EN_CHANGE,
    HMENU, HWND_TOPMOST, IDC_ARROW, IDC_HAND, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_HSCROLL, WM_LBUTTONDOWN,
    WM_PAINT, WM_SETCURSOR, WM_TIMER, WM_USER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

const CLASS: &str = "SnapForge.ColorPanel";
const TBM_GETPOS: u32 = WM_USER;
const TIMER_FEEDBACK: usize = 7;

// Control ids.
const ID_HEX: u16 = 100;
const ID_COPY: u16 = 101;
const ID_FORMAT: u16 = 102;
const ID_PREFIX: u16 = 103;
const ID_SUFFIX: u16 = 104;
const ID_COPY_HEX: u16 = 105;
const ID_COPY_RGB: u16 = 106;
const ID_PICK: u16 = 107;
const ID_CLOSE: u16 = 108;
/// Channel c (0..6: R G B H S V): slider 200 + c, edit 210 + c.
const ID_SLIDER: u16 = 200;
const ID_EDIT: u16 = 210;

/// Client size and layout in logical pixels.
const CW: i32 = 440;
const CH: i32 = 428;
const PREVIEW: (i32, i32, i32, i32) = (14, 14, 118, 102);
const SLIDER_Y: i32 = 132;
const ROW_H: i32 = 30;
const SWATCH_Y: i32 = 346;
const SWATCH: i32 = 20;
const SWATCH_GAP: i32 = 5;
const BUTTON_Y: i32 = 384;

const CHANNELS: [(&str, i32); 6] = [("R", 255), ("G", 255), ("B", 255), ("H", 359), ("S", 100), ("V", 100)];

struct Panel {
    hwnd: HWND,
    dpi: u32,
    font: windows::Win32::Graphics::Gdi::HFONT,
    font_bold: windows::Win32::Graphics::Gdi::HFONT,
    /// (control, x, y, w, h) in logical pixels.
    controls: Vec<(HWND, i32, i32, i32, i32)>,
    labels_bold: Vec<HWND>,
    updowns: Vec<(HWND, HWND)>,
    sliders: [HWND; 6],
    edits: [HWND; 6],
    hex: HWND,
    out_label: HWND,
    copy_btn: HWND,
    format_combo: HWND,
    prefix: HWND,
    suffix: HWND,
    original: Rgb,
    color: Rgb,
    hsv: (i32, i32, i32),
    format: ColorFormat,
    prefix_text: String,
    suffix_text: String,
    recent: Vec<u32>,
    dib: Option<Dib>,
    text: gfx::Text,
}

thread_local! {
    static STATE: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    with(|p| vec![hwnd_to_isize(p.hwnd)]).unwrap_or_default()
}

/// Show the panel for `c` near `near` (screen), or retarget the open panel.
pub fn open(c: Rgb, near: Point) {
    if let Some(h) = with(|p| {
        p.original = c;
        p.recent = crate::settings::get().tools.recent_colors.clone();
        p.set_color(c, None);
        p.hwnd
    }) {
        // SAFETY: our own window.
        unsafe {
            let _ = SetForegroundWindow(h);
            let _ = InvalidateRect(h, None, false);
        }
        return;
    }
    if !wnd::register(CLASS, Some(proc), IDC_ARROW) {
        return;
    }
    // SAFETY: plain call; registers the trackbar and up-down classes.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES | ICC_UPDOWN_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    let mon = win::monitor_at(near);
    let dpi = mon.as_ref().map_or(96, |m| m.dpi);
    let work = mon.map_or_else(win::virtual_screen, |m| m.work);
    let (style, ex) = styles();
    let (w, h) = window_size(dpi);
    let r = crate::overlay::loupe::place(near, (w, h), win::scale(24, dpi), &work);
    let title = crate::i18n::t("tools-color-title");
    let Some(hwnd) = wnd::create(ex, CLASS, &title, style, r, HWND::default()) else {
        return;
    };
    win::exclude_from_capture(hwnd);
    win::set_dark_titlebar(hwnd, false);
    let s = crate::settings::get().tools.clone();
    let mut p = Panel {
        hwnd,
        dpi,
        font: wnd::ui_font(9.0, dpi, false),
        font_bold: wnd::ui_font(9.0, dpi, true),
        controls: Vec::new(),
        labels_bold: Vec::new(),
        updowns: Vec::new(),
        sliders: [HWND::default(); 6],
        edits: [HWND::default(); 6],
        hex: HWND::default(),
        out_label: HWND::default(),
        copy_btn: HWND::default(),
        format_combo: HWND::default(),
        prefix: HWND::default(),
        suffix: HWND::default(),
        original: c,
        color: c,
        hsv: color::hsv_ints(c),
        format: s.color_format,
        prefix_text: s.color_prefix.clone(),
        suffix_text: s.color_suffix.clone(),
        recent: s.recent_colors.clone(),
        dib: None,
        text: gfx::Text::default(),
    };
    p.build();
    p.layout();
    p.set_color(c, None);
    STATE.with(|st| *st.borrow_mut() = Some(p));
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Change the adjusted color (keeps the picked one), as if the user moved a slider.
#[cfg(test)]
pub(super) fn set_adjusted(c: Rgb) {
    with(|p| {
        p.hsv = color::hsv_ints(c);
        p.set_color(c, None);
    });
}

pub fn close() {
    let Some(p) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    let (format, prefix, suffix) = (p.format, p.prefix_text.clone(), p.suffix_text.clone());
    // SAFETY: our own window; fonts are deleted after the controls using them are gone.
    unsafe {
        let _ = KillTimer(p.hwnd, TIMER_FEEDBACK);
        let _ = DestroyWindow(p.hwnd);
    }
    wnd::delete_font(p.font);
    wnd::delete_font(p.font_bold);
    let changed = {
        let s = crate::settings::get();
        s.tools.color_format != format || s.tools.color_prefix != prefix || s.tools.color_suffix != suffix
    };
    if changed {
        crate::settings::update(|s| {
            s.tools.color_format = format;
            s.tools.color_prefix = prefix;
            s.tools.color_suffix = suffix;
        });
    }
    super::closed(crate::app::ScreenTool::ColorPicker);
}

fn styles() -> (WINDOW_STYLE, WINDOW_EX_STYLE) {
    (WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN, WS_EX_TOPMOST | WS_EX_TOOLWINDOW)
}

fn window_size(dpi: u32) -> (i32, i32) {
    let (style, ex) = styles();
    let mut rc = RECT { left: 0, top: 0, right: win::scale(CW, dpi), bottom: win::scale(CH, dpi) };
    // SAFETY: valid RECT pointer.
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut rc, style, false, ex, dpi);
    }
    (rc.right - rc.left, rc.bottom - rc.top)
}

fn swatch_rect(i: usize) -> Rect {
    Rect::new(PREVIEW.0 + i as i32 * (SWATCH + SWATCH_GAP), SWATCH_Y, SWATCH, SWATCH)
}

impl Panel {
    fn child(
        &mut self,
        class: PCWSTR,
        text: &str,
        style: u32,
        ex: WINDOW_EX_STYLE,
        id: u16,
        r: (i32, i32, i32, i32),
    ) -> HWND {
        let t = wide(text);
        // SAFETY: valid strings; the child is owned by our window.
        let h = unsafe {
            windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                ex,
                class,
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
        self.controls.push((h, r.0, r.1, r.2, r.3));
        if style & WS_TABSTOP.0 != 0 {
            wnd::dialog_keys(h);
        }
        h
    }

    fn label(&mut self, text: &str, r: (i32, i32, i32, i32), bold: bool) -> HWND {
        // SS_CENTERIMAGE (0x200) centres single-line text vertically; SS_NOPREFIX 0x80.
        let h = self.child(windows::core::w!("STATIC"), text, 0x200 | 0x80, WINDOW_EX_STYLE(0), 0, r);
        if bold {
            self.labels_bold.push(h);
        }
        h
    }

    fn button(&mut self, text: &str, id: u16, r: (i32, i32, i32, i32), default: bool) -> HWND {
        let style = WS_TABSTOP.0 | u32::from(default);
        self.child(windows::core::w!("BUTTON"), text, style, WINDOW_EX_STYLE(0), id, r)
    }

    fn edit(&mut self, id: u16, r: (i32, i32, i32, i32), number: bool) -> HWND {
        // ES_AUTOHSCROLL 0x80, ES_NUMBER 0x2000.
        let style = WS_TABSTOP.0 | 0x80 | if number { 0x2000 } else { 0 };
        self.child(windows::core::w!("EDIT"), "", style, WS_EX_CLIENTEDGE, id, r)
    }

    fn build(&mut self) {
        let t = crate::i18n::t;
        let x0 = 148;
        self.label(&t("tools-color-hex"), (x0, 14, 52, 26), false);
        self.hex = self.edit(ID_HEX, (x0 + 56, 14, 92, 26), false);
        self.copy_btn = self.button(&t("tools-color-copy"), ID_COPY, (x0 + 160, 13, 120, 28), true);
        self.label(&t("tools-color-format"), (x0, 48, 52, 26), false);
        // CBS_DROPDOWNLIST 3, WS_VSCROLL for long lists is unnecessary (5 items).
        self.format_combo = self.child(
            windows::core::w!("COMBOBOX"),
            "",
            WS_TABSTOP.0 | 3,
            WINDOW_EX_STYLE(0),
            ID_FORMAT,
            (x0 + 56, 48, 92, 200),
        );
        self.out_label = self.label("", (x0 + 160, 48, 120, 26), true);
        self.label(&t("tools-color-prefix"), (x0, 82, 52, 26), false);
        self.prefix = self.edit(ID_PREFIX, (x0 + 56, 82, 60, 26), false);
        self.label(&t("tools-color-suffix"), (x0 + 128, 82, 44, 26), false);
        self.suffix = self.edit(ID_SUFFIX, (x0 + 176, 82, 60, 26), false);
        for (i, f) in ColorFormat::ALL.iter().enumerate() {
            let w = wide(&t(f.label_key()));
            // SAFETY: valid NUL-terminated string; the combo copies it.
            unsafe {
                SendMessageW(self.format_combo, CB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
                if *f == self.format {
                    SendMessageW(self.format_combo, CB_SETCURSEL, WPARAM(i), LPARAM(0));
                }
            }
        }
        wnd::set_window_text(self.prefix, &self.prefix_text.clone());
        wnd::set_window_text(self.suffix, &self.suffix_text.clone());

        for (c, (name, max)) in CHANNELS.iter().enumerate() {
            let y = SLIDER_Y + c as i32 * ROW_H + if c >= 3 { 8 } else { 0 };
            self.label(name, (14, y, 22, 26), true);
            let s = self.child(
                TRACKBAR_CLASSW,
                "",
                WS_TABSTOP.0 | TBS_NOTICKS,
                WINDOW_EX_STYLE(0),
                ID_SLIDER + c as u16,
                (38, y, 300, 26),
            );
            // SAFETY: trackbar messages on our own control.
            unsafe {
                SendMessageW(s, TBM_SETRANGE, WPARAM(0), LPARAM((*max as isize) << 16));
                SendMessageW(s, TBM_SETPAGESIZE, WPARAM(0), LPARAM(if *max > 100 { 16 } else { 10 }));
            }
            self.sliders[c] = s;
            let e = self.edit(ID_EDIT + c as u16, (346, y, 80, 26), true);
            self.edits[c] = e;
            // UDS_ALIGNRIGHT 4 | UDS_ARROWKEYS 0x20 | UDS_NOTHOUSANDS 0x80 | UDS_HOTTRACK 0x100.
            let ud_style = UDS_SETBUDDYINT | 4 | 0x20 | 0x80 | 0x100;
            let t2 = wide("");
            // SAFETY: creating an up-down control attached to the edit above.
            let ud = unsafe {
                windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    UPDOWN_CLASSW,
                    pcwstr(&t2),
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ud_style),
                    0,
                    0,
                    0,
                    0,
                    self.hwnd,
                    None,
                    wnd::hinstance(),
                    None,
                )
            }
            .unwrap_or_default();
            // SAFETY: up-down messages on our own control.
            unsafe {
                SendMessageW(ud, UDM_SETRANGE32, WPARAM(0), LPARAM(*max as isize));
            }
            self.updowns.push((ud, e));
        }
        self.label(&t("tools-color-recent"), (14, SWATCH_Y - 26, 200, 22), true);
        let bw = 98;
        self.button(&t("tools-color-copy-hex"), ID_COPY_HEX, (14, BUTTON_Y, bw, 30), false);
        self.button(&t("tools-color-copy-rgb"), ID_COPY_RGB, (14 + bw + 6, BUTTON_Y, bw, 30), false);
        self.button(&t("tools-color-pick-again"), ID_PICK, (CW - 14 - 2 * bw - 6, BUTTON_Y, bw, 30), false);
        self.button(&t("tools-close"), ID_CLOSE, (CW - 14 - bw, BUTTON_Y, bw, 30), false);
    }

    fn layout(&mut self) {
        let dpi = self.dpi;
        let s = |v: i32| win::scale(v, dpi);
        for &(h, x, y, w, hh) in &self.controls {
            let f = if self.labels_bold.contains(&h) { self.font_bold } else { self.font };
            wnd::set_font(h, f);
            // SAFETY: our own child windows.
            unsafe {
                let _ = SetWindowPos(h, None, s(x), s(y), s(w), s(hh), SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        for &(ud, buddy) in &self.updowns {
            wnd::set_font(ud, self.font);
            // SAFETY: re-attach so the up-down re-aligns to the moved edit.
            unsafe {
                SendMessageW(ud, UDM_SETBUDDY, WPARAM(buddy.0 as usize), LPARAM(0));
            }
        }
    }

    /// Show `c` in every control except `skip` (the one the user is typing in).
    fn set_color(&mut self, c: Rgb, skip: Option<HWND>) {
        self.color = c;
        self.sync(skip);
    }

    fn sync(&mut self, skip: Option<HWND>) {
        let c = self.color;
        let vals = [c.r as i32, c.g as i32, c.b as i32, self.hsv.0, self.hsv.1, self.hsv.2];
        for i in 0..6 {
            // SAFETY: messages to our own controls. Notifications they trigger re-enter
            // the window procedure while the state is borrowed and are ignored there.
            unsafe {
                SendMessageW(self.sliders[i], TBM_SETPOS, WPARAM(1), LPARAM(vals[i] as isize));
            }
            if Some(self.edits[i]) != skip {
                wnd::set_window_text(self.edits[i], &vals[i].to_string());
                if let Some(&(ud, _)) = self.updowns.get(i) {
                    // SAFETY: as above.
                    unsafe {
                        SendMessageW(ud, UDM_SETPOS32, WPARAM(0), LPARAM(vals[i] as isize));
                    }
                }
            }
        }
        if Some(self.hex) != skip {
            wnd::set_window_text(self.hex, &color::format_value(c, ColorFormat::Html, false));
        }
        self.update_output();
        // SAFETY: our own window.
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn formatted(&self) -> String {
        let lower = crate::settings::get().tools.color_hex_lowercase;
        color::format(self.color, self.format, lower, &self.prefix_text, &self.suffix_text)
    }

    fn update_output(&mut self) {
        let s = self.formatted();
        wnd::set_window_text(self.out_label, &s);
    }

    fn channel_changed(&mut self, ch: usize, v: i32, from: Option<HWND>) {
        let max = CHANNELS[ch].1;
        let v = v.clamp(0, max);
        match ch {
            0..=2 => {
                let mut c = self.color;
                match ch {
                    0 => c.r = v as u8,
                    1 => c.g = v as u8,
                    _ => c.b = v as u8,
                }
                self.hsv = color::hsv_ints(c);
                self.set_color(c, from);
            }
            _ => {
                match ch {
                    3 => self.hsv.0 = v,
                    4 => self.hsv.1 = v,
                    _ => self.hsv.2 = v,
                }
                let c = color::from_hsv_ints(self.hsv.0, self.hsv.1, self.hsv.2);
                self.set_color(c, from);
            }
        }
    }

    fn copy(&mut self, text: String) {
        match crate::output::clipboard::copy_text(&text) {
            Ok(()) => {
                log::info!("color panel: copied {text}");
                crate::settings::update(|s| s.tools.push_recent_color(self.color.to_u32()));
                self.recent = crate::settings::get().tools.recent_colors.clone();
                wnd::set_window_text(self.copy_btn, &crate::i18n::t("tools-copied"));
                // SAFETY: our own window.
                unsafe {
                    SetTimer(self.hwnd, TIMER_FEEDBACK, 1200, None);
                    let _ = InvalidateRect(self.hwnd, None, false);
                }
            }
            Err(e) => crate::app::notify(crate::i18n::t("app-name"), format!("{e:#}"), true),
        }
    }

    fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
        wnd::delete_font(std::mem::replace(&mut self.font, wnd::ui_font(9.0, dpi, false)));
        wnd::delete_font(std::mem::replace(&mut self.font_bold, wnd::ui_font(9.0, dpi, true)));
        self.layout();
    }

    fn swatch_at(&self, p: Point) -> Option<u32> {
        let lp = Point::new(p.x * 96 / self.dpi as i32, p.y * 96 / self.dpi as i32);
        (0..self.recent.len()).find(|&i| swatch_rect(i).contains(lp)).and_then(|i| self.recent.get(i).copied())
    }

    fn paint(&mut self, hdc: HDC, clip: RECT) {
        let mut rc = RECT::default();
        // SAFETY: our own window.
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let Some(mut dib) = Dib::reuse(self.dib.take(), rc.right.max(1), rc.bottom.max(1)) else {
            return;
        };
        render_custom(&mut dib, &mut self.text, self.dpi, self.original, self.color, &self.recent);
        // SAFETY: blit the invalid part of the back buffer.
        unsafe {
            let _ = BitBlt(
                hdc,
                clip.left,
                clip.top,
                clip.right - clip.left,
                clip.bottom - clip.top,
                dib.dc,
                clip.left,
                clip.top,
                SRCCOPY,
            );
        }
        self.dib = Some(dib);
    }
}

/// Paint the custom parts (background, preview, swatches) into `dib`.
pub(super) fn render_custom(
    dib: &mut Dib,
    text: &mut gfx::Text,
    dpi: u32,
    original: Rgb,
    current: Rgb,
    recent: &[u32],
) {
    let s = |v: i32| super::card::sc(v as f32, dpi);
    let (dw, dh) = (dib.w as u32, dib.h as u32);
    gfx::fill_rect_raw(dib, Rect::new(0, 0, dib.w, dib.h), [255, 255, 255, 255]);
    let (px, py, pw, ph) = (s(PREVIEW.0), s(PREVIEW.1), s(PREVIEW.2), s(PREVIEW.3));
    let r = s(10);
    if let Some(mut pm) = dib.pixmap() {
        // Preview: picked color on top, edited color below.
        let top_h = (ph * 0.42).round();
        gfx::fill(&mut pm, gfx::rrect(px, py, pw, ph, r), col(current.r, current.g, current.b, 255));
        if original != current {
            let mut pb = tiny_skia::PathBuilder::new();
            // Top band with rounded top corners only: a rounded rect clipped by a plain rect.
            if let Some(full) = gfx::rrect(px, py, pw, top_h + r, r) {
                let mut mask_pm = tiny_skia::Mask::new(dw, dh);
                if let (Some(m), Some(rect)) = (mask_pm.as_mut(), tiny_skia::Rect::from_xywh(px, py, pw, top_h)) {
                    m.fill_path(
                        &tiny_skia::PathBuilder::from_rect(rect),
                        tiny_skia::FillRule::Winding,
                        false,
                        tiny_skia::Transform::identity(),
                    );
                    pm.fill_path(
                        &full,
                        &gfx::paint(col(original.r, original.g, original.b, 255)),
                        tiny_skia::FillRule::Winding,
                        tiny_skia::Transform::identity(),
                        Some(m),
                    );
                }
            }
            pb.move_to(px, py + top_h);
            pb.line_to(px + pw, py + top_h);
            gfx::stroke(&mut pm, pb.finish(), col(255, 255, 255, 150), 1.0);
        }
        gfx::stroke(&mut pm, gfx::rrect(px + 0.5, py + 0.5, pw - 1.0, ph - 1.0, r), col(0, 0, 0, 40), 1.0);
        // Separator above the sliders and above the swatches.
        let sep = |pm: &mut tiny_skia::PixmapMut, y: f32| {
            gfx::line(pm, (s(14), y), (s(CW - 14), y), col(0, 0, 0, 22), 1.0);
        };
        sep(&mut pm, s(SLIDER_Y - 8));
        sep(&mut pm, s(SWATCH_Y - 32));
        // Recent swatches; empty slots as dashed-looking outlines.
        for i in 0..crate::settings::tools::MAX_RECENT_COLORS {
            let rr = swatch_rect(i);
            let (x, y, w) = (s(rr.x), s(rr.y), s(rr.w));
            match recent.get(i) {
                Some(&v) => {
                    let c = Rgb::from_u32(v);
                    gfx::fill(&mut pm, gfx::rrect(x, y, w, w, s(5)), col(c.r, c.g, c.b, 255));
                    gfx::stroke(&mut pm, gfx::rrect(x + 0.5, y + 0.5, w - 1.0, w - 1.0, s(5)), col(0, 0, 0, 45), 1.0);
                }
                None => {
                    gfx::stroke(&mut pm, gfx::rrect(x + 0.5, y + 0.5, w - 1.0, w - 1.0, s(5)), col(0, 0, 0, 28), 1.0)
                }
            }
        }
    }
    // Captions inside the preview, in a readable contrast color.
    let cap = |c: Rgb| if c.luma() > 0.55 { [20, 20, 20, 200] } else { [255, 255, 255, 220] };
    let fpx = super::card::sci(10.0, dpi);
    if original != current {
        text.draw(
            dib,
            &crate::i18n::t("tools-color-original"),
            (px + s(8)) as i32,
            (py + s(5)) as i32,
            fpx,
            gfx::Face::Ui,
            cap(original),
            gfx::Align::Left,
        );
    }
    text.draw(
        dib,
        &crate::i18n::t("tools-color-current"),
        (px + s(8)) as i32,
        (py + ph - s(19)) as i32,
        fpx,
        gfx::Face::Ui,
        cap(current),
        gfx::Align::Left,
    );
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HSCROLL => {
            let ctl = HWND(lp.0 as *mut _);
            with(|p| {
                if let Some(ch) = p.sliders.iter().position(|&s| s == ctl) {
                    let v = SendMessageW(ctl, TBM_GETPOS, WPARAM(0), LPARAM(0)).0 as i32;
                    p.channel_changed(ch, v, None);
                }
            });
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as u16;
            let code = ((wp.0 >> 16) & 0xFFFF) as u32;
            let ctl = HWND(lp.0 as *mut _);
            on_command(id, code, ctl);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_FEEDBACK => {
            let _ = KillTimer(hwnd, TIMER_FEEDBACK);
            with(|p| wnd::set_window_text(p.copy_btn, &crate::i18n::t("tools-color-copy")));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let pt = wnd::point_from_lparam(lp);
            if let Some(v) = with(|p| p.swatch_at(pt)).flatten() {
                with(|p| {
                    let c = Rgb::from_u32(v);
                    p.hsv = color::hsv_ints(c);
                    p.set_color(c, None);
                    let t = p.formatted();
                    p.copy(t);
                });
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            // Right-click on a swatch offers its formats.
            let at = wnd::point_from_lparam(lp);
            let mut pt = windows::Win32::Foundation::POINT { x: at.x, y: at.y };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            if let Some(v) = with(|p| p.swatch_at(Point::new(pt.x, pt.y))).flatten() {
                let c = Rgb::from_u32(v);
                let items: Vec<MenuItem> = ColorFormat::ALL
                    .iter()
                    .enumerate()
                    .map(|(i, f)| MenuItem::item(i as u32 + 1, color::format_value(c, *f, false)))
                    .collect();
                let id = wnd::popup_menu(hwnd, at, &items);
                if let Some(f) = (id as usize).checked_sub(1).and_then(|i| ColorFormat::ALL.get(i)) {
                    let text = color::format_value(c, *f, false);
                    with(|p| p.copy(text));
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let mut pt = windows::Win32::Foundation::POINT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            if with(|p| p.swatch_at(Point::new(pt.x, pt.y)).is_some()).unwrap_or(false) {
                if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                    SetCursor(c);
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut _);
            SetBkMode(hdc, TRANSPARENT);
            let out = with(|p| p.out_label.0 as isize == lp.0).unwrap_or(false);
            SetTextColor(hdc, if out { COLORREF(0x00B0_5A3A) } else { COLORREF(0x0030_2A26) });
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with(|p| p.paint(hdc, ps.rcPaint));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            // SAFETY: lParam points to the suggested window RECT.
            let r = &*(lp.0 as *const RECT);
            with(|p| p.set_dpi(dpi));
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn on_command(id: u16, code: u32, ctl: HWND) {
    match (id, code) {
        (ID_COPY, BN_CLICKED) | (1, _) => {
            with(|p| {
                let t = p.formatted();
                p.copy(t);
            });
        }
        (ID_COPY_HEX, BN_CLICKED) => {
            with(|p| p.copy(color::format_value(p.color, ColorFormat::Html, false)));
        }
        (ID_COPY_RGB, BN_CLICKED) => {
            with(|p| p.copy(color::format_value(p.color, ColorFormat::Rgb, false)));
        }
        (ID_PICK, BN_CLICKED) => {
            super::open(crate::app::ScreenTool::ColorPicker);
        }
        (ID_CLOSE, BN_CLICKED) => close(),
        (ID_FORMAT, CBN_SELCHANGE) => {
            with(|p| {
                // SAFETY: plain query on our own combo box.
                let i = unsafe { SendMessageW(p.format_combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 };
                if let Some(f) = usize::try_from(i).ok().and_then(|i| ColorFormat::ALL.get(i)) {
                    p.format = *f;
                    p.update_output();
                }
            });
        }
        (ID_PREFIX | ID_SUFFIX, EN_CHANGE) => {
            with(|p| {
                let t = wnd::window_text(ctl);
                if id == ID_PREFIX {
                    p.prefix_text = t;
                } else {
                    p.suffix_text = t;
                }
                p.update_output();
            });
        }
        (ID_HEX, EN_CHANGE) => {
            with(|p| {
                if let Some(c) = color::parse(&wnd::window_text(ctl)) {
                    if c != p.color {
                        p.hsv = color::hsv_ints(c);
                        p.set_color(c, Some(ctl));
                    }
                }
            });
        }
        (i, EN_CHANGE) if (ID_EDIT..ID_EDIT + 6).contains(&i) => {
            with(|p| {
                let ch = (i - ID_EDIT) as usize;
                if let Ok(v) = wnd::window_text(ctl).trim().parse::<i32>() {
                    p.channel_changed(ch, v, Some(ctl));
                }
            });
        }
        _ => {}
    }
}
