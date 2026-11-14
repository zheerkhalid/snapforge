//! Win32 implementation of [`ScrollDriver`]: wheel input via `SendInput`, page scrolling
//! via `WM_VSCROLL`/`WM_HSCROLL`, stop detection via `GetAsyncKeyState`, and the small
//! "press Esc or click to stop" indicator window.

use super::{ScrollDriver, ScrollTarget};
use crate::capture::stitch::Axis;
use crate::capture::{CaptureError, GrabOptions};
use crate::common::win::{
    cursor_pos, exclude_from_capture, hwnd_from_isize, monitor_at, pcwstr, round_corners, scale, wide,
};
use crate::common::{Image, Point, Rect};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CALCRECT,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, FW_SEMIBOLD, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, IsChild, LoadCursorW,
    PeekMessageW, PostMessageW, RegisterClassW, SetCursorPos, ShowWindow, TranslateMessage, WindowFromPoint, IDC_HAND,
    MA_NOACTIVATE, MSG, PM_REMOVE, SB_ENDSCROLL, SB_PAGEDOWN, SB_PAGEUP, SW_SHOWNOACTIVATE, WHEEL_DELTA, WM_HSCROLL,
    WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WM_RBUTTONDOWN, WM_VSCROLL, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const INDICATOR_CLASS: &str = "SnapForge.ScrollIndicator";

/// Set by the indicator window when it is clicked.
static INDICATOR_CLICKED: AtomicBool = AtomicBool::new(false);

struct IndicatorState {
    font: HFONT,
    text: Vec<u16>,
}

thread_local! {
    static INDICATOR: RefCell<Option<IndicatorState>> = const { RefCell::new(None) };
}

pub(super) struct Win32Driver {
    rect: Rect,
    hwnd: Option<isize>,
    opts: GrabOptions,
    indicator: Option<HWND>,
    saved_cursor: Point,
    dpi: u32,
    /// Stop inputs only count after they were all seen released once (the click that
    /// started the capture may still be held).
    armed: bool,
    stopped: bool,
    start: Instant,
}

impl Win32Driver {
    pub(super) fn new(target: &ScrollTarget, opts: GrabOptions) -> Self {
        INDICATOR_CLICKED.store(false, Ordering::Release);
        let dpi = monitor_at(target.rect.center()).map_or(96, |m| m.dpi);
        let mut d = Self {
            rect: target.rect,
            hwnd: target.hwnd.filter(|&h| h != 0),
            opts,
            indicator: None,
            saved_cursor: cursor_pos(),
            dpi,
            armed: false,
            stopped: false,
            start: Instant::now(),
        };
        match create_indicator(&target.rect, dpi) {
            Ok(h) => d.indicator = Some(h),
            Err(e) => log::warn!("scroll: indicator window failed: {e}"),
        }
        d
    }

    /// Columns (rows for horizontal) at each edge ignored by the matcher: covers the
    /// scrollbar, whose thumb moves every step and where the cursor rests.
    pub(super) fn ignore_margin(&self) -> u32 {
        let short = self.rect.w.min(self.rect.h).max(0) as u32;
        (scale(20, self.dpi).max(0) as u32).min(short / 8)
    }

    /// Where the cursor rests while scrolling: on the scrollbar side of the target (inside
    /// the ignored margin), so hover effects in the content don't change between frames.
    fn anchor(&self, axis: Axis) -> Point {
        let inset = (self.ignore_margin() as i32 / 2).max(1);
        let c = self.rect.center();
        match axis {
            Axis::Vertical => Point::new(self.rect.right() - inset, c.y),
            Axis::Horizontal => Point::new(c.x, self.rect.bottom() - inset),
        }
    }

    fn pump(&self) {
        let mut msg = MSG::default();
        // SAFETY: standard message pump for windows owned by this thread.
        unsafe {
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// The window that should get WM_VSCROLL: the child under the anchor if it belongs
    /// to the target, else the target itself.
    fn scroll_window(&self, axis: Axis) -> Option<HWND> {
        let target = hwnd_from_isize(self.hwnd?);
        let a = self.anchor(axis);
        // SAFETY: plain queries on window handles.
        unsafe {
            let under = WindowFromPoint(POINT { x: a.x, y: a.y });
            if !under.is_invalid() && (under == target || IsChild(target, under).as_bool()) {
                Some(under)
            } else {
                Some(target)
            }
        }
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: plain query.
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

impl ScrollDriver for Win32Driver {
    fn grab(&mut self) -> Result<Image, CaptureError> {
        self.pump();
        super::super::grab_rect(self.rect, &self.opts)
    }

    fn wheel(&mut self, axis: Axis, notches: i32) {
        if notches == 0 {
            return;
        }
        let a = self.anchor(axis);
        // Wheel: positive = up; HWHEEL: positive = right. `notches` > 0 means down/right.
        let (flags, delta) = match axis {
            Axis::Vertical => (MOUSEEVENTF_WHEEL, -(WHEEL_DELTA as i32) * notches.signum()),
            Axis::Horizontal => (MOUSEEVENTF_HWHEEL, WHEEL_DELTA as i32 * notches.signum()),
        };
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: delta as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        };
        let inputs = vec![input; notches.unsigned_abs() as usize];
        // SAFETY: valid INPUT array and size.
        unsafe {
            if let Err(e) = SetCursorPos(a.x, a.y) {
                log::debug!("scroll: SetCursorPos failed: {e}");
            }
            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent as usize != inputs.len() {
                log::warn!("scroll: SendInput sent {sent} of {} wheel events", inputs.len());
            }
        }
    }

    fn can_page(&self) -> bool {
        self.hwnd.is_some()
    }

    fn page(&mut self, axis: Axis, forward: bool) {
        let Some(w) = self.scroll_window(axis) else { return };
        let msg = match axis {
            Axis::Vertical => WM_VSCROLL,
            Axis::Horizontal => WM_HSCROLL,
        };
        // SB_PAGEDOWN == SB_PAGERIGHT, SB_PAGEUP == SB_PAGELEFT.
        let cmd = if forward { SB_PAGEDOWN } else { SB_PAGEUP };
        // SAFETY: posting standard scroll messages to a window.
        unsafe {
            let _ = PostMessageW(w, msg, WPARAM(cmd.0 as usize), LPARAM(0));
            let _ = PostMessageW(w, msg, WPARAM(SB_ENDSCROLL.0 as usize), LPARAM(0));
        }
    }

    fn cancelled(&mut self) -> bool {
        self.pump();
        if self.stopped {
            return true;
        }
        if INDICATOR_CLICKED.load(Ordering::Acquire) {
            self.stopped = true;
            return true;
        }
        let pressed =
            key_down(VK_ESCAPE.0) || key_down(VK_LBUTTON.0) || key_down(VK_RBUTTON.0) || key_down(VK_MBUTTON.0);
        if !self.armed {
            self.armed = !pressed;
            return false;
        }
        if pressed {
            log::info!("scroll: stopped by the user");
            self.stopped = true;
        }
        self.stopped
    }

    fn sleep_ms(&mut self, ms: u32) {
        let deadline = Instant::now() + Duration::from_millis(ms as u64);
        loop {
            self.pump();
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
        }
    }

    fn now_ms(&mut self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

impl Drop for Win32Driver {
    fn drop(&mut self) {
        // SAFETY: restoring the cursor and destroying our own window on its thread.
        unsafe {
            let _ = SetCursorPos(self.saved_cursor.x, self.saved_cursor.y);
            if let Some(h) = self.indicator.take() {
                let _ = DestroyWindow(h);
            }
        }
        if let Some(st) = INDICATOR.with(|s| s.borrow_mut().take()) {
            // SAFETY: the font was created by us and is no longer selected anywhere.
            unsafe {
                let _ = DeleteObject(st.font);
            }
        }
        self.pump();
    }
}

// ------------------------------------------------------------------ indicator window

fn create_indicator(target: &Rect, dpi: u32) -> windows::core::Result<HWND> {
    let class = wide(INDICATOR_CLASS);
    let face = wide("Segoe UI");
    let text: Vec<u16> = crate::i18n::t("notify-scroll-stop").encode_utf16().collect();
    // SAFETY: standard window class registration and creation on this thread; the font and
    // the DC are released on every path.
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(indicator_proc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        // Fails harmlessly when the class is already registered (second capture).
        RegisterClassW(&wc);

        let font = CreateFontW(
            -scale(14, dpi),
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            0,
            pcwstr(&face),
        );
        // Measure the text.
        let mut rc = RECT::default();
        let dc = GetDC(HWND::default());
        let old = SelectObject(dc, font);
        let mut measure = text.clone();
        DrawTextW(dc, &mut measure, &mut rc, DT_CALCRECT | DT_SINGLELINE);
        SelectObject(dc, old);
        ReleaseDC(HWND::default(), dc);
        let w = rc.right - rc.left + 2 * scale(16, dpi);
        let h = rc.bottom - rc.top + 2 * scale(9, dpi);
        let pos = indicator_position(target, w, h, scale(8, dpi));

        INDICATOR.with(|s| *s.borrow_mut() = Some(IndicatorState { font, text: text.clone() }));
        let hwnd = match CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            pcwstr(&class),
            pcwstr(&class),
            WS_POPUP,
            pos.x,
            pos.y,
            w,
            h,
            HWND::default(),
            None,
            hinst,
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                if let Some(st) = INDICATOR.with(|s| s.borrow_mut().take()) {
                    let _ = DeleteObject(st.font);
                }
                return Err(e);
            }
        };
        exclude_from_capture(hwnd);
        round_corners(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        Ok(hwnd)
    }
}

/// Near but outside the target when the monitor's work area allows: centered below it,
/// else above it, else inside its top edge (the window is excluded from capture anyway).
fn indicator_position(target: &Rect, w: i32, h: i32, gap: i32) -> Point {
    let work = monitor_at(target.center()).map_or(*target, |m| m.work);
    let x = (target.center().x - w / 2).clamp(work.x, (work.right() - w).max(work.x));
    if target.bottom() + gap + h <= work.bottom() {
        Point::new(x, target.bottom() + gap)
    } else if target.y - gap - h >= work.y {
        Point::new(x, target.y - gap - h)
    } else {
        Point::new(x, target.y.max(work.y) + gap)
    }
}

unsafe extern "system" fn indicator_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let bg = CreateSolidBrush(COLORREF(0x0030_2820));
            FillRect(dc, &rc, bg);
            let _ = DeleteObject(bg);
            INDICATOR.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    let old = SelectObject(dc, st.font);
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, COLORREF(0x00FF_FFFF));
                    let mut text = st.text.clone();
                    DrawTextW(dc, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    SelectObject(dc, old);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            INDICATOR_CLICKED.store(true, Ordering::Release);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
