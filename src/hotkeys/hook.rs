//! Low-level keyboard hook for PrtSc combos (DECISIONS #4).
//!
//! Runs on a dedicated thread with its own message loop, so a busy UI thread can never
//! stall system-wide keyboard input. The hook proc only reads the async modifier state,
//! compares against a small table and posts to the command bus.

use super::keys::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};
use super::{Action, Hotkey};
use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL,
    VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// An unassigned virtual key, sent to "mask" a swallowed Win/Alt combo so releasing the
/// modifier doesn't open the Start menu or activate a menu bar.
const VK_MASK: u16 = 0xE8;

struct HookState {
    combos: Vec<(Hotkey, Action)>,
    /// Keys whose keydown we swallowed; their keyup (and auto-repeats) are swallowed too.
    swallowed: Vec<u32>,
}

thread_local! {
    static STATE: RefCell<HookState> = const { RefCell::new(HookState { combos: Vec::new(), swallowed: Vec::new() }) };
}

pub struct HookThread {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl HookThread {
    /// Install the hook on a new thread. Returns an error text if the hook failed.
    pub fn start(combos: Vec<(Hotkey, Action)>) -> Result<HookThread, String> {
        let (tx, rx) = mpsc::channel::<Result<u32, String>>();
        let join = std::thread::Builder::new()
            .name("hotkey-hook".into())
            .spawn(move || run(combos, tx))
            .map_err(|e| e.to_string())?;
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(HookThread { thread_id, join: Some(join) }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            // SAFETY: posting WM_QUIT to a thread we own; harmless if it already ended.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if join.join().is_err() {
                log::error!("hotkeys: hook thread panicked");
            }
        }
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(combos: Vec<(Hotkey, Action)>, tx: mpsc::Sender<Result<u32, String>>) {
    STATE.with(|s| s.borrow_mut().combos = combos);
    // SAFETY: standard hook installation; the proc is a static function and the hook is
    // removed on this same thread before it exits.
    let hook = unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod, 0)
    };
    let hook: HHOOK = match hook {
        Ok(h) => h,
        Err(e) => {
            let _ = tx.send(Err(e.message()));
            return;
        }
    };
    // SAFETY: plain query.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = tx.send(Ok(tid));
    log::debug!("hotkeys: low-level hook installed");
    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread; the hook needs it to be called.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    log::debug!("hotkeys: low-level hook removed");
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query.
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

fn current_mods() -> u32 {
    let mut m = 0;
    if key_down(VK_CONTROL) {
        m |= MOD_CONTROL;
    }
    if key_down(VK_MENU) {
        m |= MOD_ALT;
    }
    if key_down(VK_SHIFT) {
        m |= MOD_SHIFT;
    }
    if key_down(VK_LWIN) || key_down(VK_RWIN) {
        m |= MOD_WIN;
    }
    m
}

fn send_mask_key() {
    let ki = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_MASK), dwFlags: flags, ..Default::default() } },
    };
    let inputs = [ki(Default::default()), ki(KEYEVENTF_KEYUP)];
    // SAFETY: valid INPUT array and size.
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Decide whether to swallow the event. Kept tiny: no allocation, no locks.
fn handle(vk: u32, down: bool) -> bool {
    STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return false };
        if !s.combos.iter().any(|(h, _)| h.vk == vk) {
            return false;
        }
        let already = s.swallowed.iter().position(|&v| v == vk);
        if !down {
            if let Some(i) = already {
                s.swallowed.swap_remove(i);
                return true;
            }
            return false;
        }
        if already.is_some() {
            // Auto-repeat of a key we already handled.
            return true;
        }
        let mods = current_mods();
        let Some(&(hk, action)) = s.combos.iter().find(|(h, _)| h.vk == vk && h.mods == mods) else {
            return false;
        };
        s.swallowed.push(vk);
        if hk.mods & (MOD_WIN | MOD_ALT) != 0 {
            send_mask_key();
        }
        action.post();
        true
    })
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lp.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if (down || up) && handle(kb.vkCode, down) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}
