//! Zoom & pan: keyframe interpolation with easing, and the visible source rectangle.

use super::model::ZoomKey;
use crate::draw::RectF;

/// Interpolated view state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomView {
    pub cx: f32,
    pub cy: f32,
    pub scale: f32,
}

impl ZoomView {
    pub const IDENTITY: ZoomView = ZoomView { cx: 0.5, cy: 0.5, scale: 1.0 };

    fn of(k: &ZoomKey) -> ZoomView {
        ZoomView { cx: finite(k.cx, 0.5), cy: finite(k.cy, 0.5), scale: clamp_scale(k.scale) }
    }

    /// Source rectangle shown for a `w`×`h` frame. Keeps the frame's aspect ratio and stays
    /// inside the frame.
    pub fn rect(&self, w: f32, h: f32) -> RectF {
        let s = clamp_scale(self.scale);
        let (rw, rh) = (w / s, h / s);
        let x = (self.cx * w - rw / 2.0).clamp(0.0, (w - rw).max(0.0));
        let y = (self.cy * h - rh / 2.0).clamp(0.0, (h - rh).max(0.0));
        RectF::new(x, y, rw, rh)
    }

    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-4
    }
}

pub const MAX_SCALE: f32 = 8.0;

fn finite(v: f32, d: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        d
    }
}

fn clamp_scale(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(1.0, MAX_SCALE)
    } else {
        1.0
    }
}

/// Ease-in-out (cubic), 0..1 → 0..1.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Blend two views. Scale is interpolated geometrically so zooming feels uniform.
pub fn blend(a: ZoomView, b: ZoomView, t: f32) -> ZoomView {
    let scale = (a.scale.ln() + (b.scale.ln() - a.scale.ln()) * t).exp();
    ZoomView { cx: lerp(a.cx, b.cx, t), cy: lerp(a.cy, b.cy, t), scale }
}

/// View at source time `t`. Before the first keyframe's transition the whole frame is shown;
/// each keyframe is reached at its time, easing from the previous state over its transition
/// (which starts no earlier than the previous keyframe), then held. `keys` need not be sorted.
pub fn view_at(keys: &[ZoomKey], t: u64) -> ZoomView {
    let mut sorted: Vec<&ZoomKey> = keys.iter().collect();
    sorted.sort_by_key(|k| k.time_ms);
    let mut prev = ZoomView::IDENTITY;
    let mut prev_time = 0u64;
    for k in sorted {
        if t >= k.time_ms {
            prev = ZoomView::of(k);
            prev_time = k.time_ms;
            continue;
        }
        let start = k.time_ms.saturating_sub(k.transition_ms).max(prev_time);
        if t > start && k.time_ms > start {
            let f = (t - start) as f32 / (k.time_ms - start) as f32;
            return blend(prev, ZoomView::of(k), ease(f));
        }
        return prev;
    }
    prev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(time_ms: u64, cx: f32, cy: f32, scale: f32) -> ZoomKey {
        ZoomKey { time_ms, cx, cy, scale, transition_ms: 1000 }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn no_keys_is_identity() {
        assert_eq!(view_at(&[], 123), ZoomView::IDENTITY);
        assert_eq!(ZoomView::IDENTITY.rect(1920.0, 1080.0), RectF::new(0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn eases_into_a_key_and_holds() {
        let keys = [key(5_000, 0.25, 0.25, 2.0)];
        assert_eq!(view_at(&keys, 3_000), ZoomView::IDENTITY);
        assert_eq!(view_at(&keys, 4_000), ZoomView::IDENTITY);
        let mid = view_at(&keys, 4_500);
        assert!(close(mid.cx, 0.375) && close(mid.cy, 0.375), "{mid:?}");
        assert!(close(mid.scale, 2f32.sqrt()), "{mid:?}");
        // Eased: a quarter of the way in time is less than a quarter of the way in space.
        let q = view_at(&keys, 4_250);
        assert!(q.cx > 0.5 - 0.25 * 0.25 && q.cx < 0.5);
        let at = view_at(&keys, 5_000);
        assert!(close(at.scale, 2.0) && close(at.cx, 0.25));
        assert_eq!(view_at(&keys, 60_000), at);
    }

    #[test]
    fn chains_keys_and_clips_transition_to_previous_key() {
        let keys = [key(2_000, 0.5, 0.5, 1.0), key(1_000, 0.2, 0.8, 3.0), key(2_500, 0.5, 0.5, 1.0)];
        // Unsorted input: 1000 (3x) reached first.
        assert!(close(view_at(&keys, 1_000).scale, 3.0));
        // Key at 2000 transitions over 1000..2000 (transition 1000 starts exactly at prev key).
        let m = view_at(&keys, 1_500);
        assert!(close(m.scale, 3f32.sqrt()), "{m:?}");
        // Key at 2500 has a 1000 ms transition but the previous key is at 2000: clipped.
        assert!(close(view_at(&keys, 2_000).scale, 1.0));
        assert!(close(view_at(&keys, 2_250).scale, 1.0));
    }

    #[test]
    fn rect_stays_inside_the_frame() {
        let v = ZoomView { cx: 0.0, cy: 1.0, scale: 2.0 };
        assert_eq!(v.rect(1000.0, 500.0), RectF::new(0.0, 250.0, 500.0, 250.0));
        let v = ZoomView { cx: 0.5, cy: 0.5, scale: 100.0 };
        let r = v.rect(800.0, 600.0);
        assert!(close(r.w, 100.0) && close(r.h, 75.0));
        let bad = ZoomView { cx: f32::NAN, cy: 0.5, scale: f32::INFINITY };
        let _ = bad.rect(10.0, 10.0);
        assert!(close(ease(0.5), 0.5) && ease(0.0) == 0.0 && ease(1.0) == 1.0);
    }
}
