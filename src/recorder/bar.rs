//! Recorder UI thread: the floating control bar, the region frame, the F5 zoom hotkey,
//! and the low-level mouse hook for click rings and click sounds.
//!
//! Everything here runs on its own thread with its own message loop, so the app's UI
//! thread is never involved while recording. Both windows are layered, topmost, never
//! activated, and excluded from capture (`WDA_EXCLUDEFROMCAPTURE`).

use super::video::{Click, Live};
use crate::common::win::{self, pcwstr, wide};
use crate::common::Rect;
use crate::settings::recorder::{parse_color, ControlBarSettings};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_F5};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_REGION_MOVE: u32 = WM_APP + 0x51;
const WM_CLICK_SOUND: u32 = WM_APP + 0x52;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_REDRAW: usize = 1;
const HOTKEY_ZOOM: i32 = 0x5F10;
const ID_SIZE: u32 = 1;
const ID_PAUSE: u32 = 2;
const ID_STOP: u32 = 3;
const ID_ZOOM: u32 = 4;
/// Region frame band: an invisible grab margin outside the visible 2 px line.
const FRAME_BAND: i32 = 8;

pub struct BarConfig {
    pub colors: ControlBarSettings,
    /// Show a draggable frame around the region (region targets).
    pub frame: Option<Rect>,
    /// Monitor the region lives on (frame drag is kept inside it).
    pub monitor: Rect,
    pub click_sounds: bool,
    pub click_volume: u32,
}

pub struct BarHandle {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl BarHandle {
    pub fn close(mut self) {
        // SAFETY: posting WM_QUIT to the bar thread's queue.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Palette {
    bg: [u8; 4],
    fg: [u8; 4],
    accent: [u8; 4],
}

struct BarState {
    hwnd: HWND,
    frame: HWND,
    live: Arc<Live>,
    cfg: BarConfig,
    pal: Palette,
    large: bool,
    hover: u32,
    wav: Vec<u8>,
    blink: u32,
}

thread_local! {
    static STATE: RefCell<Option<BarState>> = const { RefCell::new(None) };
}

/// Mouse-hook context (the hook procedure has no user pointer).
static HOOK: Mutex<Option<(Arc<Live>, isize, bool)>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut BarState) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn spawn(cfg: BarConfig, live: Arc<Live>) -> Option<BarHandle> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let join = std::thread::Builder::new()
        .name("rec-ui".into())
        .spawn(move || {
            // SAFETY: plain query.
            let tid = unsafe { GetCurrentThreadId() };
            let _ = tx.send(tid);
            run(cfg, live);
        })
        .ok()?;
    let thread_id = rx.recv().ok()?;
    Some(BarHandle { thread_id, join: Some(join) })
}

fn class(name: &str, proc_: WNDPROC) -> Vec<u16> {
    let cls = wide(name);
    // SAFETY: class registration with a static wndproc (re-registering fails harmlessly).
    unsafe {
        if let Ok(hinst) = GetModuleHandleW(None) {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc_,
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: pcwstr(&cls),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
    }
    cls
}

fn create_layered(cls: &[u16]) -> HWND {
    // SAFETY: standard window creation for a registered class.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            pcwstr(cls),
            pcwstr(cls),
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
        .unwrap_or_default()
    }
}

fn run(cfg: BarConfig, live: Arc<Live>) {
    let bar_cls = class("SnapForge.RecorderBar", Some(bar_proc));
    let frame_cls = class("SnapForge.RecorderFrame", Some(frame_proc));
    let hwnd = create_layered(&bar_cls);
    if hwnd.is_invalid() {
        log::error!("recorder: could not create the control bar");
        return;
    }
    win::exclude_from_capture(hwnd);
    let frame = if cfg.frame.is_some() { create_layered(&frame_cls) } else { HWND::default() };
    if !frame.is_invalid() {
        win::exclude_from_capture(frame);
        live.frame_hwnd.store(win::hwnd_to_isize(frame), std::sync::atomic::Ordering::Release);
    }
    let pal = Palette {
        bg: parse_color(&cfg.colors.background).unwrap_or([31, 31, 38, 230]),
        fg: parse_color(&cfg.colors.foreground).unwrap_or([242, 242, 245, 255]),
        accent: parse_color(&cfg.colors.accent).unwrap_or([229, 72, 77, 255]),
    };
    let wav = click_wav(cfg.click_volume);
    let large = cfg.colors.large;
    // Click rings and sounds: low-level mouse hook on this thread.
    if let Ok(mut h) = HOOK.lock() {
        *h = Some((live.clone(), win::hwnd_to_isize(hwnd), cfg.click_sounds));
    }
    // SAFETY: hook procedure is a static function; unhooked before the thread exits.
    let hook =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), GetModuleHandleW(None).unwrap_or_default(), 0).ok() };
    // SAFETY: plain registration; failure only disables the F5 zoom.
    let zoom_ok = unsafe { RegisterHotKey(hwnd, HOTKEY_ZOOM, MOD_NOREPEAT, VK_F5.0 as u32).is_ok() };
    if !zoom_ok {
        log::warn!("recorder: F5 is in use by another program; live zoom is disabled");
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(BarState { hwnd, frame, live, cfg, pal, large, hover: 0, wav, blink: 0 });
    });
    place_bar();
    paint_bar();
    paint_frame();
    // SAFETY: timer on our own window; message loop until WM_QUIT.
    unsafe {
        let _ = SetTimer(hwnd, TIMER_REDRAW, 100, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if !frame.is_invalid() {
            let _ = ShowWindow(frame, SW_SHOWNOACTIVATE);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
        if let Ok(mut h) = HOOK.lock() {
            *h = None;
        }
        let _ = UnregisterHotKey(hwnd, HOTKEY_ZOOM);
        let _ = PlaySoundW(windows::core::PCWSTR::null(), None, SND_NODEFAULT);
        let _ = DestroyWindow(hwnd);
        if !frame.is_invalid() {
            let _ = DestroyWindow(frame);
        }
    }
    STATE.with(|s| *s.borrow_mut() = None);
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && (wp.0 as u32 == WM_LBUTTONDOWN || wp.0 as u32 == WM_RBUTTONDOWN) {
        // SAFETY: for WH_MOUSE_LL, lParam points to an MSLLHOOKSTRUCT.
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if let Ok(h) = HOOK.try_lock() {
            if let Some((live, bar, sounds)) = h.as_ref() {
                live.push_click(Click {
                    pos: crate::common::Point::new(info.pt.x, info.pt.y),
                    at: super::clock::qpc_hns(),
                    right: wp.0 as u32 == WM_RBUTTONDOWN,
                });
                if *sounds {
                    let _ = PostMessageW(win::hwnd_from_isize(*bar), WM_CLICK_SOUND, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

// ------------------------------------------------------------------ layout

fn dpi_scale(hwnd: HWND) -> f32 {
    win::dpi_for_window(hwnd) as f32 / 96.0
}

struct Layout {
    w: i32,
    h: i32,
    dot: (f32, f32, f32),
    text: RECT,
    meter: (f32, f32, f32, f32),
    pause: RECT,
    stop: RECT,
}

fn layout(large: bool, s: f32) -> Layout {
    let k = if large { 1.45 } else { 1.0 } * s;
    let px = |v: f32| (v * k).round() as i32;
    let h = px(36.0);
    let b = px(24.0);
    let y0 = (h - b) / 2;
    Layout {
        w: px(232.0),
        h,
        dot: (12.0 * k + 6.0 * k, h as f32 / 2.0, 6.0 * k),
        text: RECT { left: px(30.0), top: 0, right: px(98.0), bottom: h },
        meter: (102.0 * k, h as f32 / 2.0 - 3.0 * k, 60.0 * k, 6.0 * k),
        pause: RECT { left: px(170.0), top: y0, right: px(170.0) + b, bottom: y0 + b },
        stop: RECT { left: px(200.0), top: y0, right: px(200.0) + b, bottom: y0 + b },
    }
}

fn place_bar() {
    with_state(|st| {
        let s = dpi_scale(st.hwnd);
        let l = layout(st.large, s);
        let m = st.cfg.monitor;
        let (x, y) = match (st.cfg.colors.x, st.cfg.colors.y) {
            (Some(x), Some(y))
                if win::monitors().iter().any(|mi| mi.rect.contains(crate::common::Point::new(x + 4, y + 4))) =>
            {
                (x, y)
            }
            _ => {
                // Above the region if there is room, else top centre of the monitor.
                match st.cfg.frame {
                    Some(r) if r.y - l.h - FRAME_BAND - 8 >= m.y => (r.x + (r.w - l.w) / 2, r.y - l.h - FRAME_BAND - 8),
                    _ => (m.x + (m.w - l.w) / 2, m.y + (12.0 * s) as i32),
                }
            }
        };
        // SAFETY: positioning our own window.
        unsafe {
            let _ = SetWindowPos(st.hwnd, HWND_TOPMOST, x, y, l.w, l.h, SWP_NOACTIVATE);
        }
    });
}

fn hit(st: &BarState, x: i32, y: i32) -> u32 {
    let l = layout(st.large, dpi_scale(st.hwnd));
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&l.pause) {
        ID_PAUSE
    } else if inside(&l.stop) {
        ID_STOP
    } else {
        0
    }
}

// ------------------------------------------------------------------ painting

fn sk(c: [u8; 4], a: f32) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], (c[3] as f32 * a).round().clamp(0.0, 255.0) as u8)
}

fn fill(pm: &mut Pixmap, path: Option<tiny_skia::Path>, c: Color) {
    if let Some(p) = path {
        let mut paint = Paint::default();
        paint.set_color(c);
        paint.anti_alias = true;
        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
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

fn format_elapsed(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A 32-bpp top-down DIB section in its own memory DC.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Dib> {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: DC and section are released in Drop.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            if bits.is_null() {
                let _ = DeleteObject(bmp);
                let _ = DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bmp);
            Some(Dib { dc, bmp, old, bits: bits as *mut u8, w, h })
        }
    }

    fn data(&mut self) -> &mut [u8] {
        // SAFETY: the section holds w*h*4 bytes while the DIB lives.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize) }
    }

    fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: valid pointers to locals; the DC holds a premultiplied 32-bpp bitmap.
        unsafe {
            let _ = GdiFlush();
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT { x, y }),
                Some(&SIZE { cx: self.w, cy: self.h }),
                self.dc,
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: restores and frees the objects created in `new`.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Draw `text` with GDI into a mask and blend it in `color` onto premultiplied BGRA.
fn draw_text(dst: &mut [u8], w: i32, h: i32, r: RECT, text: &str, px: i32, color: [u8; 4]) {
    let Some(mut mask) = Dib::new(w, h) else { return };
    mask.data().fill(0);
    let face = wide("Segoe UI Semibold");
    let mut t: Vec<u16> = text.encode_utf16().collect();
    let mut rc = r;
    // SAFETY: GDI drawing into our memory DC; the font is released after use.
    unsafe {
        let font = CreateFontW(
            -px,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            ANTIALIASED_QUALITY.0 as u32,
            0,
            pcwstr(&face),
        );
        let old = SelectObject(mask.dc, font);
        SetBkMode(mask.dc, TRANSPARENT);
        SetTextColor(mask.dc, COLORREF(0x00FF_FFFF));
        DrawTextW(mask.dc, &mut t, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(mask.dc, old);
        let _ = DeleteObject(font);
        let _ = GdiFlush();
    }
    let m = mask.data();
    let a = color[3] as u32;
    for (d, s) in dst.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
        let cov = s[1] as u32 * a / 255;
        if cov == 0 {
            continue;
        }
        let src = [color[2], color[1], color[0]];
        for c in 0..3 {
            d[c] = ((src[c] as u32 * cov + d[c] as u32 * (255 - cov)) / 255) as u8;
        }
        d[3] = (cov + d[3] as u32 * (255 - cov) / 255).min(255) as u8;
    }
}

fn paint_bar() {
    let Some((hwnd, large, pal_bg, pal_fg, pal_accent, hover, blink)) =
        with_state(|st| (st.hwnd, st.large, st.pal.bg, st.pal.fg, st.pal.accent, st.hover, st.blink))
    else {
        return;
    };
    let s = dpi_scale(hwnd);
    let l = layout(large, s);
    let k = if large { 1.45 } else { 1.0 } * s;
    let status = super::status();
    let Some(mut pm) = Pixmap::new(l.w as u32, l.h as u32) else { return };
    let (w, h) = (l.w as f32, l.h as f32);
    fill(&mut pm, round_rect(0.5, 0.5, w - 1.0, h - 1.0, h / 2.0), sk(pal_bg, 1.0));
    // Record dot: solid while recording, blinking while paused.
    let dot_alpha = if status.paused {
        if blink % 10 < 5 {
            1.0
        } else {
            0.25
        }
    } else {
        1.0
    };
    fill(&mut pm, PathBuilder::from_circle(l.dot.0, l.dot.1, l.dot.2), sk(pal_accent, dot_alpha));
    // Level meter.
    let (mx, my, mw, mh) = l.meter;
    fill(&mut pm, round_rect(mx, my, mw, mh, mh / 2.0), sk(pal_fg, 0.18));
    let pos = super::dsp::meter_position(status.level.0);
    if pos > 0.0 {
        let c = if pos > 0.92 {
            [229, 72, 77, 255]
        } else if pos > 0.75 {
            [245, 165, 36, 255]
        } else {
            [70, 200, 120, 255]
        };
        fill(&mut pm, round_rect(mx, my, (mw * pos).max(mh), mh, mh / 2.0), sk(c, 1.0));
    }
    // Buttons.
    for (id, r) in [(ID_PAUSE, l.pause), (ID_STOP, l.stop)] {
        let (x, y, bw, bh) = (r.left as f32, r.top as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32);
        if hover == id {
            fill(&mut pm, round_rect(x, y, bw, bh, 5.0 * k), sk(pal_fg, 0.16));
        }
        let (cx, cy) = (x + bw / 2.0, y + bh / 2.0);
        if id == ID_STOP {
            let q = 5.0 * k;
            fill(&mut pm, round_rect(cx - q, cy - q, 2.0 * q, 2.0 * q, 1.5 * k), sk(pal_fg, 1.0));
        } else if status.paused {
            // Play triangle = resume.
            let mut pb = PathBuilder::new();
            pb.move_to(cx - 4.0 * k, cy - 6.0 * k);
            pb.line_to(cx + 6.0 * k, cy);
            pb.line_to(cx - 4.0 * k, cy + 6.0 * k);
            pb.close();
            fill(&mut pm, pb.finish(), sk(pal_fg, 1.0));
        } else {
            for dx in [-4.0, 1.5] {
                fill(&mut pm, round_rect(cx + dx * k, cy - 6.0 * k, 2.5 * k, 12.0 * k, 1.0 * k), sk(pal_fg, 1.0));
            }
        }
    }
    let Some(mut dib) = Dib::new(l.w, l.h) else { return };
    {
        let d = dib.data();
        for (o, s) in d.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
            o.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
    }
    let text = format_elapsed(status.elapsed_secs);
    let (dw, dh) = (dib.w, dib.h);
    draw_text(dib.data(), dw, dh, l.text, &text, (13.0 * k).round() as i32, pal_fg);
    let mut wr = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetWindowRect(hwnd, &mut wr);
    }
    dib.present(hwnd, wr.left, wr.top);
}

fn paint_frame() {
    let Some((frame, accent, region)) =
        with_state(|st| (st.frame, st.pal.accent, st.live.region.lock().map(|r| *r).unwrap_or_default()))
    else {
        return;
    };
    if frame.is_invalid() {
        return;
    }
    let b = FRAME_BAND;
    let (w, h) = (region.w + 2 * b, region.h + 2 * b);
    let Some(mut dib) = Dib::new(w, h) else { return };
    let d = dib.data();
    d.fill(0);
    let line = 2;
    let premul = |c: [u8; 4]| {
        let a = c[3] as u32;
        [(c[2] as u32 * a / 255) as u8, (c[1] as u32 * a / 255) as u8, (c[0] as u32 * a / 255) as u8, c[3]]
    };
    let solid = premul([accent[0], accent[1], accent[2], 255]);
    let white = premul([255, 255, 255, 255]);
    for y in 0..h {
        for x in 0..w {
            let in_inner = x >= b && x < w - b && y >= b && y < h - b;
            if in_inner {
                continue; // fully transparent: clicks go through to the recorded app
            }
            let near = x >= b - line && x < w - b + line && y >= b - line && y < h - b + line;
            let px = if near {
                // Dashed accent/white line, 8 px segments.
                let t = if y < b || y >= h - b { x } else { y };
                if (t / 8) % 2 == 0 {
                    solid
                } else {
                    white
                }
            } else {
                [0, 0, 0, 1] // invisible but hit-testable grab band
            };
            let i = ((y * w + x) * 4) as usize;
            d[i..i + 4].copy_from_slice(&px);
        }
    }
    dib.present(frame, region.x - b, region.y - b);
}

// ------------------------------------------------------------------ window procedures

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut p);
            let id = with_state(|st| hit(st, p.x, p.y)).unwrap_or(0);
            LRESULT(if id != 0 { HTCLIENT as isize } else { HTCAPTION as isize })
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
            let changed = with_state(|st| {
                let h = hit(st, x, y);
                let c = h != st.hover;
                st.hover = h;
                c
            });
            if changed == Some(true) {
                let mut tme = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT>() as u32,
                    dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut tme);
                paint_bar();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_state(|st| st.hover = 0);
            paint_bar();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
            match with_state(|st| hit(st, x, y)).unwrap_or(0) {
                ID_PAUSE => {
                    super::toggle_pause();
                    paint_bar();
                }
                ID_STOP => super::stop(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP | WM_NCRBUTTONUP => {
            context_menu(hwnd);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_EXITSIZEMOVE => {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            crate::settings::update(|s| {
                s.recorder.control_bar.x = Some(r.left);
                s.recorder.control_bar.y = Some(r.top);
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_REDRAW => {
            with_state(|st| st.blink = st.blink.wrapping_add(1));
            paint_bar();
            LRESULT(0)
        }
        WM_HOTKEY if wp.0 as i32 == HOTKEY_ZOOM => {
            with_state(|st| {
                let z = !st.live.zoom_on.load(std::sync::atomic::Ordering::Relaxed);
                st.live.zoom_on.store(z, std::sync::atomic::Ordering::Relaxed);
            });
            LRESULT(0)
        }
        WM_CLICK_SOUND => {
            with_state(|st| {
                // SAFETY: the WAV buffer lives in the state until the thread ends, and
                // playback is stopped (PlaySound(NULL)) before that.
                let _ = PlaySoundW(
                    windows::core::PCWSTR(st.wav.as_ptr() as *const u16),
                    None,
                    SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
                );
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            place_bar();
            paint_bar();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn context_menu(hwnd: HWND) {
    let Some(large) = with_state(|st| st.large) else { return };
    let paused = super::status().paused;
    // SAFETY: menu created, tracked and destroyed here; owner is our window.
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let add = |id: u32, key: &str, checked: bool| {
            let t = wide(&crate::i18n::t(key));
            let f = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(m, f, id as usize, pcwstr(&t));
        };
        add(ID_PAUSE, if paused { "rec-resume" } else { "rec-pause" }, false);
        add(ID_STOP, "rec-stop", false);
        add(ID_ZOOM, "rec-zoom", false);
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        add(ID_SIZE, "rec-large-bar", large);
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, p.x, p.y, hwnd, None);
        let _ = DestroyMenu(m);
        match cmd.0 as u32 {
            ID_PAUSE => super::toggle_pause(),
            ID_STOP => super::stop(),
            ID_ZOOM => {
                with_state(|st| {
                    let z = !st.live.zoom_on.load(std::sync::atomic::Ordering::Relaxed);
                    st.live.zoom_on.store(z, std::sync::atomic::Ordering::Relaxed);
                });
            }
            ID_SIZE => {
                with_state(|st| st.large = !st.large);
                crate::settings::update(|s| s.recorder.control_bar.large = !large);
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let l = layout(!large, dpi_scale(hwnd));
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.left, r.top, l.w, l.h, SWP_NOACTIVATE);
            }
            _ => {}
        }
    }
    paint_bar();
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTCAPTION as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOVING => {
            // Keep the region inside its monitor while dragging.
            // SAFETY: for WM_MOVING, lParam points to the proposed window RECT.
            let r = &mut *(lp.0 as *mut RECT);
            if let Some(m) = with_state(|st| st.cfg.monitor) {
                let b = FRAME_BAND;
                let inner = Rect::new(r.left + b, r.top + b, r.right - r.left - 2 * b, r.bottom - r.top - 2 * b);
                let c = inner.clamp_inside(&m);
                let (dx, dy) = (c.x - inner.x, c.y - inner.y);
                r.left += dx;
                r.right += dx;
                r.top += dy;
                r.bottom += dy;
            }
            LRESULT(1)
        }
        WM_MOVE => {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            with_state(|st| {
                if let Ok(mut g) = st.live.region.lock() {
                    g.x = r.left + FRAME_BAND;
                    g.y = r.top + FRAME_BAND;
                }
            });
            LRESULT(0)
        }
        WM_REGION_MOVE => {
            let v = wp.0 as u64;
            let (x, y) = ((v >> 32) as u32 as i32, v as u32 as i32);
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                x - FRAME_BAND,
                y - FRAME_BAND,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------------ click sound

/// A short click (noise burst plus a decaying 1.8 kHz tick) as an in-memory WAV.
pub fn click_wav(volume: u32) -> Vec<u8> {
    let rate = 44_100u32;
    let n = (rate as f32 * 0.035) as usize;
    let vol = volume.min(100) as f32 / 100.0;
    let mut rng = crate::common::rng::Rng::new(0x5EED_C11C);
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let noise = (rng.next_u64() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0;
        let v = 0.55 * noise * (-t / 0.003).exp()
            + 0.45 * (2.0 * std::f32::consts::PI * 1800.0 * t).sin() * (-t / 0.008).exp();
        let s = (v * vol * 0.9 * 32767.0).clamp(-32768.0, 32767.0) as i16;
        pcm.extend_from_slice(&s.to_le_bytes());
    }
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(&pcm);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_and_volume() {
        let w = click_wav(50);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        let len = u32::from_le_bytes([w[40], w[41], w[42], w[43]]) as usize;
        assert_eq!(len + 44, w.len());
        let silent = click_wav(0);
        assert!(silent[44..].iter().all(|&b| b == 0));
        assert_eq!(format_elapsed(3725), "01:02:05");
    }
}
