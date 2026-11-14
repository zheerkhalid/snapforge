//! Image editor (egui/eframe, glow backend) on its own thread.
//!
//! CONTRACT:
//! * The editor thread and window are created lazily on first use, hidden (not destroyed)
//!   when closed, and reused, so the second open is instant.
//! * All functions are callable from any thread and never block.

use crate::common::Image;
use crate::settings::Destination;
use std::path::PathBuf;
use std::sync::atomic::AtomicI64;

/// Bench support: QPC timestamp of the first frame presented after the most recent
/// [`open_image`] call.
pub static LAST_IMAGE_PRESENTED_QPC: AtomicI64 = AtomicI64::new(0);

/// Open an image in a new tab and bring the editor to the front.
/// `continue_to`: when previewing, show a "Continue to <dest>" button that sends the
/// (possibly edited) image on via `AppCommand::Deliver`.
pub fn open_image(img: Image, title: String, path: Option<PathBuf>, continue_to: Option<Destination>) {
    let _ = (img, title, path, continue_to);
}

/// Open files from disk in tabs.
pub fn open_files(paths: Vec<PathBuf>) {
    let _ = paths;
}

/// Show the editor (empty if no tabs).
pub fn show() {}

pub fn is_running() -> bool {
    false
}

/// Ask the editor thread to exit (app shutdown).
pub fn shutdown() {}
