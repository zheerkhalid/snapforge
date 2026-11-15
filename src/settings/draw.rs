//! Settings for Phase 3 draw/annotation tools (keep `#[serde(default)]` so older settings files
//! still load): style presets per tool, preset and recent colors, default tool and the last style
//! used with each tool.

use crate::draw::{default_style, Color, Style, Tool, ToolOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How many recent colors are remembered.
pub const MAX_RECENT_COLORS: usize = 16;

/// A tool's style and options (a preset's payload).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolStyle {
    pub style: Style,
    pub options: ToolOptions,
}

impl ToolStyle {
    /// Built-in defaults for a tool.
    pub fn for_tool(tool: Tool) -> Self {
        Self { style: default_style(tool), options: ToolOptions::for_tool(tool) }
    }
}

/// A named, saved style for one tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    pub name: String,
    pub tool: Tool,
    pub style: Style,
    pub options: ToolOptions,
}

impl Default for Preset {
    fn default() -> Self {
        Self { name: String::new(), tool: Tool::Arrow, style: Style::default(), options: ToolOptions::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawSettings {
    /// Tool selected when the draw editor opens.
    pub default_tool: Tool,
    /// Palette shown in color pickers.
    pub preset_colors: Vec<Color>,
    /// Most recently used colors, newest first.
    pub recent_colors: Vec<Color>,
    /// Last style used with each tool, keyed by [`Tool::id`].
    pub last_styles: BTreeMap<String, ToolStyle>,
    /// Saved named presets.
    pub presets: Vec<Preset>,
    /// Offset applied when pasting or duplicating annotations, in pixels.
    pub paste_offset: f32,
    /// When an annotated image is saved, also save an editable `.sfcap` project next to it
    /// (or in `<settings>/autosave` if that folder is not writable).
    pub autosave_project: bool,
}

impl Default for DrawSettings {
    fn default() -> Self {
        Self {
            default_tool: Tool::Arrow,
            preset_colors: vec![
                Color::RED,
                Color::ORANGE,
                Color::YELLOW,
                Color::GREEN,
                Color::rgb(0x00, 0xAC, 0xC1),
                Color::BLUE,
                Color::PURPLE,
                Color::rgb(0xD8, 0x1B, 0x60),
                Color::rgb(0x6D, 0x4C, 0x41),
                Color::WHITE,
                Color::rgb(0x9E, 0x9E, 0x9E),
                Color::rgb(0x1A, 0x1A, 0x1A),
            ],
            recent_colors: Vec::new(),
            last_styles: BTreeMap::new(),
            presets: Vec::new(),
            paste_offset: 12.0,
            autosave_project: true,
        }
    }
}

impl DrawSettings {
    /// Style and options to start a new object with: the last used, else the built-in default.
    pub fn style_for(&self, tool: Tool) -> ToolStyle {
        self.last_styles.get(tool.id()).cloned().unwrap_or_else(|| ToolStyle::for_tool(tool))
    }

    /// Remember the style last used with a tool.
    pub fn remember(&mut self, tool: Tool, style: ToolStyle) {
        self.last_styles.insert(tool.id().to_string(), style);
    }

    /// Move a color to the front of the recent list (deduplicated, capped).
    pub fn push_recent_color(&mut self, c: Color) {
        self.recent_colors.retain(|x| *x != c);
        self.recent_colors.insert(0, c);
        self.recent_colors.truncate(MAX_RECENT_COLORS);
    }

    /// Saved presets for one tool.
    pub fn presets_for(&self, tool: Tool) -> impl Iterator<Item = &Preset> {
        self.presets.iter().filter(move |p| p.tool == tool)
    }

    /// Add or replace (by tool and name) a preset.
    pub fn save_preset(&mut self, preset: Preset) {
        match self.presets.iter_mut().find(|p| p.tool == preset.tool && p.name == preset.name) {
            Some(p) => *p = preset,
            None => self.presets.push(preset),
        }
    }

    pub fn delete_preset(&mut self, tool: Tool, name: &str) -> bool {
        let n = self.presets.len();
        self.presets.retain(|p| !(p.tool == tool && p.name == name));
        self.presets.len() != n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip_with_presets() {
        let mut s = DrawSettings::default();
        s.push_recent_color(Color::RED);
        s.push_recent_color(Color::BLUE);
        s.push_recent_color(Color::RED);
        assert_eq!(s.recent_colors, vec![Color::RED, Color::BLUE]);
        let mut ts = ToolStyle::for_tool(Tool::Callout);
        ts.style.outline = Some(crate::draw::Outline::default());
        ts.options.eraser_background = Some(Color::WHITE);
        s.remember(Tool::Callout, ts.clone());
        s.save_preset(Preset { name: "Big red".into(), tool: Tool::Arrow, ..Default::default() });
        let text = toml::to_string_pretty(&s).expect("serialize");
        let back: DrawSettings = toml::from_str(&text).expect("parse");
        assert_eq!(back, s);
        assert_eq!(back.style_for(Tool::Callout), ts);
        assert_eq!(back.style_for(Tool::Step), ToolStyle::for_tool(Tool::Step));
        assert_eq!(back.presets_for(Tool::Arrow).count(), 1);
    }

    #[test]
    fn empty_table_loads_defaults() {
        let s: DrawSettings = toml::from_str("").expect("parse");
        assert_eq!(s, DrawSettings::default());
    }
}
