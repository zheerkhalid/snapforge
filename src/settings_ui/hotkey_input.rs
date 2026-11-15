//! Hotkey capture (egui key events → hotkey text) and conflict warnings.

use crate::hotkeys::{self, keys, Hotkey};
use crate::i18n;
use crate::settings::{CaptureMode, HotkeySettings};
use eframe::egui::{Event, Key, Modifiers};

/// The hotkey rows shown on the Hotkeys tab: (label key, field accessor index).
pub const ROWS: usize = 8 + 9;

/// Fluent key of a row's label.
pub fn row_label_key(i: usize) -> &'static str {
    match CaptureMode::ALL.get(i) {
        Some(m) => m.label_key(),
        None if i == 7 => "repeat-last",
        None => HotkeySettings::default().extras().get(i.saturating_sub(8)).map(|e| e.0).unwrap_or("repeat-last"),
    }
}

pub fn row_text(h: &HotkeySettings, i: usize) -> &str {
    match CaptureMode::ALL.get(i) {
        Some(&m) => h.for_mode(m),
        None if i == 7 => &h.repeat_last,
        None => h.extras().get(i.saturating_sub(8)).map(|e| e.1.as_str()).unwrap_or(""),
    }
}

pub fn row_text_mut(h: &mut HotkeySettings, i: usize) -> &mut String {
    match CaptureMode::ALL.get(i) {
        Some(CaptureMode::ActiveWindow) => &mut h.active_window,
        Some(CaptureMode::WindowObject) => &mut h.window_object,
        Some(CaptureMode::Region) => &mut h.region,
        Some(CaptureMode::Freehand) => &mut h.freehand,
        Some(CaptureMode::FullScreen) => &mut h.full_screen,
        Some(CaptureMode::Scrolling) => &mut h.scrolling,
        Some(CaptureMode::FixedRegion) => &mut h.fixed_region,
        None => match i {
            8 => &mut h.record_region,
            9 => &mut h.record_full_screen,
            10 => &mut h.stop_recording,
            11 => &mut h.pause_recording,
            12 => &mut h.capture_text,
            13 => &mut h.pin_capture,
            14 => &mut h.color_picker,
            15 => &mut h.magnifier,
            16 => &mut h.ruler,
            _ => &mut h.repeat_last,
        },
    }
}

/// Virtual-key code for an egui key, limited to keys the hotkey parser can name.
/// Tab, Escape and Backspace are reserved for focus, cancel and clear.
pub fn vk_for_key(key: Key) -> Option<u32> {
    let vk = match key {
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::Space => 0x20,
        Key::Enter => 0x0D,
        _ => {
            let name = key.name();
            let b = name.as_bytes();
            if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
                b[0] as u32
            } else {
                let n = name.strip_prefix('F').and_then(|r| r.parse::<u32>().ok())?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                0x70 + n - 1
            }
        }
    };
    Some(vk)
}

pub fn mod_bits(m: Modifiers, win: bool) -> u32 {
    let mut bits = 0;
    if m.ctrl {
        bits |= keys::MOD_CONTROL;
    }
    if m.shift {
        bits |= keys::MOD_SHIFT;
    }
    if m.alt {
        bits |= keys::MOD_ALT;
    }
    if win {
        bits |= keys::MOD_WIN;
    }
    bits
}

/// Hotkey text for a key press (same text `hotkeys::parse` reads), e.g. "Ctrl+Shift+A".
pub fn combo_text(m: Modifiers, win: bool, key: Key) -> Option<String> {
    let vk = vk_for_key(key)?;
    Some(hotkeys::format(&Hotkey { mods: mod_bits(m, win), vk }))
}

pub fn combo_text_vk(m: Modifiers, win: bool, vk: u32) -> String {
    hotkeys::format(&Hotkey { mods: mod_bits(m, win), vk })
}

/// What a frame's input means for an active hotkey capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    Set(String),
    Clear,
    Cancel,
}

/// Interpret key events while capturing. Esc cancels and Backspace clears (both without
/// modifiers); keys that cannot be a hotkey are ignored. `win` = a Windows key is held
/// (egui does not report it on Windows).
pub fn capture_from_events(events: &[Event], win: bool) -> Option<Captured> {
    for e in events {
        match e {
            Event::Key { key, pressed: true, modifiers, .. } => {
                let plain = !modifiers.ctrl && !modifiers.alt && !modifiers.shift && !win;
                match key {
                    Key::Escape if plain => return Some(Captured::Cancel),
                    Key::Backspace if plain => return Some(Captured::Clear),
                    _ => {
                        if let Some(t) = combo_text(*modifiers, win, *key) {
                            return Some(Captured::Set(t));
                        }
                    }
                }
            }
            // egui-winit turns Ctrl+C / Ctrl+X / Ctrl+V into clipboard events.
            Event::Copy => return Some(Captured::Set(combo_text_vk(Modifiers::CTRL, win, 0x43))),
            Event::Cut => return Some(Captured::Set(combo_text_vk(Modifiers::CTRL, win, 0x58))),
            Event::Paste(_) => return Some(Captured::Set(combo_text_vk(Modifiers::CTRL, win, 0x56))),
            _ => {}
        }
    }
    None
}

/// Windows (and very common application) shortcuts worth warning about, with the Fluent
/// key that describes them.
pub const SYSTEM_HOTKEYS: &[(&str, &str)] = &[
    ("PrtSc", "settings-sys-prtsc"),
    ("Alt+PrtSc", "settings-sys-alt-prtsc"),
    ("Win+PrtSc", "settings-sys-win-prtsc"),
    ("Alt+Win+PrtSc", "settings-sys-gamebar-shot"),
    ("Shift+Win+S", "settings-sys-snip"),
    ("Alt+Win+R", "settings-sys-gamebar-record"),
    ("Win+G", "settings-sys-gamebar"),
    ("Win+V", "settings-sys-clipboard-history"),
    ("Win+D", "settings-sys-desktop"),
    ("Win+E", "settings-sys-explorer"),
    ("Win+L", "settings-sys-lock"),
    ("Win+R", "settings-sys-run"),
    ("Win+Tab", "settings-sys-task-view"),
    ("Alt+Tab", "settings-sys-switch"),
    ("Alt+F4", "settings-sys-close"),
    ("Ctrl+C", "settings-sys-copy"),
    ("Ctrl+V", "settings-sys-paste"),
    ("Ctrl+X", "settings-sys-cut"),
    ("Ctrl+Z", "settings-sys-undo"),
    ("Ctrl+S", "settings-sys-save"),
    ("Ctrl+A", "settings-sys-select-all"),
    ("F1", "settings-sys-help"),
];

/// The system shortcut a hotkey collides with, if any. PrtSc combinations are skipped when
/// SnapForge takes PrtSc over with its keyboard hook.
pub fn system_conflict(text: &str, prtsc_override: bool) -> Option<&'static str> {
    let h = hotkeys::parse(text)?;
    if prtsc_override && h.vk == keys::VK_SNAPSHOT {
        return None;
    }
    SYSTEM_HOTKEYS.iter().find(|(t, _)| hotkeys::parse(t) == Some(h)).map(|(_, k)| *k)
}

/// Warnings for the Hotkeys tab: duplicates within SnapForge, unparsable text, and
/// collisions with system shortcuts.
pub fn warnings(h: &HotkeySettings) -> Vec<String> {
    let mut out = hotkeys::conflicts(h);
    for i in 0..ROWS {
        let text = row_text(h, i);
        if text.trim().is_empty() {
            continue;
        }
        let action = i18n::t(row_label_key(i));
        if hotkeys::parse(text).is_none() {
            out.push(i18n::tf("hotkey-invalid", &[("key", text.trim()), ("action", &action)]));
        } else if let Some(what) = system_conflict(text, h.prtsc_override) {
            out.push(i18n::tf(
                "settings-hotkey-system",
                &[("key", &hotkeys::display(text)), ("what", &i18n::t(what)), ("action", &action)],
            ));
        }
    }
    out
}

/// Per-row status for the warning icon: true when the row is part of any warning.
pub fn row_has_warning(h: &HotkeySettings, i: usize) -> bool {
    let text = row_text(h, i);
    if text.trim().is_empty() {
        return false;
    }
    let Some(me) = hotkeys::parse(text) else { return true };
    if system_conflict(text, h.prtsc_override).is_some() {
        return true;
    }
    (0..ROWS).any(|j| j != i && hotkeys::parse(row_text(h, j)) == Some(me))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_row_reads_back_what_it_writes() {
        let mut h = crate::settings::HotkeySettings::default();
        for i in 0..super::ROWS {
            *super::row_text_mut(&mut h, i) = format!("Ctrl+F{}", i + 1);
        }
        for i in 0..super::ROWS {
            assert_eq!(super::row_text(&h, i), format!("Ctrl+F{}", i + 1), "row {i}");
            assert!(!super::row_label_key(i).is_empty());
        }
    }

    use super::*;

    fn key(k: Key, m: Modifiers) -> Event {
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }
    }

    #[test]
    fn key_mapping() {
        let cs = Modifiers { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(combo_text(cs, false, Key::A).as_deref(), Some("Ctrl+Shift+A"));
        assert_eq!(combo_text(Modifiers::ALT, false, Key::F1).as_deref(), Some("Alt+F1"));
        assert_eq!(combo_text(Modifiers::NONE, true, Key::Num7).as_deref(), Some("Win+7"));
        assert_eq!(combo_text(Modifiers::CTRL, false, Key::ArrowLeft).as_deref(), Some("Ctrl+Left"));
        assert_eq!(combo_text(Modifiers::NONE, false, Key::PageDown).as_deref(), Some("PageDown"));
        assert_eq!(combo_text(Modifiers::NONE, false, Key::F24).as_deref(), Some("F24"));
        assert_eq!(combo_text(Modifiers::NONE, false, Key::F25), None);
        assert_eq!(combo_text(Modifiers::NONE, false, Key::Minus), None);
        // Everything we produce parses back to the same text.
        for k in Key::ALL {
            if let Some(t) = combo_text(cs, false, *k) {
                assert_eq!(hotkeys::display(&t), t, "{k:?}");
            }
        }
    }

    #[test]
    fn capture_events() {
        assert_eq!(capture_from_events(&[key(Key::Escape, Modifiers::NONE)], false), Some(Captured::Cancel));
        assert_eq!(capture_from_events(&[key(Key::Backspace, Modifiers::NONE)], false), Some(Captured::Clear));
        assert_eq!(
            capture_from_events(&[key(Key::Backspace, Modifiers::CTRL)], false),
            None,
            "Ctrl+Backspace is not a nameable hotkey"
        );
        assert_eq!(
            capture_from_events(&[key(Key::Minus, Modifiers::NONE), key(Key::S, Modifiers::CTRL)], false),
            Some(Captured::Set("Ctrl+S".into()))
        );
        assert_eq!(capture_from_events(&[Event::Copy], false), Some(Captured::Set("Ctrl+C".into())));
        assert_eq!(capture_from_events(&[], false), None);
    }

    #[test]
    fn system_warnings() {
        assert_eq!(system_conflict("Win+Shift+S", true), Some("settings-sys-snip"));
        assert_eq!(system_conflict("alt+f4", false), Some("settings-sys-close"));
        assert_eq!(system_conflict("Alt+PrtSc", true), None, "hook takes PrtSc over");
        assert_eq!(system_conflict("Alt+PrtSc", false), Some("settings-sys-alt-prtsc"));
        assert_eq!(system_conflict("Ctrl+F9", false), None);
        for (t, _) in SYSTEM_HOTKEYS {
            assert!(hotkeys::parse(t).is_some(), "{t}");
        }

        let d = HotkeySettings::default();
        assert!(warnings(&d).is_empty(), "{:?}", warnings(&d));
        assert!((0..ROWS).all(|i| !row_has_warning(&d, i)));

        let s = HotkeySettings {
            region: "Shift+Win+S".into(),
            freehand: "Ctrl+F8".into(),
            repeat_last: "ctrl+f8".into(),
            scrolling: "Ctrl+Bogus".into(),
            ..HotkeySettings::default()
        };
        let w = warnings(&s);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w.iter().any(|m| m.contains("Ctrl+F8")));
        assert!(w.iter().any(|m| m.contains("Shift+Win+S")));
        assert!(w.iter().any(|m| m.contains("Ctrl+Bogus")));
        assert!(row_has_warning(&s, 2));
        assert!(row_has_warning(&s, 3));
        assert!(row_has_warning(&s, 7));
        assert!(row_has_warning(&s, 5));
        assert!(!row_has_warning(&s, 0));
    }

    #[test]
    fn rows_cover_all_fields() {
        let mut h = HotkeySettings::default();
        for i in 0..ROWS {
            *row_text_mut(&mut h, i) = format!("F{}", i + 1);
        }
        for i in 0..ROWS {
            assert_eq!(row_text(&h, i), format!("F{}", i + 1));
        }
    }
}
