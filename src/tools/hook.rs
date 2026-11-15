//! Low-level mouse and keyboard hooks for the click-through tools (crosshair, focus).
//!
//! Their windows are `WS_EX_TRANSPARENT`, so they never see input. A dedicated thread
//! (never the UI thread, see DECISIONS #4) installs `WH_MOUSE_LL` + `WH_KEYBOARD_LL`
//! while one of them is open, swallows only the gestures the tools own (Esc, right-click
//! for the crosshair, wheel and Ctrl+Shift+drag for the focus spotlight) and posts them
//! to the tools' message window. Mouse moves are coalesced: one pending message at most.

use crate::common::win::{hwnd_from_isize, hwnd_to_isize};
use crate::common::Point;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_USER,
};

/// Messages posted to the target window.
pub const WM_HOOK_MOVE: u32 = WM_APP + 0x40;
pub const WM_HOOK_ESC: u32 = WM_APP + 0x41;
pub const WM_HOOK_RCLICK: u32 = WM_APP + 0x42;
/// WPARAM = wheel delta as i16.
pub const WM_HOOK_WHEEL: u32 = WM_APP + 0x43;
/// WPARAM = 0 start, 1 end; LPARAM = packed point (see [`unpack_point`]).
pub const WM_HOOK_DRAG: u32 = WM_APP + 0x44;

/// What the hook should intercept (set by the tools on the UI thread).
pub static WANT_ESC: AtomicBool = AtomicBool::new(false);
pub static WANT_RCLICK: AtomicBool = AtomicBool::new(false);
pub static WANT_WHEEL: AtomicBool = AtomicBool::new(false);
pub static WANT_CTRL_SHIFT_DRAG: AtomicBool = AtomicBool::new(false);

static TARGET: AtomicIsize = AtomicIsize::new(0);
static THREAD: AtomicU32 = AtomicU32::new(0);
static MOVE_PENDING: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static ESC_SWALLOWED: AtomicBool = AtomicBool::new(false);
static RDOWN_SWALLOWED: AtomicBool = AtomicBool::new(false);

const WM_HOOK_STOP: u32 = WM_USER + 1;

pub fn pack_point(p: Point) -> LPARAM {
    LPARAM(((p.y as u32 as u64) << 32 | p.x as u32 as u64) as i64 as isize)
}

pub fn unpack_point(lp: LPARAM) -> Point {
    let v = lp.0 as i64 as u64;
    Point::new(v as u32 as i32, (v >> 32) as u32 as i32)
}

/// The UI thread acknowledges a move message, so the hook may post the next one.
pub fn move_handled() {
    MOVE_PENDING.store(false, Ordering::Release);
}

pub fn is_running() -> bool {
    THREAD.load(Ordering::Acquire) != 0
}

/// Start the hook thread (idempotent). `target` receives the WM_HOOK_* messages.
pub fn start(target: HWND) -> bool {
    TARGET.store(hwnd_to_isize(target), Ordering::Release);
    if is_running() {
        return true;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("tools-hook".into()).spawn(move || run(tx));
    if let Err(e) = spawned {
        log::error!("tools: hook thread failed to start: {e}");
        return false;
    }
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Some(tid)) => {
            THREAD.store(tid, Ordering::Release);
            true
        }
        _ => {
            log::error!("tools: low-level hooks unavailable");
            false
        }
    }
}

/// Stop the hook thread (idempotent).
pub fn stop() {
    let tid = THREAD.swap(0, Ordering::AcqRel);
    for f in [&WANT_ESC, &WANT_RCLICK, &WANT_WHEEL, &WANT_CTRL_SHIFT_DRAG, &DRAGGING, &MOVE_PENDING] {
        f.store(false, Ordering::Release);
    }
    if tid != 0 {
        // SAFETY: plain post to the hook thread's queue (created before it reported its id).
        unsafe {
            let _ = PostThreadMessageW(tid, WM_HOOK_STOP, WPARAM(0), LPARAM(0));
        }
    }
}

fn run(ready: mpsc::Sender<Option<u32>>) {
    // SAFETY: hooks are installed and removed on this thread, which pumps messages as
    // low-level hooks require.
    unsafe {
        let mut msg = MSG::default();
        // Create the message queue before reporting the thread id.
        let _ = PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE);
        let hi = super::wnd::hinstance();
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hi, 0);
        let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), hi, 0);
        let (mouse, keys) = match (mouse, keys) {
            (Ok(m), Ok(k)) => (m, k),
            (m, k) => {
                log::error!("tools: SetWindowsHookExW failed: {:?} {:?}", m.as_ref().err(), k.as_ref().err());
                if let Ok(m) = m {
                    let _ = UnhookWindowsHookEx(m);
                }
                if let Ok(k) = k {
                    let _ = UnhookWindowsHookEx(k);
                }
                let _ = ready.send(None);
                return;
            }
        };
        let _ = ready.send(Some(GetCurrentThreadId()));
        loop {
            let r = GetMessageW(&mut msg, HWND::default(), 0, 0);
            if r.0 <= 0 || msg.message == WM_HOOK_STOP || msg.message == WM_QUIT {
                break;
            }
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(mouse);
        let _ = UnhookWindowsHookEx(keys);
        log::debug!("tools: hook thread stopped");
    }
}

fn post(msg: u32, wp: WPARAM, lp: LPARAM) {
    let t = TARGET.load(Ordering::Acquire);
    if t != 0 {
        // SAFETY: posting to the tools' message window; harmless if it is gone.
        unsafe {
            let _ = PostMessageW(hwnd_from_isize(t), msg, wp, lp);
        }
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: plain query.
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && is_running() {
        // SAFETY: for HC_ACTION, lParam points to an MSLLHOOKSTRUCT.
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let pt = Point::new(info.pt.x, info.pt.y);
        let swallow = match wp.0 as u32 {
            WM_MOUSEMOVE => {
                if !MOVE_PENDING.swap(true, Ordering::AcqRel) {
                    post(WM_HOOK_MOVE, WPARAM(0), pack_point(pt));
                }
                false
            }
            WM_RBUTTONDOWN if WANT_RCLICK.load(Ordering::Acquire) => {
                RDOWN_SWALLOWED.store(true, Ordering::Release);
                true
            }
            WM_RBUTTONUP if RDOWN_SWALLOWED.swap(false, Ordering::AcqRel) => {
                post(WM_HOOK_RCLICK, WPARAM(0), pack_point(pt));
                true
            }
            WM_MOUSEWHEEL if WANT_WHEEL.load(Ordering::Acquire) => {
                let delta = (info.mouseData >> 16) as u16 as i16;
                post(WM_HOOK_WHEEL, WPARAM(delta as u16 as usize), pack_point(pt));
                true
            }
            WM_LBUTTONDOWN
                if WANT_CTRL_SHIFT_DRAG.load(Ordering::Acquire) && key_down(VK_CONTROL.0) && key_down(VK_SHIFT.0) =>
            {
                DRAGGING.store(true, Ordering::Release);
                post(WM_HOOK_DRAG, WPARAM(0), pack_point(pt));
                true
            }
            WM_LBUTTONUP if DRAGGING.swap(false, Ordering::AcqRel) => {
                post(WM_HOOK_DRAG, WPARAM(1), pack_point(pt));
                true
            }
            _ => false,
        };
        if swallow {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn key_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && is_running() {
        // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
        let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if info.vkCode == VK_ESCAPE.0 as u32 {
            let m = wp.0 as u32;
            if (m == WM_KEYDOWN || m == WM_SYSKEYDOWN) && WANT_ESC.load(Ordering::Acquire) {
                ESC_SWALLOWED.store(true, Ordering::Release);
                post(WM_HOOK_ESC, WPARAM(0), LPARAM(0));
                return LRESULT(1);
            }
            if m == WM_KEYUP && ESC_SWALLOWED.swap(false, Ordering::AcqRel) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_packing_keeps_negative_coordinates() {
        for p in [Point::new(0, 0), Point::new(-1920, -5), Point::new(7680, -2160), Point::new(i32::MIN, i32::MAX)] {
            assert_eq!(unpack_point(pack_point(p)), p);
        }
    }
}
