//! Screen ruler: a translucent, topmost, draggable ruler (horizontal or vertical,
//! resizable length) with px / cm / inch ticks, a live cursor marker and drag-to-measure;
//! plus a protractor mode (angle between two arms from a centre).
//!
//! Mouse: drag the tick band to measure, drag elsewhere to move, drag the far end to
//! resize; chips: units, rotate, close. Keys: Esc close, R rotate, U units, P protractor,
//! C calibrate, Ctrl+C copy the measurement, arrows move 1 px (Shift: 10). Right-click
//! opens the menu. Measurements are auto-copied when enabled.

use super::calibrate;
use super::card::{self, sc, sci};
use super::gfx::{self, col, Align, Dib, Face, Text};
use super::measure::{self, Tick};
use super::wnd::{self, MenuItem};
use crate::app::ScreenTool;
use crate::common::win::{self, hwnd_to_isize};
use crate::common::{Point, Rect};
use crate::settings::tools::RulerUnit;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, KillTimer, LoadCursorW, SetCursor, SetForegroundWindow, SetTimer, ShowWindow,
    HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM, IDC_SIZEALL, IDC_SIZENS, IDC_SIZEWE, SW_SHOW, WM_CAPTURECHANGED,
    WM_CLOSE, WM_CONTEXTMENU, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_SETCURSOR, WM_TIMER, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS: &str = "SnapForge.Ruler";
const TIMER_ID: usize = 1;
/// Cursor-marker polling while the ruler is open.
const TIMER_MS: u32 = 30;
/// Logical thickness.
const THICK: f32 = 62.0;
const MIN_LEN: f32 = 220.0;
const PROT_SIZE: f32 = 320.0;
const END_GRIP: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ruler,
    Protractor,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    None,
    Move { cursor: Point, pos: Point },
    Measure,
    Resize { cursor: Point, len: i32 },
    Arm(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Chip {
    Unit,
    Rotate,
    Mode,
    Close,
}

struct Ruler {
    hwnd: HWND,
    pos: Point,
    mode: Mode,
    /// Physical length along the axis.
    len: i32,
    vertical: bool,
    unit: RulerUnit,
    opacity: u8,
    auto_copy: bool,
    /// Protractor diameter (physical) and arm angles (degrees, CCW from +x).
    prot_size: i32,
    arms: [f64; 2],
    drag: Drag,
    /// Measured span along the axis (physical pixels from zero).
    measure: Option<(f64, f64)>,
    cursor_u: Option<f64>,
    hover: Option<Chip>,
    dpi: u32,
    ppi: f64,
    text: Text,
    dib: Option<Dib>,
    /// Static layer (body + ticks), horizontal orientation, with its key.
    base: Option<(BaseKey, Dib)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BaseKey {
    len: i32,
    dpi: u32,
    unit: RulerUnit,
    ppi: f64,
    opacity: u8,
}

thread_local! {
    static STATE: RefCell<Option<Ruler>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Ruler) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    STATE.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

pub fn hwnds() -> Vec<isize> {
    let mut v = with(|r| vec![hwnd_to_isize(r.hwnd)]).unwrap_or_default();
    v.extend(calibrate::hwnds());
    v
}

pub fn open() -> bool {
    if let Some(h) = with(|r| r.hwnd) {
        // SAFETY: our own window.
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return true;
    }
    if !wnd::register(CLASS, Some(proc), IDC_ARROW) {
        return false;
    }
    let s = crate::settings::get().tools.clone();
    let cursor = win::cursor_pos();
    let mon = win::monitor_at(cursor);
    let dpi = mon.as_ref().map_or(96, |m| m.dpi);
    let work = mon.as_ref().map_or_else(win::virtual_screen, |m| m.work);
    let axis = if s.ruler_vertical { work.h } else { work.w };
    let len = sci(s.ruler_length as f32, dpi).clamp(sci(MIN_LEN, dpi), axis.max(sci(MIN_LEN, dpi)));
    let thick = sci(THICK, dpi);
    let pos = if s.ruler_vertical {
        Point::new((cursor.x + sci(40.0, dpi)).min(work.right() - thick), work.y + (work.h - len) / 2)
    } else {
        Point::new(work.x + (work.w - len) / 2, (cursor.y + sci(40.0, dpi)).min(work.bottom() - thick))
    };
    let Some(hwnd) = wnd::create(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        CLASS,
        "SnapForge Ruler",
        WS_POPUP,
        Rect::new(pos.x, pos.y, 1, 1),
        HWND::default(),
    ) else {
        return false;
    };
    win::exclude_from_capture(hwnd);
    let ppi = mon.as_ref().map_or(measure::DEFAULT_PPI, calibrate::ppi_for);
    STATE.with(|st| {
        *st.borrow_mut() = Some(Ruler {
            hwnd,
            pos,
            mode: Mode::Ruler,
            len,
            vertical: s.ruler_vertical,
            unit: s.ruler_units,
            opacity: s.ruler_opacity.clamp(30, 100),
            auto_copy: s.ruler_auto_copy,
            prot_size: sci(PROT_SIZE, dpi),
            arms: [0.0, 45.0],
            drag: Drag::None,
            measure: None,
            cursor_u: None,
            hover: None,
            dpi,
            ppi,
            text: Text::default(),
            dib: None,
            base: None,
        })
    });
    render();
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
    }
    true
}

pub fn close() {
    calibrate::close();
    let Some(r) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else {
        return;
    };
    // SAFETY: our own window and timer.
    unsafe {
        let _ = KillTimer(r.hwnd, TIMER_ID);
        let _ = DestroyWindow(r.hwnd);
    }
    let length = (r.len as i64 * 96 / r.dpi.max(1) as i64) as u32;
    let changed = {
        let t = &crate::settings::get().tools;
        t.ruler_length != length || t.ruler_vertical != r.vertical || t.ruler_units != r.unit
    };
    if changed {
        crate::settings::update(|s| {
            s.tools.ruler_length = length;
            s.tools.ruler_vertical = r.vertical;
            s.tools.ruler_units = r.unit;
        });
    }
    super::closed(ScreenTool::Ruler);
}

/// The calibration dialog stored a new diagonal: re-read the PPI.
pub fn calibration_changed() {
    with(|r| {
        r.refresh_monitor();
        r.base = None;
    });
    render();
}

impl Ruler {
    fn size(&self) -> (i32, i32) {
        match self.mode {
            Mode::Protractor => (self.prot_size, self.prot_size),
            Mode::Ruler => {
                let t = sci(THICK, self.dpi);
                if self.vertical {
                    (t, self.len)
                } else {
                    (self.len, t)
                }
            }
        }
    }

    fn rect(&self) -> Rect {
        let (w, h) = self.size();
        Rect::new(self.pos.x, self.pos.y, w, h)
    }

    /// Monitor under the ruler's centre: DPI and PPI.
    fn refresh_monitor(&mut self) {
        if let Some(m) = win::monitor_at(self.rect().center()) {
            if m.dpi != self.dpi {
                // Keep the physical length; thickness and text follow the new DPI.
                self.prot_size = (self.prot_size as i64 * m.dpi as i64 / self.dpi.max(1) as i64) as i32;
                self.dpi = m.dpi;
            }
            self.ppi = calibrate::ppi_for(&m);
        }
    }

    /// Window-local point → (along, across) in the horizontal layout.
    fn to_uv(&self, p: Point) -> (f64, f64) {
        let t = sci(THICK, self.dpi);
        if self.vertical {
            (p.y as f64 + 0.5, (t - 1 - p.x) as f64 + 0.5)
        } else {
            (p.x as f64 + 0.5, p.y as f64 + 0.5)
        }
    }

    fn measurement_text(&self) -> Option<String> {
        match self.mode {
            Mode::Ruler => {
                let (a, b) = self.measure?;
                Some(measure::format_length((b - a).abs().round(), self.unit, self.ppi))
            }
            Mode::Protractor => Some(format!(
                "{:.1}\u{00B0}",
                measure::angle_between((0.0, 0.0), arm_dir(self.arms[0]), arm_dir(self.arms[1]))
            )),
        }
    }

    fn copy_measurement(&self) {
        if let Some(t) = self.measurement_text() {
            match crate::output::clipboard::copy_text(&t) {
                Ok(()) => log::info!("ruler: copied {t}"),
                Err(e) => log::warn!("ruler: clipboard: {e:#}"),
            }
        }
    }

    fn view(&self) -> View {
        View {
            len: self.len,
            dpi: self.dpi,
            unit: self.unit,
            ppi: self.ppi,
            opacity: self.opacity,
            cursor_u: self.cursor_u,
            measure: self.measure,
            hover: self.hover,
        }
    }

    fn chip_at(&self, p: Point) -> Option<Chip> {
        match self.mode {
            Mode::Ruler => {
                let (u, v) = self.to_uv(p);
                ruler_chips(&self.view(), &mut Text::default())
                    .into_iter()
                    .find(|(_, r)| r.contains(Point::new(u as i32, v as i32)))
                    .map(|(c, _)| c)
            }
            Mode::Protractor => {
                protractor_chips(self.prot_size, self.dpi).into_iter().find(|(_, r)| r.contains(p)).map(|(c, _)| c)
            }
        }
    }

    fn in_tick_band(&self, p: Point) -> bool {
        let (_, v) = self.to_uv(p);
        v < sc(THICK * 0.5, self.dpi) as f64
    }

    fn at_far_end(&self, p: Point) -> bool {
        let (u, _) = self.to_uv(p);
        u > (self.len as f64 - sc(END_GRIP, self.dpi) as f64)
    }
}

/// Unit direction vector (screen coordinates, y down) for an angle CCW from +x.
fn arm_dir(deg: f64) -> (f64, f64) {
    let r = deg.to_radians();
    (r.cos(), -r.sin())
}

pub(super) struct View {
    pub len: i32,
    pub dpi: u32,
    pub unit: RulerUnit,
    pub ppi: f64,
    pub opacity: u8,
    pub cursor_u: Option<f64>,
    pub measure: Option<(f64, f64)>,
    pub hover: Option<Chip>,
}

fn chip_label(unit: RulerUnit) -> String {
    unit.suffix().to_string()
}

/// Chip rectangles in the horizontal layout (right end, bottom row).
fn ruler_chips(v: &View, text: &mut Text) -> Vec<(Chip, Rect)> {
    let dpi = v.dpi;
    let h = sci(22.0, dpi);
    let y = sci(THICK, dpi) - sci(7.0, dpi) - h;
    let mut x = v.len - sci(END_GRIP + 2.0, dpi);
    let mut out = Vec::new();
    for c in [Chip::Close, Chip::Rotate, Chip::Mode, Chip::Unit] {
        let w = if c == Chip::Unit {
            text.measure(&chip_label(v.unit), sci(11.5, dpi), Face::UiSemibold).0 + sci(16.0, dpi)
        } else {
            h
        };
        x -= w;
        out.push((c, Rect::new(x, y, w, h)));
        x -= sci(4.0, dpi);
    }
    out
}

fn protractor_chips(size: i32, dpi: u32) -> Vec<(Chip, Rect)> {
    let h = sci(22.0, dpi);
    let y = size / 2 + (size as f32 * 0.26) as i32;
    let c = size / 2;
    vec![(Chip::Mode, Rect::new(c - h - sci(3.0, dpi), y, h, h)), (Chip::Close, Rect::new(c + sci(3.0, dpi), y, h, h))]
}

#[allow(clippy::too_many_arguments)]
fn draw_chip(
    dib: &mut Dib,
    text: &mut Text,
    c: Chip,
    r: Rect,
    unit: RulerUnit,
    hover: bool,
    protractor: bool,
    dpi: u32,
) {
    if let Some(mut pm) = dib.pixmap() {
        let a = if hover { 70 } else { 34 };
        gfx::fill(
            &mut pm,
            gfx::rrect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, r.h as f32 / 2.0),
            col(255, 255, 255, a),
        );
        let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
        draw_icon(&mut pm, c, (cx, cy), sc(5.0, dpi), protractor, sc(1.4, dpi));
    }
    if c == Chip::Unit {
        let s = chip_label(unit);
        let fpx = sci(11.5, dpi);
        let (_, th) = text.measure(&s, fpx, Face::UiSemibold);
        text.draw(dib, &s, r.x + r.w / 2, r.y + (r.h - th) / 2, fpx, Face::UiSemibold, card::TEXT, Align::Center);
    }
}

/// Small vector icons for the chips; `h` is half the icon size, `w` the stroke width.
fn draw_icon(pm: &mut tiny_skia::PixmapMut, c: Chip, (cx, cy): (f32, f32), h: f32, protractor: bool, w: f32) {
    let st = tiny_skia::Stroke {
        width: w,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    let mut pb = tiny_skia::PathBuilder::new();
    let arc = |pb: &mut tiny_skia::PathBuilder, ox: f32, oy: f32, r: f32, from: f32, span: f32, steps: usize| {
        for i in 0..=steps {
            let a = from + span * i as f32 / steps as f32;
            let (x, y) = (ox + r * a.cos(), oy - r * a.sin());
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
    };
    match c {
        Chip::Unit => return,
        Chip::Close => {
            let k = h * 0.8;
            pb.move_to(cx - k, cy - k);
            pb.line_to(cx + k, cy + k);
            pb.move_to(cx + k, cy - k);
            pb.line_to(cx - k, cy + k);
        }
        Chip::Rotate => {
            // Three-quarter circle with an arrowhead at its end.
            let start = std::f32::consts::FRAC_PI_4;
            arc(&mut pb, cx, cy, h, start, std::f32::consts::PI * 1.5, 24);
            let (ex, ey) = (cx + h * start.cos(), cy - h * start.sin());
            pb.move_to(ex - h * 0.75, ey - h * 0.05);
            pb.line_to(ex, ey);
            pb.line_to(ex + h * 0.05, ey - h * 0.75);
        }
        Chip::Mode if protractor => {
            // A tiny ruler (back to ruler mode).
            pb.move_to(cx - h * 1.1, cy - h * 0.45);
            pb.line_to(cx + h * 1.1, cy - h * 0.45);
            pb.line_to(cx + h * 1.1, cy + h * 0.55);
            pb.line_to(cx - h * 1.1, cy + h * 0.55);
            pb.close();
            for i in 0..4 {
                let x = cx - h * 0.6 + i as f32 * h * 0.4;
                pb.move_to(x, cy - h * 0.45);
                pb.line_to(x, cy - h * if i % 2 == 0 { 0.0 } else { 0.15 });
            }
        }
        Chip::Mode => {
            // An angle with its arc (protractor).
            let (ox, oy) = (cx - h * 0.9, cy + h * 0.8);
            pb.move_to(cx + h, oy);
            pb.line_to(ox, oy);
            pb.line_to(cx + h * 0.6, cy - h);
            arc(&mut pb, ox, oy, h, 0.0, 1.6f32.atan2(1.5), 8);
        }
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &gfx::paint(col(240, 242, 247, 255)), &st, tiny_skia::Transform::identity(), None);
    }
}

/// The static layer: translucent body, ticks and labels (horizontal orientation).
pub(super) fn render_base(v: &View, text: &mut Text) -> Option<Dib> {
    let dpi = v.dpi;
    let t = sci(THICK, dpi);
    let mut dib = Dib::new(v.len, t)?;
    let a = (v.opacity as u32 * 255 / 100) as u8;
    {
        let mut pm = dib.pixmap()?;
        let (w, h) = (v.len as f32, t as f32);
        gfx::fill(&mut pm, gfx::rrect(0.0, 0.0, w, h, sc(7.0, dpi)), col(26, 28, 36, a));
        // A faint top highlight gives the glass a little depth.
        gfx::fill(&mut pm, gfx::rrect(0.0, 0.0, w, sc(THICK * 0.5, dpi), sc(7.0, dpi)), col(255, 255, 255, 10));
        gfx::stroke(&mut pm, gfx::rrect(0.5, 0.5, w - 1.0, h - 1.0, sc(7.0, dpi)), col(255, 255, 255, 46), 1.0);
        // End grip.
        let gx = w - sc(END_GRIP * 0.5, dpi);
        gfx::line(
            &mut pm,
            (gx - sc(2.0, dpi), h / 2.0 - sc(8.0, dpi)),
            (gx - sc(2.0, dpi), h / 2.0 + sc(8.0, dpi)),
            col(255, 255, 255, 70),
            sc(1.5, dpi),
        );
        gfx::line(
            &mut pm,
            (gx + sc(2.0, dpi), h / 2.0 - sc(8.0, dpi)),
            (gx + sc(2.0, dpi), h / 2.0 + sc(8.0, dpi)),
            col(255, 255, 255, 70),
            sc(1.5, dpi),
        );
    }
    let ticks: Vec<Tick> = measure::ticks((v.len - sci(END_GRIP, dpi)) as f64, v.unit, v.ppi, sc(3.0, dpi) as f64);
    let lens = [sc(6.0, dpi), sc(11.0, dpi), sc(18.0, dpi)];
    {
        let mut pm = dib.pixmap()?;
        let mut pb = [tiny_skia::PathBuilder::new(), tiny_skia::PathBuilder::new(), tiny_skia::PathBuilder::new()];
        for tk in &ticks {
            // Crisp 1 px lines on pixel centres; the zero tick sits just inside the edge.
            let x = (tk.pos.round() as f32 + 0.5).max(1.5);
            pb[tk.level as usize].move_to(x, 0.0);
            pb[tk.level as usize].line_to(x, lens[tk.level as usize]);
        }
        let alphas = [150u8, 200, 255];
        for (i, b) in pb.into_iter().enumerate() {
            if let Some(p) = b.finish() {
                let s = tiny_skia::Stroke { width: 1.0, ..Default::default() };
                pm.stroke_path(
                    &p,
                    &gfx::paint(col(235, 238, 245, alphas[i])),
                    &s,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
    }
    let fpx = sci(10.5, dpi);
    let ly = (lens[2] + sc(1.0, dpi)) as i32;
    let max_x = v.len - sci(END_GRIP + 4.0, dpi);
    for tk in ticks.iter().filter(|t| t.label.is_some()) {
        if let Some(l) = &tk.label {
            let x = tk.pos.round() as i32 + sci(3.0, dpi);
            let (tw, _) = text.measure(l, fpx, Face::Ui);
            if x + tw < max_x {
                text.draw(&mut dib, l, x, ly, fpx, Face::Ui, [220, 224, 232, 255], Align::Left);
            }
        }
    }
    Some(dib)
}

/// Full ruler frame in the horizontal orientation: base + measurement, cursor marker,
/// readout and chips.
pub(super) fn render_ruler(base: &Dib, text: &mut Text, v: &View) -> Option<Dib> {
    let dpi = v.dpi;
    let t = sci(THICK, dpi);
    let mut dib = Dib::new(v.len, t)?;
    dib.data_mut().copy_from_slice(base.data());
    let acc = card::ACCENT;
    if let Some((a, b)) = v.measure {
        let (l, r) = (a.min(b) as f32, a.max(b) as f32);
        let mut pm = dib.pixmap()?;
        gfx::fill(
            &mut pm,
            tiny_skia::Rect::from_xywh(l, 1.0, (r - l).max(1.0), t as f32 - 2.0).map(tiny_skia::PathBuilder::from_rect),
            col(acc[0], acc[1], acc[2], 70),
        );
        for x in [l, r] {
            let x = x.round() + 0.5;
            gfx::line(&mut pm, (x, 0.0), (x, t as f32), col(140, 160, 255, 255), 1.0);
        }
    }
    if let Some(u) = v.cursor_u {
        let x = u.floor() as f32 + 0.5;
        let mut pm = dib.pixmap()?;
        gfx::line(&mut pm, (x, 0.0), (x, t as f32), col(255, 92, 120, 255), 1.0);
        // Small triangle at the tick edge.
        let mut pb = tiny_skia::PathBuilder::new();
        let s = sc(5.0, dpi);
        pb.move_to(x - s, 0.0);
        pb.line_to(x + s, 0.0);
        pb.line_to(x, s * 1.2);
        pb.close();
        gfx::fill(&mut pm, pb.finish(), col(255, 92, 120, 255));
    }
    // Readout, bottom-left.
    let readout = if let Some((a, b)) = v.measure {
        let d = (b - a).abs().round();
        let mut s = format!("\u{2194} {}", measure::format_length(d, v.unit, v.ppi));
        if v.unit != RulerUnit::Px {
            s.push_str(&format!("  ({} px)", d as i64));
        }
        s
    } else if let Some(u) = v.cursor_u {
        format!("\u{25B8} {}", measure::format_length(u.floor(), v.unit, v.ppi))
    } else {
        crate::i18n::t("tools-ruler-hint")
    };
    let rpx = sci(11.5, dpi);
    let ry = t - sci(7.0, dpi) - sci(22.0, dpi) + (sci(22.0, dpi) - text.measure("0", rpx, Face::UiSemibold).1) / 2;
    let chips = ruler_chips(v, text);
    let chips_left = chips.iter().map(|(_, r)| r.x).min().unwrap_or(v.len);
    let (rw, _) = text.measure(&readout, rpx, Face::UiSemibold);
    if sci(10.0, dpi) + rw < chips_left - sci(6.0, dpi) {
        let face = if v.measure.is_some() || v.cursor_u.is_some() { Face::UiSemibold } else { Face::Ui };
        let c = if v.measure.is_some() || v.cursor_u.is_some() { card::TEXT } else { card::TEXT_DIM };
        text.draw(&mut dib, &readout, sci(10.0, dpi), ry, rpx, face, c, Align::Left);
    }
    for (c, r) in chips {
        draw_chip(&mut dib, text, c, r, v.unit, v.hover == Some(c), false, dpi);
    }
    Some(dib)
}

/// Rotate a horizontal-layout DIB 90° clockwise (zero ends up at the top, ticks on the right).
pub(super) fn rotate_cw(src: &Dib) -> Option<Dib> {
    let (w, h) = (src.w, src.h);
    let mut out = Dib::new(h, w)?;
    let s = src.data();
    let d = out.data_mut();
    for y in 0..h {
        for x in 0..w {
            let si = ((y * w + x) * 4) as usize;
            let (ox, oy) = (h - 1 - y, x);
            let di = ((oy * h + ox) * 4) as usize;
            d[di..di + 4].copy_from_slice(&s[si..si + 4]);
        }
    }
    Some(out)
}

pub(super) struct ProtView {
    pub size: i32,
    pub dpi: u32,
    pub opacity: u8,
    pub arms: [f64; 2],
    pub hover: Option<Chip>,
}

/// Protractor frame: disc with degree ticks, two arms, the arc between them and the angle.
pub(super) fn render_protractor(cache: Option<Dib>, text: &mut Text, v: &ProtView) -> Option<Dib> {
    let dpi = v.dpi;
    let s = v.size;
    let mut dib = Dib::reuse(cache, s, s)?;
    dib.clear();
    let c = s as f32 / 2.0;
    let r = c - 1.5;
    let a = (v.opacity as u32 * 255 / 100) as u8;
    {
        let mut pm = dib.pixmap()?;
        gfx::fill(&mut pm, gfx::circle(c, c, r), col(26, 28, 36, a));
        gfx::stroke(&mut pm, gfx::circle(c, c, r - 0.5), col(255, 255, 255, 50), 1.0);
        let mut pbs = [tiny_skia::PathBuilder::new(), tiny_skia::PathBuilder::new(), tiny_skia::PathBuilder::new()];
        for deg in 0..360 {
            let level = if deg % 10 == 0 {
                2
            } else if deg % 5 == 0 {
                1
            } else {
                0
            };
            let len = [sc(5.0, dpi), sc(9.0, dpi), sc(14.0, dpi)][level];
            let (dx, dy) = arm_dir(deg as f64);
            let (dx, dy) = (dx as f32, dy as f32);
            pbs[level].move_to(c + dx * (r - 1.0), c + dy * (r - 1.0));
            pbs[level].line_to(c + dx * (r - len), c + dy * (r - len));
        }
        let alphas = [120u8, 190, 255];
        for (i, b) in pbs.into_iter().enumerate() {
            if let Some(p) = b.finish() {
                let st = tiny_skia::Stroke { width: if i == 2 { 1.3 } else { 1.0 }, ..Default::default() };
                pm.stroke_path(
                    &p,
                    &gfx::paint(col(235, 238, 245, alphas[i])),
                    &st,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
    }
    // Degree labels every 30°.
    let fpx = sci(10.0, dpi);
    for deg in (0..360).step_by(30) {
        let (dx, dy) = arm_dir(deg as f64);
        let rr = r - sc(26.0, dpi);
        let (x, y) = (c + dx as f32 * rr, c + dy as f32 * rr);
        let l = deg.to_string();
        let (_, th) = text.measure(&l, fpx, Face::Ui);
        text.draw(&mut dib, &l, x as i32, y as i32 - th / 2, fpx, Face::Ui, [205, 210, 222, 255], Align::Center);
    }
    // Arc between the arms (counter-clockwise from arm 0 to arm 1, the smaller side).
    let acc = card::ACCENT;
    let (a0, a1) = (v.arms[0], v.arms[1]);
    let sweep = (a1 - a0).rem_euclid(360.0);
    let (start, span) = if sweep <= 180.0 { (a0, sweep) } else { (a1, 360.0 - sweep) };
    {
        let mut pm = dib.pixmap()?;
        let ar = sc(46.0, dpi);
        let steps = (span.ceil() as usize).max(2);
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(c, c);
        for i in 0..=steps {
            let d = start + span * i as f64 / steps as f64;
            let (dx, dy) = arm_dir(d);
            pb.line_to(c + dx as f32 * ar, c + dy as f32 * ar);
        }
        pb.close();
        gfx::fill(&mut pm, pb.finish(), col(acc[0], acc[1], acc[2], 90));
        for (i, &deg) in v.arms.iter().enumerate() {
            let (dx, dy) = arm_dir(deg);
            let end = (c + dx as f32 * (r - sc(2.0, dpi)), c + dy as f32 * (r - sc(2.0, dpi)));
            gfx::line(&mut pm, (c, c), end, col(0, 0, 0, 120), sc(3.5, dpi));
            let lc = if i == 0 { col(255, 255, 255, 240) } else { col(140, 160, 255, 255) };
            gfx::line(&mut pm, (c, c), end, lc, sc(1.6, dpi));
            let (hx, hy) = handle_pos(c, r, deg, dpi);
            gfx::fill(&mut pm, gfx::circle(hx, hy, sc(7.0, dpi)), col(0, 0, 0, 110));
            gfx::fill(&mut pm, gfx::circle(hx, hy, sc(5.5, dpi)), lc);
        }
        gfx::fill(&mut pm, gfx::circle(c, c, sc(4.0, dpi)), col(255, 255, 255, 255));
    }
    // Angle readout below the centre.
    let ang = measure::angle_between((0.0, 0.0), arm_dir(a0), arm_dir(a1));
    let big = format!("{ang:.1}\u{00B0}");
    let ty = (c + sc(14.0, dpi)) as i32;
    let (bw, _) = text.measure(&big, sci(20.0, dpi), Face::UiSemibold);
    if let Some(mut pm) = dib.pixmap() {
        let (pw, ph) = (bw as f32 + sc(22.0, dpi), sc(50.0, dpi));
        gfx::fill(
            &mut pm,
            gfx::rrect(c - pw / 2.0, ty as f32 - sc(2.0, dpi), pw, ph, sc(10.0, dpi)),
            col(18, 20, 26, 205),
        );
    }
    text.draw(&mut dib, &big, c as i32, ty, sci(20.0, dpi), Face::UiSemibold, card::TEXT, Align::Center);
    let small = format!("{:.1}\u{00B0}", 360.0 - ang);
    text.draw(&mut dib, &small, c as i32, ty + sci(28.0, dpi), sci(11.0, dpi), Face::Ui, card::TEXT_DIM, Align::Center);
    for (ch, rr) in protractor_chips(s, dpi) {
        draw_chip(&mut dib, text, ch, rr, RulerUnit::Px, v.hover == Some(ch), true, dpi);
    }
    Some(dib)
}

/// Where an arm's drag handle sits.
fn handle_pos(c: f32, r: f32, deg: f64, dpi: u32) -> (f32, f32) {
    let (dx, dy) = arm_dir(deg);
    let hr = r - sc(46.0, dpi);
    (c + dx as f32 * hr, c + dy as f32 * hr)
}

fn render() {
    with(|r| {
        let dib = match r.mode {
            Mode::Ruler => {
                let v = r.view();
                let key = BaseKey { len: r.len, dpi: r.dpi, unit: r.unit, ppi: r.ppi, opacity: r.opacity };
                if r.base.as_ref().map(|(k, _)| *k) != Some(key) {
                    r.base = render_base(&v, &mut r.text).map(|d| (key, d));
                }
                let Some((_, base)) = &r.base else {
                    return;
                };
                let frame = render_ruler(base, &mut r.text, &v);
                if r.vertical {
                    frame.as_ref().and_then(rotate_cw)
                } else {
                    frame
                }
            }
            Mode::Protractor => {
                let v = ProtView { size: r.prot_size, dpi: r.dpi, opacity: r.opacity, arms: r.arms, hover: r.hover };
                render_protractor(r.dib.take(), &mut r.text, &v)
            }
        };
        if let Some(d) = dib {
            gfx::present(r.hwnd, &d, r.pos);
            r.dib = Some(d);
        }
    });
}

/// Poll the cursor for the marker (the cursor is usually over other windows).
fn poll_cursor() {
    let p = win::cursor_pos();
    let changed = with(|r| {
        if r.mode != Mode::Ruler {
            return false;
        }
        let rect = r.rect();
        let local = Point::new(p.x - rect.x, p.y - rect.y);
        let (u, _) = r.to_uv(local);
        let new = (u >= 0.0 && u < r.len as f64).then_some(u);
        if new != r.cursor_u {
            r.cursor_u = new;
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if changed {
        render();
    }
}

fn set_unit(next: RulerUnit) {
    with(|r| {
        r.unit = next;
    });
    render();
}

fn rotate() {
    with(|r| {
        r.mode = Mode::Ruler;
        // Rotate around the pointer end so the ruler stays where the user looks.
        r.vertical = !r.vertical;
        r.measure = None;
        if let Some(m) = win::monitor_at(r.pos) {
            let (w, h) = r.size();
            let clamped = Rect::new(r.pos.x, r.pos.y, w, h).clamp_inside(&m.work);
            if r.len > if r.vertical { m.work.h } else { m.work.w } {
                r.len = if r.vertical { m.work.h } else { m.work.w }.max(sci(MIN_LEN, r.dpi));
            }
            r.pos = Point::new(clamped.x, clamped.y);
        }
    });
    render();
}

fn toggle_mode() {
    with(|r| {
        let c = r.rect().center();
        r.mode = if r.mode == Mode::Ruler { Mode::Protractor } else { Mode::Ruler };
        let (w, h) = r.size();
        r.pos = Point::new(c.x - w / 2, c.y - h / 2);
        r.dib = None;
    });
    render();
}

fn calibrate() {
    let center = with(|r| r.rect().center()).unwrap_or_else(win::cursor_pos);
    if let Some(m) = win::monitor_at(center) {
        calibrate::open(m);
    }
}

fn context_menu(hwnd: HWND, at: Point) {
    let Some((unit, vertical, mode, auto, opacity)) = with(|r| (r.unit, r.vertical, r.mode, r.auto_copy, r.opacity))
    else {
        return;
    };
    let t = crate::i18n::t;
    let units = RulerUnit::ALL
        .iter()
        .enumerate()
        .map(|(i, u)| MenuItem::check(10 + i as u32, t(&format!("tools-unit-{}", u.suffix())), *u == unit))
        .collect();
    let opac = [40u8, 60, 80, 90, 100]
        .iter()
        .map(|&o| MenuItem::check(100 + o as u32, format!("{o}%"), o == opacity))
        .collect();
    let items = vec![
        MenuItem::Sub { text: t("tools-ruler-units"), items: units },
        MenuItem::check(1, t("tools-ruler-vertical"), vertical && mode == Mode::Ruler),
        MenuItem::check(2, t("tools-ruler-protractor"), mode == Mode::Protractor),
        MenuItem::Sub { text: t("tools-ruler-opacity"), items: opac },
        MenuItem::Separator,
        MenuItem::check(3, t("tools-ruler-autocopy"), auto),
        MenuItem::item(4, t("tools-ruler-copy")),
        MenuItem::item(5, t("tools-ruler-calibrate")),
        MenuItem::Separator,
        MenuItem::item(9, t("tools-close")),
    ];
    match wnd::popup_menu(hwnd, at, &items) {
        id @ 10..=12 => set_unit(RulerUnit::ALL[(id - 10) as usize]),
        1 => rotate(),
        2 => toggle_mode(),
        3 => {
            let v = !auto;
            with(|r| r.auto_copy = v);
            crate::settings::update(|s| s.tools.ruler_auto_copy = v);
        }
        4 => {
            with(|r| r.copy_measurement());
        }
        5 => calibrate(),
        9 => close(),
        id @ 140..=200 => {
            let o = (id - 100) as u8;
            with(|r| r.opacity = o);
            crate::settings::update(|s| s.tools.ruler_opacity = o);
            render();
        }
        _ => {}
    }
}

fn on_chip(c: Chip) {
    match c {
        Chip::Unit => {
            let next = with(|r| r.unit.next());
            if let Some(n) = next {
                set_unit(n);
            }
        }
        Chip::Rotate => rotate(),
        Chip::Mode => toggle_mode(),
        Chip::Close => close(),
    }
}

fn on_down(hwnd: HWND, p: Point) {
    if let Some(c) = with(|r| r.chip_at(p)).flatten() {
        on_chip(c);
        return;
    }
    let cursor = win::cursor_pos();
    with(|r| {
        r.drag = match r.mode {
            Mode::Ruler if r.at_far_end(p) => Drag::Resize { cursor, len: r.len },
            Mode::Ruler if r.in_tick_band(p) => {
                let (u, _) = r.to_uv(p);
                let u = u.floor();
                r.measure = Some((u, u));
                Drag::Measure
            }
            Mode::Ruler => Drag::Move { cursor, pos: r.pos },
            Mode::Protractor => {
                let c = r.prot_size as f32 / 2.0;
                let rad = c - 1.5;
                let hit = (0..2).find(|&i| {
                    let (hx, hy) = handle_pos(c, rad, r.arms[i], r.dpi);
                    measure::distance((hx as f64, hy as f64), (p.x as f64, p.y as f64)) < sc(12.0, r.dpi) as f64
                });
                match hit {
                    Some(i) => Drag::Arm(i),
                    None => Drag::Move { cursor, pos: r.pos },
                }
            }
        };
    });
    // SAFETY: capture the mouse for the drag on our own window.
    unsafe {
        SetCapture(hwnd);
    }
    render();
}

fn on_drag_move(p: Point) {
    let cursor = win::cursor_pos();
    let changed = with(|r| match r.drag {
        Drag::None => {
            let h = r.chip_at(p);
            if h != r.hover {
                r.hover = h;
                return true;
            }
            false
        }
        Drag::Move { cursor: c0, pos } => {
            r.pos = Point::new(pos.x + cursor.x - c0.x, pos.y + cursor.y - c0.y);
            if let Some(d) = &r.dib {
                gfx::present(r.hwnd, d, r.pos);
            }
            false
        }
        Drag::Measure => {
            let (u, _) = r.to_uv(p);
            if let Some((a, _)) = r.measure {
                r.measure = Some((a, u.floor().clamp(0.0, r.len as f64)));
            }
            true
        }
        Drag::Resize { cursor: c0, len } => {
            let d = if r.vertical { cursor.y - c0.y } else { cursor.x - c0.x };
            let max = win::virtual_screen();
            r.len = (len + d).clamp(sci(MIN_LEN, r.dpi), if r.vertical { max.h } else { max.w });
            true
        }
        Drag::Arm(i) => {
            let c = r.prot_size as f64 / 2.0;
            let mut a = measure::direction_deg((c, c), (p.x as f64 + 0.5, p.y as f64 + 0.5));
            if wnd::mods().shift {
                a = (a / 15.0).round() * 15.0 % 360.0;
            }
            r.arms[i] = a;
            true
        }
    })
    .unwrap_or(false);
    if changed {
        render();
    }
}

fn on_up() {
    let copy = with(|r| {
        let d = std::mem::replace(&mut r.drag, Drag::None);
        if let Drag::Move { .. } = d {
            r.refresh_monitor();
        }
        let measured = matches!(d, Drag::Measure) && r.measure.is_some_and(|(a, b)| (a - b).abs() >= 1.0);
        if matches!(d, Drag::Measure) && !measured {
            r.measure = None;
        }
        r.auto_copy && (measured || matches!(d, Drag::Arm(_)))
    })
    .unwrap_or(false);
    // SAFETY: plain call.
    unsafe {
        let _ = ReleaseCapture();
    }
    if copy {
        with(|r| r.copy_measurement());
    }
    render();
}

fn nudge(dx: i32, dy: i32) {
    with(|r| {
        r.pos = Point::new(r.pos.x + dx, r.pos.y + dy);
        r.refresh_monitor();
    });
    render();
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_ID => {
            poll_cursor();
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            on_down(hwnd, wnd::point_from_lparam(lp));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            on_drag_move(wnd::point_from_lparam(lp));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_up();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if with(|r| r.drag != Drag::None).unwrap_or(false) {
                with(|r| r.drag = Drag::None);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Ctrl+wheel resizes the protractor.
            if wnd::mods().ctrl {
                let n = wnd::wheel_notches(wp);
                with(|r| {
                    if r.mode == Mode::Protractor {
                        let c = r.rect().center();
                        r.prot_size = (r.prot_size + n * sci(20.0, r.dpi)).clamp(sci(200.0, r.dpi), sci(900.0, r.dpi));
                        r.pos = Point::new(c.x - r.prot_size / 2, c.y - r.prot_size / 2);
                    }
                });
                render();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let m = wnd::mods();
            let step = if m.shift { 10 } else { 1 };
            match wp.0 as u16 {
                k if k == VK_ESCAPE.0 => close(),
                k if k == VK_LEFT.0 => nudge(-step, 0),
                k if k == VK_RIGHT.0 => nudge(step, 0),
                k if k == VK_UP.0 => nudge(0, -step),
                k if k == VK_DOWN.0 => nudge(0, step),
                k if k == b'C' as u16 && m.ctrl => {
                    with(|r| r.copy_measurement());
                }
                k if k == b'C' as u16 => calibrate(),
                k if k == b'R' as u16 => rotate(),
                k if k == b'U' as u16 => on_chip(Chip::Unit),
                k if k == b'P' as u16 => toggle_mode(),
                _ => {}
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
            let mut pt = windows::Win32::Foundation::POINT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            let p = Point::new(pt.x, pt.y);
            let shape = with(|r| {
                if r.chip_at(p).is_some() {
                    IDC_HAND
                } else if r.mode == Mode::Ruler && r.at_far_end(p) {
                    if r.vertical {
                        IDC_SIZENS
                    } else {
                        IDC_SIZEWE
                    }
                } else if r.mode == Mode::Ruler && r.in_tick_band(p) {
                    IDC_IBEAM
                } else {
                    IDC_SIZEALL
                }
            })
            .unwrap_or(IDC_ARROW);
            if let Ok(c) = LoadCursorW(None, shape) {
                SetCursor(c);
            }
            LRESULT(1)
        }
        WM_DPICHANGED => LRESULT(0),
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
    fn arm_directions() {
        let (x, y) = arm_dir(90.0);
        assert!(x.abs() < 1e-12 && (y + 1.0).abs() < 1e-12, "90 degrees points up (screen y negative)");
        // direction_deg is the inverse of arm_dir.
        for d in [0.0, 30.0, 135.0, 270.0, 359.0] {
            let (dx, dy) = arm_dir(d);
            assert!((measure::direction_deg((0.0, 0.0), (dx, dy)) - d).abs() < 1e-9);
        }
    }
}
