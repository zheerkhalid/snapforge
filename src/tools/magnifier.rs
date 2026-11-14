//! Screen magnifier: a round lens that follows the cursor, or a fixed, resizable pane
//! that shows the area around the cursor. 2–16x, smooth (bilinear) or pixelated,
//! optional negative. Arrows pan, the wheel zooms (Ctrl+wheel resizes the lens),
//! S / N / M toggle smooth, negative and the mode, Esc closes.
//!
//! Live pixels come from GDI BitBlt; both windows are excluded from capture, so the
//! magnifier never magnifies itself.

use super::card::{self, sc, sci};
use super::gfx::{self, col, Align, Dib, Face, Grabber, Text};
use super::lens::{self, LensOptions};
use super::wnd::{self, MenuItem};
use crate::app::ScreenTool;
use crate::common::win::{self, hwnd_to_isize};
use crate::common::{Image, Point, Rect};
use crate::settings::tools::MagnifierMode;
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_ADD, VK_DOWN, VK_ESCAPE, VK_HOME, VK_LEFT, VK_OEM_MINUS, VK_OEM_PLUS, VK_RIGHT, VK_SUBTRACT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetClientRect, GetWindowRect, KillTimer, SetCursor, SetForegroundWindow, SetTimer,
    SetWindowPos, ShowWindow, HCURSOR, HTCLIENT, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOW, WM_CLOSE, WM_CONTEXTMENU, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_TIMER, WS_CAPTION, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};

const LENS_CLASS: &str = "SnapForge.MagnifierLens";
const PANE_CLASS: &str = "SnapForge.MagnifierPane";
const TIMER_ID: usize = 1;
/// ~60 fps while open.
const TIMER_MS: u32 = 16;
/// Lens shadow margin (logical).
const MARGIN: f32 = 12.0;
const HUD_TIME: Duration = Duration::from_millis(1400);

struct Mag {
    hwnd: HWND,
    mode: MagnifierMode,
    zoom: u32,
    smooth: bool,
    negative: bool,
    /// Pan offset in source pixels.
    pan: Point,
    /// Lens diameter, logical pixels.
    size: u32,
    grab: Grabber,
    dib: Option<Dib>,
    text: Text,
    /// Last rendered input, to skip identical frames.
    last: Option<(Point, Rect, u64)>,
    hud_until: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<Option<Mag>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Mag) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    with(|m| vec![hwnd_to_isize(m.hwnd)]).unwrap_or_default()
}

pub fn open() -> bool {
    if let Some(h) = with(|m| m.hwnd) {
        // SAFETY: our own window.
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return true;
    }
    let s = crate::settings::get().tools.clone();
    let cursor = win::cursor_pos();
    let dpi = win::monitor_at(cursor).map_or(96, |m| m.dpi);
    let hwnd = match s.magnifier_mode {
        MagnifierMode::Lens => {
            if !wnd::register(LENS_CLASS, Some(proc), IDC_ARROW) {
                return false;
            }
            let side = lens_window_side(s.magnifier_size, dpi);
            let r = Rect::new(cursor.x - side / 2, cursor.y - side / 2, side, side);
            wnd::create(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                LENS_CLASS,
                "SnapForge Magnifier",
                WS_POPUP,
                r,
                HWND::default(),
            )
        }
        MagnifierMode::Pane => {
            if !wnd::register(PANE_CLASS, Some(proc), IDC_ARROW) {
                return false;
            }
            let r = s.magnifier_pane.map(|[x, y, w, h]| Rect::new(x, y, w, h)).filter(on_screen).unwrap_or_else(|| {
                let work = win::monitor_at(cursor).map_or_else(win::virtual_screen, |m| m.work);
                let (w, h) = (win::scale(480, dpi), win::scale(320, dpi));
                Rect::new(work.right() - w - win::scale(24, dpi), work.y + win::scale(24, dpi), w, h)
            });
            let title = crate::i18n::t("tools-magnifier-title");
            wnd::create(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                PANE_CLASS,
                &title,
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME,
                r,
                HWND::default(),
            )
        }
    };
    let Some(hwnd) = hwnd else {
        return false;
    };
    win::exclude_from_capture(hwnd);
    STATE.with(|st| {
        *st.borrow_mut() = Some(Mag {
            hwnd,
            mode: s.magnifier_mode,
            zoom: s.magnifier_zoom.clamp(lens::MIN_ZOOM, lens::MAX_ZOOM),
            smooth: s.magnifier_smooth,
            negative: s.magnifier_negative,
            pan: Point::default(),
            size: s.magnifier_size.clamp(120, 800),
            grab: Grabber::default(),
            dib: None,
            text: Text::default(),
            last: None,
            hud_until: Some(Instant::now() + HUD_TIME * 2),
        })
    });
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
    }
    tick(true);
    true
}

fn on_screen(r: &Rect) -> bool {
    win::monitors().iter().any(|m| !m.work.intersect(r).is_empty())
}

pub fn close() {
    let Some(m) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    let pane = if m.mode == MagnifierMode::Pane {
        let mut rc = RECT::default();
        // SAFETY: our own window, still alive.
        unsafe {
            let _ = GetWindowRect(m.hwnd, &mut rc);
        }
        Some([rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top])
    } else {
        None
    };
    // SAFETY: our own window and timer.
    unsafe {
        let _ = KillTimer(m.hwnd, TIMER_ID);
        let _ = DestroyWindow(m.hwnd);
    }
    let changed = {
        let s = &crate::settings::get().tools;
        s.magnifier_zoom != m.zoom
            || s.magnifier_smooth != m.smooth
            || s.magnifier_negative != m.negative
            || s.magnifier_mode != m.mode
            || s.magnifier_size != m.size
            || (pane.is_some() && s.magnifier_pane != pane)
    };
    if changed {
        crate::settings::update(|s| {
            let t = &mut s.tools;
            t.magnifier_zoom = m.zoom;
            t.magnifier_smooth = m.smooth;
            t.magnifier_negative = m.negative;
            t.magnifier_mode = m.mode;
            t.magnifier_size = m.size;
            if pane.is_some() {
                t.magnifier_pane = pane;
            }
        });
    }
    super::closed(ScreenTool::Magnifier);
}

fn lens_window_side(size: u32, dpi: u32) -> i32 {
    sci(size as f32 + 2.0 * MARGIN, dpi)
}

/// Cheap fingerprint of grabbed pixels (FNV-1a over a sparse sample).
fn fingerprint(img: &Image) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for px in img.data.chunks_exact(4).step_by(7) {
        h ^= u32::from_le_bytes([px[0], px[1], px[2], px[3]]) as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h ^ img.data.len() as u64
}

/// Render and show one frame. `force` skips the unchanged-frame check.
fn tick(force: bool) {
    let cursor = win::cursor_pos();
    with(|m| {
        let dpi = win::monitor_at(cursor).map_or(96, |mi| mi.dpi);
        let center = Point::new(cursor.x + m.pan.x, cursor.y + m.pan.y);
        let hud = m.hud_until.is_some_and(|t| Instant::now() < t);
        match m.mode {
            MagnifierMode::Lens => {
                let d = sci(m.size as f32, dpi);
                let src_rect = lens::grab_rect(center, (d, d), m.zoom as f64);
                let Some(src) = m.grab.grab(src_rect) else {
                    return;
                };
                let key = (center, src_rect, fingerprint(&src) ^ hud as u64 ^ (m.zoom as u64) << 40);
                let side = lens_window_side(m.size, dpi);
                let pos = Point::new(cursor.x - side / 2, cursor.y - side / 2);
                if !force && m.last == Some(key) {
                    // Same pixels: only follow the cursor.
                    // SAFETY: our own window.
                    unsafe {
                        let _ = SetWindowPos(m.hwnd, HWND_TOPMOST, pos.x, pos.y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                    return;
                }
                m.last = Some(key);
                let view = View {
                    src: &src,
                    src_rect,
                    center,
                    zoom: m.zoom,
                    opts: LensOptions { smooth: m.smooth, negative: m.negative },
                    dpi,
                    hud: hud.then(|| hud_text(m)),
                };
                if let Some(dib) = render_lens(m.dib.take(), &mut m.text, &view, m.size) {
                    gfx::present(m.hwnd, &dib, pos);
                    m.dib = Some(dib);
                }
            }
            MagnifierMode::Pane => {
                let mut rc = RECT::default();
                // SAFETY: our own window.
                unsafe {
                    let _ = GetClientRect(m.hwnd, &mut rc);
                }
                let (w, h) = (rc.right.max(1), rc.bottom.max(1));
                let src_rect = lens::grab_rect(center, (w, h), m.zoom as f64);
                let Some(src) = m.grab.grab(src_rect) else {
                    return;
                };
                let key = (
                    center,
                    Rect::new(src_rect.x, src_rect.y, w, h),
                    fingerprint(&src) ^ hud as u64 ^ (m.zoom as u64) << 40,
                );
                if !force && m.last == Some(key) {
                    return;
                }
                m.last = Some(key);
                let pdpi = win::dpi_for_window(m.hwnd);
                let view = View {
                    src: &src,
                    src_rect,
                    center,
                    zoom: m.zoom,
                    opts: LensOptions { smooth: m.smooth, negative: m.negative },
                    dpi: pdpi,
                    hud: Some(hud_text(m)),
                };
                if let Some(dib) = render_pane(m.dib.take(), &mut m.text, &view, w, h) {
                    m.dib = Some(dib);
                    // SAFETY: our own window.
                    unsafe {
                        let _ = InvalidateRect(m.hwnd, None, false);
                    }
                }
            }
        }
    });
}

fn hud_text(m: &Mag) -> String {
    let mut s = format!("{}\u{00D7}", m.zoom);
    if m.smooth {
        s.push_str(" \u{00B7} ");
        s.push_str(&crate::i18n::t("tools-mag-smooth"));
    }
    if m.negative {
        s.push_str(" \u{00B7} ");
        s.push_str(&crate::i18n::t("tools-mag-negative"));
    }
    s
}

pub(super) struct View<'a> {
    pub src: &'a Image,
    pub src_rect: Rect,
    pub center: Point,
    pub zoom: u32,
    pub opts: LensOptions,
    pub dpi: u32,
    pub hud: Option<String>,
}

/// The round lens: magnified pixels in a circle with a ring, a centre marker and a
/// zoom badge. `size` is the logical diameter.
pub(super) fn render_lens(cache: Option<Dib>, text: &mut Text, v: &View, size: u32) -> Option<Dib> {
    let dpi = v.dpi;
    let side = lens_window_side(size, dpi);
    let d = sci(size as f32, dpi);
    let m = (side - d) as f32 / 2.0;
    let mut dib = Dib::reuse(cache, side, side)?;
    dib.clear();
    let mut px = vec![0u8; (d * d * 4) as usize];
    lens::magnify(v.src, v.src_rect, v.center, v.zoom as f64, v.opts, &mut px, d, d);
    let r = d as f32 / 2.0;
    {
        let mut pm = dib.pixmap()?;
        // Shadow.
        for i in 0..6 {
            let t = (i + 1) as f32 / 6.0;
            gfx::fill(
                &mut pm,
                gfx::circle(m + r, m + r + sc(3.0, dpi), r + sc(8.0, dpi) * t),
                col(0, 0, 0, (22.0 * (1.0 - t) + 3.0) as u8),
            );
        }
    }
    draw_pixels_in(&mut dib, &px, d, d, m, m, gfx::circle(m + r, m + r, r));
    {
        let mut pm = dib.pixmap()?;
        // Ring: dark outside, light inside.
        gfx::stroke(&mut pm, gfx::circle(m + r, m + r, r - 0.5), col(20, 22, 28, 220), sc(1.5, dpi).max(1.0));
        gfx::stroke(
            &mut pm,
            gfx::circle(m + r, m + r, r - sc(2.0, dpi)),
            col(255, 255, 255, 170),
            sc(1.5, dpi).max(1.0),
        );
        draw_marker(&mut pm, m + r, m + r, v.zoom as f32 * dpi as f32 / 96.0, dpi, v.opts.negative);
    }
    if let Some(h) = &v.hud {
        badge(&mut dib, text, h, (m + r) as i32, (m + d as f32 - sc(34.0, dpi)) as i32, dpi, Align::Center);
    }
    Some(dib)
}

/// The pane: magnified pixels filling the client area plus marker and badge.
pub(super) fn render_pane(cache: Option<Dib>, text: &mut Text, v: &View, w: i32, h: i32) -> Option<Dib> {
    let dpi = v.dpi;
    let mut dib = Dib::reuse(cache, w, h)?;
    let mut px = vec![0u8; (w * h * 4) as usize];
    lens::magnify(v.src, v.src_rect, v.center, v.zoom as f64, v.opts, &mut px, w, h);
    dib.blit_raw(&px, w, h, 0, 0);
    {
        let mut pm = dib.pixmap()?;
        // Pixel-accurate marker around the centre source pixel.
        let cx = lens::output_coord(v.center.x as f64 + 0.5, w, v.center.x, v.zoom as f64) as f32;
        let cy = lens::output_coord(v.center.y as f64 + 0.5, h, v.center.y, v.zoom as f64) as f32;
        draw_marker(&mut pm, cx, cy, v.zoom as f32, dpi, v.opts.negative);
    }
    if let Some(hud) = &v.hud {
        badge(&mut dib, text, hud, sci(10.0, dpi), sci(10.0, dpi), dpi, Align::Left);
    }
    Some(dib)
}

/// Fill `shape` with a BGRA block placed at (x, y).
fn draw_pixels_in(dib: &mut Dib, px: &[u8], w: i32, h: i32, x: f32, y: f32, shape: Option<tiny_skia::Path>) {
    let (Some(size), Some(shape)) = (tiny_skia::IntSize::from_wh(w as u32, h as u32), shape) else {
        return;
    };
    let Some(src) = tiny_skia::Pixmap::from_vec(px.to_vec(), size) else {
        return;
    };
    let Some(mut pm) = dib.pixmap() else {
        return;
    };
    let mut p = tiny_skia::Paint { anti_alias: true, ..Default::default() };
    p.shader = tiny_skia::Pattern::new(
        src.as_ref(),
        tiny_skia::SpreadMode::Pad,
        tiny_skia::FilterQuality::Nearest,
        1.0,
        tiny_skia::Transform::from_translate(x, y),
    );
    pm.fill_path(&shape, &p, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
}

/// A square outline around the centre pixel (`cell` = its size on screen) with short
/// guide ticks; two-tone so it shows on any background.
fn draw_marker(pm: &mut tiny_skia::PixmapMut, cx: f32, cy: f32, cell: f32, dpi: u32, negative: bool) {
    let half = (cell / 2.0).max(sc(3.0, dpi));
    let _ = negative;
    let r = gfx::rrect(cx - half - 1.0, cy - half - 1.0, 2.0 * half + 2.0, 2.0 * half + 2.0, 1.0);
    gfx::stroke(pm, r.clone(), col(0, 0, 0, 160), 3.0);
    gfx::stroke(pm, r, col(255, 255, 255, 230), 1.2);
    let gap = half + sc(4.0, dpi);
    let len = sc(8.0, dpi);
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let a = (cx + dx * gap, cy + dy * gap);
        let b = (cx + dx * (gap + len), cy + dy * (gap + len));
        gfx::line(pm, a, b, col(0, 0, 0, 150), 3.0);
        gfx::line(pm, a, b, col(255, 255, 255, 230), 1.2);
    }
}

fn badge(dib: &mut Dib, text: &mut Text, s: &str, x: i32, y: i32, dpi: u32, align: Align) {
    let fpx = sci(11.5, dpi);
    let (tw, th) = text.measure(s, fpx, Face::UiSemibold);
    let pad = sci(9.0, dpi);
    let h = th + sci(6.0, dpi);
    let w = tw + 2 * pad;
    let x0 = match align {
        Align::Left => x,
        Align::Center => x - w / 2,
        Align::Right => x - w,
    };
    if let Some(mut pm) = dib.pixmap() {
        gfx::fill(
            &mut pm,
            gfx::rrect(x0 as f32, y as f32, w as f32, h as f32, h as f32 / 2.0),
            col(card::BG[0], card::BG[1], card::BG[2], 215),
        );
    }
    text.draw(dib, s, x0 + pad, y + sci(3.0, dpi), fpx, Face::UiSemibold, card::TEXT, Align::Left);
}

fn change(f: impl FnOnce(&mut Mag)) {
    with(|m| {
        f(m);
        m.hud_until = Some(Instant::now() + HUD_TIME);
    });
    tick(true);
}

fn switch_mode() {
    let Some(mode) = with(|m| m.mode) else {
        return;
    };
    let next = if mode == MagnifierMode::Lens { MagnifierMode::Pane } else { MagnifierMode::Lens };
    with(|m| m.mode = next);
    // Close persists the new mode; reopen builds the other window kind.
    close();
    super::open(ScreenTool::Magnifier);
}

fn context_menu(hwnd: HWND, at: Point) {
    let Some((zoom, smooth, negative, mode)) = with(|m| (m.zoom, m.smooth, m.negative, m.mode)) else {
        return;
    };
    let t = crate::i18n::t;
    let zooms: Vec<MenuItem> = [2u32, 3, 4, 6, 8, 12, 16]
        .iter()
        .map(|&z| MenuItem::check(100 + z, format!("{z}\u{00D7}"), z == zoom))
        .collect();
    let items = vec![
        MenuItem::Sub { text: t("tools-mag-zoom"), items: zooms },
        MenuItem::check(1, t("tools-mag-smooth"), smooth),
        MenuItem::check(2, t("tools-mag-negative"), negative),
        MenuItem::Separator,
        MenuItem::check(3, t("tools-mag-lens"), mode == MagnifierMode::Lens),
        MenuItem::check(4, t("tools-mag-pane"), mode == MagnifierMode::Pane),
        MenuItem::Separator,
        MenuItem::item(9, t("tools-close")),
    ];
    match wnd::popup_menu(hwnd, at, &items) {
        1 => change(|m| m.smooth = !m.smooth),
        2 => change(|m| m.negative = !m.negative),
        3 if mode != MagnifierMode::Lens => switch_mode(),
        4 if mode != MagnifierMode::Pane => switch_mode(),
        9 => close(),
        id @ 102..=116 => change(|m| m.zoom = id - 100),
        _ => {}
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ID => {
            tick(false);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if with(|m| m.mode == MagnifierMode::Lens).unwrap_or(false) {
                tick(false);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let n = wnd::wheel_notches(wp);
            if wnd::mods().ctrl {
                change(|m| m.size = (m.size as i32 + n * 20).clamp(120, 800) as u32);
            } else {
                change(|m| m.zoom = lens::step_magnifier_zoom(m.zoom, n > 0));
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let step = if wnd::mods().shift { 10 } else { 1 };
            match wp.0 as u16 {
                k if k == VK_ESCAPE.0 => close(),
                k if k == VK_LEFT.0 => change(|m| m.pan.x -= step),
                k if k == VK_RIGHT.0 => change(|m| m.pan.x += step),
                k if k == VK_UP.0 => change(|m| m.pan.y -= step),
                k if k == VK_DOWN.0 => change(|m| m.pan.y += step),
                k if k == VK_HOME.0 || k == b'0' as u16 => change(|m| m.pan = Point::default()),
                k if k == VK_ADD.0 || k == VK_OEM_PLUS.0 => {
                    change(|m| m.zoom = lens::step_magnifier_zoom(m.zoom, true))
                }
                k if k == VK_SUBTRACT.0 || k == VK_OEM_MINUS.0 => {
                    change(|m| m.zoom = lens::step_magnifier_zoom(m.zoom, false))
                }
                k if k == b'S' as u16 => change(|m| m.smooth = !m.smooth),
                k if k == b'N' as u16 || k == b'I' as u16 => change(|m| m.negative = !m.negative),
                k if k == b'M' as u16 || k == VK_TAB.0 => switch_mode(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // In lens mode the cursor is always over the lens: a click ends it.
            if with(|m| m.mode == MagnifierMode::Lens).unwrap_or(false) {
                close();
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let mut at = wnd::point_from_lparam(lp);
            if lp.0 == -1 {
                at = win::cursor_pos();
            }
            context_menu(hwnd, at);
            LRESULT(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            // The lens shows its own marker; hide the system cursor over it.
            if with(|m| m.mode == MagnifierMode::Lens).unwrap_or(false) {
                SetCursor(HCURSOR::default());
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SIZE => {
            tick(true);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with(|m| {
                if let Some(d) = &m.dib {
                    let r = ps.rcPaint;
                    let _ =
                        BitBlt(hdc, r.left, r.top, r.right - r.left, r.bottom - r.top, d.dc, r.left, r.top, SRCCOPY);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if with(|m| m.mode == MagnifierMode::Pane).unwrap_or(false) {
                // SAFETY: lParam points to the suggested window RECT.
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
