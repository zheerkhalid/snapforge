//! Screen crosshair: horizontal and vertical lines through the cursor across the whole
//! virtual screen, with a small loupe showing the coordinates and the color.
//!
//! The lines are four thin click-through layered windows (left, right, above, below
//! the cursor, leaving a gap around it) that present sub-rectangles of two DIBs painted
//! once, so following the cursor costs four `UpdateLayeredWindow` calls and no
//! repainting. Input comes from the low-level hook (moves, Esc, right-click).

use super::card::{self, sc, sci};
use super::color::{self, Rgb};
use super::gfx::{self, col, Align, Dib, Face, Grabber, Text};
use super::lens::{self, Grid};
use super::wnd;
use crate::common::win::{self, hwnd_to_isize};
use crate::common::{Image, Point, Rect};
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, KillTimer, SetTimer, ShowWindow, IDC_ARROW, SW_HIDE, SW_SHOWNA, WM_DPICHANGED,
    WM_TIMER, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS: &str = "SnapForge.Crosshair";
const TIMER_ID: usize = 1;
/// Loupe refresh while the cursor rests.
const TIMER_MS: u32 = 100;
/// Line thickness: 1 px core with a 1 px translucent halo each side.
const THICK: i32 = 3;
/// Gap around the cursor (logical).
const GAP: i32 = 12;
const LOUPE_W: f32 = 168.0;
const MARGIN: f32 = 12.0;
/// Line color (vivid, readable on light and dark content).
const LINE: [u8; 3] = [255, 59, 107];

struct Cross {
    /// left, right, up, down
    segs: [HWND; 4],
    loupe: HWND,
    vs: Rect,
    hdib: Dib,
    vdib: Dib,
    grab: Grabber,
    text: Text,
    dib: Option<Dib>,
    last: Option<(Point, u64)>,
}

thread_local! {
    static STATE: RefCell<Option<Cross>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Cross) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    with(|c| c.segs.iter().chain(std::iter::once(&c.loupe)).map(|h| hwnd_to_isize(*h)).collect()).unwrap_or_default()
}

/// Paint a line DIB: `len` long along the axis, THICK across it.
fn line_dib(len: i32, horizontal: bool) -> Option<Dib> {
    let (w, h) = if horizontal { (len, THICK) } else { (THICK, len) };
    let mut d = Dib::new(w, h)?;
    for i in 0..THICK {
        let px = if i == THICK / 2 { gfx::premul(LINE[0], LINE[1], LINE[2], 255) } else { gfx::premul(0, 0, 0, 96) };
        let r = if horizontal { Rect::new(0, i, len, 1) } else { Rect::new(i, 0, 1, len) };
        gfx::fill_rect_raw(&mut d, r, px);
    }
    Some(d)
}

pub fn open() -> bool {
    if is_open() {
        return true;
    }
    if !wnd::register(CLASS, Some(proc), IDC_ARROW) {
        return false;
    }
    let vs = win::virtual_screen();
    let (Some(hdib), Some(vdib)) = (line_dib(vs.w, true), line_dib(vs.h, false)) else {
        return false;
    };
    let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let mut hs = Vec::new();
    for _ in 0..5 {
        match wnd::create(ex, CLASS, "", WS_POPUP, Rect::new(vs.x, vs.y, 1, 1), HWND::default()) {
            Some(h) => {
                win::exclude_from_capture(h);
                hs.push(h);
            }
            None => {
                for h in hs {
                    // SAFETY: windows we just created.
                    unsafe {
                        let _ = DestroyWindow(h);
                    }
                }
                return false;
            }
        }
    }
    let segs = [hs[0], hs[1], hs[2], hs[3]];
    let loupe = hs[4];
    STATE.with(|s| {
        *s.borrow_mut() = Some(Cross {
            segs,
            loupe,
            vs,
            hdib,
            vdib,
            grab: Grabber::default(),
            text: Text::default(),
            dib: None,
            last: None,
        })
    });
    // SAFETY: our own window.
    unsafe {
        SetTimer(loupe, TIMER_ID, TIMER_MS, None);
    }
    on_move(win::cursor_pos());
    true
}

pub fn close() {
    let Some(c) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    // SAFETY: our own windows and timer.
    unsafe {
        let _ = KillTimer(c.loupe, TIMER_ID);
        for h in c.segs.iter().chain(std::iter::once(&c.loupe)) {
            let _ = DestroyWindow(*h);
        }
    }
    super::closed(crate::app::ScreenTool::Crosshair);
}

/// Segment rectangles (left, right, up, down) for a cursor at `p` in `vs`.
pub(super) fn segments(p: Point, vs: Rect, gap: i32) -> [Rect; 4] {
    let t = THICK;
    let y = p.y - t / 2;
    let x = p.x - t / 2;
    [
        Rect::from_ltrb(vs.x, y, (p.x - gap).max(vs.x), y + t),
        Rect::from_ltrb((p.x + gap + 1).min(vs.right()), y, vs.right(), y + t),
        Rect::from_ltrb(x, vs.y, x + t, (p.y - gap).max(vs.y)),
        Rect::from_ltrb(x, (p.y + gap + 1).min(vs.bottom()), x + t, vs.bottom()),
    ]
}

pub fn on_move(p: Point) {
    let moved = with(|c| {
        let dpi = win::monitor_at(p).map_or(96, |m| m.dpi);
        let gap = win::scale(GAP, dpi);
        for (i, r) in segments(p, c.vs, gap).iter().enumerate() {
            let h = c.segs[i];
            if r.is_empty() {
                // SAFETY: our own window.
                unsafe {
                    let _ = ShowWindow(h, SW_HIDE);
                }
                continue;
            }
            let (dib, src) =
                if i < 2 { (&c.hdib, Point::new(r.x - c.vs.x, 0)) } else { (&c.vdib, Point::new(0, r.y - c.vs.y)) };
            gfx::present_sub(h, dib, src, *r);
            // SAFETY: our own window, shown without activation.
            unsafe {
                let _ = ShowWindow(h, SW_SHOWNA);
            }
        }
    });
    if moved.is_some() {
        refresh_loupe(true);
    }
}

fn refresh_loupe(force: bool) {
    let p = win::cursor_pos();
    with(|c| {
        let mon = win::monitor_at(p);
        let dpi = mon.as_ref().map_or(96, |m| m.dpi);
        let grid = Grid::fit(sci(LOUPE_W - 20.0, dpi), 8, dpi);
        let src_rect = grid.source_rect(p);
        let Some(src) = c.grab.grab(src_rect) else {
            return;
        };
        let fp = src.data.iter().step_by(3).fold(0u64, |h, &b| h.wrapping_mul(31).wrapping_add(b as u64));
        if !force && c.last == Some((p, fp)) {
            return;
        }
        c.last = Some((p, fp));
        let color = Rgb::from_bgra(src.pixel((p.x - src_rect.x) as u32, (p.y - src_rect.y) as u32));
        let Some(dib) = render_loupe(c.dib.take(), &mut c.text, &src, src_rect, p, grid, color, dpi) else {
            return;
        };
        let monr = mon.map_or(c.vs, |m| m.rect);
        let gap = sci(22.0, dpi) - sci(MARGIN, dpi);
        let r = crate::overlay::loupe::place(p, (dib.w, dib.h), gap, &monr);
        gfx::present(c.loupe, &dib, Point::new(r.x, r.y));
        // SAFETY: our own window, shown without activation.
        unsafe {
            let _ = ShowWindow(c.loupe, SW_SHOWNA);
        }
        c.dib = Some(dib);
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_loupe(
    cache: Option<Dib>,
    text: &mut Text,
    src: &Image,
    src_rect: Rect,
    p: Point,
    grid: Grid,
    color: Rgb,
    dpi: u32,
) -> Option<Dib> {
    let m = sc(MARGIN, dpi);
    let pad = sc(10.0, dpi);
    let cw = sc(LOUPE_W, dpi);
    let side = grid.side() as f32;
    let ch = pad + side + sc(8.0, dpi) + sc(40.0, dpi) + pad * 0.6;
    let mut dib = Dib::reuse(cache, (cw + 2.0 * m).ceil() as i32, (ch + 2.0 * m).ceil() as i32)?;
    dib.clear();
    card::draw_card(&mut dib, m, m, cw, ch, dpi);
    let gx = (m + (cw - side) / 2.0).round();
    let gy = (m + pad).round();
    let mut px = vec![0u8; (grid.side() * grid.side() * 4) as usize];
    lens::render_grid(src, src_rect, p, grid, &mut px);
    card::draw_rounded_pixels(&mut dib, &px, grid.side(), grid.side(), gx, gy, sc(6.0, dpi));
    // Crosshair guide through the grid centre, matching the screen lines.
    if let Some(mut pm) = dib.pixmap() {
        let c = gx + side / 2.0;
        let cy = gy + side / 2.0;
        let h = grid.cell as f32 / 2.0 + 1.0;
        let lc = col(LINE[0], LINE[1], LINE[2], 200);
        gfx::line(&mut pm, (gx + 1.0, cy), (c - h - 1.0, cy), lc, 1.0);
        gfx::line(&mut pm, (c + h + 1.0, cy), (gx + side - 1.0, cy), lc, 1.0);
        gfx::line(&mut pm, (c, gy + 1.0), (c, cy - h - 1.0), lc, 1.0);
        gfx::line(&mut pm, (c, cy + h + 1.0), (c, gy + side - 1.0), lc, 1.0);
    }
    let ty = (gy + side + sc(8.0, dpi)) as i32;
    let sw = sc(16.0, dpi);
    let sx = m + pad;
    if let Some(mut pm) = dib.pixmap() {
        gfx::fill(
            &mut pm,
            gfx::rrect(sx, ty as f32 + sc(2.0, dpi), sw, sw, sc(4.0, dpi)),
            col(color.r, color.g, color.b, 255),
        );
        gfx::stroke(
            &mut pm,
            gfx::rrect(sx + 0.5, ty as f32 + sc(2.0, dpi) + 0.5, sw - 1.0, sw - 1.0, sc(4.0, dpi)),
            col(255, 255, 255, 60),
            1.0,
        );
    }
    let hex = color::format_value(color, crate::settings::tools::ColorFormat::Html, false);
    let tx = (sx + sw + sc(7.0, dpi)) as i32;
    text.draw(&mut dib, &hex, tx, ty, sci(12.5, dpi), Face::UiSemibold, card::TEXT, Align::Left);
    let rgb = format!("{}, {}, {}", color.r, color.g, color.b);
    text.draw(
        &mut dib,
        &rgb,
        (m + cw - pad) as i32,
        ty + sci(1.0, dpi),
        sci(11.0, dpi),
        Face::Ui,
        card::TEXT_DIM,
        Align::Right,
    );
    let pos = format!("X {}   Y {}", p.x, p.y);
    text.draw(
        &mut dib,
        &pos,
        (m + cw / 2.0) as i32,
        ty + sci(20.0, dpi),
        sci(11.5, dpi),
        Face::Ui,
        card::TEXT,
        Align::Center,
    );
    Some(dib)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ID => {
            refresh_loupe(false);
            LRESULT(0)
        }
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_leave_a_gap_and_reach_the_edges() {
        let vs = Rect::new(-1920, -200, 4480, 1640);
        let p = Point::new(100, 300);
        let [l, r, u, d] = segments(p, vs, 12);
        assert_eq!(l, Rect::from_ltrb(-1920, 299, 88, 302));
        assert_eq!(r, Rect::from_ltrb(113, 299, 2560, 302));
        assert_eq!(u, Rect::from_ltrb(99, -200, 102, 288));
        assert_eq!(d, Rect::from_ltrb(99, 313, 102, 1440));
        // The core (middle) pixel row passes through the cursor.
        assert_eq!(l.y + THICK / 2, p.y);
        assert_eq!(u.x + THICK / 2, p.x);
        // At the very edge, the segment on that side is empty.
        let [l, ..] = segments(Point::new(-1920, 0), vs, 12);
        assert!(l.is_empty());
    }
}
