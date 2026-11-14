//! Settings for Phase 5 screen tools: color picker, magnifier, crosshair, focus, ruler.
//! (Keep `#[serde(default)]` so older settings files still load.)

use serde::{Deserialize, Serialize};

/// Maximum number of remembered picked colors.
pub const MAX_RECENT_COLORS: usize = 16;

/// Clipboard text format for picked colors (see `tools::color::format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ColorFormat {
    /// `255, 128, 0`
    Rgb,
    /// `FF8000`
    #[default]
    Hex,
    /// `16744448` (the 0xRRGGBB value in decimal)
    Decimal,
    /// `#FF8000`
    Html,
    /// `$000080FF` (Delphi `TColor`: $00BBGGRR)
    Delphi,
}

impl ColorFormat {
    pub const ALL: [ColorFormat; 5] =
        [ColorFormat::Rgb, ColorFormat::Hex, ColorFormat::Decimal, ColorFormat::Html, ColorFormat::Delphi];

    /// Fluent message id for the human label.
    pub fn label_key(self) -> &'static str {
        match self {
            ColorFormat::Rgb => "tools-format-rgb",
            ColorFormat::Hex => "tools-format-hex",
            ColorFormat::Decimal => "tools-format-decimal",
            ColorFormat::Html => "tools-format-html",
            ColorFormat::Delphi => "tools-format-delphi",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MagnifierMode {
    /// A round lens that follows the cursor.
    #[default]
    Lens,
    /// A fixed, movable and resizable pane showing the area around the cursor.
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FocusShape {
    #[default]
    Circle,
    RoundRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RulerUnit {
    #[default]
    Px,
    Cm,
    Inch,
}

impl RulerUnit {
    pub const ALL: [RulerUnit; 3] = [RulerUnit::Px, RulerUnit::Cm, RulerUnit::Inch];

    pub fn suffix(self) -> &'static str {
        match self {
            RulerUnit::Px => "px",
            RulerUnit::Cm => "cm",
            RulerUnit::Inch => "in",
        }
    }

    pub fn next(self) -> RulerUnit {
        match self {
            RulerUnit::Px => RulerUnit::Cm,
            RulerUnit::Cm => RulerUnit::Inch,
            RulerUnit::Inch => RulerUnit::Px,
        }
    }
}

/// Ruler calibration of one monitor: its physical diagonal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorCalibration {
    /// GDI device name (`\\.\DISPLAY1`) plus resolution, e.g. `\\.\DISPLAY1@2560x1440`,
    /// so a different panel on the same output is not mistaken for this one.
    pub monitor: String,
    pub diagonal_in: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsSettings {
    // ---- color picker
    pub color_format: ColorFormat,
    pub color_prefix: String,
    pub color_suffix: String,
    /// Hex digits in lower case (`ff8000`) for the Hex, HTML and Delphi formats.
    pub color_hex_lowercase: bool,
    /// Most recent first, 0xRRGGBB, at most [`MAX_RECENT_COLORS`].
    pub recent_colors: Vec<u32>,
    /// Loupe zoom (screen pixels per source pixel at 96 DPI).
    pub picker_zoom: u32,
    /// Open the fine-tune panel after a pick.
    pub picker_show_panel: bool,

    // ---- magnifier
    /// 2..=16.
    pub magnifier_zoom: u32,
    pub magnifier_smooth: bool,
    pub magnifier_negative: bool,
    pub magnifier_mode: MagnifierMode,
    /// Lens diameter in logical pixels (96 DPI).
    pub magnifier_size: u32,
    /// Pane placement [x, y, w, h] in physical pixels.
    pub magnifier_pane: Option<[i32; 4]>,

    // ---- focus
    /// Spotlight radius (circle) or half-height (rounded rect), logical pixels.
    pub focus_size: u32,
    pub focus_shape: FocusShape,
    /// Dim opacity in percent (0 = no dimming, 100 = black).
    pub focus_opacity: u8,

    // ---- ruler
    pub ruler_units: RulerUnit,
    pub ruler_calibration: Vec<MonitorCalibration>,
    pub ruler_auto_copy: bool,
    pub ruler_vertical: bool,
    /// Ruler length in logical pixels.
    pub ruler_length: u32,
    /// Background opacity in percent (30..=100).
    pub ruler_opacity: u8,
}

impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            color_format: ColorFormat::Hex,
            color_prefix: String::new(),
            color_suffix: String::new(),
            color_hex_lowercase: false,
            recent_colors: Vec::new(),
            picker_zoom: 10,
            picker_show_panel: true,
            magnifier_zoom: 4,
            magnifier_smooth: false,
            magnifier_negative: false,
            magnifier_mode: MagnifierMode::Lens,
            magnifier_size: 280,
            magnifier_pane: None,
            focus_size: 160,
            focus_shape: FocusShape::Circle,
            focus_opacity: 65,
            ruler_units: RulerUnit::Px,
            ruler_calibration: Vec::new(),
            ruler_auto_copy: false,
            ruler_vertical: false,
            ruler_length: 640,
            ruler_opacity: 88,
        }
    }
}

impl ToolsSettings {
    /// Remember a picked color (most recent first, no duplicates, capped).
    pub fn push_recent_color(&mut self, rgb: u32) {
        let rgb = rgb & 0x00FF_FFFF;
        self.recent_colors.retain(|&c| c != rgb);
        self.recent_colors.insert(0, rgb);
        self.recent_colors.truncate(MAX_RECENT_COLORS);
    }

    /// Stored diagonal for a monitor key, if calibrated.
    pub fn calibration_for(&self, monitor: &str) -> Option<f64> {
        self.ruler_calibration.iter().find(|c| c.monitor == monitor).map(|c| c.diagonal_in)
    }

    /// Store (or with `None`, forget) a monitor's diagonal.
    pub fn set_calibration(&mut self, monitor: &str, diagonal_in: Option<f64>) {
        self.ruler_calibration.retain(|c| c.monitor != monitor);
        if let Some(d) = diagonal_in {
            self.ruler_calibration.push(MonitorCalibration { monitor: monitor.to_string(), diagonal_in: d });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_colors_are_mru_unique_and_capped() {
        let mut s = ToolsSettings::default();
        for c in 0..20u32 {
            s.push_recent_color(c);
        }
        assert_eq!(s.recent_colors.len(), MAX_RECENT_COLORS);
        assert_eq!(s.recent_colors[0], 19);
        s.push_recent_color(10);
        assert_eq!(s.recent_colors[0], 10);
        assert_eq!(s.recent_colors.iter().filter(|&&c| c == 10).count(), 1);
        s.push_recent_color(0xFF12_3456);
        assert_eq!(s.recent_colors[0], 0x12_3456);
    }

    #[test]
    fn calibration_store() {
        let mut s = ToolsSettings::default();
        s.set_calibration("A", Some(27.0));
        s.set_calibration("B", Some(15.6));
        s.set_calibration("A", Some(24.0));
        assert_eq!(s.calibration_for("A"), Some(24.0));
        assert_eq!(s.ruler_calibration.len(), 2);
        s.set_calibration("A", None);
        assert_eq!(s.calibration_for("A"), None);
    }

    #[test]
    fn toml_roundtrip() {
        let mut s = ToolsSettings::default();
        s.push_recent_color(0xFF8000);
        s.set_calibration("\\\\.\\DISPLAY1@2560x1440", Some(27.0));
        let text = toml::to_string_pretty(&s).expect("serialize");
        let back: ToolsSettings = toml::from_str(&text).expect("parse");
        assert_eq!(s, back);
        let partial: ToolsSettings = toml::from_str("color_format = \"delphi\"\n").expect("parse");
        assert_eq!(partial.color_format, ColorFormat::Delphi);
        assert_eq!(partial.magnifier_zoom, 4);
    }
}
