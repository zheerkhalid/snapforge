//! Draw tool keyboard map: Tab / Shift+Tab cycling and the F-key shortcuts (documented in
//! docs/USAGE.md, "Draw tools"). Pure, so it is unit-tested.

use crate::draw::Tool;

/// Tool shortcuts. F2–F12 jump to the most used tools, Shift+F-keys to the rest and
/// Ctrl+F2/F3/F5 to the inserts (F1 is the shortcut list).
pub const FKEYS: [(Tool, &str); 25] = [
    (Tool::Select, "F2"),
    (Tool::Text, "F3"),
    (Tool::Arrow, "F4"),
    (Tool::Line, "F5"),
    (Tool::Rect, "F6"),
    (Tool::Ellipse, "F7"),
    (Tool::Highlighter, "F8"),
    (Tool::Step, "F9"),
    (Tool::Blur, "F10"),
    (Tool::Eraser, "F11"),
    (Tool::Pencil, "F12"),
    (Tool::Callout, "Shift+F2"),
    (Tool::Curve, "Shift+F3"),
    (Tool::Polyline, "Shift+F4"),
    (Tool::Elbow, "Shift+F5"),
    (Tool::Polygon, "Shift+F6"),
    (Tool::Bracket, "Shift+F7"),
    (Tool::LineHighlighter, "Shift+F8"),
    (Tool::Magnifier, "Shift+F9"),
    (Tool::Pixelate, "Shift+F10"),
    (Tool::Fill, "Shift+F11"),
    (Tool::Image, "Shift+F12"),
    (Tool::Emoji, "Ctrl+F2"),
    (Tool::Sticker, "Ctrl+F3"),
    (Tool::Cursor, "Ctrl+F5"),
];

pub fn shortcut(tool: Tool) -> Option<&'static str> {
    FKEYS.iter().find(|(t, _)| *t == tool).map(|(_, k)| *k)
}

/// The order of the palette and of Tab cycling: grouped like the palette.
pub const ORDER: [Tool; 25] = [
    Tool::Select,
    Tool::Text,
    Tool::Callout,
    Tool::Step,
    Tool::Magnifier,
    Tool::Emoji,
    Tool::Sticker,
    Tool::Arrow,
    Tool::Line,
    Tool::Curve,
    Tool::Polyline,
    Tool::Elbow,
    Tool::Rect,
    Tool::Ellipse,
    Tool::Polygon,
    Tool::Bracket,
    Tool::Highlighter,
    Tool::LineHighlighter,
    Tool::Image,
    Tool::Cursor,
    Tool::Blur,
    Tool::Pixelate,
    Tool::Eraser,
    Tool::Fill,
    Tool::Pencil,
];

/// Palette groups (indices into [`ORDER`] where a new group starts).
pub const GROUP_STARTS: [usize; 7] = [0, 1, 7, 12, 16, 18, 20];

/// Next (`dir > 0`) or previous tool in [`ORDER`]; from no draw tool, Tab starts at the
/// first tool and Shift+Tab at the last.
pub fn cycle(current: Option<Tool>, dir: i32) -> Tool {
    let n = ORDER.len() as i32;
    match current.and_then(|c| ORDER.iter().position(|t| *t == c)) {
        Some(i) => ORDER[(i as i32 + dir.signum()).rem_euclid(n) as usize],
        None if dir < 0 => ORDER[ORDER.len() - 1],
        None => ORDER[0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::commands::Shortcut;

    #[test]
    fn order_covers_every_tool_once() {
        for t in Tool::ALL {
            assert_eq!(ORDER.iter().filter(|x| **x == t).count(), 1, "{t:?}");
        }
        assert!(GROUP_STARTS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn tab_cycles_in_palette_order_and_wraps() {
        assert_eq!(cycle(None, 1), Tool::Select);
        assert_eq!(cycle(None, -1), Tool::Pencil);
        assert_eq!(cycle(Some(Tool::Select), 1), Tool::Text);
        assert_eq!(cycle(Some(Tool::Text), -1), Tool::Select);
        assert_eq!(cycle(Some(Tool::Pencil), 1), Tool::Select);
        assert_eq!(cycle(Some(Tool::Select), -1), Tool::Pencil);
        // A full lap visits every tool.
        let mut t = Tool::Select;
        let mut seen = vec![t];
        for _ in 1..ORDER.len() {
            t = cycle(Some(t), 1);
            seen.push(t);
        }
        assert_eq!(seen, ORDER.to_vec());
        assert_eq!(cycle(Some(t), 1), Tool::Select);
    }

    #[test]
    fn fkey_map_is_complete_unique_and_parses() {
        for t in Tool::ALL {
            assert!(shortcut(t).is_some(), "{t:?} has no key");
        }
        let mut seen = std::collections::HashSet::new();
        for (t, k) in FKEYS {
            let s = Shortcut::parse(k).unwrap_or_else(|| panic!("{k} does not parse"));
            assert!(seen.insert(s), "{k} used twice");
            assert_ne!(s.key, eframe::egui::Key::F1, "{t:?}: F1 is the shortcut list");
        }
        assert_eq!(shortcut(Tool::Select), Some("F2"));
        assert_eq!(shortcut(Tool::Text), Some("F3"));
        assert_eq!(shortcut(Tool::Arrow), Some("F4"));
        assert_eq!(shortcut(Tool::Pencil), Some("F12"));
        assert_eq!(shortcut(Tool::Callout), Some("Shift+F2"));
        assert_eq!(shortcut(Tool::Cursor), Some("Ctrl+F5"));
    }
}
