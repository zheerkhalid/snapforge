//! Delay countdown: a small rounded, translucent layered window with a big number and a
//! progress ring, centred on the monitor under the cursor and excluded from capture.

use super::paint::Dib;
use crate::common::win::{hwnd_from_isize, hwnd_to_isize, pcwstr, wide};
use crate::common::Rect;
use std::cell::Cell;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, DrawTextW, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER,
    BLENDFUNCTION, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, IsWindow, KillTimer, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow, HWND_TOPMOST, MSG,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, ULW_ALPHA, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const CLASS: &str = "SnapForge.Countdown";
const TIMER_ID: usize = 0x5F01;
/// Base size at 96 DPI.
const BASE: i32 = 160;

thread_local! {
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wp: windows::Win32::Foundation::WPARAM,
    lp: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn ensure_window() -> Option<HWND> {
    let existing = hwnd_from_isize(HWND_CELL.with(|c| c.get()));
    // SAFETY: IsWindow accepts any value.
    if !existing.is_invalid() && unsafe { IsWindow(existing) }.as_bool() {
        return Some(existing);
    }
    let class = wide(CLASS);
    // SAFETY: standard class registration and window creation with a static wndproc.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            pcwstr(&class),
            pcwstr(&class),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinst,
            None,
        )
    };
    match hwnd {
        Ok(h) => {
            crate::common::win::exclude_from_capture(h);
            HWND_CELL.with(|c| c.set(hwnd_to_isize(h)));
            Some(h)
        }
        Err(e) => {
            log::error!("countdown: CreateWindowEx failed: {e}");
            None
        }
    }
}

fn esc_down() -> bool {
    // SAFETY: plain key state query.
    unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 }
}

/// Countdown placement: centred on `mon`.
pub fn place(mon: &Rect, size: i32) -> Rect {
    Rect::new(mon.x + (mon.w - size) / 2, mon.y + (mon.h - size) / 2, size, size)
}

pub fn countdown(seconds: u32) -> bool {
    if seconds == 0 {
        return true;
    }
    if ACTIVE.with(|a| a.replace(true)) {
        log::warn!("countdown: already running");
        return false;
    }
    let ok = run(seconds);
    ACTIVE.with(|a| a.set(false));
    ok
}

fn run(seconds: u32) -> bool {
    let Some(hwnd) = ensure_window() else {
        // No window: still honour the delay so the capture happens at the right time.
        std::thread::sleep(Duration::from_secs(seconds as u64));
        return true;
    };
    let cursor = crate::common::win::cursor_pos();
    let (mon, dpi) =
        crate::common::win::monitor_at(cursor).map_or((crate::common::win::virtual_screen(), 96), |m| (m.rect, m.dpi));
    let size = crate::common::win::scale(BASE, dpi);
    let r = place(&mon, size);
    let Some(mut painter) = Painter::new(size, dpi) else {
        std::thread::sleep(Duration::from_secs(seconds as u64));
        return true;
    };
    let total = Duration::from_secs(seconds as u64);
    let start = Instant::now();
    painter.present(hwnd, &r, seconds, 0.0);
    // SAFETY: our own window; shown without activation so focus stays where it was.
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.x, r.y, r.w, r.h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetTimer(hwnd, TIMER_ID, 33, None);
    }
    // An Esc still held from before the countdown must be released first.
    let mut esc_armed = !esc_down();
    let mut result = true;
    let mut msg = MSG::default();
    loop {
        // SAFETY: standard message loop.
        let got = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) };
        if got.0 == 0 {
            // SAFETY: hand WM_QUIT back to the outer loop.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            result = false;
            break;
        }
        if got.0 == -1 {
            break;
        }
        if msg.message == WM_TIMER && msg.hwnd == hwnd && msg.wParam.0 == TIMER_ID {
            let down = esc_down();
            if !down {
                esc_armed = true;
            } else if esc_armed {
                result = false;
                break;
            }
            let elapsed = start.elapsed();
            if elapsed >= total {
                break;
            }
            let left = (total - elapsed).as_secs_f32();
            let progress = elapsed.as_secs_f32() / total.as_secs_f32();
            painter.present(hwnd, &r, left.ceil() as u32, progress);
            continue;
        }
        // SAFETY: msg filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: our own window.
    unsafe {
        let _ = KillTimer(hwnd, TIMER_ID);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    result
}

/// Renders frames with tiny-skia; the digits come from GDI as a coverage mask.
pub struct Painter {
    size: i32,
    dpi: u32,
    dib: Dib,
    text: Dib,
    text_for: Option<u32>,
}

impl Painter {
    pub fn new(size: i32, dpi: u32) -> Option<Self> {
        Some(Self { size, dpi, dib: Dib::new(size, size)?, text: Dib::new(size, size)?, text_for: None })
    }

    fn s(&self, v: f32) -> f32 {
        v * self.dpi as f32 / 96.0
    }

    /// GDI-rendered white-on-black text (number + hint) used as an alpha mask.
    fn text_mask(&mut self, number: u32) {
        if self.text_for == Some(number) {
            return;
        }
        self.text.data_mut().fill(0);
        let face = wide("Segoe UI Semibold");
        let face_small = wide("Segoe UI");
        let big = -(self.size as f32 * 0.30) as i32;
        let small = -(self.s(11.5)) as i32;
        let size = self.size;
        let hint = crate::i18n::t("countdown-cancel");
        let ring_cy = (size as f32 * 0.44) as i32;
        // SAFETY: fonts are created, selected into our own DIB DC, then deleted after use.
        unsafe {
            let dc = self.text.dc;
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            // ANTIALIASED_QUALITY (4): grayscale AA, so the green channel is a clean mask.
            let f_big = CreateFontW(big, 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 4, 0, pcwstr(&face));
            let f_small = CreateFontW(small, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 4, 0, pcwstr(&face_small));
            let old = SelectObject(dc, f_big);
            let mut num: Vec<u16> = number.to_string().encode_utf16().collect();
            let half = (size as f32 * 0.25) as i32;
            let mut rc = RECT { left: 0, top: ring_cy - half, right: size, bottom: ring_cy + half };
            DrawTextW(dc, &mut num, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, f_small);
            let mut h: Vec<u16> = hint.encode_utf16().collect();
            let mut rc =
                RECT { left: 0, top: (size as f32 * 0.80) as i32, right: size, bottom: (size as f32 * 0.95) as i32 };
            DrawTextW(dc, &mut h, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            SelectObject(dc, old);
            let _ = DeleteObject(f_big);
            let _ = DeleteObject(f_small);
        }
        self.text_for = Some(number);
    }

    /// Render one frame (premultiplied RGBA) for `number` seconds left and `progress` 0..1.
    pub fn frame(&mut self, number: u32, progress: f32) -> Option<Pixmap> {
        let n = self.size as f32;
        let mut pm = Pixmap::new(self.size as u32, self.size as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        // Background card.
        paint.set_color(Color::from_rgba8(22, 24, 31, 222));
        let bg = rounded_rect(0.5, 0.5, n - 1.0, n - 1.0, n * 0.14)?;
        pm.fill_path(&bg, &paint, FillRule::Winding, Transform::identity(), None);
        paint.set_color(Color::from_rgba8(255, 255, 255, 28));
        let stroke = Stroke { width: self.s(1.0), ..Default::default() };
        pm.stroke_path(&bg, &paint, &stroke, Transform::identity(), None);
        // Ring track and remaining-time arc.
        let (cx, cy, r) = (n / 2.0, n * 0.44, n * 0.30);
        let ring = Stroke { width: n * 0.045, line_cap: LineCap::Round, ..Default::default() };
        paint.set_color(Color::from_rgba8(255, 255, 255, 36));
        if let Some(track) = PathBuilder::from_circle(cx, cy, r) {
            pm.stroke_path(&track, &paint, &ring, Transform::identity(), None);
        }
        let remaining = (1.0 - progress).clamp(0.0, 1.0);
        if remaining > 0.002 {
            paint.set_color(Color::from_rgba8(0x4F, 0x6B, 0xFF, 255));
            if let Some(arc) = arc(cx, cy, r, remaining) {
                pm.stroke_path(&arc, &paint, &ring, Transform::identity(), None);
            }
        }
        // White text through the GDI coverage mask.
        self.text_mask(number);
        let mask = self.text.data();
        for (px, m) in pm.data_mut().chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            let cov = m[1] as u32;
            if cov == 0 {
                continue;
            }
            for c in px.iter_mut() {
                *c = (cov + (*c as u32 * (255 - cov) + 127) / 255).min(255) as u8;
            }
        }
        Some(pm)
    }

    fn present(&mut self, hwnd: HWND, r: &Rect, number: u32, progress: f32) {
        let Some(pm) = self.frame(number, progress) else {
            return;
        };
        // RGBA → BGRA (both premultiplied).
        for (d, s) in self.dib.data_mut().chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        let dst = POINT { x: r.x, y: r.y };
        let sz = SIZE { cx: r.w, cy: r.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: valid pointers to locals; the DIB DC holds a premultiplied 32-bpp bitmap.
        unsafe {
            if let Err(e) = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&dst),
                Some(&sz),
                self.dib.dc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            ) {
                log::warn!("countdown: UpdateLayeredWindow failed: {e}");
            }
        }
    }
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Clockwise arc from 12 o'clock covering `frac` of the circle.
fn arc(cx: f32, cy: f32, r: f32, frac: f32) -> Option<tiny_skia::Path> {
    let steps = ((frac * 120.0).ceil() as usize).max(2);
    let mut pb = PathBuilder::new();
    for i in 0..=steps {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * i as f32 / steps as f32;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_is_centred() {
        assert_eq!(place(&Rect::new(-1920, 0, 1920, 1080), 160), Rect::new(-1040, 460, 160, 160));
    }

    #[test]
    fn frame_has_translucent_rounded_corners_and_opaque_text() {
        let mut p = Painter::new(160, 96).expect("dib");
        let pm = p.frame(3, 0.25).expect("pixmap");
        let px = |x: u32, y: u32| pm.pixel(x, y).map(|c| (c.red(), c.alpha())).unwrap_or((0, 0));
        assert_eq!(px(0, 0).1, 0, "rounded corner is transparent");
        let (_, a) = px(80, 150);
        assert!(a > 150 && a < 255, "card is translucent: {a}");
        // Some pixel of the number is solid white.
        assert!(pm.pixels().iter().any(|c| c.alpha() == 255 && c.red() == 255 && c.green() == 255));
    }
}
