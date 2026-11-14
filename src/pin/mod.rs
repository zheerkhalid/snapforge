//! Pin to Screen: borderless, always-on-top image windows with zoom, opacity, Draw Mode
//! (pen, text, step stamps, undo) and canvas expand/crop.
//!
//! CONTRACT: every function here runs on the UI thread (the thread that runs the app's
//! message loop). Pins live in a thread-local registry, so calls from another thread see
//! no pins; worker threads should post `AppCommand::PinImage` instead.
//!
//! Mouse: drag moves; wheel zooms 10%–1000% around the cursor; Ctrl+wheel sets opacity;
//! Ctrl+drag on a border expands or crops the canvas; double-click or Esc closes;
//! right-click opens the menu. Keys: Space and Enter per `settings.pin`, D toggles Draw
//! Mode, Ctrl+Z undo, Ctrl+C copy, Ctrl+S save as, arrows nudge.

mod annot;
pub(crate) mod gdi;
mod geom;
mod keys;
mod paint;
mod palette;
#[cfg(test)]
mod preview;
mod window;

use crate::common::{Image, Rect};

/// Pin `img` to the screen. `at` is the image's screen rectangle (pinned exactly there,
/// 1 physical pixel per image pixel); `None` centres it on the cursor (scaled down to fit
/// very large images on the monitor).
pub fn pin_image(img: Image, at: Option<Rect>) {
    window::create(img, at);
}

/// Images of all open pins (with their annotations), oldest first.
pub fn pinned_images() -> Vec<Image> {
    window::all().into_iter().filter_map(window::image_of).collect()
}

/// Number of open pins.
pub fn count() -> usize {
    window::all().len()
}

/// Close every pin.
pub fn close_all() {
    for h in window::all() {
        window::close(h);
    }
}

/// Open every pin's image in the editor (one tab each). The pins stay open.
pub fn send_all_to_editor() {
    for h in window::all() {
        window::send_to_editor(h);
    }
}
