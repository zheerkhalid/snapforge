//! Floating capture toolbar (native Win32, owner-drawn, excluded from captures).
//!
//! CONTRACT: called on the UI thread. Buttons and menus send `crate::app::post(…)`.
//! Persists position/options in `settings.toolbar`.

pub mod icons;
pub mod layout;
mod paint;
#[cfg(test)]
mod preview;

use crate::app::{self, AppCommand};
use crate::common::win::{self, hwnd_to_isize, pcwstr, wide};
use crate::common::{Point, Rect};
use crate::i18n::{t, tf};
use crate::settings::{self, Theme as ThemeSetting};
use crate::tray::menu::{self, Menu};
use anyhow::{Context, Result};
use layout::{Button, Layout, Theme};
use paint::{Dib, IconCache, View};
use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, TOOLTIPS_CLASSW, TTF_SUBCLASS,
    TTM_ADDTOOLW, TTM_DELTOOLW, TTM_SETMAXTIPWIDTH, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, GetWindowRect, IsWindowVisible, KillTimer, LoadCursorW, LoadImageW,
    RegisterClassW, SendMessageW, SetTimer, SetWindowPos, ShowWindow, HICON, HTCAPTION, HTCLIENT, HWND_TOPMOST,
    IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_CONTEXTMENU, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND,
    WM_EXITSIZEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK,
    WM_NCMOUSEMOVE, WM_NCRBUTTONUP, WM_PAINT, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS: &str = "SnapForge.Toolbar";
const TIMER_HIDE: usize = 1;
const TIMER_ANIM: usize = 2;
const HIDE_POLL_MS: u32 = 100;
const HIDE_DELAY_MS: u128 = 800;
const ANIM_MS: f32 = 120.0;
/// Visible strip (at 96 DPI) when slid up.
const STRIP: i32 = 4;
/// Distance (at 96 DPI) within which a drag snaps to the monitor's top edge.
const SNAP: i32 = 12;

const ID_BIG: u32 = menu::ID_USER;
const ID_LOCK: u32 = menu::ID_USER + 1;
const ID_AUTOHIDE: u32 = menu::ID_USER + 2;
const ID_MINIMIZE: u32 = menu::ID_USER + 3;
const ID_EXIT: u32 = menu::ID_USER + 4;
const ID_BUTTON: u32 = menu::ID_USER + 10; // + index in Button::configurable()

#[derive(Debug, Clone, Copy, PartialEq)]
enum Slide {
    Shown,
    Hidden,
    Anim { from: i32, to: i32, start: Instant, hiding: bool },
}

struct Toolbar {
    hwnd: HWND,
    tip: HWND,
    tool_count: usize,
    dpi: u32,
    theme: Theme,
    layout: Layout,
    hover: Option<usize>,
    pressed: Option<usize>,
    tracking: bool,
    icons: IconCache,
    back: Option<Dib>,
    app_icon: (i32, HICON),
    /// Top-left when fully shown (physical pixels).
    pos: Point,
    slide: Slide,
    outside_since: Option<Instant>,
    hide_timer: bool,
    moving: bool,
    menu_open: bool,
}

impl Drop for Toolbar {
    fn drop(&mut self) {
        if !self.app_icon.1.is_invalid() {
            // SAFETY: we own the icon handle.
            unsafe {
                let _ = DestroyIcon(self.app_icon.1);
            }
        }
    }
}

thread_local! {
    static TB: RefCell<Option<Toolbar>> = const { RefCell::new(None) };
}

/// Run `f` on the toolbar state. Returns None if not created or already borrowed
/// (re-entrant message during a call that holds the state).
fn with<R>(f: impl FnOnce(&mut Toolbar) -> R) -> Option<R> {
    TB.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

// ---------------------------------------------------------------- helpers

fn is_dark() -> bool {
    match settings::get().general.theme {
        ThemeSetting::System => win::system_dark_mode(),
        ThemeSetting::Light => false,
        ThemeSetting::Dark => true,
    }
}

fn current_buttons() -> Vec<Button> {
    layout::buttons_from_ids(&settings::get().toolbar.buttons)
}

fn compute_layout(dpi: u32) -> Layout {
    let big = settings::get().toolbar.big_buttons;
    layout::compute(&current_buttons(), dpi, big)
}

fn load_app_icon(size: i32) -> HICON {
    // SAFETY: loads icon resource 1 (MAKEINTRESOURCE(1)) from our own module.
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return HICON::default() };
        LoadImageW(hinst, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default()
    }
}

fn window_rect(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    win::rect_from_win(&r)
}

fn set_pos(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: moving our own window; the state borrow is not held by callers.
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn set_rect(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    // SAFETY: as above.
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn invalidate(hwnd: HWND) {
    // SAFETY: plain call on our own window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Default position: top-centre of the primary monitor's work area.
fn default_pos(w: i32) -> Point {
    let mons = win::monitors();
    let work = mons.iter().find(|m| m.primary).or(mons.first()).map(|m| m.work).unwrap_or(Rect::new(0, 0, 1280, 720));
    Point::new(work.x + (work.w - w) / 2, work.y)
}

/// True when enough of `r` is on some monitor to be grabbed.
fn on_screen(r: Rect) -> bool {
    win::monitors().iter().any(|m| {
        let i = r.intersect(&m.rect);
        i.w >= (r.w / 3).max(1) && i.h >= (r.h / 2).max(1)
    })
}

/// The monitor rect when `r` is docked at a top edge that has no monitor above it.
fn docked_monitor(r: Rect) -> Option<Rect> {
    let m = win::monitor_at(Point::new(r.x + r.w / 2, r.y + r.h / 2))?;
    if r.y > m.rect.y {
        return None;
    }
    let above = Point::new((r.x + r.w / 2).clamp(m.rect.x, m.rect.right() - 1), m.rect.y - 1);
    if win::monitors().iter().any(|o| o.rect.contains(above)) {
        return None;
    }
    Some(m.rect)
}

fn tooltip_text(b: Button) -> String {
    match b {
        Button::Mode(m) => {
            let label = t(m.label_key());
            let hk = crate::hotkeys::display(settings::get().hotkeys.for_mode(m));
            if hk.is_empty() {
                label
            } else {
                format!("{label}  ({hk})")
            }
        }
        Button::Output => {
            let d = settings::get().output.destination;
            tf("toolbar-output-tip", &[("dest", &t(d.label_key()))])
        }
        Button::Delay => tf("toolbar-delay-tip", &[("delay", &menu::delay_label(settings::get().capture.delay_secs))]),
        Button::Menu => t("toolbar-menu"),
        Button::Minimize => t("toolbar-minimize"),
    }
}

// ---------------------------------------------------------------- tooltips

fn tool_info(hwnd: HWND, id: usize) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_SUBCLASS,
        hwnd,
        uId: id,
        ..Default::default()
    }
}

/// Re-register one tooltip tool per button. Call without holding the state borrow.
fn update_tooltips() {
    let Some((hwnd, tip, old, items, dark)) = with(|tb| {
        let items: Vec<(Button, Rect)> = tb.layout.items.iter().map(|i| (i.button, i.rect)).collect();
        let old = tb.tool_count;
        tb.tool_count = items.len();
        (tb.hwnd, tb.tip, old, items, tb.theme.dark)
    }) else {
        return;
    };
    if tip.is_invalid() {
        return;
    }
    // SAFETY: messages to our own tooltip control with valid TTTOOLINFOW structs; the
    // text buffer outlives each (synchronous) call and is copied by the control.
    unsafe {
        for id in 0..old {
            let ti = tool_info(hwnd, id);
            SendMessageW(tip, TTM_DELTOOLW, WPARAM(0), LPARAM(&ti as *const _ as isize));
        }
        for (id, (b, r)) in items.iter().enumerate() {
            let mut text = wide(&tooltip_text(*b));
            let mut ti = tool_info(hwnd, id);
            ti.rect = win::rect_to_win(r);
            ti.lpszText = windows::core::PWSTR(text.as_mut_ptr());
            SendMessageW(tip, TTM_ADDTOOLW, WPARAM(0), LPARAM(&ti as *const _ as isize));
        }
        let theme = wide(if dark { "DarkMode_Explorer" } else { "Explorer" });
        let _ = SetWindowTheme(tip, pcwstr(&theme), None);
    }
}

fn apply_border_color(hwnd: HWND, th: &Theme) {
    let c = th.border;
    let v = COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16);
    // SAFETY: valid pointer and size; harmless on Windows 10.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &v as *const _ as *const c_void, 4);
    }
}

// ---------------------------------------------------------------- public API

pub fn create() -> Result<()> {
    if hwnd() != 0 {
        return Ok(());
    }
    // SAFETY: plain initialisation call with a valid struct.
    unsafe {
        let icc =
            INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_BAR_CLASSES };
        let _ = InitCommonControlsEx(&icc);
    }
    let class = wide(CLASS);
    let hinst = unsafe { GetModuleHandleW(None) }.context("module handle")?;
    // SAFETY: class registration with a static wndproc (repeat registration fails harmlessly).
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }

    // Initial position and the DPI of the monitor it lands on.
    let (sx, sy) = {
        let s = settings::get();
        (s.toolbar.x, s.toolbar.y)
    };
    let probe = compute_layout(96);
    let mut pos = match (sx, sy) {
        (Some(x), Some(y)) => Point::new(x, y),
        _ => default_pos(probe.w),
    };
    let dpi = win::monitor_at(Point::new(pos.x + probe.w / 2, pos.y + probe.h / 2)).map(|m| m.dpi).unwrap_or(96);
    let mut lay = compute_layout(dpi);
    if !on_screen(Rect::new(pos.x, pos.y, lay.w, lay.h)) {
        let dpi0 = win::monitors().iter().find(|m| m.primary).map(|m| m.dpi).unwrap_or(96);
        lay = compute_layout(dpi0);
        pos = default_pos(lay.w);
    }

    let title = wide("SnapForge");
    // SAFETY: creating our own popup window; the state is stored after creation.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            pcwstr(&class),
            pcwstr(&title),
            WS_POPUP,
            pos.x,
            pos.y,
            lay.w,
            lay.h,
            None,
            None,
            hinst,
            None,
        )
    }
    .context("toolbar window")?;
    win::round_corners(hwnd);
    win::exclude_from_capture(hwnd);

    // SAFETY: tooltip control owned by the toolbar.
    let tip = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            None,
            WS_POPUP | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
            0,
            0,
            0,
            0,
            hwnd,
            None,
            hinst,
            None,
        )
    }
    .unwrap_or_default();
    if !tip.is_invalid() {
        win::exclude_from_capture(tip);
        // SAFETY: message to our own tooltip control.
        unsafe {
            SendMessageW(tip, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(400));
        }
    }

    let real_dpi = win::dpi_for_window(hwnd);
    if real_dpi != dpi {
        lay = compute_layout(real_dpi);
        set_rect(hwnd, pos.x, pos.y, lay.w, lay.h);
    }
    let theme = Theme::new(is_dark());
    apply_border_color(hwnd, &theme);
    let app_size = lay.app_icon.w;
    let tb = Toolbar {
        hwnd,
        tip,
        tool_count: 0,
        dpi: real_dpi,
        theme,
        layout: lay,
        hover: None,
        pressed: None,
        tracking: false,
        icons: IconCache::default(),
        back: None,
        app_icon: (app_size, load_app_icon(app_size)),
        pos,
        slide: Slide::Shown,
        outside_since: None,
        hide_timer: false,
        moving: false,
        menu_open: false,
    };
    TB.with(|c| *c.borrow_mut() = Some(tb));
    update_tooltips();
    log::debug!("toolbar created at {},{}", pos.x, pos.y);
    Ok(())
}

pub fn show(visible: bool) {
    let Some(h) = with(|tb| tb.hwnd) else { return };
    if visible {
        ensure_on_screen();
        // SAFETY: showing our own window without activating it.
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                h,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
            );
        }
        arm_hide();
    } else {
        let pos = with(|tb| {
            stop_timers(tb);
            tb.slide = Slide::Shown;
            tb.hover = None;
            tb.pressed = None;
            tb.pos
        });
        // SAFETY: hiding our own window.
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
        if let Some(p) = pos {
            set_pos(h, p.x, p.y);
        }
    }
    if settings::get().toolbar.visible != visible {
        settings::update(|s| s.toolbar.visible = visible);
    }
}

pub fn toggle() {
    show(!is_visible());
}

pub fn is_visible() -> bool {
    let h = hwnd();
    // SAFETY: plain query.
    h != 0 && unsafe { IsWindowVisible(win::hwnd_from_isize(h)).as_bool() }
}

/// Settings changed (destination icon, delay, visible buttons, size…): relayout/repaint.
pub fn refresh() {
    let Some((hwnd, size_changed, pos, slide)) = with(|tb| {
        let dark = is_dark();
        if dark != tb.theme.dark {
            tb.theme = Theme::new(dark);
            tb.icons.clear();
            apply_border_color(tb.hwnd, &tb.theme);
        }
        let lay = compute_layout(tb.dpi);
        let changed = lay.w != tb.layout.w || lay.h != tb.layout.h;
        if lay.app_icon.w != tb.app_icon.0 {
            let old = std::mem::replace(&mut tb.app_icon, (lay.app_icon.w, load_app_icon(lay.app_icon.w)));
            if !old.1.is_invalid() {
                // SAFETY: we own the old icon.
                unsafe {
                    let _ = DestroyIcon(old.1);
                }
            }
        }
        if changed {
            tb.icons.clear();
            tb.back = None;
        }
        tb.layout = lay;
        tb.hover = None;
        // Auto-hide turned off while slid up: bring it back.
        if !settings::get().toolbar.auto_hide && tb.slide != Slide::Shown {
            stop_timers(tb);
            tb.slide = Slide::Shown;
        }
        (tb.hwnd, changed, tb.pos, tb.slide)
    }) else {
        return;
    };
    let (w, h) = with(|tb| (tb.layout.w, tb.layout.h)).unwrap_or((0, 0));
    let y = if slide == Slide::Hidden { hidden_y(pos.y, h) } else { pos.y };
    if size_changed {
        set_rect(hwnd, pos.x, y, w, h);
    } else if slide == Slide::Shown {
        set_pos(hwnd, pos.x, pos.y);
    }
    update_tooltips();
    invalidate(hwnd);
    arm_hide();
}

/// Toolbar window handle (0 if not created), so captures can exclude it.
pub fn hwnd() -> isize {
    TB.with(|c| c.try_borrow().ok().and_then(|g| g.as_ref().map(|t| hwnd_to_isize(t.hwnd)))).unwrap_or(0)
}

// ---------------------------------------------------------------- position / auto-hide

fn hidden_y(shown_y: i32, h: i32) -> i32 {
    let dpi = with(|tb| tb.dpi).unwrap_or(96);
    shown_y - h + win::scale(STRIP, dpi).max(2)
}

fn ensure_on_screen() {
    let Some((hwnd, pos, w, h)) = with(|tb| (tb.hwnd, tb.pos, tb.layout.w, tb.layout.h)) else { return };
    if on_screen(Rect::new(pos.x, pos.y, w, h)) {
        return;
    }
    let p = default_pos(w);
    log::info!("toolbar: saved position off-screen, moving to {},{}", p.x, p.y);
    with(|tb| {
        tb.pos = p;
        tb.slide = Slide::Shown;
    });
    set_pos(hwnd, p.x, p.y);
}

fn stop_timers(tb: &mut Toolbar) {
    // SAFETY: killing our own timers (no-op if not running).
    unsafe {
        let _ = KillTimer(tb.hwnd, TIMER_HIDE);
        let _ = KillTimer(tb.hwnd, TIMER_ANIM);
    }
    tb.hide_timer = false;
}

fn can_auto_hide(tb: &Toolbar) -> bool {
    let s = settings::get();
    // SAFETY: plain query.
    let visible = unsafe { IsWindowVisible(tb.hwnd).as_bool() };
    s.toolbar.auto_hide
        && visible
        && !tb.moving
        && !tb.menu_open
        && tb.pressed.is_none()
        && docked_monitor(Rect::new(tb.pos.x, tb.pos.y, tb.layout.w, tb.layout.h)).is_some()
}

/// Start polling for "cursor outside for 800 ms" (only while shown and docked).
fn arm_hide() {
    with(|tb| {
        if tb.slide == Slide::Shown && can_auto_hide(tb) && !tb.hide_timer {
            tb.outside_since = None;
            tb.hide_timer = true;
            // SAFETY: timer on our own window.
            unsafe {
                SetTimer(tb.hwnd, TIMER_HIDE, HIDE_POLL_MS, None);
            }
        }
    });
}

fn disarm_hide(tb: &mut Toolbar) {
    if tb.hide_timer {
        // SAFETY: killing our own timer.
        unsafe {
            let _ = KillTimer(tb.hwnd, TIMER_HIDE);
        }
        tb.hide_timer = false;
    }
}

fn start_slide(tb: &mut Toolbar, hiding: bool) {
    let cur = window_rect(tb.hwnd).y;
    let to = if hiding { hidden_y_for(tb) } else { tb.pos.y };
    if cur == to {
        tb.slide = if hiding { Slide::Hidden } else { Slide::Shown };
        return;
    }
    tb.slide = Slide::Anim { from: cur, to, start: Instant::now(), hiding };
    // SAFETY: timer on our own window; ~USER_TIMER_MINIMUM for smooth motion, only while animating.
    unsafe {
        SetTimer(tb.hwnd, TIMER_ANIM, 10, None);
    }
}

fn hidden_y_for(tb: &Toolbar) -> i32 {
    tb.pos.y - tb.layout.h + win::scale(STRIP, tb.dpi).max(2)
}

fn on_hide_timer() {
    with(|tb| {
        if tb.slide != Slide::Shown || !can_auto_hide(tb) {
            disarm_hide(tb);
            return;
        }
        let r = window_rect(tb.hwnd);
        if r.contains(win::cursor_pos()) {
            tb.outside_since = None;
            return;
        }
        let since = *tb.outside_since.get_or_insert_with(Instant::now);
        if since.elapsed().as_millis() >= HIDE_DELAY_MS {
            disarm_hide(tb);
            tb.hover = None;
            start_slide(tb, true);
        }
    });
}

fn on_anim_timer() {
    let step = with(|tb| {
        let Slide::Anim { from, to, start, hiding } = tb.slide else {
            // SAFETY: killing our own timer.
            unsafe {
                let _ = KillTimer(tb.hwnd, TIMER_ANIM);
            }
            return None;
        };
        let t = (start.elapsed().as_secs_f32() * 1000.0 / ANIM_MS).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        let y = from + ((to - from) as f32 * eased).round() as i32;
        if t >= 1.0 {
            // SAFETY: killing our own timer.
            unsafe {
                let _ = KillTimer(tb.hwnd, TIMER_ANIM);
            }
            tb.slide = if hiding { Slide::Hidden } else { Slide::Shown };
        }
        Some((tb.hwnd, tb.pos.x, y, t >= 1.0 && !hiding))
    })
    .flatten();
    if let Some((hwnd, x, y, shown)) = step {
        set_pos(hwnd, x, y);
        if shown {
            arm_hide();
        }
    }
}

/// Cursor touched the window: slide down if it is (or is going) up.
fn reveal(tb: &mut Toolbar) {
    let going_up = matches!(tb.slide, Slide::Hidden | Slide::Anim { hiding: true, .. });
    if going_up {
        start_slide(tb, false);
    }
}

fn on_exit_move() {
    let Some((hwnd, r, dpi)) = with(|tb| {
        tb.moving = false;
        (tb.hwnd, window_rect(tb.hwnd), tb.dpi)
    }) else {
        return;
    };
    let mut p = Point::new(r.x, r.y);
    // Snap to the top edge of the monitor when close (or dragged above it).
    if let Some(m) = win::monitor_at(Point::new(r.x + r.w / 2, r.y.max(r.y + r.h / 2))) {
        if p.y < m.rect.y || p.y - m.rect.y <= win::scale(SNAP, dpi) {
            p.y = m.rect.y;
        }
    }
    if p.y != r.y {
        set_pos(hwnd, p.x, p.y);
    }
    with(|tb| tb.pos = p);
    settings::update(|s| {
        s.toolbar.x = Some(p.x);
        s.toolbar.y = Some(p.y);
    });
    arm_hide();
}

fn on_dpi_changed(new_dpi: u32, suggested: &RECT) {
    let Some(hwnd) = with(|tb| {
        tb.dpi = new_dpi;
        tb.layout = compute_layout(new_dpi);
        tb.icons.clear();
        tb.back = None;
        let size = tb.layout.app_icon.w;
        let old = std::mem::replace(&mut tb.app_icon, (size, load_app_icon(size)));
        if !old.1.is_invalid() {
            // SAFETY: we own the old icon.
            unsafe {
                let _ = DestroyIcon(old.1);
            }
        }
        if tb.slide == Slide::Shown {
            tb.pos = Point::new(suggested.left, suggested.top);
        }
        tb.hwnd
    }) else {
        return;
    };
    let (w, h) = with(|tb| (tb.layout.w, tb.layout.h)).unwrap_or((0, 0));
    set_rect(hwnd, suggested.left, suggested.top, w, h);
    update_tooltips();
    invalidate(hwnd);
}

// ---------------------------------------------------------------- menus & clicks

fn screen_rect(hwnd: HWND, r: &Rect) -> RECT {
    let wr = window_rect(hwnd);
    RECT { left: wr.x + r.x, top: wr.y + r.y, right: wr.x + r.right(), bottom: wr.y + r.bottom() }
}

/// Run a modal menu with the hide timer suspended and the state borrow released.
fn modal_menu(f: impl FnOnce()) {
    with(|tb| {
        tb.menu_open = true;
        disarm_hide(tb);
    });
    f();
    let h = with(|tb| {
        tb.menu_open = false;
        tb.pressed = None;
        tb.hover = None;
        tb.hwnd
    });
    if let Some(h) = h {
        invalidate(h);
    }
    // Settings may have changed through the menu; repaint with fresh state.
    refresh();
}

fn show_context_menu(x: i32, y: i32) {
    let Some(m) = Menu::new() else { return };
    let (big, locked, auto_hide, ids) = {
        let s = settings::get();
        (s.toolbar.big_buttons, s.toolbar.locked, s.toolbar.auto_hide, s.toolbar.buttons.clone())
    };
    m.item(ID_BIG, &t("toolbar-big-buttons"), big);
    m.item(ID_LOCK, &t("toolbar-lock"), locked);
    m.item(ID_AUTOHIDE, &t("toolbar-auto-hide"), auto_hide);
    if let Some(sub) = Menu::new() {
        let visible = layout::buttons_from_ids(&ids);
        for (i, b) in Button::configurable().into_iter().enumerate() {
            let label = match b {
                Button::Mode(mode) => t(mode.label_key()),
                Button::Output => t("output-to"),
                Button::Delay => t("delay"),
                _ => t("toolbar-menu"),
            };
            sub.item(ID_BUTTON + i as u32, &label, visible.contains(&b));
        }
        m.submenu(sub, &t("toolbar-customize"));
    }
    m.separator();
    m.item(ID_MINIMIZE, &t("toolbar-minimize"), false);
    m.item(ID_EXIT, &t("tray-exit"), false);
    let mut chosen = 0;
    modal_menu(|| chosen = m.track(x, y, None));
    drop(m);
    match chosen {
        0 => {}
        ID_BIG => settings::update(|s| s.toolbar.big_buttons = !s.toolbar.big_buttons),
        ID_LOCK => settings::update(|s| s.toolbar.locked = !s.toolbar.locked),
        ID_AUTOHIDE => settings::update(|s| s.toolbar.auto_hide = !s.toolbar.auto_hide),
        ID_MINIMIZE => {
            show(false);
            return;
        }
        ID_EXIT => {
            app::post(AppCommand::Exit);
            return;
        }
        id if id >= ID_BUTTON => {
            if let Some(b) = Button::configurable().get((id - ID_BUTTON) as usize).copied() {
                settings::update(|s| s.toolbar.buttons = layout::toggle_button(&s.toolbar.buttons, b));
            }
        }
        _ => {}
    }
    refresh();
}

fn activate(index: usize) {
    let Some((hwnd, item)) = with(|tb| (tb.hwnd, tb.layout.items.get(index).copied())) else { return };
    let Some(item) = item else { return };
    match item.button {
        Button::Mode(m) => app::post(AppCommand::Capture(m)),
        Button::Minimize => show(false),
        b @ (Button::Output | Button::Delay | Button::Menu) => {
            let r = screen_rect(hwnd, &item.rect);
            with(|tb| tb.pressed = Some(index));
            invalidate(hwnd);
            modal_menu(|| match b {
                Button::Output => menu::show_output(r.left, r.bottom, Some(r)),
                Button::Delay => menu::show_delay(r.left, r.bottom, Some(r)),
                _ => menu::show_full(r.left, r.bottom, Some(r)),
            });
        }
    }
}

fn is_menu_button(b: Button) -> bool {
    matches!(b, Button::Output | Button::Delay | Button::Menu)
}

// ---------------------------------------------------------------- window procedure

fn lparam_point(lp: LPARAM) -> Point {
    Point::new((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard BeginPaint/EndPaint pair on our window.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    with(|tb| {
        let (w, h) = (tb.layout.w, tb.layout.h);
        if tb.back.as_ref().map(|b| (b.w, b.h)) != Some((w, h)) {
            tb.back = Dib::new(w, h);
        }
        let (dest, delay) = {
            let s = settings::get();
            (s.output.destination, s.capture.delay_secs)
        };
        let view = View {
            layout: &tb.layout,
            theme: tb.theme,
            hover: tb.hover,
            pressed: tb.pressed,
            dest,
            delay,
            app_icon: tb.app_icon.1,
        };
        if let Some(back) = tb.back.as_mut() {
            paint::render(back, &view, &mut tb.icons);
            // SAFETY: both DCs are valid for the duration of the call.
            unsafe {
                let _ = BitBlt(hdc, 0, 0, w, h, back.dc, 0, 0, SRCCOPY);
            }
        }
    });
    // SAFETY: matches BeginPaint above.
    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

fn set_hover(hit: Option<usize>) {
    let changed = with(|tb| {
        let changed = tb.hover != hit;
        tb.hover = hit;
        if !tb.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: tb.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: valid struct for our own window.
            tb.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        disarm_hide(tb);
        reveal(tb);
        changed.then_some(tb.hwnd)
    })
    .flatten();
    if let Some(h) = changed {
        invalidate(h);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            on_paint(hwnd);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let p = lparam_point(lp);
            let r = window_rect(hwnd);
            let local = Point::new(p.x - r.x, p.y - r.y);
            let locked = settings::get().toolbar.locked;
            let grip = with(|tb| tb.layout.in_grip(local)).unwrap_or(false);
            return LRESULT(if grip && !locked { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_NCLBUTTONDBLCLK => return LRESULT(0),
        WM_NCMOUSEMOVE => {
            with(reveal);
        }
        WM_MOUSEMOVE => {
            let p = lparam_point(lp);
            let hit = with(|tb| tb.layout.hit(p)).flatten();
            set_hover(hit);
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            let h = with(|tb| {
                tb.tracking = false;
                tb.hover = None;
                tb.hwnd
            });
            if let Some(h) = h {
                invalidate(h);
            }
            arm_hide();
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let p = lparam_point(lp);
            let hit = with(|tb| tb.layout.hit(p).map(|i| (i, tb.layout.items[i].button))).flatten();
            match hit {
                Some((i, b)) if is_menu_button(b) => activate(i),
                Some((i, _)) => {
                    with(|tb| tb.pressed = Some(i));
                    SetCapture(hwnd);
                    invalidate(hwnd);
                }
                None => {}
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let p = lparam_point(lp);
            let fire = with(|tb| {
                let pressed = tb.pressed.take();
                let hit = tb.layout.hit(p);
                pressed.filter(|&i| Some(i) == hit)
            })
            .flatten();
            let _ = ReleaseCapture();
            invalidate(hwnd);
            if let Some(i) = fire {
                activate(i);
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP | WM_NCRBUTTONUP => {
            let mut pt = POINT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
            show_context_menu(pt.x, pt.y);
            return LRESULT(0);
        }
        WM_CONTEXTMENU => return LRESULT(0),
        WM_ENTERSIZEMOVE => {
            with(|tb| {
                tb.moving = true;
                disarm_hide(tb);
            });
        }
        WM_EXITSIZEMOVE => {
            on_exit_move();
            return LRESULT(0);
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_HIDE => on_hide_timer(),
                TIMER_ANIM => on_anim_timer(),
                _ => {}
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            if lp.0 != 0 {
                // SAFETY: for WM_DPICHANGED, lParam points to the suggested RECT.
                let r = *(lp.0 as *const RECT);
                on_dpi_changed(dpi, &r);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            if lp.0 != 0 {
                // SAFETY: lParam is either null or a NUL-terminated wide string.
                let s = PCWSTR(lp.0 as *const u16).to_string().unwrap_or_default();
                if s == "ImmersiveColorSet" {
                    refresh();
                }
            }
        }
        WM_DISPLAYCHANGE => {
            ensure_on_screen();
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
