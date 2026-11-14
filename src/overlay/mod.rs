//! Full-screen capture overlays (frozen screenshot) and the delay countdown.
//!
//! CONTRACT:
//! * [`select`] runs modally on the calling thread (the UI thread) with a nested message
//!   loop, shows a topmost overlay over the whole virtual screen painted from the frozen
//!   screenshot, and returns when the user finishes or cancels.
//! * The overlay window should be created once and reused (hidden between uses) for speed.
//! * Behaviour per mode is documented on [`OverlayMode`]. Settings used:
//!   `capture.show_magnifier`, `capture.show_crosshair`, `capture.capture_on_release`,
//!   `capture.fixed_presets`, `capture.fixed_index` (persist when changed).

mod countdown;
pub mod logic;
pub mod loupe;
mod paint;
mod render;
pub mod scene;
mod window;

#[cfg(test)]
mod harness;

use crate::capture::WindowEntry;
use crate::common::{Image, Point, Rect};
use std::sync::atomic::{AtomicBool, AtomicI64};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    /// Drag a rectangle (magnifier loupe, size/position readout, Tab switches the dragged
    /// corner, arrows nudge 1 px, Ctrl+Z / right-click reselects, Esc cancels). Clicking
    /// without dragging selects the window under the cursor.
    Region,
    /// Draw a freehand outline.
    Freehand,
    /// Hover-highlight windows/objects; wheel changes nesting level; Ctrl+click
    /// multi-selects; click (or Enter) finishes.
    WindowObject,
    /// Fixed-size rectangle follows the cursor; Shift+arrows resize; Space/wheel cycles presets.
    Fixed,
    /// Pick the target for a scrolling capture: click a window/control, or drag an area.
    ScrollPick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Rect(Rect),
    Polygon(Vec<Point>),
    Multi(Vec<Rect>),
    Window { hwnd: isize, rect: Rect },
}

/// The frozen screen the overlay paints. `image` covers `origin` (the virtual screen).
pub struct Frozen {
    pub image: Image,
    pub origin: Rect,
    /// Snapshot of top-level windows at freeze time (front first).
    pub windows: Vec<WindowEntry>,
}

/// Bench support: when set, [`select`] cancels itself right after its first paint.
pub static BENCH_AUTOCANCEL: AtomicBool = AtomicBool::new(false);
/// Bench support: QPC timestamp of the most recent overlay first paint.
pub static LAST_FIRST_PAINT_QPC: AtomicI64 = AtomicI64::new(0);

pub fn select(frozen: &Frozen, mode: OverlayMode) -> Option<Selection> {
    window::select(frozen, mode)
}

/// Produce the final image for a selection from the frozen screen: crop for rectangles
/// and windows; anti-aliased transparent mask outside a polygon; transparent outside
/// the union of rectangles for multi-selection (bounding box size).
pub fn render_selection(frozen: &Frozen, sel: &Selection) -> Image {
    render::render_selection(frozen, sel)
}

/// Modal on-screen countdown (excluded from capture). Returns false if cancelled (Esc).
pub fn countdown(seconds: u32) -> bool {
    countdown::countdown(seconds)
}

/// The overlay window handle (0 before the first [`select`]); pass it to
/// `capture::snapshot_windows` as an exclusion.
pub fn overlay_hwnd() -> isize {
    window::overlay_hwnd()
}

/// Free the cached screen-sized bitmaps kept between overlays (about 8 bytes per
/// virtual-screen pixel). The next [`select`] re-allocates them.
pub fn release_idle_resources() {
    window::release_idle_resources()
}
