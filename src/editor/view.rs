//! Zoom / pan state of one document view.
//!
//! All math here works in *physical* pixels so that 100% zoom means one image pixel per
//! screen pixel, regardless of the egui `pixels_per_point`. The canvas converts to points
//! when painting.

/// 1%.
pub const MIN_ZOOM: f32 = 0.01;
/// 5000%.
pub const MAX_ZOOM: f32 = 50.0;

/// Zoom menu presets, in percent.
pub const PRESETS: [u32; 17] = [1, 5, 10, 25, 33, 50, 67, 75, 100, 150, 200, 300, 400, 800, 1600, 3200, 5000];

/// Preset steps for the zoom buttons / keyboard (+ / -).
const STEPS: [f32; 25] = [
    0.01, 0.02, 0.03, 0.05, 0.0833, 0.125, 0.1667, 0.25, 0.3333, 0.5, 0.6667, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0,
    8.0, 12.0, 16.0, 24.0, 32.0, 50.0,
];

pub fn clamp_zoom(z: f32) -> f32 {
    if z.is_finite() {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// How the view follows window resizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitMode {
    /// Free zoom/pan chosen by the user.
    None,
    /// Whole image visible (recomputed when the canvas resizes).
    Window,
    /// Image width fills the canvas.
    Width,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// Image px → physical screen px.
    pub zoom: f32,
    /// Physical-px position of the image's top-left corner relative to the canvas top-left.
    pub offset: [f32; 2],
    pub fit: FitMode,
    /// False until the first layout with a real canvas size.
    pub initialized: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self { zoom: 1.0, offset: [0.0, 0.0], fit: FitMode::None, initialized: false }
    }
}

/// Zoom that makes `img` fit inside `view` (both physical px).
pub fn fit_window_zoom(img: [f32; 2], view: [f32; 2]) -> f32 {
    if img[0] <= 0.0 || img[1] <= 0.0 || view[0] <= 0.0 || view[1] <= 0.0 {
        return 1.0;
    }
    clamp_zoom((view[0] / img[0]).min(view[1] / img[1]))
}

/// Zoom that makes the image width fill `view` width.
pub fn fit_width_zoom(img: [f32; 2], view: [f32; 2]) -> f32 {
    if img[0] <= 0.0 || view[0] <= 0.0 {
        return 1.0;
    }
    clamp_zoom(view[0] / img[0])
}

/// Next preset step above (`dir > 0`) or below the current zoom.
pub fn step_zoom(zoom: f32, dir: i32) -> f32 {
    let eps = 1e-3;
    if dir > 0 {
        STEPS.iter().copied().find(|&s| s > zoom * (1.0 + eps)).unwrap_or(MAX_ZOOM)
    } else {
        STEPS.iter().rev().copied().find(|&s| s < zoom * (1.0 - eps)).unwrap_or(MIN_ZOOM)
    }
}

impl ViewState {
    /// Initial view: fit if the image is larger than the canvas, else 100% centred.
    pub fn reset(&mut self, img: [f32; 2], view: [f32; 2]) {
        if img[0] > view[0] || img[1] > view[1] {
            self.fit = FitMode::Window;
            self.zoom = fit_window_zoom(img, view);
        } else {
            self.fit = FitMode::None;
            self.zoom = 1.0;
        }
        self.offset = [0.0, 0.0];
        self.clamp(img, view);
        self.initialized = true;
    }

    /// Re-apply the fit mode (after a resize) and keep the image in a sensible position.
    pub fn layout(&mut self, img: [f32; 2], view: [f32; 2]) {
        if !self.initialized {
            self.reset(img, view);
            return;
        }
        match self.fit {
            FitMode::Window => self.zoom = fit_window_zoom(img, view),
            FitMode::Width => self.zoom = fit_width_zoom(img, view),
            FitMode::None => {}
        }
        self.clamp(img, view);
    }

    pub fn fit_window(&mut self, img: [f32; 2], view: [f32; 2]) {
        self.fit = FitMode::Window;
        self.zoom = fit_window_zoom(img, view);
        self.clamp(img, view);
    }

    pub fn fit_width(&mut self, img: [f32; 2], view: [f32; 2]) {
        self.fit = FitMode::Width;
        self.zoom = fit_width_zoom(img, view);
        self.offset[1] = 0.0;
        self.clamp(img, view);
    }

    /// Set the zoom keeping the image point under `anchor` (canvas physical px) fixed.
    pub fn zoom_around(&mut self, new_zoom: f32, anchor: [f32; 2], img: [f32; 2], view: [f32; 2]) {
        let new_zoom = clamp_zoom(new_zoom);
        for (i, a) in anchor.iter().enumerate() {
            let image_pt = (a - self.offset[i]) / self.zoom;
            self.offset[i] = a - image_pt * new_zoom;
        }
        self.zoom = new_zoom;
        self.fit = FitMode::None;
        self.clamp(img, view);
    }

    /// Zoom around the canvas centre.
    pub fn set_zoom_centered(&mut self, new_zoom: f32, img: [f32; 2], view: [f32; 2]) {
        self.zoom_around(new_zoom, [view[0] / 2.0, view[1] / 2.0], img, view);
    }

    pub fn pan(&mut self, delta: [f32; 2], img: [f32; 2], view: [f32; 2]) {
        self.offset[0] += delta[0];
        self.offset[1] += delta[1];
        if self.fit == FitMode::Window {
            // Panning a fitted image does nothing visible; keep the fit.
        } else {
            self.fit = FitMode::None;
        }
        self.clamp(img, view);
    }

    /// Centre the image on an axis where it is smaller than the canvas; otherwise
    /// keep the canvas covered (no empty gap at either edge).
    pub fn clamp(&mut self, img: [f32; 2], view: [f32; 2]) {
        for i in 0..2 {
            let scaled = img[i] * self.zoom;
            if scaled <= view[i] {
                self.offset[i] = ((view[i] - scaled) / 2.0).round();
            } else {
                self.offset[i] = self.offset[i].clamp(view[i] - scaled, 0.0);
            }
        }
    }

    /// Zoom so the image rectangle `r` ([x, y, w, h] in image px) fills ~90% of the
    /// canvas, centred on it.
    pub fn zoom_to_rect(&mut self, r: [f32; 4], img: [f32; 2], view: [f32; 2]) {
        if r[2] <= 0.0 || r[3] <= 0.0 || view[0] <= 0.0 || view[1] <= 0.0 {
            return;
        }
        let z = clamp_zoom((view[0] * 0.9 / r[2]).min(view[1] * 0.9 / r[3]));
        self.zoom = z;
        self.fit = FitMode::None;
        let c = [r[0] + r[2] / 2.0, r[1] + r[3] / 2.0];
        self.offset = [view[0] / 2.0 - c[0] * z, view[1] / 2.0 - c[1] * z];
        self.clamp(img, view);
    }

    /// Canvas physical px → image px.
    pub fn screen_to_image(&self, p: [f32; 2]) -> [f32; 2] {
        [(p[0] - self.offset[0]) / self.zoom, (p[1] - self.offset[1]) / self.zoom]
    }

    /// Zoom as a whole-number percentage for the status bar.
    pub fn percent(&self) -> u32 {
        (self.zoom * 100.0).round().max(1.0) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: [f32; 2] = [800.0, 600.0];

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn small_image_opens_at_100_centred() {
        let mut v = ViewState::default();
        v.layout([200.0, 100.0], VIEW);
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.fit, FitMode::None);
        assert_eq!(v.offset, [300.0, 250.0]);
    }

    #[test]
    fn large_image_fits_window() {
        let mut v = ViewState::default();
        v.layout([1600.0, 600.0], VIEW);
        assert_eq!(v.fit, FitMode::Window);
        assert!(approx(v.zoom, 0.5));
        // Width fills exactly, height centred.
        assert!(approx(v.offset[0], 0.0));
        assert!(approx(v.offset[1], 150.0));
        // Resizing keeps it fitted.
        v.layout([1600.0, 600.0], [400.0, 600.0]);
        assert!(approx(v.zoom, 0.25));
    }

    #[test]
    fn fit_calculations() {
        assert!(approx(fit_window_zoom([1000.0, 2000.0], VIEW), 0.3));
        assert!(approx(fit_width_zoom([1000.0, 2000.0], VIEW), 0.8));
        // Degenerate input never yields NaN / 0.
        assert_eq!(fit_window_zoom([0.0, 10.0], VIEW), 1.0);
        assert_eq!(fit_width_zoom([10.0, 10.0], [0.0, 0.0]), 1.0);
        // Tiny image fit is clamped to the max zoom.
        assert_eq!(fit_window_zoom([1.0, 1.0], [10_000.0, 10_000.0]), MAX_ZOOM);
    }

    #[test]
    fn fit_width_starts_at_top() {
        let mut v = ViewState::default();
        let img = [400.0, 4000.0];
        v.layout(img, VIEW);
        v.offset[1] = -500.0;
        v.fit_width(img, VIEW);
        assert!(approx(v.zoom, 2.0));
        assert_eq!(v.offset, [0.0, 0.0]);
    }

    #[test]
    fn zoom_around_keeps_anchor_fixed() {
        let img = [4000.0, 3000.0];
        let mut v = ViewState { zoom: 1.0, offset: [-1000.0, -1000.0], fit: FitMode::None, initialized: true };
        let anchor = [300.0, 200.0];
        let before = v.screen_to_image(anchor);
        v.zoom_around(2.0, anchor, img, VIEW);
        let after = v.screen_to_image(anchor);
        assert!(approx(before[0], after[0]) && approx(before[1], after[1]));
        assert_eq!(v.zoom, 2.0);
        assert_eq!(v.fit, FitMode::None);
    }

    #[test]
    fn zoom_is_clamped() {
        let img = [100.0, 100.0];
        let mut v = ViewState::default();
        v.layout(img, VIEW);
        v.zoom_around(1000.0, [10.0, 10.0], img, VIEW);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.zoom_around(0.0001, [10.0, 10.0], img, VIEW);
        assert_eq!(v.zoom, MIN_ZOOM);
        assert_eq!(clamp_zoom(f32::NAN), 1.0);
        assert_eq!(v.percent(), 1);
    }

    #[test]
    fn pan_is_clamped_to_edges() {
        let img = [2000.0, 2000.0];
        let mut v = ViewState { zoom: 1.0, offset: [0.0, 0.0], fit: FitMode::None, initialized: true };
        v.pan([500.0, 500.0], img, VIEW);
        assert_eq!(v.offset, [0.0, 0.0]);
        v.pan([-5000.0, -5000.0], img, VIEW);
        assert_eq!(v.offset, [VIEW[0] - 2000.0, VIEW[1] - 2000.0]);
    }

    #[test]
    fn zoom_to_selection_centres_it() {
        let img = [4000.0, 3000.0];
        let mut v = ViewState::default();
        v.layout(img, VIEW);
        v.zoom_to_rect([1000.0, 1000.0, 80.0, 60.0], img, VIEW);
        assert!(approx(v.zoom, 9.0));
        let c = v.screen_to_image([VIEW[0] / 2.0, VIEW[1] / 2.0]);
        assert!(approx(c[0], 1040.0) && approx(c[1], 1030.0));
        // A 1×1 selection is limited to 5000%.
        v.zoom_to_rect([0.0, 0.0, 1.0, 1.0], img, VIEW);
        assert_eq!(v.zoom, MAX_ZOOM);
        assert!(PRESETS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!((PRESETS[0], PRESETS[16]), (1, 5000));
    }

    #[test]
    fn steps_move_monotonically() {
        assert_eq!(step_zoom(1.0, 1), 1.5);
        assert_eq!(step_zoom(1.0, -1), 0.75);
        assert_eq!(step_zoom(0.9, 1), 1.0);
        assert_eq!(step_zoom(MAX_ZOOM, 1), MAX_ZOOM);
        assert_eq!(step_zoom(MIN_ZOOM, -1), MIN_ZOOM);
    }
}
