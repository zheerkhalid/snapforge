//! Global hotkeys.
//!
//! CONTRACT:
//! * [`start`] registers every hotkey from `settings.hotkeys` and returns human-readable
//!   problems (conflicts, keys taken by other programs). It owns its own hidden message
//!   window on the UI thread for `RegisterHotKey`, and, for PrtSc combos when
//!   `prtsc_override` is on (or registration fails), a low-level keyboard hook running
//!   on a DEDICATED thread (never the UI thread).
//! * On a hotkey press it calls `crate::app::post(AppCommand::Capture(mode))`
//!   (or `AppCommand::RepeatLast`).

mod hook;
pub mod keys;

use crate::app::{self, AppCommand};
use crate::common::win::{pcwstr, wide};
use crate::i18n;
use crate::settings::{self, CaptureMode, HotkeySettings};
use std::cell::RefCell;
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_HOTKEY, WNDCLASSW,
};

/// A key combination. `mods` uses the RegisterHotKey MOD_* bits; `vk` is a virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

/// What a hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Capture(CaptureMode),
    RepeatLast,
}

impl Action {
    fn post(self) {
        match self {
            Action::Capture(m) => app::post(AppCommand::Capture(m)),
            Action::RepeatLast => app::post(AppCommand::RepeatLast),
        }
    }

    fn label(self) -> String {
        match self {
            Action::Capture(m) => i18n::t(m.label_key()),
            Action::RepeatLast => i18n::t("repeat-last"),
        }
    }
}

/// Parse text such as "Ctrl+Shift+PrtSc", "Alt+F1", "Win+Shift+S". Case-insensitive.
pub fn parse(s: &str) -> Option<Hotkey> {
    keys::parse(s)
}

/// Canonical text for a hotkey (inverse of [`parse`]).
pub fn format(h: &Hotkey) -> String {
    keys::format(h)
}

/// Canonical text for a settings value (the raw text when it does not parse; "" when empty).
pub fn display(text: &str) -> String {
    match parse(text) {
        Some(h) => format(&h),
        None => text.trim().to_string(),
    }
}

/// Every assignment as (raw text, action), in a stable order.
fn assignments(s: &HotkeySettings) -> Vec<(&str, Action)> {
    let mut v: Vec<(&str, Action)> = CaptureMode::ALL.iter().map(|&m| (s.for_mode(m), Action::Capture(m))).collect();
    v.push((s.repeat_last.as_str(), Action::RepeatLast));
    v
}

/// Parsed assignments with duplicates removed (first wins), plus problem messages.
fn resolve(s: &HotkeySettings) -> (Vec<(Hotkey, Action)>, Vec<String>) {
    let mut out: Vec<(Hotkey, Action)> = Vec::new();
    let mut problems = Vec::new();
    for (text, action) in assignments(s) {
        if text.trim().is_empty() {
            continue;
        }
        match parse(text) {
            None => problems.push(i18n::tf("hotkey-invalid", &[("key", text.trim()), ("action", &action.label())])),
            Some(h) => {
                if let Some((_, first)) = out.iter().find(|(o, _)| *o == h) {
                    problems.push(i18n::tf(
                        "hotkey-conflict",
                        &[("key", &format(&h)), ("a", &first.label()), ("b", &action.label())],
                    ));
                } else {
                    out.push((h, action));
                }
            }
        }
    }
    (out, problems)
}

/// Duplicate assignments within SnapForge, as messages.
pub fn conflicts(s: &HotkeySettings) -> Vec<String> {
    let mut seen: Vec<(Hotkey, Action)> = Vec::new();
    let mut msgs = Vec::new();
    for (text, action) in assignments(s) {
        let Some(h) = parse(text) else { continue };
        if let Some((_, first)) = seen.iter().find(|(o, _)| *o == h) {
            msgs.push(i18n::tf(
                "hotkey-conflict",
                &[("key", &format(&h)), ("a", &first.label()), ("b", &action.label())],
            ));
        } else {
            seen.push((h, action));
        }
    }
    msgs
}

// ---------------------------------------------------------------- runtime

const CLASS: &str = "SnapForge.Hotkeys";

#[derive(Default)]
struct UiState {
    hwnd: Option<HWND>,
    /// RegisterHotKey id - 1 → action.
    registered: Vec<Action>,
}

thread_local! {
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
}

static HOOK: Mutex<Option<hook::HookThread>> = Mutex::new(None);

fn create_window() -> Option<HWND> {
    let class = wide(CLASS);
    // SAFETY: class registration (repeat registrations fail harmlessly) and a
    // message-only window owned by this (UI) thread.
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassW(&wc);
        match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            pcwstr(&class),
            pcwstr(&class),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinst,
            None,
        ) {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("hotkeys: cannot create message window: {e}");
                None
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_HOTKEY {
        let id = wp.0;
        let action =
            UI.with(|u| u.try_borrow().ok().and_then(|u| id.checked_sub(1).and_then(|i| u.registered.get(i).copied())));
        if let Some(a) = action {
            a.post();
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Register all hotkeys from settings. Call on the UI thread.
pub fn start() -> Vec<String> {
    let s = settings::get().hotkeys.clone();
    let (combos, mut problems) = resolve(&s);
    let Some(hwnd) = UI.with(|u| u.borrow().hwnd).or_else(create_window) else {
        problems.push("hotkeys: cannot create message window".into());
        return problems;
    };
    let mut registered: Vec<Action> = Vec::new();
    let mut hooked: Vec<(Hotkey, Action)> = Vec::new();
    for (h, action) in combos {
        let is_prtsc = h.vk == keys::VK_SNAPSHOT;
        if is_prtsc && s.prtsc_override {
            hooked.push((h, action));
            continue;
        }
        let id = registered.len() as i32 + 1;
        // SAFETY: registering a hotkey for our own message window.
        let r = unsafe { RegisterHotKey(hwnd, id, HOT_KEY_MODIFIERS(h.mods) | MOD_NOREPEAT, h.vk) };
        match r {
            Ok(()) => registered.push(action),
            Err(e) if is_prtsc => {
                log::info!("hotkeys: {} not registrable ({e}); using the keyboard hook", format(&h));
                hooked.push((h, action));
            }
            Err(e) => {
                log::warn!("hotkeys: RegisterHotKey {} failed: {e}", format(&h));
                problems.push(i18n::tf("notify-hotkey-failed", &[("key", &format(&h))]));
            }
        }
    }
    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.hwnd = Some(hwnd);
        u.registered = registered;
    });
    if !hooked.is_empty() {
        let keys_text: Vec<String> = hooked.iter().map(|(h, _)| format(h)).collect();
        match hook::HookThread::start(hooked) {
            Ok(t) => {
                if let Ok(mut g) = HOOK.lock() {
                    *g = Some(t);
                }
            }
            Err(e) => {
                log::error!("hotkeys: keyboard hook failed: {e}");
                problems.push(i18n::tf("hotkey-hook-failed", &[("key", &keys_text.join(", "))]));
            }
        }
    }
    problems
}

/// Re-read settings and re-register.
pub fn reload() -> Vec<String> {
    stop();
    start()
}

/// Unregister everything, end the hook thread and destroy the message window.
pub fn stop() {
    let (hwnd, count) = UI.with(|u| {
        let mut u = u.borrow_mut();
        let count = u.registered.len();
        u.registered.clear();
        (u.hwnd.take(), count)
    });
    if let Some(hwnd) = hwnd {
        // SAFETY: unregistering our own ids and destroying our own window on its thread.
        unsafe {
            for id in 1..=count as i32 {
                let _ = UnregisterHotKey(hwnd, id);
            }
            let _ = DestroyWindow(hwnd);
        }
    }
    let t = HOOK.lock().ok().and_then(|mut g| g.take());
    if let Some(t) = t {
        t.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_conflicts_in_defaults() {
        assert!(conflicts(&HotkeySettings::default()).is_empty());
    }

    #[test]
    fn detects_duplicates() {
        let s = HotkeySettings {
            region: "Ctrl+PrtSc".into(),
            repeat_last: "ctrl + printscreen".into(),
            full_screen: "Alt+F1".into(),
            scrolling: "alt+f1".into(),
            ..HotkeySettings::default()
        };
        let c = conflicts(&s);
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c.iter().any(|m| m.contains("Ctrl+PrtSc")));
        assert!(c.iter().any(|m| m.contains("Alt+F1")));
        let (combos, problems) = resolve(&s);
        assert_eq!(problems.len(), 2);
        assert_eq!(combos.iter().filter(|(h, _)| format(h) == "Alt+F1").count(), 1);
    }

    #[test]
    fn invalid_text_is_reported_not_conflicting() {
        let s = HotkeySettings { region: "Ctrl+Bogus".into(), freehand: "Ctrl+Bogus".into(), ..Default::default() };
        assert!(conflicts(&s).is_empty());
        let (_, problems) = resolve(&s);
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn empty_is_unassigned() {
        let s = HotkeySettings { region: String::new(), repeat_last: String::new(), ..Default::default() };
        assert!(conflicts(&s).is_empty());
        assert_eq!(display(""), "");
        assert_eq!(display("ctrl+prtsc"), "Ctrl+PrtSc");
    }
}
