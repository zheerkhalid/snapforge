//! Floating capture toolbar (native Win32, owner-drawn, excluded from captures).
//!
//! CONTRACT: called on the UI thread. Buttons and menus send `crate::app::post(…)`.
//! Persists position/options in `settings.toolbar`.

use anyhow::Result;

pub fn create() -> Result<()> {
    Ok(())
}

pub fn show(visible: bool) {
    let _ = visible;
}

pub fn toggle() {}

pub fn is_visible() -> bool {
    false
}

/// Settings changed (destination icon, delay, visible buttons, size…): relayout/repaint.
pub fn refresh() {}

/// Toolbar window handle (0 if not created), so captures can exclude it.
pub fn hwnd() -> isize {
    0
}
