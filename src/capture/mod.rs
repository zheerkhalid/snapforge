//! Screen capture backends and window enumeration.
//!
//! CONTRACT (used by app, overlay, scroll, bench; keep these signatures):
//! * All functions are blocking and callable from any thread. Implementations may keep
//!   a dedicated worker thread for D3D/WGC objects internally.
//! * Coordinates are physical pixels in virtual-screen space (process is PerMonitorV2).
//! * Returned images are premultiplied BGRA ([`Image`]), fully opaque for screen pixels.
//! * Backend order for `CaptureEngine::Auto`: WGC → DXGI Desktop Duplication → GDI BitBlt.

pub mod scroll;
pub mod stitch;

use crate::common::{Image, Point, Rect};
use crate::settings::CaptureEngine;

#[derive(Debug, Clone, Copy)]
pub struct GrabOptions {
    pub engine: CaptureEngine,
    pub include_cursor: bool,
}

impl GrabOptions {
    pub fn from_settings() -> Self {
        let s = crate::settings::get();
        Self { engine: s.capture.engine, include_cursor: s.capture.include_cursor }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("empty capture area")]
    Empty,
    #[error("the window no longer exists")]
    WindowGone,
    #[error("capture cancelled")]
    Cancelled,
    #[error("capture failed: {0}")]
    Failed(String),
}

/// A top-level window in the z-order snapshot taken when an overlay opens.
#[derive(Debug, Clone)]
pub struct WindowEntry {
    pub hwnd: isize,
    /// Visible bounds (DWM extended frame bounds), virtual-screen coordinates.
    pub rect: Rect,
    pub title: String,
    pub class: String,
}

/// Capture a rectangle of the virtual screen.
pub fn grab_rect(rect: Rect, opts: &GrabOptions) -> Result<Image, CaptureError> {
    let _ = (rect, opts);
    Err(CaptureError::Failed("not implemented".into()))
}

/// Capture a single window (even if partly covered, where WGC allows).
/// Returns the image and the window's visible bounds.
pub fn grab_window(hwnd: isize, opts: &GrabOptions) -> Result<(Image, Rect), CaptureError> {
    let _ = (hwnd, opts);
    Err(CaptureError::Failed("not implemented".into()))
}

/// Visible, non-cloaked top-level windows in z-order (front first), including menus
/// and tooltips; excludes `exclude` (our own windows) and the desktop/shell background.
pub fn snapshot_windows(exclude: &[isize]) -> Vec<WindowEntry> {
    let _ = exclude;
    Vec::new()
}

/// Nested element rectangles under `p` inside top-level window `top_hwnd`, from the
/// outermost (the window itself) to the innermost (child controls, then UI Automation
/// elements where available). Used by the window/object overlay (wheel changes level).
pub fn element_chain_at(top_hwnd: isize, p: Point) -> Vec<Rect> {
    let _ = (top_hwnd, p);
    Vec::new()
}

/// Release cached GPU/duplication resources (called when the app goes idle).
pub fn release_idle_resources() {}

/// Time each backend capturing `rect` (for `--bench`). Returns (name, ms or error).
pub fn bench_engines(rect: Rect) -> Vec<(&'static str, Result<f64, String>)> {
    let _ = rect;
    Vec::new()
}
