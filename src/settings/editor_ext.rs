//! Phase 2 editor settings: workspaces, undo memory, canvas background, scanning.
//! (`#[serde(default)]` everywhere so older settings files still load.)

use serde::{Deserialize, Serialize};

/// A named group of tabs that remembers its own last folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    /// Stable id (never reused within a settings file).
    pub id: u64,
    pub name: String,
    pub last_folder: String,
    /// Files open in this workspace, in tab order (restored when the editor starts).
    pub open_files: Vec<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self { id: 1, name: "Main".into(), last_folder: String::new(), open_files: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanSettings {
    pub deskew: bool,
    pub auto_contrast: bool,
    pub auto_crop: bool,
    /// Keep scanning (one tab per page) until the scan dialog is cancelled.
    pub batch: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self { deskew: true, auto_contrast: false, auto_crop: true, batch: false }
    }
}

/// How Hide Private Details covers what it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactStyle {
    #[default]
    Pixelate,
    Blur,
    /// A solid black bar (nothing of the text survives).
    Bar,
}

/// Last choices of Tools ▸ Hide Private Details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactSettings {
    pub email: bool,
    pub phone: bool,
    pub card: bool,
    pub style: RedactStyle,
}

impl Default for RedactSettings {
    fn default() -> Self {
        Self { email: true, phone: true, card: true, style: RedactStyle::Pixelate }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorExtSettings {
    /// Show the workspace strip above the tabs.
    pub workspaces_enabled: bool,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: u64,
    /// Reopen each workspace's files when the editor starts.
    pub restore_workspaces: bool,
    /// Uncompressed undo memory per document before old steps are compressed (MB).
    pub undo_raw_mb: u32,
    /// Hard cap on undo memory per document (MB); the oldest steps are dropped beyond it.
    pub undo_total_mb: u32,
    /// Straight RGBA fill for uncovered areas (moved selections, canvas growth).
    pub background: [u8; 4],
    pub scan: ScanSettings,
    /// Last "Save All with sequence" choices.
    pub sequence_base: String,
    pub sequence_start: u32,
    pub sequence_digits: u32,
    pub sequence_folder: String,
    pub redact: RedactSettings,
}

impl Default for EditorExtSettings {
    fn default() -> Self {
        Self {
            workspaces_enabled: false,
            workspaces: vec![Workspace::default()],
            active_workspace: 1,
            restore_workspaces: true,
            undo_raw_mb: 384,
            undo_total_mb: 1536,
            background: [255, 255, 255, 255],
            scan: ScanSettings::default(),
            sequence_base: "Image".into(),
            sequence_start: 1,
            sequence_digits: 3,
            sequence_folder: String::new(),
            redact: RedactSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial() {
        let s = EditorExtSettings::default();
        let text = toml::to_string_pretty(&s).expect("ser");
        let back: EditorExtSettings = toml::from_str(&text).expect("de");
        assert_eq!(s, back);
        let p: EditorExtSettings =
            toml::from_str("workspaces_enabled = true\n[[workspaces]]\nname = \"A\"\n").expect("de");
        assert!(p.workspaces_enabled);
        assert_eq!(p.workspaces[0].name, "A");
        assert_eq!(p.undo_raw_mb, 384);
        assert_eq!(p.redact, RedactSettings::default());
        let r: EditorExtSettings = toml::from_str("[redact]\nphone = false\nstyle = \"bar\"\n").expect("de");
        assert!(r.redact.email && !r.redact.phone);
        assert_eq!(r.redact.style, RedactStyle::Bar);
    }
}
