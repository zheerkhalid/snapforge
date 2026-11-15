//! Video editor data model: kept source segments, title cards, zoom keyframes and timed
//! annotations. Everything is plain serde data so the whole edit state can be snapshotted for
//! undo and written to a `.sfvid` project as is.
//!
//! Time convention: segments, zoom keyframes, title anchors and annotation timings are all in
//! **source** milliseconds (position in the original video). The output timeline (after cuts and
//! with title cards inserted) is derived from them by [`super::timeline::build`], so cutting a
//! range never shifts an annotation away from the content it points at.

use crate::draw::{Color, Object};
use serde::{Deserialize, Serialize};

/// Shortest segment or title card we keep, in milliseconds.
pub const MIN_ITEM_MS: u64 = 100;

/// A kept range of the source video, `[start_ms, end_ms)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Segment {
    pub const fn new(start_ms: u64, end_ms: u64) -> Self {
        Self { start_ms, end_ms }
    }
    pub fn len(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn contains(&self, t: u64) -> bool {
        t >= self.start_ms && t < self.end_ms
    }
}

/// Where a title card sits in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Before all video.
    Start,
    /// After all video.
    End,
    /// Just before this source time (if that time was cut, before the next kept frame).
    Source(u64),
}

/// Title card background.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Background {
    Solid {
        color: Color,
    },
    /// Linear gradient from `from` to `to`; `angle` in degrees (0 = left to right, 90 = top to
    /// bottom).
    Gradient {
        from: Color,
        to: Color,
        angle: f32,
    },
}

impl Default for Background {
    fn default() -> Self {
        Background::Gradient { from: Color::rgb(0x23, 0x2A, 0x6B), to: Color::rgb(0x4F, 0x6B, 0xFF), angle: 35.0 }
    }
}

/// A full-frame card with text, inserted into the output timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TitleCard {
    pub id: u64,
    pub anchor: Anchor,
    pub duration_ms: u64,
    pub title: String,
    pub subtitle: String,
    pub background: Background,
    pub text_color: Color,
    /// Title size in points (the subtitle is 45% of it). Scaled with the output height
    /// relative to 1080 lines.
    pub font_size: f32,
}

impl Default for TitleCard {
    fn default() -> Self {
        Self {
            id: 0,
            anchor: Anchor::Start,
            duration_ms: 3000,
            title: String::new(),
            subtitle: String::new(),
            background: Background::default(),
            text_color: Color::WHITE,
            font_size: 54.0,
        }
    }
}

/// A zoom & pan keyframe: the view reaches this state at `time_ms` (source time), moving from the
/// previous state over the `transition_ms` before it, and holds it until the next keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZoomKey {
    pub time_ms: u64,
    /// View centre as a fraction of the frame (0..1).
    pub cx: f32,
    pub cy: f32,
    /// Magnification (1 = whole frame).
    pub scale: f32,
    pub transition_ms: u64,
}

impl Default for ZoomKey {
    fn default() -> Self {
        Self { time_ms: 0, cx: 0.5, cy: 0.5, scale: 1.0, transition_ms: 600 }
    }
}

/// Everything the user edits (one undo step = one snapshot of this).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EditState {
    pub segments: Vec<Segment>,
    pub titles: Vec<TitleCard>,
    pub zoom: Vec<ZoomKey>,
    /// Draw objects in source pixel coordinates; `timing` is in source milliseconds.
    pub annotations: Vec<Object>,
}

impl EditState {
    /// The whole source kept, nothing added.
    pub fn new(duration_ms: u64) -> Self {
        Self { segments: vec![Segment::new(0, duration_ms.max(1))], ..Self::default() }
    }

    pub fn next_title_id(&self) -> u64 {
        self.titles.iter().map(|t| t.id).max().unwrap_or(0) + 1
    }

    /// Sort zoom keys by time (stable for equal times).
    pub fn sort_zoom(&mut self) {
        self.zoom.sort_by_key(|k| k.time_ms);
    }
}
