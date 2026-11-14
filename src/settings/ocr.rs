//! Settings for Phase 5 capture text (OCR). Owned by that module's agent: add fields here
//! (keep `#[serde(default)]` so older settings files still load).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrSettings {
    /// BCP-47 tag of the recognizer language (e.g. "en-US"); empty = the user's profile languages.
    pub language: String,
    /// Copy the recognized text to the clipboard as soon as it is ready.
    pub auto_copy: bool,
    /// Keep the line breaks of the recognized text (off = join lines with spaces).
    pub keep_line_breaks: bool,
    /// Last result-window placement [x, y, w, h] in physical pixels.
    pub window: Option<[i32; 4]>,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self { language: String::new(), auto_copy: false, keep_line_breaks: true, window: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_fills_defaults() {
        let s: OcrSettings = toml::from_str("auto_copy = true\n").expect("parse");
        assert!(s.auto_copy);
        assert!(s.keep_line_breaks);
        assert!(s.language.is_empty());
    }
}
