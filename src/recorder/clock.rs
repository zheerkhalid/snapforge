//! The media clock shared by audio and video.
//!
//! All times are in 100 ns units ("hns", the unit of Media Foundation and of the QPC
//! positions WASAPI reports). Media time is wall time since the start of the recording
//! minus the time spent paused, so pausing and resuming shifts every later timestamp back
//! by the length of the pause and the file plays without a gap.

/// QueryPerformanceCounter converted to 100 ns units (same scale as WASAPI's `qpcposition`).
pub fn qpc_hns() -> i64 {
    let t = crate::common::time::qpc_now();
    let f = qpc_freq();
    (t as i128 * 10_000_000 / f as i128) as i64
}

fn qpc_freq() -> i64 {
    use std::sync::OnceLock;
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer.
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

pub const HNS_PER_SEC: i64 = 10_000_000;

#[derive(Debug, Clone)]
pub struct MediaClock {
    start: i64,
    /// Closed pauses `[from, to)` plus at most one open pause at the end.
    pauses: Vec<(i64, Option<i64>)>,
}

impl MediaClock {
    pub fn new(start_hns: i64) -> Self {
        Self { start: start_hns, pauses: Vec::new() }
    }

    pub fn start(&self) -> i64 {
        self.start
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.pauses.last(), Some((_, None)))
    }

    /// Returns false if already paused.
    pub fn pause(&mut self, at: i64) -> bool {
        if self.is_paused() {
            return false;
        }
        let at = at.max(self.start).max(self.pauses.last().and_then(|p| p.1).unwrap_or(i64::MIN));
        self.pauses.push((at, None));
        true
    }

    /// Returns false if not paused.
    pub fn resume(&mut self, at: i64) -> bool {
        match self.pauses.last_mut() {
            Some((from, end @ None)) => {
                *end = Some(at.max(*from));
                true
            }
            _ => false,
        }
    }

    /// Total paused time before wall time `t`.
    fn paused_before(&self, t: i64) -> i64 {
        let mut sum = 0;
        for &(from, to) in &self.pauses {
            if from >= t {
                break;
            }
            let end = to.unwrap_or(i64::MAX).min(t);
            sum += end - from;
        }
        sum
    }

    /// Media time of wall time `t`, or `None` when `t` lies before the start or inside a
    /// pause (audio captured then is discarded).
    pub fn media_time(&self, t: i64) -> Option<i64> {
        if t < self.start {
            return None;
        }
        for &(from, to) in &self.pauses {
            if t >= from && to.map_or(true, |to| t < to) {
                return None;
            }
        }
        Some(t - self.start - self.paused_before(t))
    }

    /// Media time "now": like [`media_time`] but clamps to the pause start while paused.
    pub fn media_now(&self, t: i64) -> i64 {
        (t.max(self.start) - self.start - self.paused_before(t)).max(0)
    }

    /// Total time spent paused so far (for the control bar).
    pub fn total_paused(&self, now: i64) -> i64 {
        self.paused_before(now)
    }
}

/// Media time of video frame `index` at `fps` (exact, no accumulated rounding).
pub fn frame_time(index: u64, fps: u32) -> i64 {
    (index as i128 * HNS_PER_SEC as i128 / fps.max(1) as i128) as i64
}

/// Index of the first frame whose time is `>= t`.
pub fn frame_at_or_after(t: i64, fps: u32) -> u64 {
    if t <= 0 {
        return 0;
    }
    let n = t as i128 * fps.max(1) as i128;
    ((n + HNS_PER_SEC as i128 - 1) / HNS_PER_SEC as i128) as u64
}

/// Audio sample index (at `rate`) of media time `t`, rounded down.
pub fn sample_index(t: i64, rate: u32) -> i64 {
    (t as i128 * rate as i128 / HNS_PER_SEC as i128) as i64
}

/// Media time of audio sample `index` at `rate`.
pub fn sample_time(index: i64, rate: u32) -> i64 {
    (index as i128 * HNS_PER_SEC as i128 / rate.max(1) as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = HNS_PER_SEC;

    #[test]
    fn no_pause() {
        let c = MediaClock::new(100 * S);
        assert_eq!(c.media_time(99 * S), None);
        assert_eq!(c.media_time(100 * S), Some(0));
        assert_eq!(c.media_time(105 * S), Some(5 * S));
        assert_eq!(c.media_now(50 * S), 0);
    }

    #[test]
    fn pause_shifts_later_timestamps() {
        let mut c = MediaClock::new(0);
        assert!(c.pause(10 * S));
        assert!(!c.pause(11 * S));
        assert!(c.is_paused());
        // Inside the pause: dropped; "now" clamps to the pause start.
        assert_eq!(c.media_time(12 * S), None);
        assert_eq!(c.media_now(12 * S), 10 * S);
        assert!(c.resume(15 * S));
        assert!(!c.resume(16 * S));
        assert_eq!(c.media_time(15 * S), Some(10 * S));
        assert_eq!(c.media_time(20 * S), Some(15 * S));
        // Before the pause is unaffected.
        assert_eq!(c.media_time(9 * S), Some(9 * S));
        // A second pause accumulates.
        c.pause(30 * S);
        c.resume(32 * S);
        assert_eq!(c.media_time(40 * S), Some(33 * S));
        assert_eq!(c.total_paused(40 * S), 7 * S);
        // Media time stays monotonic across the whole timeline.
        let mut last = -1;
        for t in (0..50 * S).step_by((S / 7) as usize) {
            let m = c.media_now(t);
            assert!(m >= last);
            last = m;
        }
    }

    #[test]
    fn frame_math() {
        assert_eq!(frame_time(0, 30), 0);
        assert_eq!(frame_time(30, 30), S);
        assert_eq!(frame_time(1, 60), 166_666);
        assert_eq!(frame_at_or_after(0, 30), 0);
        assert_eq!(frame_at_or_after(1, 30), 1);
        assert_eq!(frame_at_or_after(S, 30), 30);
        assert_eq!(frame_at_or_after(S + 1, 30), 31);
        // 600 frames at 60 fps end exactly at 10 s.
        assert_eq!(frame_time(600, 60), 10 * S);
        assert_eq!(sample_index(S, 48_000), 48_000);
        assert_eq!(sample_time(24_000, 48_000), S / 2);
    }
}
