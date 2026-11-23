//! Settings for the capture history (`src/caplog.rs`, Tools ▸ Capture History).
//! Keep `#[serde(default)]` so older settings files still load.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    /// Remember every capture in the history.
    pub enabled: bool,
    /// How many captures to keep (older ones are dropped). Clamped to 5..=500 when used.
    pub keep: u32,
    /// Recognize the text of the captures (while the history window is open) so the search
    /// finds them by their contents.
    pub index_text: bool,
}

impl HistorySettings {
    pub const KEEP_MIN: u32 = 5;
    pub const KEEP_MAX: u32 = 500;

    /// [`Self::keep`] within its valid range.
    pub fn keep_clamped(&self) -> usize {
        self.keep.clamp(Self::KEEP_MIN, Self::KEEP_MAX) as usize
    }
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self { enabled: true, keep: 50, index_text: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_fills_defaults() {
        let s: HistorySettings = toml::from_str("keep = 20\n").expect("parse");
        assert_eq!(s.keep, 20);
        assert!(s.enabled);
        assert!(s.index_text);
    }

    #[test]
    fn keep_is_clamped_when_used() {
        let mut s = HistorySettings::default();
        assert_eq!(s.keep_clamped(), 50);
        s.keep = 0;
        assert_eq!(s.keep_clamped(), 5);
        s.keep = 100_000;
        assert_eq!(s.keep_clamped(), 500);
    }

    #[test]
    fn older_settings_files_get_the_section() {
        let s: crate::settings::Settings = toml::from_str("").expect("parse");
        assert_eq!(s.history, HistorySettings::default());
    }
}
