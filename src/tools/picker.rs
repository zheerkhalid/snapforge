//! Screen color picker: a live zoom loupe next to the cursor; click picks.
//!
//! A nearly invisible "catcher" window (alpha 1, excluded from capture) covers the
//! virtual screen to own the mouse and keyboard; the loupe is a separate click-through
//! layered window. Space freezes the screen: the catcher then becomes opaque and shows
//! the frozen screenshot, and the loupe samples from it.

use super::card::{self, sc, sci};
use super::color::{self, Rgb};
use super::gfx::{self, col, Align, Dib, Face, Grabber, Text};
use super::lens::{self, Grid};
use super::wnd;
use crate::app::ScreenTool;
use crate::common::win::{self, hwnd_to_isize};
use crate::common::{Image, Point, Rect};
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, FillRect, GetStockObject, BLACK_BRUSH, HBRUSH, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_ADD, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_OEM_MINUS, VK_OEM_PLUS, VK_RETURN, VK_RIGHT, VK_SPACE,
    VK_SUBTRACT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, KillTimer, SetCursorPos, SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
    SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW, IDC_CROSS, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CATCHER_CLASS: &str = "SnapForge.PickerCatcher";
const LOUPE_CLASS: &str = "SnapForge.PickerLoupe";
const TIMER_ID: usize = 1;
/// Live refresh while the cursor rests (screen content may change).
const TIMER_MS: u32 = 50;
/// Logical card width.
const CARD_W: f32 = 212.0;
/// Room around the card for its shadow.
const MARGIN: f32 = 14.0;

struct Frozen {
    dib: Dib,
    origin: Rect,
}

struct Picker {
    catcher: HWND,
    loupe: HWND,
    origin: Rect,
    grab: Grabber,
    frozen: Option<Frozen>,
    zoom: u32,
    text: Text,
    dib: Option<Dib>,
    last: Option<(Point, u32)>,
    pressed: bool,
    was_active: bool,
}

thread_local! {
    static STATE: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    with(|p| vec![hwnd_to_isize(p.catcher), hwnd_to_isize(p.loupe)]).unwrap_or_default()
}

pub fn open() -> bool {
    if let Some(c) = with(|p| p.catcher) {
        // SAFETY: our own window.
        unsafe {
            let _ = SetForegroundWindow(c);
        }
        return true;
    }
    if !wnd::register(CATCHER_CLASS, Some(catcher_proc), IDC_CROSS)
        || !wnd::register(LOUPE_CLASS, Some(loupe_proc), IDC_ARROW)
    {
        return false;
    }
    let vs = win::virtual_screen();
    let Some(catcher) = wnd::create(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        CATCHER_CLASS,
        "SnapForge Color Picker",
        WS_POPUP,
        vs,
        HWND::default(),
    ) else {
        return false;
    };
    let Some(loupe) = wnd::create(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
        LOUPE_CLASS,
        "",
        WS_POPUP,
        Rect::new(0, 0, 1, 1),
        HWND::default(),
    ) else {
        // SAFETY: the window we just created.
        unsafe {
            let _ = DestroyWindow(catcher);
        }
        return false;
    };
    win::exclude_from_capture(catcher);
    win::exclude_from_capture(loupe);
    let zoom = crate::settings::get().tools.picker_zoom.clamp(lens::LOUPE_ZOOMS[0], 32);
    STATE.with(|s| {
        *s.borrow_mut() = Some(Picker {
            catcher,
            loupe,
            origin: vs,
            grab: Grabber::default(),
            frozen: None,
            zoom,
            text: Text::default(),
            dib: None,
            last: None,
            pressed: false,
            was_active: false,
        })
    });
    // SAFETY: our own windows; alpha 1 keeps the catcher invisible but hit-testable.
    unsafe {
        let _ = SetLayeredWindowAttributes(catcher, COLORREF(0), 1, LWA_ALPHA);
        let _ = ShowWindow(catcher, SW_SHOW);
        let _ = SetForegroundWindow(catcher);
        let _ = SetFocus(catcher);
        SetTimer(catcher, TIMER_ID, TIMER_MS, None);
    }
    refresh(true);
    true
}

pub fn close() {
    let Some(p) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    let zoom = p.zoom;
    // SAFETY: our own windows and timer.
    unsafe {
        let _ = KillTimer(p.catcher, TIMER_ID);
        let _ = DestroyWindow(p.loupe);
        let _ = DestroyWindow(p.catcher);
    }
    drop(p);
    if crate::settings::get().tools.picker_zoom != zoom {
        crate::settings::update(|s| s.tools.picker_zoom = zoom);
    }
    super::closed(ScreenTool::ColorPicker);
}

impl Picker {
    /// Pixels of `r` (screen coordinates) from the frozen image or the live screen.
    fn sample(&mut self, r: Rect) -> Option<Image> {
        match &self.frozen {
            Some(f) => {
                let local = r.offset(-f.origin.x, -f.origin.y);
                let clip = local.intersect(&Rect::new(0, 0, f.dib.w, f.dib.h));
                let mut out = Image::filled(r.w as u32, r.h as u32, [0, 0, 0, 255]);
                let stride = f.dib.w as usize * 4;
                let data = f.dib.data();
                for row in 0..clip.h {
                    let s = (clip.y + row) as usize * stride + clip.x as usize * 4;
                    let d = (((clip.y - local.y + row) * r.w + (clip.x - local.x)) * 4) as usize;
                    out.data[d..d + clip.w as usize * 4].copy_from_slice(&data[s..s + clip.w as usize * 4]);
                }
                Some(out)
            }
            None => self.grab.grab(r),
        }
    }

    fn color_at(&mut self, p: Point) -> Rgb {
        self.sample(Rect::new(p.x, p.y, 1, 1)).map(|i| Rgb::from_bgra(i.pixel(0, 0))).unwrap_or_default()
    }
}

/// Re-render the loupe at the cursor. `force` re-renders even if nothing moved.
fn refresh(force: bool) {
    let cursor = win::cursor_pos();
    with(|p| {
        let dpi = win::monitor_at(cursor).map_or(96, |m| m.dpi);
        let grid = Grid::fit(sci(CARD_W - 24.0, dpi), p.zoom, dpi);
        let src_rect = grid.source_rect(cursor);
        let Some(src) = p.sample(src_rect) else {
            return;
        };
        let center_px = src.pixel((cursor.x - src_rect.x) as u32, (cursor.y - src_rect.y) as u32);
        let color = Rgb::from_bgra(center_px);
        let key = (cursor, color.to_u32());
        if !force && p.frozen.is_some() && p.last == Some(key) {
            return;
        }
        p.last = Some(key);
        let view = CardView {
            src: &src,
            src_rect,
            center: cursor,
            grid,
            color,
            frozen: p.frozen.is_some(),
            zoom: p.zoom,
            dpi,
        };
        let Some(dib) = render_card(p.dib.take(), &mut p.text, &view) else {
            return;
        };
        let mon = win::monitor_at(cursor).map_or(p.origin, |m| m.rect);
        let gap = sci(18.0, dpi) - sci(MARGIN, dpi);
        let r = crate::overlay::loupe::place(cursor, (dib.w, dib.h), gap, &mon);
        gfx::present(p.loupe, &dib, Point::new(r.x, r.y));
        // SAFETY: our own window; shown without activation.
        unsafe {
            let _ = SetWindowPos(
                p.loupe,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        p.dib = Some(dib);
    });
}

pub(super) struct CardView<'a> {
    pub src: &'a Image,
    pub src_rect: Rect,
    pub center: Point,
    pub grid: Grid,
    pub color: Rgb,
    pub frozen: bool,
    pub zoom: u32,
    pub dpi: u32,
}

/// Draw the loupe card (grid + readout). Returns the DIB (reusing `cache` if it fits).
pub(super) fn render_card(cache: Option<Dib>, text: &mut Text, v: &CardView) -> Option<Dib> {
    let dpi = v.dpi;
    let m = sc(MARGIN, dpi);
    let pad = sc(12.0, dpi);
    let cw = sc(CARD_W, dpi);
    let side = v.grid.side() as f32;
    let readout_h = sc(48.0, dpi);
    let foot_h = sc(40.0, dpi);
    let ch = pad + side + sc(12.0, dpi) + readout_h + sc(8.0, dpi) + foot_h + pad * 0.5;
    let (w, h) = ((cw + 2.0 * m).ceil() as i32, (ch + 2.0 * m).ceil() as i32);
    let mut dib = Dib::reuse(cache, w, h)?;
    dib.clear();
    card::draw_card(&mut dib, m, m, cw, ch, dpi);

    // Zoom grid, centred horizontally.
    let gx = (m + (cw - side) / 2.0).round();
    let gy = (m + pad).round();
    let mut px = vec![0u8; (v.grid.side() * v.grid.side() * 4) as usize];
    lens::render_grid(v.src, v.src_rect, v.center, v.grid, &mut px);
    card::draw_rounded_pixels(&mut dib, &px, v.grid.side(), v.grid.side(), gx, gy, sc(8.0, dpi));
    if v.frozen {
        card::pill(
            &mut dib,
            text,
            &crate::i18n::t("tools-frozen"),
            (gx + sc(8.0, dpi)) as i32,
            (gy + sc(8.0, dpi)) as i32,
            dpi,
            card::ACCENT,
        );
    }

    // Swatch.
    let sx = m + pad;
    let sy = gy + side + sc(12.0, dpi);
    let sw = readout_h;
    if let Some(mut pm) = dib.pixmap() {
        let c = v.color;
        gfx::fill(&mut pm, gfx::rrect(sx, sy, sw, sw, sc(8.0, dpi)), col(c.r, c.g, c.b, 255));
        gfx::stroke(
            &mut pm,
            gfx::rrect(sx + 0.5, sy + 0.5, sw - 1.0, sw - 1.0, sc(8.0, dpi)),
            col(255, 255, 255, 60),
            1.0,
        );
    }

    // Readout: HEX large, RGB and decimal below.
    let tx = (sx + sw + sc(12.0, dpi)) as i32;
    let ty = sy as i32 - sci(3.0, dpi);
    let c = v.color;
    let hex = color::format_value(c, crate::settings::tools::ColorFormat::Html, false);
    text.draw(&mut dib, &hex, tx, ty, sci(17.0, dpi), Face::UiSemibold, card::TEXT, Align::Left);
    let line2 = format!("RGB  {}, {}, {}", c.r, c.g, c.b);
    text.draw(&mut dib, &line2, tx, ty + sci(23.0, dpi), sci(12.0, dpi), Face::Ui, card::TEXT_DIM, Align::Left);
    let line3 = format!("DEC  {}", c.to_u32());
    text.draw(&mut dib, &line3, tx, ty + sci(38.0, dpi), sci(12.0, dpi), Face::Ui, card::TEXT_DIM, Align::Left);

    // Footer: position and zoom, then the hint.
    let fy = (sy + readout_h + sc(10.0, dpi)) as i32;
    if let Some(mut pm) = dib.pixmap() {
        gfx::line(
            &mut pm,
            (m + pad, fy as f32 - sc(4.0, dpi)),
            (m + cw - pad, fy as f32 - sc(4.0, dpi)),
            col(255, 255, 255, 22),
            1.0,
        );
    }
    let pos = format!("X {}   Y {}", v.center.x, v.center.y);
    text.draw(&mut dib, &pos, (m + pad) as i32, fy, sci(11.5, dpi), Face::Ui, card::TEXT, Align::Left);
    text.draw(
        &mut dib,
        &format!("{}\u{00D7}", v.zoom),
        (m + cw - pad) as i32,
        fy,
        sci(11.5, dpi),
        Face::UiSemibold,
        card::TEXT,
        Align::Right,
    );
    let hint = crate::i18n::t("tools-picker-hint");
    text.draw(
        &mut dib,
        &hint,
        (m + cw / 2.0) as i32,
        fy + sci(19.0, dpi),
        sci(10.5, dpi),
        Face::Ui,
        card::TEXT_DIM,
        Align::Center,
    );
    Some(dib)
}

fn toggle_freeze() {
    let done = with(|p| {
        if p.frozen.take().is_some() {
            // SAFETY: our own window; back to the invisible catcher.
            unsafe {
                let _ = SetLayeredWindowAttributes(p.catcher, COLORREF(0), 1, LWA_ALPHA);
            }
            return;
        }
        let vs = p.origin;
        let mut g = Grabber::default();
        if g.grab_dib(vs).is_none() {
            log::warn!("picker: could not freeze the screen");
            return;
        }
        if let Some(dib) = g.into_dib() {
            p.frozen = Some(Frozen { dib, origin: vs });
            // SAFETY: our own window; opaque now, painted from the frozen DIB.
            unsafe {
                let _ = SetLayeredWindowAttributes(p.catcher, COLORREF(0), 255, LWA_ALPHA);
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(p.catcher, None, false);
            }
        }
    });
    if done.is_some() {
        refresh(true);
    }
}

fn change_zoom(up: bool) {
    with(|p| p.zoom = lens::step_loupe_zoom(p.zoom, up));
    refresh(true);
}

fn nudge(dx: i32, dy: i32) {
    let c = win::cursor_pos();
    // SAFETY: plain call.
    unsafe {
        let _ = SetCursorPos(c.x + dx, c.y + dy);
    }
    refresh(true);
}

/// Pick the color under the cursor: copy it, remember it, close, open the fine-tune panel.
fn pick() {
    let cursor = win::cursor_pos();
    let Some(c) = with(|p| p.color_at(cursor)) else {
        return;
    };
    close();
    deliver(c);
    let show = crate::settings::get().tools.picker_show_panel;
    if show {
        super::finetune::open(c, cursor);
    }
}

/// Copy a color in the configured format and add it to the recent colors.
pub(super) fn deliver(c: Rgb) {
    let text = color::format_with_settings(c);
    match crate::output::clipboard::copy_text(&text) {
        Ok(()) => log::info!("picker: copied {text}"),
        Err(e) => {
            log::warn!("picker: clipboard: {e:#}");
            crate::app::notify(crate::i18n::t("app-name"), format!("{e:#}"), true);
        }
    }
    crate::settings::update(|s| s.tools.push_recent_color(c.to_u32()));
}

unsafe extern "system" fn catcher_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE | WM_TIMER => {
            refresh(false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            with(|p| p.pressed = true);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if with(|p| std::mem::take(&mut p.pressed)).unwrap_or(false) {
                pick();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            close();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            change_zoom(wnd::wheel_notches(wp) > 0);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let step = if wnd::mods().shift { 10 } else { 1 };
            match wp.0 as u16 {
                k if k == VK_ESCAPE.0 => close(),
                k if k == VK_SPACE.0 => toggle_freeze(),
                k if k == VK_ADD.0 || k == VK_OEM_PLUS.0 => change_zoom(true),
                k if k == VK_SUBTRACT.0 || k == VK_OEM_MINUS.0 => change_zoom(false),
                k if k == VK_LEFT.0 => nudge(-step, 0),
                k if k == VK_RIGHT.0 => nudge(step, 0),
                k if k == VK_UP.0 => nudge(0, -step),
                k if k == VK_DOWN.0 => nudge(0, step),
                k if k == VK_RETURN.0 => pick(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let inactive = (wp.0 & 0xFFFF) as u32 == WA_INACTIVE;
            let close_now = with(|p| {
                if inactive {
                    p.was_active
                } else {
                    p.was_active = true;
                    false
                }
            })
            .unwrap_or(false);
            if close_now {
                // Another window took the foreground (Alt+Tab, Win key): stop picking.
                close();
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let drawn = with(|p| {
                if let Some(f) = &p.frozen {
                    let r = ps.rcPaint;
                    let _ = BitBlt(
                        hdc,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        f.dib.dc,
                        r.left,
                        r.top,
                        SRCCOPY,
                    );
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if !drawn {
                FillRect(hdc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        // Physical pixels throughout: ignore the suggested DPI resize.
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn loupe_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
