//! Screen focus: dims everything except a spotlight around the cursor (circle or
//! rounded rect). The wheel resizes it, Ctrl+wheel changes the dimming, and
//! Ctrl+Shift+drag pins a fixed rectangle instead (Ctrl+Shift+click goes back to
//! following the cursor). Esc exits.
//!
//! One click-through layered window per monitor, each with a monitor-sized DIB that is
//! filled once; each move rewrites only the old and new spotlight boxes and pushes just
//! that dirty rectangle with `UpdateLayeredWindowIndirect`. Input comes from the
//! low-level hook. The overlay is deliberately *not* excluded from capture, so the
//! spotlight shows in screen shares and recordings (that is what it is for).

use super::card::sc;
use super::gfx::{self, Dib};
use super::wnd;
use crate::common::win::{self, hwnd_to_isize};
use crate::common::{Point, Rect};
use crate::settings::tools::FocusShape;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, ShowWindow, IDC_ARROW, SW_SHOWNA, WM_DPICHANGED, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS: &str = "SnapForge.Focus";
/// Soft edge width (logical).
const FEATHER: f32 = 3.0;
const MIN_SIZE: u32 = 30;
const MAX_SIZE: u32 = 900;

/// A spotlight: centre and shape in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub cx: f32,
    pub cy: f32,
    pub half_w: f32,
    pub half_h: f32,
    pub radius: f32,
    pub feather: f32,
}

impl Spot {
    pub fn circle(c: Point, r: f32, feather: f32) -> Spot {
        Spot { cx: c.x as f32 + 0.5, cy: c.y as f32 + 0.5, half_w: r, half_h: r, radius: r, feather }
    }

    pub fn rect(r: Rect, radius: f32, feather: f32) -> Spot {
        Spot {
            cx: r.x as f32 + r.w as f32 / 2.0,
            cy: r.y as f32 + r.h as f32 / 2.0,
            half_w: r.w as f32 / 2.0,
            half_h: r.h as f32 / 2.0,
            radius: radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0),
            feather,
        }
    }

    /// Signed distance from the spotlight edge (negative inside).
    pub fn distance(&self, x: f32, y: f32) -> f32 {
        let qx = (x - self.cx).abs() - (self.half_w - self.radius);
        let qy = (y - self.cy).abs() - (self.half_h - self.radius);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - self.radius
    }

    /// How much of the dim layer covers pixel centre (x, y): 0 inside, 1 outside,
    /// smooth across the feather.
    pub fn dim(&self, x: f32, y: f32) -> f32 {
        let f = self.feather.max(0.5);
        let t = ((self.distance(x, y) + f / 2.0) / f).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Pixel bounds affected by the spotlight (including the feather and the ring).
    pub fn bounds(&self) -> Rect {
        let e = self.feather + 2.0;
        Rect::from_ltrb(
            (self.cx - self.half_w - e).floor() as i32,
            (self.cy - self.half_h - e).floor() as i32,
            (self.cx + self.half_w + e).ceil() as i32,
            (self.cy + self.half_h + e).ceil() as i32,
        )
    }
}

/// Premultiplied BGRA of the overlay at a pixel: black at `dim` × `alpha` with a faint
/// light ring on the spotlight edge.
pub fn overlay_pixel(spot: &Spot, x: f32, y: f32, alpha: u8) -> [u8; 4] {
    let a = spot.dim(x, y) * alpha as f32 / 255.0;
    let ring = (1.0 - spot.distance(x, y).abs() / 1.2).clamp(0.0, 1.0) * 0.28;
    let out_a = ring + a * (1.0 - ring);
    let c = (255.0 * ring).round() as u8;
    [c, c, c, (out_a * 255.0).round() as u8]
}

struct Monitor {
    hwnd: HWND,
    rect: Rect,
    dib: Dib,
    /// Spotlight box last drawn, monitor-local.
    last: Rect,
}

struct Focus {
    mons: Vec<Monitor>,
    size: u32,
    shape: FocusShape,
    opacity: u8,
    fixed: Option<Rect>,
    drag_from: Option<Point>,
}

thread_local! {
    static STATE: RefCell<Option<Focus>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Focus) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    with(|f| f.mons.iter().map(|m| hwnd_to_isize(m.hwnd)).collect()).unwrap_or_default()
}

fn dim_alpha(opacity: u8) -> u8 {
    (opacity.min(100) as u32 * 255 / 100) as u8
}

pub fn open() -> bool {
    if is_open() {
        return true;
    }
    if !wnd::register(CLASS, Some(proc), IDC_ARROW) {
        return false;
    }
    let s = crate::settings::get().tools.clone();
    let alpha = dim_alpha(s.focus_opacity);
    let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let mut mons = Vec::new();
    for mi in win::monitors() {
        let Some(mut dib) = Dib::new(mi.rect.w, mi.rect.h) else {
            log::warn!("focus: no memory for a {}x{} overlay", mi.rect.w, mi.rect.h);
            continue;
        };
        let full = Rect::new(0, 0, dib.w, dib.h);
        gfx::fill_rect_raw(&mut dib, full, [0, 0, 0, alpha]);
        let Some(hwnd) = wnd::create(ex, CLASS, "", WS_POPUP, mi.rect, HWND::default()) else {
            continue;
        };
        gfx::present(hwnd, &dib, Point::new(mi.rect.x, mi.rect.y));
        mons.push(Monitor { hwnd, rect: mi.rect, dib, last: Rect::default() });
    }
    if mons.is_empty() {
        return false;
    }
    STATE.with(|st| {
        *st.borrow_mut() = Some(Focus {
            mons,
            size: s.focus_size.clamp(MIN_SIZE, MAX_SIZE),
            shape: s.focus_shape,
            opacity: s.focus_opacity.min(100),
            fixed: None,
            drag_from: None,
        })
    });
    redraw();
    with(|f| {
        for m in &f.mons {
            // SAFETY: our own windows, shown without activation.
            unsafe {
                let _ = ShowWindow(m.hwnd, SW_SHOWNA);
            }
        }
    });
    true
}

pub fn close() {
    let Some(f) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    for m in &f.mons {
        // SAFETY: our own windows.
        unsafe {
            let _ = DestroyWindow(m.hwnd);
        }
    }
    let changed = {
        let t = &crate::settings::get().tools;
        t.focus_size != f.size || t.focus_opacity != f.opacity
    };
    if changed {
        crate::settings::update(|s| {
            s.tools.focus_size = f.size;
            s.tools.focus_opacity = f.opacity;
        });
    }
    super::closed(crate::app::ScreenTool::Focus);
}

impl Focus {
    fn spot(&self, cursor: Point) -> Spot {
        let dpi = win::monitor_at(cursor).map_or(96, |m| m.dpi);
        let feather = sc(FEATHER, dpi);
        if let Some(from) = self.drag_from {
            return Spot::rect(Rect::from_corners(from, cursor), sc(8.0, dpi), feather);
        }
        if let Some(r) = self.fixed {
            return Spot::rect(r, sc(8.0, dpi), feather);
        }
        let r = sc(self.size as f32, dpi);
        match self.shape {
            FocusShape::Circle => Spot::circle(cursor, r, feather),
            FocusShape::RoundRect => {
                let (hw, hh) = (r * 1.6, r);
                let rect = Rect::new(cursor.x - hw as i32, cursor.y - hh as i32, (2.0 * hw) as i32, (2.0 * hh) as i32);
                Spot::rect(rect, sc(18.0, dpi), feather)
            }
        }
    }
}

/// Paint the dim layer with `spot` cut out into a monitor DIB; `area` is monitor-local.
pub(super) fn paint_area(dib: &mut Dib, origin: Point, area: Rect, spot: Option<&Spot>, alpha: u8) {
    let area = area.intersect(&Rect::new(0, 0, dib.w, dib.h));
    if area.is_empty() {
        return;
    }
    let stride = dib.w as usize * 4;
    let data = dib.data_mut();
    for y in area.y..area.bottom() {
        let row = y as usize * stride;
        let sy = (origin.y + y) as f32 + 0.5;
        for x in area.x..area.right() {
            let px = match spot {
                Some(s) => overlay_pixel(s, (origin.x + x) as f32 + 0.5, sy, alpha),
                None => [0, 0, 0, alpha],
            };
            let i = row + x as usize * 4;
            data[i..i + 4].copy_from_slice(&px);
        }
    }
}

fn redraw() {
    let cursor = win::cursor_pos();
    with(|f| {
        let spot = f.spot(cursor);
        let alpha = dim_alpha(f.opacity);
        let bounds = spot.bounds();
        for m in &mut f.mons {
            let origin = Point::new(m.rect.x, m.rect.y);
            let new = bounds.offset(-origin.x, -origin.y).intersect(&Rect::new(0, 0, m.rect.w, m.rect.h));
            if new.is_empty() && m.last.is_empty() {
                continue;
            }
            // Restore the previous box to plain dim, then cut the new spotlight.
            paint_area(&mut m.dib, origin, m.last, None, alpha);
            paint_area(&mut m.dib, origin, new, Some(&spot), alpha);
            let dirty = m.last.union(&new);
            gfx::present_dirty(m.hwnd, &m.dib, origin, dirty);
            m.last = new;
        }
    });
}

/// Refill every monitor (opacity changed).
fn refill() {
    with(|f| {
        let alpha = dim_alpha(f.opacity);
        for m in &mut f.mons {
            let full = Rect::new(0, 0, m.rect.w, m.rect.h);
            gfx::fill_rect_raw(&mut m.dib, full, [0, 0, 0, alpha]);
            m.last = full;
        }
    });
    redraw();
}

pub fn on_move(_p: Point) {
    let follow = with(|f| f.fixed.is_none() || f.drag_from.is_some()).unwrap_or(false);
    if follow {
        redraw();
    }
}

pub fn on_wheel(notches: i32) {
    if notches == 0 {
        return;
    }
    // SAFETY: plain query; the hook delivers wheel input asynchronously.
    let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) } < 0;
    if ctrl {
        with(|f| f.opacity = (f.opacity as i32 + notches * 5).clamp(10, 95) as u8);
        refill();
    } else {
        with(|f| f.size = (f.size as i32 + notches * 16).clamp(MIN_SIZE as i32, MAX_SIZE as i32) as u32);
        redraw();
    }
}

/// Ctrl+Shift+drag from the hook: `start` true on press, false on release.
pub fn on_drag(start: bool, p: Point) {
    with(|f| {
        if start {
            f.drag_from = Some(p);
            f.fixed = None;
        } else if let Some(from) = f.drag_from.take() {
            let r = Rect::from_corners(from, p);
            // A click (no real drag) goes back to following the cursor.
            f.fixed = (r.w > 8 && r.h > 8).then_some(r);
        }
    });
    redraw();
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_spot_is_clear_inside_and_dim_outside() {
        let s = Spot::circle(Point::new(100, 100), 50.0, 4.0);
        assert_eq!(s.dim(100.5, 100.5), 0.0);
        assert_eq!(s.dim(130.0, 100.0), 0.0);
        assert_eq!(s.dim(160.0, 100.0), 1.0);
        let edge = s.dim(150.5, 100.5);
        assert!(edge > 0.2 && edge < 0.8, "{edge}");
        // Monotonic across the feather.
        let a = s.dim(149.0, 100.5);
        let b = s.dim(152.0, 100.5);
        assert!(a < edge && edge < b);
        assert_eq!(overlay_pixel(&s, 0.5, 0.5, 200), [0, 0, 0, 200]);
        assert_eq!(overlay_pixel(&s, 100.5, 100.5, 200), [0, 0, 0, 0]);
    }

    #[test]
    fn rect_spot_distance() {
        let s = Spot::rect(Rect::new(0, 0, 200, 100), 10.0, 2.0);
        assert!((s.distance(100.0, 50.0) + 50.0).abs() < 1e-4, "centre is half the height inside");
        assert!(s.distance(100.0, -5.0) > 4.9);
        assert!(s.distance(5.0, 50.0) < 0.0);
        // The corner is rounded: the exact corner point is outside.
        assert!(s.distance(0.5, 0.5) > 0.0);
        assert_eq!(s.dim(100.0, 50.0), 0.0);
        assert_eq!(s.dim(250.0, 50.0), 1.0);
        let b = s.bounds();
        assert!(b.x < 0 && b.right() > 200 && b.y < 0 && b.bottom() > 100);
    }

    #[test]
    fn paint_area_restores_plain_dim() {
        let Some(mut dib) = Dib::new(64, 64) else {
            return;
        };
        let spot = Spot::circle(Point::new(132, 32), 10.0, 2.0);
        let origin = Point::new(100, 0);
        paint_area(&mut dib, origin, Rect::new(0, 0, 64, 64), Some(&spot), 160);
        let at = |d: &Dib, x: usize, y: usize| d.data()[(y * 64 + x) * 4 + 3];
        assert_eq!(at(&dib, 32, 32), 0);
        assert_eq!(at(&dib, 0, 0), 160);
        paint_area(&mut dib, origin, spot.bounds().offset(-100, 0), None, 160);
        assert_eq!(at(&dib, 32, 32), 160);
    }
}
