//! Scrolling capture: auto-scroll a window or area and stitch the frames.
//!
//! CONTRACT: [`run`] is blocking and must be called from a dedicated (non-UI) thread.
//! It creates its own small indicator window ("Scrolling… Esc or click to stop",
//! excluded from capture) on that thread and pumps messages while it works.
//!
//! # How it works
//!
//! The loop ([`run_loop`]) is platform-independent and talks to the screen through a
//! [`ScrollDriver`]; the real driver (Win32: `SendInput` wheel, `WM_VSCROLL` fallback,
//! `GetAsyncKeyState`, indicator window) lives in `scroll_win.rs`, and the unit tests use a
//! fake driver that serves a synthetic page.
//!
//! * With `start_at_top` (the default) the view is first scrolled to the top (and, for
//!   `Both`, the left) with large wheel bursts until it stops moving, so the whole page is
//!   captured. Without it, capture starts at the current scroll position.
//! * Each step is adaptive: the first step sends one wheel notch and measures how many
//!   pixels it moved; later steps send enough notches to move about 75 % of the scrolling
//!   part of the frame (sticky header and footer excluded). If the wheel never moves
//!   anything and the target window is known, the loop falls back to
//!   `WM_VSCROLL`/`WM_HSCROLL` `SB_PAGEDOWN`/`SB_PAGERIGHT`.
//! * After every step the loop waits for the screen to settle: at least `step_delay_ms`,
//!   then until the content has started to move (many apps, browsers especially, apply
//!   wheel input a frame or more late, and smooth scrolling starts after that), then until
//!   it has stopped moving (several identical grabs in a row). Deciding too early made a
//!   late scroll look like "the wheel does nothing", and the next step then jumped a whole
//!   screen, leaving no overlap to stitch.
//! * It stops when the content did not move twice in a row (end reached), at
//!   `max_screens` frames, after three mismatches in a row, or when the user presses Esc or
//!   clicks. A stop by the user keeps what was captured; only if nothing beyond the first
//!   screen was captured does [`run`] return [`CaptureError::Cancelled`].
//! * `Both`: the page is captured as vertical columns. After each column the loop scrolls
//!   back up by the amount it went down, steps right, and captures the next column; the
//!   columns are joined horizontally with the same matcher ([`stitch::join_columns`]).

use super::stitch::{self, Axis, MismatchPolicy, PushResult, StitchOptions, Stitcher};
use super::{CaptureError, GrabOptions};
use crate::common::{Image, Rect};
use crate::settings::{ScrollDirection, ScrollSettings};

#[path = "scroll_win.rs"]
mod win;

#[derive(Debug, Clone)]
pub struct ScrollTarget {
    /// Screen area to capture each step (window client area or user-selected area).
    pub rect: Rect,
    /// Window that receives the scroll input, if known.
    pub hwnd: Option<isize>,
}

pub fn run(target: ScrollTarget, settings: &ScrollSettings, opts: &GrabOptions) -> Result<Image, CaptureError> {
    if target.rect.is_empty() {
        return Err(CaptureError::Empty);
    }
    // The cursor sits over the target while scrolling; never bake it into the frames.
    let opts = GrabOptions { include_cursor: false, ..*opts };
    let mut driver = win::Win32Driver::new(&target, opts);
    let cfg = LoopConfig::new(settings, driver.ignore_margin());
    log::info!("scrolling capture: {:?} {:?}, {:?}", target.rect, target.hwnd, settings.direction);
    let t0 = crate::common::time::qpc_now();
    let result = run_loop(&mut driver, &cfg);
    drop(driver); // restores the cursor and closes the indicator
    let ms = crate::common::time::qpc_ms(t0, crate::common::time::qpc_now());
    match &result {
        Ok(img) => log::info!("scrolling capture done: {}x{} in {ms:.0} ms", img.width, img.height),
        Err(e) => log::info!("scrolling capture ended: {e}"),
    }
    result
}

/// Platform interactions of the scroll loop.
pub trait ScrollDriver {
    /// Capture the target area.
    fn grab(&mut self) -> Result<Image, CaptureError>;
    /// Send `notches` wheel notches; positive scrolls towards the end (down / right).
    fn wheel(&mut self, axis: Axis, notches: i32);
    /// Whether [`page`](Self::page) can work (a target window is known).
    fn can_page(&self) -> bool;
    /// Scroll one page with `WM_VSCROLL` / `WM_HSCROLL`.
    fn page(&mut self, axis: Axis, forward: bool);
    /// True once the user asked to stop (Esc, click). Also pumps window messages.
    fn cancelled(&mut self) -> bool;
    /// Wait, keeping the indicator responsive.
    fn sleep_ms(&mut self, ms: u32);
    /// Monotonic clock in milliseconds.
    fn now_ms(&mut self) -> f64;
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub direction: ScrollDirection,
    pub max_screens: u32,
    pub step_delay_ms: u32,
    /// Poll interval while waiting for the screen to settle.
    pub settle_poll_ms: u32,
    /// Give up waiting for a stable screen after this long (then use the last grab).
    pub settle_timeout_ms: u32,
    /// After a scroll step, wait this long at most for the content to start moving.
    pub move_timeout_ms: u32,
    /// Consecutive identical grabs (after the first) that count as "at rest".
    pub stable_grabs: u32,
    /// Scroll to the start of the page first.
    pub start_at_top: bool,
    /// Fraction of the scrolling extent to move per step.
    pub step_fraction: f64,
    pub max_notches: i32,
    /// Stop after this many mismatches in a row.
    pub max_mismatches: u32,
    pub stitch: StitchOptions,
}

impl LoopConfig {
    pub fn new(s: &ScrollSettings, ignore_margin: u32) -> Self {
        Self {
            direction: s.direction,
            max_screens: s.max_screens.max(1),
            step_delay_ms: s.step_delay_ms,
            settle_poll_ms: 30,
            settle_timeout_ms: 2500,
            move_timeout_ms: 800,
            stable_grabs: 2,
            start_at_top: s.start_at_top,
            step_fraction: 0.75,
            max_notches: 60,
            max_mismatches: 3,
            stitch: StitchOptions { ignore_margin, mismatch: MismatchPolicy::AppendWithSeparator },
        }
    }
}

/// Run the whole scrolling capture with `d`.
pub fn run_loop<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig) -> Result<Image, CaptureError> {
    if d.cancelled() {
        return Err(CaptureError::Cancelled);
    }
    if cfg.start_at_top {
        match cfg.direction {
            ScrollDirection::Vertical => to_start(d, cfg, Axis::Vertical)?,
            ScrollDirection::Horizontal => to_start(d, cfg, Axis::Horizontal)?,
            ScrollDirection::Both => {
                to_start(d, cfg, Axis::Vertical)?;
                to_start(d, cfg, Axis::Horizontal)?;
            }
        }
        if d.cancelled() {
            return Err(CaptureError::Cancelled);
        }
    }
    let first = match settle(d, cfg, 0)? {
        Some(f) => f,
        None => return Err(CaptureError::Cancelled),
    };
    if first.is_empty() {
        return Err(CaptureError::Empty);
    }
    match cfg.direction {
        ScrollDirection::Vertical | ScrollDirection::Horizontal => {
            let axis = if cfg.direction == ScrollDirection::Vertical { Axis::Vertical } else { Axis::Horizontal };
            let r = capture_axis(d, cfg, axis, first, cfg.max_screens);
            if r.cancelled && r.added == 0 {
                return Err(CaptureError::Cancelled);
            }
            Ok(r.image)
        }
        ScrollDirection::Both => run_both(d, cfg, first),
    }
}

/// Grab until the screen is at rest, after waiting at least `min_delay` ms. `None` when
/// cancelled meanwhile.
fn settle<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, min_delay: u32) -> Result<Option<Image>, CaptureError> {
    if min_delay > 0 {
        d.sleep_ms(min_delay);
    }
    let first = d.grab()?;
    rest(d, cfg, first)
}

/// After a scroll step: wait at least `step_delay_ms`, then until the content has moved away
/// from `before` (or `move_timeout_ms` passed), then until it is at rest.
fn settle_after<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, before: &Image) -> Result<Option<Image>, CaptureError> {
    d.sleep_ms(cfg.step_delay_ms);
    let start = d.now_ms();
    let mut cur = d.grab()?;
    while !moved(before, &cur, cfg.stitch.ignore_margin) {
        if d.cancelled() {
            return Ok(None);
        }
        if d.now_ms() - start >= cfg.move_timeout_ms as f64 {
            return Ok(Some(cur));
        }
        d.sleep_ms(cfg.settle_poll_ms);
        cur = d.grab()?;
    }
    rest(d, cfg, cur)
}

/// Re-grab until `stable_grabs` grabs in a row equal the one before (or the timeout passes).
fn rest<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, mut a: Image) -> Result<Option<Image>, CaptureError> {
    let start = d.now_ms();
    let mut same = 0;
    loop {
        if d.cancelled() {
            return Ok(None);
        }
        d.sleep_ms(cfg.settle_poll_ms);
        let b = d.grab()?;
        if a == b {
            same += 1;
            if same >= cfg.stable_grabs.max(1) {
                return Ok(Some(b));
            }
        } else {
            same = 0;
        }
        if d.now_ms() - start >= cfg.settle_timeout_ms as f64 {
            log::debug!("scroll: screen did not settle, using the last grab");
            return Ok(Some(b));
        }
        a = b;
    }
}

/// Whether the content moved between two grabs: more than a few rows differ, so a blinking
/// caret or a small hover effect does not count. Columns within `margin` of the left and
/// right edges (the scrollbar) are left out. A mostly blank page may not reach the
/// threshold; that only costs the move timeout, the stitcher still decides what moved.
pub(crate) fn moved(a: &Image, b: &Image, margin: u32) -> bool {
    if a.width != b.width || a.height != b.height {
        return true;
    }
    let (w, h) = (a.width as usize, a.height as usize);
    if w == 0 || h == 0 {
        return false;
    }
    let m = (margin as usize).min(w / 4);
    let row = w * 4;
    let changed = (0..h)
        .filter(|&y| {
            let r = y * row + m * 4..(y + 1) * row - m * 4;
            a.data[r.clone()] != b.data[r]
        })
        .count();
    changed * 100 > h * 6 || changed >= 120
}

/// Scroll to the start of `axis` (top or left) with large wheel bursts until the view stops
/// moving. Views that the wheel does not scroll are left where they are.
fn to_start<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, axis: Axis) -> Result<(), CaptureError> {
    let mut before = d.grab()?;
    for _ in 0..40 {
        if d.cancelled() {
            return Ok(());
        }
        d.wheel(axis, -cfg.max_notches);
        let Some(after) = settle_after(d, cfg, &before)? else { return Ok(()) };
        if !moved(&before, &after, cfg.stitch.ignore_margin) {
            log::debug!("scroll {axis:?}: at the start");
            return Ok(());
        }
        before = after;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Wheel,
    Page,
}

/// Adaptive step size and the bookkeeping needed to scroll back (for `Both`).
struct Stepper {
    axis: Axis,
    method: Method,
    /// Measured pixels per wheel notch.
    ppn: Option<f64>,
    last_notches: i32,
    no_move: u32,
    undo_notches: i32,
    undo_pages: u32,
}

impl Stepper {
    fn new(axis: Axis) -> Self {
        Self { axis, method: Method::Wheel, ppn: None, last_notches: 0, no_move: 0, undo_notches: 0, undo_pages: 0 }
    }

    /// Scroll one step; returns the expected shift in pixels, if known.
    fn step<D: ScrollDriver>(&mut self, d: &mut D, cfg: &LoopConfig, scrolling_extent: u32) -> Option<u32> {
        match self.method {
            Method::Wheel => {
                let n = match self.ppn {
                    None => 1,
                    Some(p) => {
                        let want = scrolling_extent as f64 * cfg.step_fraction;
                        ((want / p).floor() as i32).clamp(1, cfg.max_notches)
                    }
                };
                self.last_notches = n;
                d.wheel(self.axis, n);
                self.ppn.map(|p| (p * n as f64).round() as u32)
            }
            Method::Page => {
                d.page(self.axis, true);
                None
            }
        }
    }

    /// Learn from the stitch result. Returns true when the end is reached.
    fn observe<D: ScrollDriver>(&mut self, d: &D, r: PushResult, hint: Option<u32>) -> bool {
        match r {
            PushResult::Added(px) => {
                self.no_move = 0;
                match self.method {
                    Method::Wheel => {
                        // A short step means the end of the page: don't learn from it.
                        let full = hint.map_or(true, |h| px as f64 >= 0.6 * h as f64);
                        if full {
                            self.ppn = Some(px as f64 / self.last_notches.max(1) as f64);
                            self.undo_notches += self.last_notches;
                        } else {
                            let p = self.ppn.unwrap_or(px as f64).max(1.0);
                            self.undo_notches += (px as f64 / p).round() as i32;
                        }
                    }
                    Method::Page => self.undo_pages += 1,
                }
                false
            }
            PushResult::Mismatch { .. } => {
                self.no_move = 0;
                match self.method {
                    Method::Wheel => self.undo_notches += self.last_notches,
                    Method::Page => self.undo_pages += 1,
                }
                false
            }
            PushResult::NoChange => {
                self.no_move += 1;
                if self.method == Method::Wheel && self.ppn.is_none() && d.can_page() {
                    log::info!("scroll: the wheel did not move the content, trying page scrolling");
                    self.method = Method::Page;
                }
                self.no_move >= 2
            }
        }
    }

    /// Scroll back by everything that moved (used between the columns of `Both`).
    fn undo<D: ScrollDriver>(&mut self, d: &mut D, cfg: &LoopConfig) {
        let mut left = self.undo_notches;
        while left > 0 {
            let n = left.min(cfg.max_notches);
            d.wheel(self.axis, -n);
            left -= n;
        }
        for _ in 0..self.undo_pages {
            d.page(self.axis, false);
        }
        self.undo_notches = 0;
        self.undo_pages = 0;
    }
}

struct AxisRun {
    image: Image,
    /// Frames kept by the stitcher (including the first).
    frames: u32,
    /// Pixels of new content beyond the first frame.
    added: u64,
    cancelled: bool,
    stepper: Stepper,
}

/// Capture along one axis, starting with the already grabbed `first` frame.
fn capture_axis<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, axis: Axis, first: Image, max_frames: u32) -> AxisRun {
    let mut st = Stitcher::with_options(axis, cfg.stitch);
    st.push(&first);
    let mut before = first;
    let mut stepper = Stepper::new(axis);
    let (mut added, mut cancelled, mut mismatches) = (0u64, false, 0u32);
    while st.frames() < max_frames {
        if d.cancelled() {
            cancelled = true;
            break;
        }
        let extent = st.scrolling_extent().unwrap_or(0);
        let hint = stepper.step(d, cfg, extent);
        let frame = match settle_after(d, cfg, &before) {
            Ok(Some(f)) => f,
            Ok(None) => {
                cancelled = true;
                break;
            }
            Err(e) => {
                log::warn!("scroll: grab failed, keeping what was captured: {e}");
                break;
            }
        };
        let r = st.push_hint(&frame, hint);
        before = frame;
        log::debug!("scroll {axis:?}: {r:?} (hint {hint:?}, extent {})", st.extent());
        if let PushResult::Added(px) = r {
            added += px as u64;
        }
        mismatches = if matches!(r, PushResult::Mismatch { .. }) { mismatches + 1 } else { 0 };
        if stepper.observe(d, r, hint) {
            log::debug!("scroll {axis:?}: end reached");
            break;
        }
        if matches!(r, PushResult::Mismatch { appended: false }) || mismatches >= cfg.max_mismatches {
            log::warn!("scroll {axis:?}: content does not line up, stopping");
            break;
        }
    }
    let frames = st.frames();
    AxisRun { image: st.finish(), frames, added, cancelled, stepper }
}

fn run_both<D: ScrollDriver>(d: &mut D, cfg: &LoopConfig, first: Image) -> Result<Image, CaptureError> {
    let mut columns: Vec<Image> = Vec::new();
    // Tracks the horizontal position using the top frame of each column.
    let mut tops = Stitcher::with_options(Axis::Horizontal, cfg.stitch);
    tops.push(&first);
    let mut h = Stepper::new(Axis::Horizontal);
    let (mut frames, mut added, mut cancelled) = (0u32, 0u64, false);
    let mut top = first;
    loop {
        let mut col = capture_axis(d, cfg, Axis::Vertical, top, cfg.max_screens.saturating_sub(frames).max(1));
        frames += col.frames;
        added += col.added;
        columns.push(std::mem::replace(&mut col.image, Image::new(0, 0)));
        if col.cancelled {
            cancelled = true;
            break;
        }
        if frames >= cfg.max_screens {
            break;
        }
        col.stepper.undo(d, cfg);
        // Back at the column's top: let the scroll-back finish before stepping right.
        let mut before = match settle(d, cfg, cfg.step_delay_ms)? {
            Some(f) => f,
            None => {
                cancelled = true;
                break;
            }
        };
        let mut next = None;
        while next.is_none() {
            if d.cancelled() {
                cancelled = true;
                break;
            }
            let hint = h.step(d, cfg, tops.scrolling_extent().unwrap_or(0));
            let frame = match settle_after(d, cfg, &before) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    cancelled = true;
                    break;
                }
                Err(e) => {
                    log::warn!("scroll: grab failed, keeping what was captured: {e}");
                    break;
                }
            };
            let r = tops.push_hint(&frame, hint);
            before = frame.clone();
            log::debug!("scroll both, next column: {r:?}");
            let end = h.observe(d, r, hint);
            match r {
                PushResult::Added(px) => {
                    added += px as u64;
                    next = Some(frame);
                }
                PushResult::Mismatch { appended: true } => next = Some(frame),
                PushResult::Mismatch { appended: false } => break,
                PushResult::NoChange => {}
            }
            if end {
                break;
            }
        }
        match next {
            Some(f) if !cancelled => top = f,
            _ => break,
        }
    }
    if cancelled && added == 0 {
        return Err(CaptureError::Cancelled);
    }
    Ok(stitch::join_columns(&columns, cfg.stitch))
}

#[cfg(test)]
#[path = "scroll_tests.rs"]
mod tests;
