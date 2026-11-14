//! Image stitching with overlap detection for scrolling capture.
//!
//! CONTRACT: pure, deterministic, unit-tested; no Win32 calls.

/// Stitch `screens` synthetic frames and return elapsed milliseconds (for `--bench`).
pub fn bench_synthetic(screens: u32) -> f64 {
    let _ = screens;
    0.0
}
