//! Settings for Phase 5 pin to screen. Owned by that module's agent: add fields here
//! (keep `#[serde(default)]` so older settings files still load).

use serde::{Deserialize, Serialize};

/// What the Space key does on a focused pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpaceAction {
    /// Collapse the pin into a small tab at the top edge of its monitor (Space again restores).
    #[default]
    HideToTab,
    /// Zoom to 100% (actual size).
    ActualSize,
    /// Toggle Draw Mode.
    ToggleDraw,
}

/// What the Enter key does on a focused pin (outside text entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EnterAction {
    #[default]
    CopyAndClose,
    SaveAndClose,
    Nothing,
}

/// What fills new space when the canvas is expanded with Ctrl+drag on a border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanvasFill {
    #[default]
    Transparent,
    White,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PinSettings {
    pub space_action: SpaceAction,
    pub enter_action: EnterAction,
    /// Open Draw Mode automatically when a pin is created.
    pub auto_draw_mode: bool,
    pub canvas_fill: CanvasFill,
    /// Opacity of new pins, 10..=100 (%).
    pub default_opacity: u8,
    /// Draw Mode pen/text/stamp color, 0xRRGGBB.
    pub draw_color: u32,
    /// Draw Mode size preset: 0 = small, 1 = medium, 2 = large.
    pub draw_size: u8,
}

impl Default for PinSettings {
    fn default() -> Self {
        Self {
            space_action: SpaceAction::HideToTab,
            enter_action: EnterAction::CopyAndClose,
            auto_draw_mode: false,
            canvas_fill: CanvasFill::Transparent,
            default_opacity: 100,
            draw_color: 0xE5_39_35,
            draw_size: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_fills_defaults() {
        let s: PinSettings = toml::from_str("space_action = \"toggle_draw\"\n").expect("parse");
        assert_eq!(s.space_action, SpaceAction::ToggleDraw);
        assert_eq!(s.enter_action, EnterAction::CopyAndClose);
        assert_eq!(s.default_opacity, 100);
    }
}
