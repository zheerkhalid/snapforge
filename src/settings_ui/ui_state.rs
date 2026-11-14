//! Window placement and other UI-only state, kept out of `settings.toml` in
//! `<settings dir>\ui_state.toml`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowState {
    /// Outer position and inner size in egui points: [x, y, w, h].
    pub rect: Option<[f32; 4]>,
    /// Tab id shown last.
    pub tab: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UiState {
    pub settings_window: WindowState,
}

pub fn path() -> PathBuf {
    crate::settings::dir().join("ui_state.toml")
}

pub fn load_from(p: &Path) -> UiState {
    std::fs::read_to_string(p).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_to(s: &UiState, p: &Path) -> anyhow::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, toml::to_string_pretty(s)?)?;
    Ok(())
}

pub fn load() -> UiState {
    load_from(&path())
}

pub fn save(s: &UiState) {
    if let Err(e) = save_to(s, &path()) {
        log::warn!("settings window: could not save ui_state.toml: {e:#}");
    }
}

/// A saved rectangle that is plausible to restore (finite, positive size, not far off-screen).
pub fn sane_rect(r: [f32; 4], min: [f32; 2]) -> Option<[f32; 4]> {
    let [x, y, w, h] = r;
    let ok = r.iter().all(|v| v.is_finite()) && x > -20_000.0 && y > -20_000.0 && x < 20_000.0 && y < 20_000.0;
    ok.then_some([x, y, w.max(min[0]), h.max(min[1])])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let dir = std::env::temp_dir().join(format!("snapforge-uistate-{}", std::process::id()));
        let p = dir.join("ui_state.toml");
        assert_eq!(load_from(&p), UiState::default());
        let s =
            UiState { settings_window: WindowState { rect: Some([10.0, 20.0, 800.0, 600.0]), tab: "hotkeys".into() } };
        save_to(&s, &p).expect("save");
        assert_eq!(load_from(&p), s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanity() {
        assert_eq!(sane_rect([1.0, 2.0, 10.0, 10.0], [100.0, 50.0]), Some([1.0, 2.0, 100.0, 50.0]));
        assert_eq!(sane_rect([f32::NAN, 2.0, 10.0, 10.0], [1.0, 1.0]), None);
        assert_eq!(sane_rect([-32000.0, -32000.0, 10.0, 10.0], [1.0, 1.0]), None, "minimized placement");
    }
}
