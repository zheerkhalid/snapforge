//! Pin windows and their Draw Mode palette (native Win32, UI thread).
//!
//! Each pin owns a `RefCell<Pin>` stored in `GWLP_USERDATA` (freed on `WM_NCDESTROY`).
//! Handlers borrow it only for short, message-free sections and run Win32 calls that can
//! re-enter the window procedure (SetWindowPos, DestroyWindow, menus, dialogs) after the
//! borrow ends. A busy borrow falls back to `DefWindowProcW` instead of panicking.

use super::annot::{self, History, Snap, Style, Tool};
use super::gdi::Dib;
use super::geom::{self, Edges};
use super::keys::{self, Cmd, Key, Mode};
use super::paint::{self, Live};
use super::palette::{self, Item};
use crate::app;
use crate::common::win::{self, hwnd_from_isize, hwnd_to_isize, pcwstr, wide};
use crate::common::{Image, Point, Rect};
use crate::i18n::{t, tf};
use crate::output::{clipboard, save};
use crate::settings::{self, pin::CanvasFill};
use crate::tray::menu::Menu;
use std::cell::RefCell;
use std::ffi::c_void;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmGetColorizationColor, DwmSetWindowAttribute, DWMWA_BORDER_COLOR,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    ChooseColorW, GetSaveFileNameW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{MARGINS, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL,
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowLongW,
    GetWindowRect, IsWindow, KillTimer, LoadCursorW, PostMessageW, RegisterClassW, SetCursor, SetForegroundWindow,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowLongW, SetWindowPos, ShowWindow,
    TrackPopupMenuEx, CS_DBLCLKS, GWLP_USERDATA, GWL_EXSTYLE, GW_OWNER, HCURSOR, HTCLIENT, HWND_TOPMOST, IDC_ARROW,
    IDC_CROSS, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, LWA_ALPHA,
    MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_CAPTURECHANGED, WM_CHAR, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOVE,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NULL, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};

const PIN_CLASS: &str = "SnapForge.Pin";
const PALETTE_CLASS: &str = "SnapForge.PinPalette";
const TIMER_BADGE: usize = 1;
const TIMER_CARET: usize = 2;
const BADGE_MS: u32 = 1200;
const CARET_MS: u32 = 530;
const DRAW_BORDER: u32 = 0xF5_9E_0B;

// Context menu ids.
const ID_COPY: u32 = 1;
const ID_SAVE_AS: u32 = 2;
const ID_EDITOR: u32 = 3;
const ID_EDITOR_ALL: u32 = 4;
const ID_ZOOM_100: u32 = 5;
const ID_DRAW: u32 = 6;
const ID_HIDE: u32 = 7;
const ID_CLOSE: u32 = 8;
const ID_CLOSE_ALL: u32 = 9;
const ID_OPACITY: u32 = 20; // + 0..=9 → 100%..10%

thread_local! {
    /// Open pins (HWNDs as isize), oldest first. UI thread only.
    static PINS: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    static CUSTOM_COLORS: RefCell<[COLORREF; 16]> = const { RefCell::new([COLORREF(0x00FF_FFFF); 16]) };
}

struct TextEntry {
    x: f32,
    y: f32,
    text: String,
    high_surrogate: Option<u16>,
}

enum Drag {
    None,
    /// Moving the pin (from the pin or the palette grip).
    Move {
        start: Point,
        origin: (f64, f64),
    },
    Stroke,
    Canvas {
        start: Point,
        edges: Edges,
        base: Image,
        base_origin: (f64, f64),
        base_transparent: bool,
    },
    TabClick {
        start: Point,
        moved: bool,
    },
}

pub(super) struct Pin {
    hwnd: HWND,
    palette: HWND,
    img: Image,
    transparent: bool,
    zoom: f64,
    /// Screen position of the image's top-left pixel.
    origin: (f64, f64),
    opacity: u8,
    mode: Mode,
    tool: Tool,
    style: Style,
    next_step: u32,
    history: History,
    drag: Drag,
    stroke: Vec<(f32, f32)>,
    text: Option<TextEntry>,
    caret_on: bool,
    /// Cursor position in image coordinates (step ghost).
    hover: Option<(f32, f32)>,
    badge: Option<String>,
    dpi: u32,
    accent: u32,
    /// Collapsed tab position (screen).
    tab_at: Point,
    wheel_acc: i32,
    back: Option<Dib>,
    pal_layout: palette::Layout,
    pal_hover: Option<Item>,
    pal_tracking: bool,
}

impl Pin {
    fn border(&self) -> i32 {
        win::scale(1, self.dpi).max(1)
    }

    fn view_size(&self) -> (i32, i32) {
        geom::view_size(self.img.width, self.img.height, self.zoom)
    }

    /// Window rect for the current state.
    fn window_rect(&self) -> Rect {
        if let Mode::Tab { .. } = self.mode {
            let (w, h) = paint::tab_size(self.dpi);
            return Rect::new(self.tab_at.x, self.tab_at.y, w, h);
        }
        let b = self.border();
        let (w, h) = self.view_size();
        Rect::new(self.origin.0.round() as i32 - b, self.origin.1.round() as i32 - b, w + 2 * b, h + 2 * b)
    }

    /// Client point → image coordinates.
    fn to_image(&self, p: Point) -> (f32, f32) {
        let b = self.border() as f64;
        geom::client_to_image(p.x as f64 - b, p.y as f64 - b, self.zoom)
    }

    fn drawing(&self) -> bool {
        matches!(self.mode, Mode::Drawing | Mode::Typing)
    }

    fn snapshot(&self) -> Snap {
        Snap { img: self.img.clone(), next_step: self.next_step, shift: (0, 0) }
    }

    /// Rasterize the pending text into the image (no-op when empty). Returns true if drawn.
    fn commit_text(&mut self) -> bool {
        let Some(te) = self.text.take() else { return false };
        if self.mode == Mode::Typing {
            self.mode = Mode::Drawing;
        }
        if te.text.trim().is_empty() {
            return false;
        }
        let snap = self.snapshot();
        let style = self.style;
        if let Some(mut pm) = annot::pixmap_mut(&mut self.img) {
            annot::text(&mut pm, te.x, te.y, &te.text, style.color, style.text_px(), tiny_skia::Transform::identity());
        }
        self.history.push(snap);
        self.transparent = self.img.has_transparency();
        true
    }

    fn live(&self) -> Live<'_> {
        match (&self.drag, self.mode, self.tool) {
            (Drag::Stroke, _, _) => {
                Live::Stroke { pts: &self.stroke, rgb: self.style.color, width: self.style.pen_width() }
            }
            (_, Mode::Typing, _) => match &self.text {
                Some(te) => Live::Text {
                    x: te.x,
                    y: te.y,
                    text: &te.text,
                    rgb: self.style.color,
                    px: self.style.text_px(),
                    caret: self.caret_on,
                },
                None => Live::None,
            },
            (Drag::None, Mode::Drawing, Tool::Step) => match self.hover {
                Some((x, y)) => {
                    Live::Step { x, y, n: self.next_step, rgb: self.style.color, radius: self.style.stamp_radius() }
                }
                None => Live::None,
            },
            _ => Live::None,
        }
    }
}

// ------------------------------------------------------------------ state access

fn state(hwnd: HWND) -> Option<&'static RefCell<Pin>> {
    // SAFETY: GWLP_USERDATA holds a `Box<RefCell<Pin>>` pointer set in `create` and cleared
    // in WM_NCDESTROY before the box is freed; pins are only touched on the UI thread.
    unsafe {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<Pin>;
        p.as_ref()
    }
}

/// Run `f` with the pin's state; `None` if the window is gone or the state is busy.
fn with<R>(hwnd: HWND, f: impl FnOnce(&mut Pin) -> R) -> Option<R> {
    let cell = state(hwnd)?;
    let mut g = cell.try_borrow_mut().ok()?;
    Some(f(&mut g))
}

fn invalidate(hwnd: HWND) {
    // SAFETY: invalidating our own window (or a stale handle, which fails harmlessly).
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn lparam_point(lp: LPARAM) -> Point {
    Point::new((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn key_down(vk: u16) -> bool {
    // SAFETY: plain key-state query.
    unsafe { GetKeyState(vk as i32) < 0 }
}

fn window_rect_of(hwnd: HWND) -> Rect {
    let mut r = RECT::default();
    // SAFETY: valid out pointer; a stale handle leaves it zeroed.
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    win::rect_from_win(&r)
}

fn window_origin(hwnd: HWND) -> Point {
    let r = window_rect_of(hwnd);
    Point::new(r.x, r.y)
}

fn accent_color() -> u32 {
    let mut c = 0u32;
    let mut opaque = BOOL(0);
    // SAFETY: valid out pointers.
    if unsafe { DwmGetColorizationColor(&mut c, &mut opaque) }.is_ok() && c != 0 {
        c & 0x00FF_FFFF
    } else {
        0x00_78_D4
    }
}

// ------------------------------------------------------------------ public helpers

pub(super) fn all() -> Vec<HWND> {
    PINS.with(|p| p.borrow().iter().map(|&h| hwnd_from_isize(h)).collect())
}

/// The pin's image (pending text committed first).
pub(super) fn image_of(hwnd: HWND) -> Option<Image> {
    let r = with(hwnd, |p| {
        if p.commit_text() {
            invalidate(p.hwnd);
        }
        p.img.clone()
    });
    if r.is_some() {
        sync(hwnd);
    }
    r
}

pub(super) fn close(hwnd: HWND) {
    // SAFETY: destroying our own window; a stale handle fails harmlessly.
    unsafe {
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
}

// ------------------------------------------------------------------ creation

fn register_classes() {
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    // SAFETY: registering window classes with static wndprocs; names outlive the call.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let pin = wide(PIN_CLASS);
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(pin_proc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&pin),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let pal = wide(PALETTE_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(palette_proc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&pal),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

/// Create a pin window. Must run on the UI thread.
pub(super) fn create(img: Image, at: Option<Rect>) {
    if img.is_empty() {
        log::warn!("pin: refusing to pin an empty image");
        return;
    }
    register_classes();
    let cfg = settings::get().pin.clone();
    let anchor = at.map(|r| r.center()).unwrap_or_else(win::cursor_pos);
    let mon = win::monitor_at(anchor);
    let dpi = mon.as_ref().map(|m| m.dpi).unwrap_or(96);
    let work = mon.as_ref().map(|m| m.work).unwrap_or_else(win::virtual_screen);

    let (w, h) = (img.width as f64, img.height as f64);
    let (zoom, origin) = match at {
        Some(r) => (1.0, (r.x as f64, r.y as f64)),
        None => {
            // Fit very large images into 90% of the work area.
            let fit = (work.w as f64 * 0.9 / w).min(work.h as f64 * 0.9 / h).clamp(geom::MIN_ZOOM, 1.0);
            let (vw, vh) = (w * fit, h * fit);
            let r = Rect::new(
                (anchor.x as f64 - vw / 2.0) as i32,
                (anchor.y as f64 - vh / 2.0) as i32,
                vw.ceil() as i32,
                vh.ceil() as i32,
            )
            .clamp_inside(&work);
            (fit, (r.x as f64, r.y as f64))
        }
    };
    let opacity = cfg.default_opacity.clamp(10, 100);
    let transparent = img.has_transparency();
    let pin = Pin {
        hwnd: HWND::default(),
        palette: HWND::default(),
        img,
        transparent,
        zoom,
        origin,
        opacity,
        mode: Mode::Normal,
        tool: Tool::Pen,
        style: Style { color: cfg.draw_color & 0x00FF_FFFF, size: cfg.draw_size.min(2) },
        next_step: 1,
        history: History::default(),
        drag: Drag::None,
        stroke: Vec::new(),
        text: None,
        caret_on: true,
        hover: None,
        badge: None,
        dpi,
        accent: accent_color(),
        tab_at: Point::default(),
        wheel_acc: 0,
        back: None,
        pal_layout: palette::layout(dpi),
        pal_hover: None,
        pal_tracking: false,
    };
    let r = pin.window_rect();
    let class = wide(PIN_CLASS);
    let title = wide(&t("pin-window-title"));
    let mut ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
    if opacity < 100 {
        ex |= WS_EX_LAYERED;
    }
    // SAFETY: creating our own popup window; the state box is attached right after.
    let hwnd = unsafe {
        CreateWindowExW(
            ex,
            pcwstr(&class),
            pcwstr(&title),
            WS_POPUP | WS_THICKFRAME,
            r.x,
            r.y,
            r.w,
            r.h,
            None,
            None,
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            log::error!("pin: CreateWindowExW failed: {e}");
            app::notify(t("app-name"), tf("notify-error", &[("error", &e.to_string())]), true);
            return;
        }
    };
    let cell = Box::new(RefCell::new(Pin { hwnd, ..pin }));
    // SAFETY: the pointer is reclaimed exactly once in WM_NCDESTROY.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(cell) as isize);
    }
    decorate(hwnd);
    set_opacity_style(hwnd, opacity);
    PINS.with(|p| p.borrow_mut().push(hwnd_to_isize(hwnd)));
    // SAFETY: showing and activating our own window so it receives keys.
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.x, r.y, r.w, r.h, SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
    log::info!("pin: opened {}x{} at {:?} (zoom {zoom:.2})", r.w, r.h, origin);
    if cfg.auto_draw_mode {
        run_cmd(hwnd, Cmd::EnterDraw);
    }
}

/// DWM shadow (1 px extended frame on a thick-frame popup whose non-client area is
/// removed in WM_NCCALCSIZE), square corners, and no DWM border (we draw our own).
fn decorate(hwnd: HWND) {
    let margins = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
    let corner = DWMWCP_DONOTROUND;
    let no_border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
                                      // SAFETY: valid pointers and sizes; failures (older Windows) are harmless.
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &no_border as *const _ as *const c_void, 4);
    }
}

fn set_opacity_style(hwnd: HWND, pct: u8) {
    // SAFETY: adjusting our own window's extended style and layered attributes.
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if pct >= 100 {
            if ex & WS_EX_LAYERED.0 != 0 {
                SetWindowLongW(hwnd, GWL_EXSTYLE, (ex & !WS_EX_LAYERED.0) as i32);
            }
        } else {
            if ex & WS_EX_LAYERED.0 == 0 {
                SetWindowLongW(hwnd, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as i32);
            }
            let a = (pct as u32 * 255 / 100) as u8;
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
        }
    }
}

/// Move/resize the window to match the state, then reposition or hide the palette.
fn sync(hwnd: HWND) {
    let Some((r, drawing, palette)) = with(hwnd, |p| (p.window_rect(), p.drawing(), p.palette)) else {
        return;
    };
    let old = window_rect_of(hwnd);
    // SAFETY: repositioning our own window.
    unsafe {
        let _ = SetWindowPos(hwnd, None, r.x, r.y, r.w, r.h, SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER);
    }
    // A pure move needs no repaint (DWM moves the pixels); content changes invalidate
    // explicitly, and a size change repaints anyway.
    if (old.w, old.h) != (r.w, r.h) {
        invalidate(hwnd);
    }
    if drawing {
        let pal = if palette.is_invalid() { create_palette(hwnd) } else { palette };
        place_palette(hwnd, pal, r);
    } else if !palette.is_invalid() {
        // SAFETY: hiding our own palette window.
        unsafe {
            let _ = ShowWindow(palette, SW_HIDE);
        }
    }
}

fn create_palette(pin: HWND) -> HWND {
    let class = wide(PALETTE_CLASS);
    let (w, h) = with(pin, |p| (p.pal_layout.w, p.pal_layout.h)).unwrap_or((300, 36));
    // SAFETY: an owned tool window; destroyed automatically with its owner.
    let pal = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            pcwstr(&class),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            w,
            h,
            pin,
            None,
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        )
    };
    match pal {
        Ok(h) => {
            win::round_corners(h);
            win::exclude_from_capture(h);
            with(pin, |p| p.palette = h);
            h
        }
        Err(e) => {
            log::warn!("pin: palette window failed: {e}");
            HWND::default()
        }
    }
}

/// Below the pin (above when there is no room), clamped to the monitor's work area.
fn place_palette(pin: HWND, pal: HWND, pr: Rect) {
    if pal.is_invalid() {
        return;
    }
    let Some((w, h, dpi)) = with(pin, |p| (p.pal_layout.w, p.pal_layout.h, p.dpi)) else { return };
    let gap = win::scale(6, dpi);
    let work = win::monitor_at(pr.center()).map(|m| m.work).unwrap_or_else(win::virtual_screen);
    let mut y = pr.bottom() + gap;
    if y + h > work.bottom() {
        y = pr.y - gap - h;
    }
    if y < work.y {
        // No room above or below: overlap the pin's bottom edge.
        y = (pr.bottom() - h - gap).max(work.y);
    }
    let r = Rect::new(pr.x, y, w, h).clamp_inside(&work);
    // SAFETY: positioning our own palette window.
    unsafe {
        let _ = SetWindowPos(pal, HWND_TOPMOST, r.x, r.y, r.w, r.h, SWP_NOACTIVATE);
        let _ = ShowWindow(pal, SW_SHOWNOACTIVATE);
    }
    invalidate(pal);
}

// ------------------------------------------------------------------ commands

fn show_badge(hwnd: HWND, text: String) {
    with(hwnd, |p| p.badge = Some(text));
    // SAFETY: timer on our own window.
    unsafe {
        SetTimer(hwnd, TIMER_BADGE, BADGE_MS, None);
    }
    invalidate(hwnd);
}

fn set_zoom(hwnd: HWND, new: f64, anchor: Option<Point>) {
    let changed = with(hwnd, |p| {
        let new = new.clamp(geom::MIN_ZOOM, geom::MAX_ZOOM);
        if (new - p.zoom).abs() < 1e-9 {
            return None;
        }
        let a = anchor.unwrap_or_else(|| p.window_rect().center());
        p.origin = geom::zoom_about(p.origin, p.zoom, new, a);
        p.zoom = new;
        p.back = None;
        Some(new)
    })
    .flatten();
    if let Some(z) = changed {
        sync(hwnd);
        show_badge(hwnd, format!("{:.0}%", z * 100.0));
    }
}

fn set_opacity(hwnd: HWND, pct: u8) {
    let pct = pct.clamp(10, 100);
    with(hwnd, |p| p.opacity = pct);
    set_opacity_style(hwnd, pct);
    show_badge(hwnd, tf("pin-badge-opacity", &[("pct", &pct.to_string())]));
}

fn copy(hwnd: HWND) -> bool {
    let Some(img) = image_of(hwnd) else { return false };
    match clipboard::copy_image(&img) {
        Ok(()) => {
            show_badge(hwnd, t("pin-badge-copied"));
            true
        }
        Err(e) => {
            log::error!("pin: copy failed: {e:#}");
            app::notify(t("app-name"), tf("notify-error", &[("error", &format!("{e:#}"))]), true);
            false
        }
    }
}

fn save_auto(hwnd: HWND) -> bool {
    let Some(img) = image_of(hwnd) else { return false };
    let (folder, ext) = {
        let s = settings::get();
        (s.output_folder(), s.output.format.extension())
    };
    let path = settings::filename::next_output_path(&folder, ext);
    match save::save_image(&img, &path) {
        Ok(()) => {
            app::notify(t("app-name"), tf("notify-saved", &[("path", &path.display().to_string())]), false);
            true
        }
        Err(e) => {
            log::error!("pin: save failed: {e:#}");
            app::notify(t("app-name"), tf("notify-error", &[("error", &format!("{e:#}"))]), true);
            false
        }
    }
}

fn save_as(hwnd: HWND) {
    let Some(img) = image_of(hwnd) else { return };
    let (folder, ext) = {
        let s = settings::get();
        (s.output_folder(), s.output.format.extension())
    };
    // A suggestion only (the dialog may be cancelled): do not use up the file-name counter.
    let suggested = settings::filename::peek_output_path(&folder, ext);
    let name = suggested.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| format!("pin.{ext}"));
    let mut file: Vec<u16> = name.encode_utf16().collect();
    file.resize(1024, 0);
    let filter: Vec<u16> = "PNG (*.png)\0*.png\0JPEG (*.jpg)\0*.jpg;*.jpeg\0BMP (*.bmp)\0*.bmp\0GIF (*.gif)\0*.gif\0TIFF (*.tif)\0*.tif;*.tiff\0WebP (*.webp)\0*.webp\0\0"
        .encode_utf16()
        .collect();
    let dir = wide(&folder.display().to_string());
    let def_ext = wide(ext);
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: pcwstr(&dir),
        lpstrDefExt: pcwstr(&def_ext),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    let _ = std::fs::create_dir_all(&folder);
    // SAFETY: all buffers outlive the modal call; no pin state is borrowed.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
    if !ok {
        return;
    }
    let path = std::path::PathBuf::from(win::from_wide(&file));
    if let Err(e) = save::save_image(&img, &path) {
        log::error!("pin: save as failed: {e:#}");
        app::notify(t("app-name"), tf("notify-error", &[("error", &format!("{e:#}"))]), true);
    } else {
        show_badge(hwnd, t("pin-badge-saved"));
    }
}

pub(super) fn send_to_editor(hwnd: HWND) {
    if let Some(img) = image_of(hwnd) {
        crate::editor::open_image(img, t("pin-window-title"), None, None);
    }
}

fn undo(hwnd: HWND) {
    let changed = with(hwnd, |p| {
        if p.text.is_some() {
            p.text = None;
            p.mode = Mode::Drawing;
            return true;
        }
        let Some(s) = p.history.pop() else { return false };
        p.origin.0 -= s.shift.0 as f64 * p.zoom;
        p.origin.1 -= s.shift.1 as f64 * p.zoom;
        p.img = s.img;
        p.next_step = s.next_step;
        p.transparent = p.img.has_transparency();
        p.back = None;
        true
    })
    .unwrap_or(false);
    if changed {
        sync(hwnd);
        refresh_palette(hwnd);
    }
}

fn refresh_palette(hwnd: HWND) {
    if let Some(pal) = with(hwnd, |p| p.palette) {
        if !pal.is_invalid() {
            invalidate(pal);
        }
    }
}

fn set_caret_timer(hwnd: HWND, on: bool) {
    // SAFETY: timer on our own window.
    unsafe {
        if on {
            SetTimer(hwnd, TIMER_CARET, CARET_MS, None);
        } else {
            let _ = KillTimer(hwnd, TIMER_CARET);
        }
    }
}

/// Execute a keyboard/menu command.
fn run_cmd(hwnd: HWND, cmd: Cmd) {
    match cmd {
        Cmd::None => {}
        Cmd::Close => close(hwnd),
        Cmd::CopyAndClose => {
            if copy(hwnd) {
                close(hwnd);
            }
        }
        Cmd::SaveAndClose => {
            if save_auto(hwnd) {
                close(hwnd);
            }
        }
        Cmd::Copy => {
            copy(hwnd);
        }
        Cmd::SaveAs => save_as(hwnd),
        Cmd::Undo => undo(hwnd),
        Cmd::ActualSize => set_zoom(hwnd, 1.0, None),
        Cmd::HideToTab => {
            with(hwnd, |p| {
                p.commit_text();
                let r = p.window_rect();
                let (tw, _) = paint::tab_size(p.dpi);
                let mon = win::monitor_at(r.center()).map(|m| m.rect).unwrap_or_else(win::virtual_screen);
                let x = (r.x + r.w / 2 - tw / 2).clamp(mon.x, mon.right() - tw);
                p.tab_at = Point::new(x, mon.y);
                p.mode = keys::next_mode(p.mode, Cmd::HideToTab);
                p.back = None;
            });
            set_caret_timer(hwnd, false);
            sync(hwnd);
        }
        Cmd::RestoreFromTab => {
            with(hwnd, |p| {
                p.mode = keys::next_mode(p.mode, Cmd::RestoreFromTab);
                p.back = None;
            });
            sync(hwnd);
        }
        Cmd::EnterDraw | Cmd::ExitDraw => {
            with(hwnd, |p| {
                if cmd == Cmd::ExitDraw {
                    p.commit_text();
                }
                p.mode = keys::next_mode(p.mode, cmd);
                p.hover = None;
            });
            set_caret_timer(hwnd, false);
            sync(hwnd);
            refresh_palette(hwnd);
        }
        Cmd::CommitText | Cmd::CancelText => {
            with(hwnd, |p| {
                if cmd == Cmd::CommitText {
                    p.commit_text();
                } else {
                    p.text = None;
                }
                p.mode = keys::next_mode(p.mode, cmd);
            });
            set_caret_timer(hwnd, false);
            invalidate(hwnd);
            refresh_palette(hwnd);
        }
    }
}

fn show_menu(hwnd: HWND, at: Point) {
    let Some((drawing, opacity)) = with(hwnd, |p| (p.drawing(), p.opacity)) else { return };
    let Some(m) = Menu::new() else { return };
    let key = |k: &str, sc: &str| format!("{}\t{sc}", t(k));
    m.item(ID_COPY, &key("pin-menu-copy", "Ctrl+C"), false);
    m.item(ID_SAVE_AS, &key("pin-menu-save-as", "Ctrl+S"), false);
    m.item(ID_EDITOR, &t("pin-menu-editor"), false);
    m.item(ID_EDITOR_ALL, &t("pin-menu-editor-all"), false);
    m.separator();
    m.item(ID_ZOOM_100, &t("pin-menu-zoom-100"), false);
    if let Some(sub) = Menu::new() {
        for i in 0..10u32 {
            sub.item(ID_OPACITY + i, &format!("{}%", 100 - i * 10), false);
        }
        let cur = ID_OPACITY + (100 - opacity.clamp(10, 100) as u32) / 10;
        sub.radio_group(ID_OPACITY, ID_OPACITY + 9, cur);
        m.submenu(sub, &t("pin-menu-opacity"));
    }
    m.item(ID_DRAW, &key("pin-menu-draw", "D"), drawing);
    m.item(ID_HIDE, &t("pin-menu-hide"), false);
    m.separator();
    m.item(ID_CLOSE, &key("pin-menu-close", "Esc"), false);
    m.item(ID_CLOSE_ALL, &t("pin-menu-close-all"), false);
    // SAFETY: modal menu owned by our pin; WM_NULL afterwards per the TrackPopupMenu docs.
    let id = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenuEx(m.0, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, at.x, at.y, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        id.0 as u32
    };
    drop(m);
    match id {
        0 => {}
        ID_COPY => run_cmd(hwnd, Cmd::Copy),
        ID_SAVE_AS => run_cmd(hwnd, Cmd::SaveAs),
        ID_EDITOR => send_to_editor(hwnd),
        ID_EDITOR_ALL => super::send_all_to_editor(),
        ID_ZOOM_100 => run_cmd(hwnd, Cmd::ActualSize),
        ID_DRAW => run_cmd(hwnd, if drawing { Cmd::ExitDraw } else { Cmd::EnterDraw }),
        ID_HIDE => run_cmd(hwnd, Cmd::HideToTab),
        ID_CLOSE => close(hwnd),
        ID_CLOSE_ALL => super::close_all(),
        id if (ID_OPACITY..ID_OPACITY + 10).contains(&id) => {
            set_opacity(hwnd, (100 - (id - ID_OPACITY) * 10) as u8);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ mouse

fn on_lbutton_down(hwnd: HWND, p_client: Point) {
    let screen = win::cursor_pos();
    let ctrl = key_down(VK_CONTROL.0);
    let alt = key_down(VK_MENU.0);
    let typing_started = with(hwnd, |p| {
        let mut typing = false;
        match p.mode {
            Mode::Tab { .. } => p.drag = Drag::TabClick { start: screen, moved: false },
            _ => {
                let b = p.border();
                let (vw, vh) = p.view_size();
                let local = Point::new(p_client.x - b, p_client.y - b);
                let edges = geom::hit_edges(local, vw, vh, win::scale(8, p.dpi));
                if ctrl && edges.any() {
                    p.commit_text();
                    p.drag = Drag::Canvas {
                        start: screen,
                        edges,
                        base: p.img.clone(),
                        base_origin: p.origin,
                        base_transparent: p.transparent,
                    };
                } else if p.drawing() && !alt {
                    let ip = p.to_image(p_client);
                    match p.tool {
                        Tool::Pen => {
                            p.commit_text();
                            p.stroke.clear();
                            p.stroke.push(ip);
                            p.drag = Drag::Stroke;
                        }
                        Tool::Text => {
                            p.commit_text();
                            // Click = top-left of the text line; centre the first line on the click.
                            let px = p.style.text_px() as f32;
                            p.text = Some(TextEntry {
                                x: ip.0,
                                y: ip.1 - px * 0.65,
                                text: String::new(),
                                high_surrogate: None,
                            });
                            p.mode = Mode::Typing;
                            p.caret_on = true;
                            typing = true;
                        }
                        Tool::Step => {
                            p.commit_text();
                            let snap = p.snapshot();
                            let (n, rgb, r) = (p.next_step, p.style.color, p.style.stamp_radius());
                            if let Some(mut pm) = annot::pixmap_mut(&mut p.img) {
                                annot::step(&mut pm, ip.0, ip.1, n, rgb, r, tiny_skia::Transform::identity());
                            }
                            p.history.push(snap);
                            p.next_step += 1;
                        }
                    }
                } else {
                    p.drag = Drag::Move { start: screen, origin: p.origin };
                }
            }
        }
        typing
    });
    let Some(typing) = typing_started else { return };
    set_caret_timer(hwnd, typing);
    // SAFETY: capturing the mouse for our own window.
    unsafe {
        SetCapture(hwnd);
    }
    invalidate(hwnd);
    refresh_palette(hwnd);
}

fn on_mouse_move(hwnd: HWND, p_client: Point) {
    let screen = win::cursor_pos();
    let fill = match settings::get().pin.canvas_fill {
        CanvasFill::Transparent => [0, 0, 0, 0],
        CanvasFill::White => [255, 255, 255, 255],
    };
    enum After {
        Nothing,
        Redraw,
        Sync,
        Badge(String),
    }
    let after = with(hwnd, |p| match &mut p.drag {
        Drag::Move { start, origin } => {
            p.origin = (origin.0 + (screen.x - start.x) as f64, origin.1 + (screen.y - start.y) as f64);
            After::Sync
        }
        Drag::Stroke => {
            let ip = p.to_image(p_client);
            if p.stroke.last() != Some(&ip) {
                p.stroke.push(ip);
            }
            After::Redraw
        }
        Drag::Canvas { start, edges, base, base_origin, base_transparent } => {
            let dx = ((screen.x - start.x) as f64 / p.zoom).round() as i32;
            let dy = ((screen.y - start.y) as f64 / p.zoom).round() as i32;
            let canvas = geom::canvas_after_drag(base.width, base.height, *edges, dx, dy);
            let grows = canvas.x < 0
                || canvas.y < 0
                || canvas.right() > base.width as i32
                || canvas.bottom() > base.height as i32;
            p.transparent = *base_transparent || (grows && fill[3] == 0);
            p.img = geom::apply_canvas(base, canvas, fill);
            p.origin = (base_origin.0 + canvas.x as f64 * p.zoom, base_origin.1 + canvas.y as f64 * p.zoom);
            p.back = None;
            After::Badge(format!("{} × {}", canvas.w, canvas.h))
        }
        Drag::TabClick { start, moved } => {
            if (screen.x - start.x).abs() > 3 || (screen.y - start.y).abs() > 3 {
                *moved = true;
            }
            After::Nothing
        }
        Drag::None => {
            if p.mode == Mode::Drawing && p.tool == Tool::Step {
                p.hover = Some(p.to_image(p_client));
                After::Redraw
            } else {
                After::Nothing
            }
        }
    });
    match after {
        Some(After::Sync) => sync(hwnd),
        Some(After::Redraw) => invalidate(hwnd),
        Some(After::Badge(b)) => {
            sync(hwnd);
            show_badge(hwnd, b);
        }
        _ => {}
    }
}

fn on_lbutton_up(hwnd: HWND) {
    let drag = with(hwnd, |p| std::mem::replace(&mut p.drag, Drag::None));
    // SAFETY: releasing our own capture (WM_CAPTURECHANGED then finds Drag::None).
    unsafe {
        let _ = ReleaseCapture();
    }
    let Some(drag) = drag else { return };
    match drag {
        Drag::Stroke => {
            with(hwnd, |p| {
                let pts = std::mem::take(&mut p.stroke);
                if pts.is_empty() {
                    return;
                }
                let snap = p.snapshot();
                let (rgb, w) = (p.style.color, p.style.pen_width());
                if let Some(mut pm) = annot::pixmap_mut(&mut p.img) {
                    annot::stroke(&mut pm, &pts, rgb, w, tiny_skia::Transform::identity());
                }
                p.history.push(snap);
            });
            invalidate(hwnd);
            refresh_palette(hwnd);
        }
        Drag::Canvas { base, base_origin, base_transparent, .. } => {
            with(hwnd, |p| {
                let shift = ((p.origin.0 - base_origin.0) / p.zoom).round() as i32;
                let shift_y = ((p.origin.1 - base_origin.1) / p.zoom).round() as i32;
                if p.img != base {
                    p.history.push(Snap { img: base, next_step: p.next_step, shift: (shift, shift_y) });
                    p.transparent = p.img.has_transparency();
                } else {
                    p.transparent = base_transparent;
                }
            });
            sync(hwnd);
            refresh_palette(hwnd);
        }
        Drag::TabClick { moved: false, .. } => run_cmd(hwnd, Cmd::RestoreFromTab),
        _ => {}
    }
}

fn on_wheel(hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
    let ctrl = (wp.0 & 0x0008) != 0; // MK_CONTROL
    let anchor = lparam_point(lp);
    let Some((notches, zoom, opacity, tab)) = with(hwnd, |p| {
        p.wheel_acc += delta;
        let n = p.wheel_acc / 120;
        p.wheel_acc -= n * 120;
        (n, p.zoom, p.opacity, matches!(p.mode, Mode::Tab { .. }))
    }) else {
        return;
    };
    if notches == 0 || tab {
        return;
    }
    if ctrl {
        set_opacity(hwnd, geom::next_opacity(opacity, notches));
    } else {
        set_zoom(hwnd, geom::next_zoom(zoom, notches), Some(anchor));
    }
}

fn cursor_for(hwnd: HWND, p_client: Point) -> windows::core::PCWSTR {
    let ctrl = key_down(VK_CONTROL.0);
    let alt = key_down(VK_MENU.0);
    with(hwnd, |p| {
        if let Mode::Tab { .. } = p.mode {
            return IDC_HAND;
        }
        let b = p.border();
        let (vw, vh) = p.view_size();
        let e = geom::hit_edges(Point::new(p_client.x - b, p_client.y - b), vw, vh, win::scale(8, p.dpi));
        if ctrl && e.any() {
            return match (e.left, e.top, e.right, e.bottom) {
                (true, true, _, _) | (_, _, true, true) => IDC_SIZENWSE,
                (true, _, _, true) | (_, true, true, _) => IDC_SIZENESW,
                (true, _, _, _) | (_, _, true, _) => IDC_SIZEWE,
                _ => IDC_SIZENS,
            };
        }
        if p.drawing() && !alt {
            return match p.tool {
                Tool::Text => IDC_IBEAM,
                _ => IDC_CROSS,
            };
        }
        IDC_SIZEALL
    })
    .unwrap_or(IDC_ARROW)
}

fn update_cursor(hwnd: HWND) {
    let c = win::cursor_pos();
    let r = window_rect_of(hwnd);
    if !r.contains(c) {
        return;
    }
    let id = cursor_for(hwnd, Point::new(c.x - r.x, c.y - r.y));
    // SAFETY: loading a shared system cursor.
    unsafe {
        SetCursor(LoadCursorW(None, id).unwrap_or(HCURSOR::default()));
    }
}

// ------------------------------------------------------------------ keyboard

fn on_key_down(hwnd: HWND, vk: u16) -> bool {
    let ctrl = key_down(VK_CONTROL.0);
    let shift = key_down(VK_SHIFT.0);
    if vk == VK_CONTROL.0 {
        update_cursor(hwnd);
        return true;
    }
    let Some(mode) = with(hwnd, |p| p.mode) else { return false };
    // Shift+Enter: new line while typing.
    if mode == Mode::Typing && vk == VK_RETURN.0 && shift {
        with(hwnd, |p| {
            if let Some(te) = p.text.as_mut() {
                te.text.push('\n');
            }
        });
        invalidate(hwnd);
        return true;
    }
    // Arrows nudge the pin (1 px, Shift = 10 px) outside text entry.
    if !matches!(mode, Mode::Typing | Mode::Tab { .. }) {
        let step = if shift { 10.0 } else { 1.0 };
        let d = match vk {
            v if v == VK_LEFT.0 => Some((-step, 0.0)),
            v if v == VK_RIGHT.0 => Some((step, 0.0)),
            v if v == VK_UP.0 => Some((0.0, -step)),
            v if v == VK_DOWN.0 => Some((0.0, step)),
            _ => None,
        };
        if let Some((dx, dy)) = d {
            with(hwnd, |p| p.origin = (p.origin.0 + dx, p.origin.1 + dy));
            sync(hwnd);
            return true;
        }
    }
    let key = match vk {
        v if v == VK_SPACE.0 => Key::Space,
        v if v == VK_RETURN.0 => Key::Enter,
        v if v == VK_ESCAPE.0 => Key::Escape,
        0x44 if !ctrl => Key::D,
        0x5A if ctrl => Key::CtrlZ,
        0x43 if ctrl => Key::CtrlC,
        0x53 if ctrl => Key::CtrlS,
        _ => return false,
    };
    let (space, enter) = {
        let s = settings::get();
        (s.pin.space_action, s.pin.enter_action)
    };
    let cmd = keys::on_key(mode, key, space, enter);
    if cmd == Cmd::None {
        return false;
    }
    run_cmd(hwnd, cmd);
    true
}

fn on_char(hwnd: HWND, unit: u16) {
    let changed = with(hwnd, |p| {
        if p.mode != Mode::Typing {
            return false;
        }
        let Some(te) = p.text.as_mut() else { return false };
        match unit {
            0x08 => {
                te.text.pop();
            }
            0x0D | 0x0A | 0x1B | 0x09 => return false,
            u if u < 0x20 => return false,
            u if (0xD800..0xDC00).contains(&u) => {
                te.high_surrogate = Some(u);
                return false;
            }
            u => {
                let units: Vec<u16> = match te.high_surrogate.take() {
                    Some(h) if (0xDC00..0xE000).contains(&u) => vec![h, u],
                    _ => vec![u],
                };
                te.text.extend(char::decode_utf16(units).filter_map(Result::ok));
            }
        }
        p.caret_on = true;
        true
    })
    .unwrap_or(false);
    if changed {
        invalidate(hwnd);
    }
}

// ------------------------------------------------------------------ painting

fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: BeginPaint/EndPaint pair on our own window; the back buffer is our DIB.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        with(hwnd, |p| {
            if p.back.as_ref().map(|d| (d.w, d.h)) != Some((w, h)) {
                p.back = Dib::new(w, h);
            }
            let Some(mut back) = p.back.take() else { return };
            {
                let buf = back.bytes();
                if let Mode::Tab { .. } = p.mode {
                    paint::render_tab(buf, w, h, &p.img, p.accent, p.dpi);
                } else {
                    let border_rgb = if p.drawing() { DRAW_BORDER } else { p.accent };
                    let v = paint::View {
                        img: &p.img,
                        transparent: p.transparent,
                        zoom: p.zoom,
                        border: p.border(),
                        border_rgb,
                        live: p.live(),
                        badge: p.badge.as_deref(),
                        dpi: p.dpi,
                    };
                    paint::render(buf, w, h, &v);
                }
            }
            let _ = BitBlt(hdc, 0, 0, w, h, back.dc, 0, 0, SRCCOPY);
            p.back = Some(back);
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

// ------------------------------------------------------------------ window procedures

unsafe extern "system" fn pin_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => return LRESULT(0),
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        WM_PAINT => {
            on_paint(hwnd);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_LBUTTONDOWN => {
            on_lbutton_down(hwnd, lparam_point(lp));
            return LRESULT(0);
        }
        WM_LBUTTONDBLCLK => {
            let mode = with(hwnd, |p| p.mode);
            match mode {
                Some(Mode::Drawing | Mode::Typing) => on_lbutton_down(hwnd, lparam_point(lp)),
                Some(Mode::Tab { .. }) => run_cmd(hwnd, Cmd::RestoreFromTab),
                Some(Mode::Normal) => close(hwnd),
                None => {}
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            on_mouse_move(hwnd, lparam_point(lp));
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            on_lbutton_up(hwnd);
            return LRESULT(0);
        }
        WM_CAPTURECHANGED => {
            // Capture lost mid-drag (e.g. a modal popup): finish like a button release.
            let dragging = with(hwnd, |p| !matches!(p.drag, Drag::None)).unwrap_or(false);
            if dragging {
                on_lbutton_up(hwnd);
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            show_menu(hwnd, win::cursor_pos());
            return LRESULT(0);
        }
        WM_CONTEXTMENU => {
            // Keyboard menu key / Shift+F10: open at the pin's top-left.
            let at = if lp.0 == -1 { window_origin(hwnd) } else { lparam_point(lp) };
            show_menu(hwnd, at);
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            on_wheel(hwnd, wp, lp);
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if on_key_down(hwnd, wp.0 as u16) {
                return LRESULT(0);
            }
        }
        WM_KEYUP => {
            if wp.0 as u16 == VK_CONTROL.0 {
                update_cursor(hwnd);
            }
        }
        WM_CHAR => {
            on_char(hwnd, wp.0 as u16);
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            if (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                update_cursor(hwnd);
                return LRESULT(1);
            }
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_BADGE => {
                    let _ = KillTimer(hwnd, TIMER_BADGE);
                    with(hwnd, |p| p.badge = None);
                }
                TIMER_CARET => {
                    with(hwnd, |p| p.caret_on = !p.caret_on);
                }
                _ => {}
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_MOVE => {
            let pal = with(hwnd, |p| if p.drawing() { p.palette } else { HWND::default() });
            if let Some(pal) = pal {
                if !pal.is_invalid() {
                    place_palette(hwnd, pal, window_rect_of(hwnd));
                }
            }
        }
        WM_DPICHANGED => {
            // Keep physical pixels 1:1: only chrome (border, palette) follows the new DPI.
            let dpi = (wp.0 & 0xFFFF) as u32;
            with(hwnd, |p| {
                p.dpi = dpi.max(96);
                p.pal_layout = palette::layout(p.dpi);
                p.back = None;
            });
            sync(hwnd);
            return LRESULT(0);
        }
        WM_DESTROY => {
            let h = hwnd_to_isize(hwnd);
            PINS.with(|p| p.borrow_mut().retain(|&x| x != h));
            log::debug!("pin: closed");
        }
        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<Pin>;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !p.is_null() {
                // SAFETY: the pointer came from Box::into_raw in `create` and is cleared above,
                // so it is freed exactly once.
                drop(Box::from_raw(p));
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn owner_of(pal: HWND) -> HWND {
    // SAFETY: plain query.
    unsafe { GetWindow(pal, GW_OWNER).unwrap_or_default() }
}

fn palette_paint(pal: HWND) {
    let pin = owner_of(pal);
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: BeginPaint/EndPaint pair on our palette; drawing via a temporary DIB.
    unsafe {
        let hdc = BeginPaint(pal, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(pal, &mut rc);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        with(pin, |p| {
            if let Some(mut dib) = Dib::new(w, h) {
                let v = palette::View {
                    tool: p.tool,
                    style: p.style,
                    hover: p.pal_hover,
                    can_undo: !p.history.is_empty() || p.text.is_some(),
                    accent: p.accent,
                    dpi: p.dpi,
                };
                palette::render(dib.bytes(), w, h, &p.pal_layout, &v);
                let _ = BitBlt(hdc, 0, 0, w, h, dib.dc, 0, 0, SRCCOPY);
            }
        });
        let _ = EndPaint(pal, &ps);
    }
}

fn choose_color(pal: HWND, current: u32) -> Option<u32> {
    let mut custom = CUSTOM_COLORS.with(|c| *c.borrow());
    let bgr = ((current & 0xFF) << 16) | (current & 0xFF00) | ((current >> 16) & 0xFF);
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: pal,
        rgbResult: COLORREF(bgr),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    // SAFETY: `custom` (16 entries) outlives the modal call; no pin state is borrowed.
    let ok = unsafe { ChooseColorW(&mut cc) }.as_bool();
    CUSTOM_COLORS.with(|c| *c.borrow_mut() = custom);
    if !ok {
        return None;
    }
    let v = cc.rgbResult.0;
    Some(((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF))
}

fn persist_style(style: Style) {
    settings::update(|s| {
        s.pin.draw_color = style.color;
        s.pin.draw_size = style.size;
    });
}

fn palette_click(pal: HWND, pt: Point) {
    let pin = owner_of(pal);
    let Some(item) = with(pin, |p| p.pal_layout.hit(pt)).flatten() else { return };
    match item {
        Item::Grip => {
            let screen = win::cursor_pos();
            with(pin, |p| p.drag = Drag::Move { start: screen, origin: p.origin });
            // SAFETY: capturing the mouse for our palette.
            unsafe {
                SetCapture(pal);
            }
        }
        Item::Tool(tool) => {
            with(pin, |p| {
                p.commit_text();
                p.tool = tool;
                p.hover = None;
            });
            set_caret_timer(pin, false);
        }
        Item::Color(i) => {
            if let Some(style) = with(pin, |p| {
                p.style.color = annot::COLORS[i];
                p.style
            }) {
                persist_style(style);
            }
        }
        Item::CustomColor => {
            let cur = with(pin, |p| p.style.color).unwrap_or(0);
            if let Some(c) = choose_color(pal, cur) {
                if let Some(style) = with(pin, |p| {
                    p.style.color = c;
                    p.style
                }) {
                    persist_style(style);
                }
            }
        }
        Item::Size => {
            if let Some(style) = with(pin, |p| {
                p.style.size = (p.style.size + 1) % 3;
                p.style
            }) {
                persist_style(style);
            }
        }
        Item::Undo => undo(pin),
        Item::Done => run_cmd(pin, Cmd::ExitDraw),
    }
    invalidate(pin);
    invalidate(pal);
}

unsafe extern "system" fn palette_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            palette_paint(hwnd);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            palette_click(hwnd, lparam_point(lp));
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let pin = owner_of(hwnd);
            let pt = lparam_point(lp);
            let screen = win::cursor_pos();
            let (moved, track) = with(pin, |p| {
                if let Drag::Move { start, origin } = p.drag {
                    p.origin = (origin.0 + (screen.x - start.x) as f64, origin.1 + (screen.y - start.y) as f64);
                    return (true, false);
                }
                p.pal_hover = p.pal_layout.hit(pt);
                let track = !p.pal_tracking;
                p.pal_tracking = true;
                (false, track)
            })
            .unwrap_or((false, false));
            if moved {
                sync(pin);
            } else {
                if track {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with(owner_of(hwnd), |p| {
                p.pal_hover = None;
                p.pal_tracking = false;
            });
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            with(owner_of(hwnd), |p| {
                if let Drag::Move { .. } = p.drag {
                    p.drag = Drag::None;
                }
            });
            if msg == WM_LBUTTONUP {
                let _ = ReleaseCapture();
            }
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            let c = win::cursor_pos();
            let r = window_rect_of(hwnd);
            let grip = with(owner_of(hwnd), |p| p.pal_layout.hit(Point::new(c.x - r.x, c.y - r.y)) == Some(Item::Grip))
                .unwrap_or(false);
            let id = if grip { IDC_SIZEALL } else { IDC_ARROW };
            SetCursor(LoadCursorW(None, id).unwrap_or(HCURSOR::default()));
            return LRESULT(1);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

#[cfg(test)]
pub(super) fn run_cmd_for_test(hwnd: HWND, cmd: Cmd) {
    run_cmd(hwnd, cmd);
}
