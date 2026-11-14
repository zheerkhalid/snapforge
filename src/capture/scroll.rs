//! Scrolling capture: auto-scroll a window or area and stitch the frames.
//!
//! CONTRACT: [`run`] is blocking and must be called from a dedicated (non-UI) thread.
//! It creates its own small indicator window ("Scrolling… Esc or click to stop",
//! excluded from capture) on that thread and pumps messages while it works.

use super::{CaptureError, GrabOptions};
use crate::common::{Image, Rect};
use crate::settings::ScrollSettings;

#[derive(Debug, Clone)]
pub struct ScrollTarget {
    /// Screen area to capture each step (window client area or user-selected area).
    pub rect: Rect,
    /// Window that receives the scroll input, if known.
    pub hwnd: Option<isize>,
}

pub fn run(target: ScrollTarget, settings: &ScrollSettings, opts: &GrabOptions) -> Result<Image, CaptureError> {
    let _ = (target, settings, opts);
    Err(CaptureError::Failed("not implemented".into()))
}
