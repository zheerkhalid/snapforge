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

use crate::settings::HotkeySettings;

/// A key combination. `mods` uses the RegisterHotKey MOD_* bits; `vk` is a virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

/// Parse text such as "Ctrl+Shift+PrtSc", "Alt+F1", "Win+Shift+S". Case-insensitive.
pub fn parse(s: &str) -> Option<Hotkey> {
    let _ = s;
    None
}

/// Canonical text for a hotkey (inverse of [`parse`]).
pub fn format(h: &Hotkey) -> String {
    let _ = h;
    String::new()
}

/// Duplicate assignments within SnapForge, as messages.
pub fn conflicts(s: &HotkeySettings) -> Vec<String> {
    let _ = s;
    Vec::new()
}

pub fn start() -> Vec<String> {
    Vec::new()
}

/// Re-read settings and re-register.
pub fn reload() -> Vec<String> {
    stop();
    start()
}

pub fn stop() {}
