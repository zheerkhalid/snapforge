//! Playback frame cache: a worker thread decodes around the playhead (and ahead of it while
//! playing, following the kept segments across cuts) into a bounded cache of display-ready
//! frames. The UI only ever reads the cache; it never waits for a decode.

use super::compose::scale_region;
use super::mf::{self, MediaInfo, VideoReader};
use super::model::Segment;
use crate::draw::RectF;
use eframe::egui::{Color32, ColorImage};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Preview frames wider than this are scaled down (the export always uses full frames).
const MAX_PREVIEW_W: u32 = 1920;
/// Memory budget for cached frames.
const CACHE_BYTES: usize = 384 << 20;
const BACK_MS: u64 = 400;
const AHEAD_PLAYING_MS: u64 = 1500;
const AHEAD_PAUSED_MS: u64 = 600;

#[derive(Clone)]
pub struct CachedFrame {
    pub pts_ms: u64,
    pub dur_ms: u64,
    pub image: Arc<ColorImage>,
}

#[derive(Default)]
struct State {
    target: u64,
    playing: bool,
    segments: Vec<Segment>,
    frames: BTreeMap<u64, CachedFrame>,
    info: Option<MediaInfo>,
    error: Option<String>,
    quit: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Decoder {
    shared: Arc<Shared>,
}

impl Decoder {
    pub fn start(path: PathBuf, segments: Vec<Segment>, wake: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared { state: Mutex::new(State { segments, ..State::default() }), cv: Condvar::new() });
        let s = shared.clone();
        let res = std::thread::Builder::new().name("video-decode".into()).spawn(move || worker(path, s, wake));
        if let Err(e) = res {
            shared.lock().error = Some(format!("cannot start the decoder: {e}"));
        }
        Self { shared }
    }

    /// Tell the worker where the playhead is (source time) and whether it is playing.
    pub fn set_target(&self, src_ms: u64, playing: bool) {
        let mut g = self.shared.lock();
        if g.target != src_ms || g.playing != playing {
            g.target = src_ms;
            g.playing = playing;
            self.shared.cv.notify_all();
        }
    }

    pub fn set_segments(&self, segments: &[Segment]) {
        let mut g = self.shared.lock();
        if g.segments != segments {
            g.segments = segments.to_vec();
            self.shared.cv.notify_all();
        }
    }

    /// The latest cached frame at or before `src_ms` (it may be a little stale while the
    /// decoder catches up; compare `pts_ms`).
    pub fn frame_at(&self, src_ms: u64) -> Option<CachedFrame> {
        let g = self.shared.lock();
        let (_, f) = g.frames.range(..=src_ms).next_back()?;
        (src_ms < f.pts_ms + 2_000).then(|| f.clone())
    }

    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }

    #[cfg(test)]
    pub fn info(&self) -> Option<MediaInfo> {
        self.shared.lock().info
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.shared.lock().quit = true;
        self.shared.cv.notify_all();
    }
}

/// Source ranges to keep cached: a little behind the target, and `ahead` ms of *kept* time after
/// it (following the segments, so frames after a cut are prefetched).
pub fn wanted_ranges(segments: &[Segment], target: u64, ahead: u64) -> Vec<(u64, u64)> {
    let mut out = vec![(target.saturating_sub(BACK_MS), target + 1)];
    let mut left = ahead;
    for s in segments.iter().filter(|s| s.end_ms > target) {
        if left == 0 {
            break;
        }
        let a = s.start_ms.max(target);
        let b = (a + left).min(s.end_ms);
        out.push((a, b));
        left -= b - a;
    }
    if segments.is_empty() {
        out.push((target, target + ahead));
    }
    out
}

fn overlaps(ranges: &[(u64, u64)], a: u64, b: u64) -> bool {
    ranges.iter().any(|&(lo, hi)| a < hi && b > lo)
}

fn to_color_image(img: &crate::common::Image) -> ColorImage {
    let img = if img.width > MAX_PREVIEW_W {
        let h = (img.height as u64 * MAX_PREVIEW_W as u64 / img.width as u64).max(1) as u32;
        scale_region(img, RectF::new(0.0, 0.0, img.width as f32, img.height as f32), MAX_PREVIEW_W, h)
    } else {
        img.clone()
    };
    let pixels = img.data.chunks_exact(4).map(|p| Color32::from_rgba_premultiplied(p[2], p[1], p[0], p[3])).collect();
    ColorImage { size: [img.width as usize, img.height as usize], pixels }
}

fn worker(path: PathBuf, shared: Arc<Shared>, wake: impl Fn()) {
    let _mf = match mf::init() {
        Ok(g) => g,
        Err(e) => {
            shared.lock().error = Some(format!("{e:#}"));
            wake();
            return;
        }
    };
    let mut reader = match VideoReader::open(&path, true) {
        Ok(r) => r,
        Err(e) => {
            shared.lock().error = Some(format!("{e:#}"));
            wake();
            return;
        }
    };
    let info = reader.info;
    shared.lock().info = Some(info);
    let frame_bytes = (info.width.min(MAX_PREVIEW_W) as usize * info.height as usize * 4).max(1);
    let max_frames = (CACHE_BYTES / frame_bytes).clamp(6, 180);
    // End (exclusive, ms) of the last decoded frame; None = must seek.
    let mut pos: Option<u64> = None;
    let mut eof = false;
    let mut goal: Option<u64> = None;
    loop {
        // Decide what to do next under the lock.
        let seek_to: Option<u64>;
        {
            let mut g = shared.lock();
            if g.quit {
                return;
            }
            let ahead = if g.playing { AHEAD_PLAYING_MS } else { AHEAD_PAUSED_MS };
            let ranges = wanted_ranges(&g.segments, g.target, ahead);
            let target = g.target;
            // Keep the newest frame at or before the target too (shown while catching up).
            let newest = g.frames.range(..=target).next_back().map(|(p, _)| *p);
            g.frames.retain(|&pts, f| Some(pts) == newest || overlaps(&ranges, pts, pts + f.dur_ms.max(1)));
            let have_target =
                g.frames.range(..=target).next_back().is_some_and(|(_, f)| target < f.pts_ms + f.dur_ms.max(1));
            let full = g.frames.len() >= max_frames;
            let action = match pos {
                _ if !have_target => match pos {
                    // Keep decoding forward: close enough, or still running up to a seek goal.
                    Some(p)
                        if !eof
                            && p <= target + 1
                            && (target < p + 1000 || goal.is_some_and(|g| p <= g && target < g + 1000)) =>
                    {
                        Some(None)
                    }
                    // Too far: seek. While playing, aim a little ahead so the run-up from the
                    // keyframe doesn't leave us behind again.
                    _ => Some(Some(if g.playing { target + 300 } else { target })),
                },
                // Still decoding from the keyframe up to the position we sought to.
                Some(p) if !eof && !full && goal.is_some_and(|g| p < g) => Some(None),
                Some(p) if !eof && !full => {
                    if overlaps(&ranges, p, p + 1) {
                        Some(None)
                    } else {
                        // In a cut gap or past the window: jump to the next wanted range, if any.
                        ranges.iter().map(|r| r.0).filter(|&a| a > p).min().map(Some)
                    }
                }
                None if !full => Some(Some(target)),
                _ => None,
            };
            match action {
                Some(s) => seek_to = s,
                None => {
                    // Nothing to do: wait for a new target.
                    let _ = shared.cv.wait_timeout(g, Duration::from_millis(250));
                    continue;
                }
            }
        }
        if let Some(t) = seek_to {
            if let Err(e) = reader.seek(t) {
                log::warn!("video editor: seek failed: {e:#}");
            }
            // Frames from the keyframe before `t` up to `t` are the run-up to this goal.
            pos = Some(0);
            goal = Some(t);
            eof = false;
        }
        match reader.next_frame() {
            Ok(Some(f)) => {
                let pts = f.pts_ms();
                let dur = ((f.dur_hns + mf::HNS_PER_MS - 1) / mf::HNS_PER_MS).max(1) as u64;
                pos = Some(pts + dur);
                // Convert outside the lock, only if still wanted.
                let wanted = {
                    let g = shared.lock();
                    let ahead = if g.playing { AHEAD_PLAYING_MS } else { AHEAD_PAUSED_MS };
                    let newest = g.frames.range(..=g.target).next_back().map(|(p, _)| *p);
                    overlaps(&wanted_ranges(&g.segments, g.target, ahead), pts, pts + dur)
                        || (pts <= g.target && newest.map_or(true, |n| pts > n))
                };
                if wanted {
                    let image = Arc::new(to_color_image(&f.image));
                    let mut g = shared.lock();
                    let target = g.target;
                    g.frames.insert(pts, CachedFrame { pts_ms: pts, dur_ms: dur, image });
                    let covers = pts <= target && target < pts + dur * 2;
                    drop(g);
                    if covers || pts > target {
                        wake();
                    }
                }
            }
            Ok(None) => {
                eof = true;
                // Hold the last frame for targets past the end.
                std::thread::sleep(Duration::from_millis(5));
                let g = shared.lock();
                if g.frames.is_empty() && !g.quit {
                    let _ = shared.cv.wait_timeout(g, Duration::from_millis(100));
                }
            }
            Err(e) => {
                log::warn!("video editor: decode error: {e:#}");
                shared.lock().error = Some(format!("{e:#}"));
                wake();
                pos = None;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wanted_ranges_follow_segments() {
        let segs = [Segment::new(0, 1_000), Segment::new(3_000, 10_000)];
        let r = wanted_ranges(&segs, 800, 1_000);
        assert_eq!(r, vec![(400, 801), (800, 1_000), (3_000, 3_800)]);
        assert!(overlaps(&r, 3_500, 3_533));
        assert!(!overlaps(&r, 2_000, 2_033));
    }

    #[test]
    #[ignore = "needs Media Foundation with an H.264 encoder (run with --ignored)"]
    fn cache_serves_seeks_and_follows_cuts() {
        let path = std::env::temp_dir().join(format!("sf-ve-dec-{}.mp4", std::process::id()));
        {
            let _mf = mf::init().expect("mf");
            mf::tests::write_test_clip(&path, 160, 120, 30, 3, false).expect("clip");
        }
        let wait_for = |d: &Decoder, src: u64| -> Option<CachedFrame> {
            let t0 = std::time::Instant::now();
            while t0.elapsed() < Duration::from_secs(20) {
                if let Some(f) = d.frame_at(src).filter(|f| src < f.pts_ms + f.dur_ms + 1) {
                    return Some(f);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None
        };
        let d = Decoder::start(path.clone(), vec![Segment::new(0, 1_000), Segment::new(2_000, 3_000)], || {});
        d.set_target(1_500, false);
        let f = wait_for(&d, 1_500).expect("frame at 1.5 s");
        assert!(f.pts_ms <= 1_500 && 1_500 - f.pts_ms < 40, "{}", f.pts_ms);
        assert_eq!(f.image.size, [160, 120]);
        // Backwards seek.
        d.set_target(200, false);
        let f = wait_for(&d, 200).expect("frame at 0.2 s");
        assert!(f.pts_ms <= 200 && 200 - f.pts_ms < 40);
        // Playing near the end of the first segment prefetches the start of the next one.
        d.set_target(900, true);
        let t0 = std::time::Instant::now();
        while d.frame_at(2_010).filter(|f| f.pts_ms >= 1_990).is_none() && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(d.frame_at(2_010).is_some_and(|f| f.pts_ms >= 1_990), "no prefetch after the cut");
        assert!(d.error().is_none());
        assert_eq!(d.info().map(|i| (i.width, i.height)), Some((160, 120)));
        drop(d);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&path);
    }
}
