//! Toolbar layout and theme colours (pure, no Win32).

use crate::common::{Point, Rect};
use crate::settings::CaptureMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Mode(CaptureMode),
    Output,
    Delay,
    Menu,
    Minimize,
}

impl Button {
    /// Every configurable button id, in canonical order.
    pub fn configurable() -> Vec<Button> {
        let mut v: Vec<Button> = CaptureMode::ALL.iter().map(|&m| Button::Mode(m)).collect();
        v.extend([Button::Output, Button::Delay, Button::Menu]);
        v
    }

    pub fn id(self) -> &'static str {
        match self {
            Button::Mode(m) => m.id(),
            Button::Output => "output",
            Button::Delay => "delay",
            Button::Menu => "menu",
            Button::Minimize => "minimize",
        }
    }

    pub fn from_id(s: &str) -> Option<Button> {
        match s.trim().to_ascii_lowercase().as_str() {
            "output" => Some(Button::Output),
            "delay" => Some(Button::Delay),
            "menu" => Some(Button::Menu),
            other => CaptureMode::from_id(other).map(Button::Mode),
        }
    }
}

/// Buttons from the settings list (unknown and duplicate ids are skipped).
pub fn buttons_from_ids(ids: &[String]) -> Vec<Button> {
    let mut v: Vec<Button> = Vec::new();
    for id in ids {
        if let Some(b) = Button::from_id(id) {
            if !v.contains(&b) {
                v.push(b);
            }
        }
    }
    v
}

/// Toggle `b` in the id list, inserting it at its canonical position relative to the others.
pub fn toggle_button(ids: &[String], b: Button) -> Vec<String> {
    let mut cur = buttons_from_ids(ids);
    if let Some(i) = cur.iter().position(|&x| x == b) {
        cur.remove(i);
    } else {
        let order = Button::configurable();
        let rank = |x: &Button| order.iter().position(|o| o == x).unwrap_or(usize::MAX);
        let r = rank(&b);
        let at = cur.iter().position(|x| rank(x) > r).unwrap_or(cur.len());
        cur.insert(at, b);
    }
    cur.into_iter().map(|b| b.id().to_string()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub button: Button,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pub scale: f32,
    /// Drag area (dots + app icon).
    pub grip: Rect,
    pub app_icon: Rect,
    /// Top-left of the 2 × 3 grip dot grid and the dot pitch.
    pub dots: Point,
    pub dot_pitch: f32,
    pub dot_radius: f32,
    /// Vertical separators: x position (centre), top and bottom.
    pub separators: Vec<i32>,
    pub sep_top: i32,
    pub sep_bottom: i32,
    pub items: Vec<Item>,
    /// Icon size in pixels.
    pub icon: i32,
    pub corner: f32,
}

fn px(v: f32, s: f32) -> i32 {
    (v * s).round() as i32
}

pub fn compute(buttons: &[Button], dpi: u32, big: bool) -> Layout {
    let s = dpi.max(48) as f32 / 96.0;
    let btn = px(if big { 40.0 } else { 28.0 }, s);
    let icon = px(if big { 28.0 } else { 20.0 }, s);
    let pad = px(if big { 5.0 } else { 4.0 }, s);
    let gap = px(2.0, s).max(1);
    let h = btn + 2 * pad;

    // Grip: dots, then the app icon.
    let dot_pitch = 4.0 * s;
    let dot_radius = 1.1 * s;
    let dots_w = px(4.0 + 1.1 * 2.0, s);
    let app = px(if big { 22.0 } else { 18.0 }, s);
    let mut x = pad + px(4.0, s);
    let dots = Point::new(x, (h as f32 / 2.0 - dot_pitch).round() as i32);
    x += dots_w + px(6.0, s);
    let app_icon = Rect::new(x, (h - app) / 2, app, app);
    x += app + px(6.0, s);
    let grip = Rect::new(0, 0, x, h);

    let sep_gap = px(4.0, s);
    let mut separators = vec![x];
    x += sep_gap + 1;

    let mut items = Vec::with_capacity(buttons.len() + 1);
    for &b in buttons {
        let w = if b == Button::Output { btn + px(10.0, s) } else { btn };
        items.push(Item { button: b, rect: Rect::new(x, pad, w, btn) });
        x += w + gap;
    }
    if !buttons.is_empty() {
        x -= gap;
        x += sep_gap;
        separators.push(x);
        x += sep_gap + 1;
    }
    let min_w = (btn as f32 * 0.85).round() as i32;
    items.push(Item { button: Button::Minimize, rect: Rect::new(x, pad, min_w, btn) });
    x += min_w + pad;

    Layout {
        w: x,
        h,
        scale: s,
        grip,
        app_icon,
        dots,
        dot_pitch,
        dot_radius,
        separators,
        sep_top: pad + px(5.0, s),
        sep_bottom: h - pad - px(5.0, s),
        items,
        icon,
        corner: if big { 7.0 * s } else { 5.0 * s },
    }
}

impl Layout {
    pub fn hit(&self, p: Point) -> Option<usize> {
        self.items.iter().position(|it| it.rect.contains(p))
    }

    pub fn in_grip(&self, p: Point) -> bool {
        self.grip.contains(p)
    }
}

/// RGBA colours for one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub bg: [u8; 4],
    pub border: [u8; 4],
    pub hover: [u8; 4],
    pub pressed: [u8; 4],
    pub separator: [u8; 4],
    pub grip: [u8; 4],
    pub fg: [u8; 4],
    pub badge: [u8; 4],
    pub badge_text: [u8; 4],
}

impl Theme {
    pub fn new(dark: bool) -> Theme {
        if dark {
            Theme {
                dark,
                bg: [0x20, 0x21, 0x26, 255],
                border: [0x3A, 0x3C, 0x45, 255],
                hover: [255, 255, 255, 22],
                pressed: [255, 255, 255, 38],
                separator: [0x3A, 0x3C, 0x45, 255],
                grip: [0x70, 0x73, 0x80, 255],
                fg: [0xE8, 0xE9, 0xEE, 255],
                badge: [0xFF, 0xB8, 0x2E, 255],
                badge_text: [0x1A, 0x1A, 0x1A, 255],
            }
        } else {
            Theme {
                dark,
                bg: [0xFA, 0xFA, 0xFC, 255],
                border: [0xD9, 0xDB, 0xE3, 255],
                hover: [0x10, 0x18, 0x40, 18],
                pressed: [0x10, 0x18, 0x40, 34],
                separator: [0xDD, 0xDF, 0xE6, 255],
                grip: [0xA2, 0xA6, 0xB3, 255],
                fg: [0x23, 0x26, 0x2E, 255],
                badge: [0xF5, 0xA6, 0x14, 255],
                badge_text: [0x1A, 0x1A, 0x1A, 255],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip() {
        for b in Button::configurable() {
            assert_eq!(Button::from_id(b.id()), Some(b));
        }
        assert_eq!(Button::from_id("minimize"), None);
    }

    #[test]
    fn toggle_keeps_order() {
        let ids: Vec<String> = ["menu", "region"].map(String::from).to_vec();
        let v = toggle_button(&ids, Button::Mode(CaptureMode::ActiveWindow));
        assert_eq!(v, ["active", "menu", "region"]);
        let v = toggle_button(&v, Button::Menu);
        assert_eq!(v, ["active", "region"]);
        let v = toggle_button(&v, Button::Delay);
        assert_eq!(v, ["active", "region", "delay"]);
    }

    #[test]
    fn layout_scales() {
        let b = buttons_from_ids(&crate::settings::ToolbarSettings::default().buttons);
        assert_eq!(b.len(), 10);
        let l1 = compute(&b, 96, false);
        let l2 = compute(&b, 192, false);
        let big = compute(&b, 96, true);
        assert_eq!(l1.items[0].rect.h, 28);
        assert_eq!(l2.items[0].rect.h, 56);
        assert_eq!(big.items[0].rect.h, 40);
        assert!(l2.w > l1.w * 19 / 10);
        assert_eq!(l1.items.last().map(|i| i.button), Some(Button::Minimize));
        // Items never overlap and stay inside the window.
        for w in l1.items.windows(2) {
            assert!(w[0].rect.right() <= w[1].rect.x);
        }
        assert!(l1.items.iter().all(|i| i.rect.right() <= l1.w && i.rect.bottom() <= l1.h));
        assert_eq!(l1.hit(l1.items[3].rect.center()), Some(3));
        assert!(l1.in_grip(Point::new(2, 2)));
    }
}
