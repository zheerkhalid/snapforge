//! Hotkey text <-> (modifiers, virtual key) conversion.

use super::Hotkey;

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

pub const VK_SNAPSHOT: u32 = 0x2C;

/// Canonical names first: [`key_name`] returns the first entry for a code.
const NAMED_KEYS: &[(&str, u32)] = &[
    ("PrtSc", VK_SNAPSHOT),
    ("PrintScreen", VK_SNAPSHOT),
    ("Print", VK_SNAPSHOT),
    ("PrtScn", VK_SNAPSHOT),
    ("PrintScrn", VK_SNAPSHOT),
    ("Snapshot", VK_SNAPSHOT),
    ("Left", 0x25),
    ("LeftArrow", 0x25),
    ("Up", 0x26),
    ("UpArrow", 0x26),
    ("Right", 0x27),
    ("RightArrow", 0x27),
    ("Down", 0x28),
    ("DownArrow", 0x28),
    ("Insert", 0x2D),
    ("Ins", 0x2D),
    ("Delete", 0x2E),
    ("Del", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PgUp", 0x21),
    ("PageDown", 0x22),
    ("PgDn", 0x22),
    ("PgDown", 0x22),
    ("Space", 0x20),
    ("Spacebar", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Return", 0x0D),
    ("Pause", 0x13),
    ("Break", 0x13),
    ("Num*", 0x6A),
    ("NumMultiply", 0x6A),
    ("NumPlus", 0x6B),
    ("NumAdd", 0x6B),
    ("Num-", 0x6D),
    ("NumMinus", 0x6D),
    ("NumSubtract", 0x6D),
    ("Num.", 0x6E),
    ("NumDecimal", 0x6E),
    ("NumDel", 0x6E),
    ("Num/", 0x6F),
    ("NumDivide", 0x6F),
    ("NumSlash", 0x6F),
];

fn modifier_bit(tok: &str) -> Option<u32> {
    match tok.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "ctl" => Some(MOD_CONTROL),
        "alt" | "menu" => Some(MOD_ALT),
        "shift" => Some(MOD_SHIFT),
        "win" | "windows" | "super" | "meta" | "lwin" | "rwin" => Some(MOD_WIN),
        _ => None,
    }
}

fn key_code(tok: &str) -> Option<u32> {
    let lower = tok.to_ascii_lowercase();
    if let Some(&(_, vk)) = NAMED_KEYS.iter().find(|(n, _)| n.to_ascii_lowercase() == lower) {
        return Some(vk);
    }
    let bytes = lower.as_bytes();
    // A–Z, 0–9
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_lowercase() {
            return Some((c - b'a' + b'A') as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    // F1–F24
    if let Some(n) = lower.strip_prefix('f').and_then(|r| r.parse::<u32>().ok()) {
        if (1..=24).contains(&n) && !lower[1..].starts_with('0') {
            return Some(0x70 + n - 1);
        }
    }
    // Num0–Num9 (also "Numpad0")
    for prefix in ["numpad", "num"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit() {
                return Some(0x60 + (rest.as_bytes()[0] - b'0') as u32);
            }
        }
    }
    None
}

/// Canonical display name for a virtual-key code.
pub fn key_name(vk: u32) -> String {
    if let Some(&(n, _)) = NAMED_KEYS.iter().find(|(_, v)| *v == vk) {
        return n.to_string();
    }
    match vk {
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk).map(String::from).unwrap_or_default(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        _ => format!("VK{vk:02X}"),
    }
}

pub fn parse(s: &str) -> Option<Hotkey> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    // Split on '+' but keep a trailing "Num+" style key working ("Ctrl+Num+").
    let toks: Vec<&str> = s.split('+').map(str::trim).collect();
    let mut i = 0;
    while i < toks.len() {
        let mut tok = toks[i].to_string();
        // "Num+" splits into "Num" and "" — rejoin.
        if tok.eq_ignore_ascii_case("num") && i + 1 < toks.len() && toks[i + 1].is_empty() {
            tok = "NumPlus".into();
            i += 1;
        }
        if tok.is_empty() {
            return None;
        }
        if let Some(bit) = modifier_bit(&tok) {
            if mods & bit != 0 {
                return None;
            }
            mods |= bit;
        } else {
            if vk.is_some() {
                return None;
            }
            vk = Some(key_code(&tok)?);
        }
        i += 1;
    }
    vk.map(|vk| Hotkey { mods, vk })
}

pub fn format(h: &Hotkey) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(5);
    if h.mods & MOD_CONTROL != 0 {
        parts.push("Ctrl".into());
    }
    if h.mods & MOD_SHIFT != 0 {
        parts.push("Shift".into());
    }
    if h.mods & MOD_ALT != 0 {
        parts.push("Alt".into());
    }
    if h.mods & MOD_WIN != 0 {
        parts.push("Win".into());
    }
    parts.push(key_name(h.vk));
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        let h = parse("ctrl+shift+prtsc").expect("parse");
        assert_eq!(h, Hotkey { mods: MOD_CONTROL | MOD_SHIFT, vk: VK_SNAPSHOT });
        assert_eq!(format(&h), "Ctrl+Shift+PrtSc");
        assert_eq!(parse("PrintScreen"), parse("Print"));
        assert_eq!(format(&parse("Win + Shift + s").expect("parse")), "Shift+Win+S");
        assert_eq!(format(&parse("alt+f1").expect("parse")), "Alt+F1");
        assert_eq!(parse("F24").map(|h| h.vk), Some(0x87));
        assert_eq!(parse("Ctrl+Num+").map(|h| h.vk), Some(0x6B));
        assert_eq!(parse("Numpad7").map(|h| h.vk), Some(0x67));
    }

    #[test]
    fn rejects_bad_input() {
        for s in ["", "Ctrl", "Ctrl+Shift", "Ctrl+A+B", "F25", "F0", "F01", "Ctrl+Ctrl+A", "Hyper+A", "Ctrl++A", "AB"] {
            assert_eq!(parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn roundtrip_all_keys() {
        let mut vks: Vec<u32> = NAMED_KEYS.iter().map(|k| k.1).collect();
        vks.extend(0x41..=0x5A);
        vks.extend(0x30..=0x39);
        vks.extend(0x70..=0x87);
        vks.extend(0x60..=0x69);
        for vk in vks {
            for mods in 0..16 {
                let h = Hotkey { mods, vk };
                let text = format(&h);
                assert_eq!(parse(&text), Some(h), "{text}");
                assert_eq!(parse(&text.to_lowercase()), Some(h), "{text}");
            }
        }
    }

    #[test]
    fn settings_defaults_are_canonical() {
        let d = crate::settings::HotkeySettings::default();
        for m in crate::settings::CaptureMode::ALL {
            let text = d.for_mode(m);
            let h = parse(text).expect("default parses");
            assert_eq!(format(&h), text);
        }
    }
}
