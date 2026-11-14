//! Notification-area (tray) icon and menu.
//!
//! CONTRACT: called on the UI thread. Owns its own hidden message window. Menu actions
//! are sent with `crate::app::post(AppCommand::…)`. Re-adds the icon when Explorer
//! restarts ("TaskbarCreated"). Uses the EXE's icon resource id 1.

use anyhow::Result;

pub fn create() -> Result<()> {
    Ok(())
}

/// Show a balloon/toast-style notification from the tray icon.
pub fn notify(title: &str, text: &str, is_error: bool) {
    let _ = (title, text, is_error);
}

/// Settings changed (tooltip, checked menu states are read when the menu opens).
pub fn refresh() {}

pub fn remove() {}
