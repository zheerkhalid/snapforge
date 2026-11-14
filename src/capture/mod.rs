//! Screen capture backends and window enumeration.
//!
//! CONTRACT (used by app, overlay, scroll, bench; keep these signatures):
//! * All functions are blocking and callable from any thread. Implementations may keep
//!   a dedicated worker thread for D3D/WGC objects internally.
//! * Coordinates are physical pixels in virtual-screen space (process is PerMonitorV2).
//! * Returned images are premultiplied BGRA ([`Image`]), fully opaque for screen pixels.
//! * Backend order for `CaptureEngine::Auto`: WGC → DXGI Desktop Duplication → GDI BitBlt.
//!   For still captures of SDR monitors Auto uses the measured fastest reliable order
//!   ([`AUTO_STILL_ORDER_WARM`] / [`AUTO_STILL_ORDER_COLD`], see DECISIONS.md); HDR
//!   monitors always go to WGC first, and so do window captures.
//!
//! Internals: `gdi` (BitBlt), `dxgi` (Desktop Duplication), `wgc` (Windows.Graphics.Capture),
//! `worker` (thread owning all GPU objects, released when idle), `hdr` (tone mapping),
//! `cursor` (cursor compositing), `windows_enum` (z-order snapshot), `uia` (element chain).

mod cursor;
mod d3d;
mod dxgi;
mod gdi;
pub mod hdr;
pub mod scroll;
pub mod stitch;
mod uia;
mod wgc;
mod windows_enum;
mod worker;

use crate::common::time::{qpc_ms, qpc_now};
use crate::common::{win, Image, Point, Rect};
use crate::settings::CaptureEngine;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic, IsWindow};

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

/// A concrete backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Wgc,
    Dxgi,
    Gdi,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Wgc => "wgc",
            Backend::Dxgi => "dxgi",
            Backend::Gdi => "gdi",
        }
    }
}

/// Auto order for still captures of SDR monitors while DXGI duplications are cached.
/// Chosen from measurements (DECISIONS.md): warm DXGI is the fastest path by far.
pub const AUTO_STILL_ORDER_WARM: [Backend; 3] = [Backend::Dxgi, Backend::Gdi, Backend::Wgc];
/// Auto order when nothing is cached: GDI needs no device setup, so it wins the first
/// grab; DXGI is warmed in the background for the grabs that follow (scrolling, repeats).
pub const AUTO_STILL_ORDER_COLD: [Backend; 3] = [Backend::Gdi, Backend::Dxgi, Backend::Wgc];
/// Auto order when an HDR monitor is involved (only WGC tone-maps correctly).
const AUTO_HDR_ORDER: [Backend; 3] = [Backend::Wgc, Backend::Dxgi, Backend::Gdi];

/// Capture a rectangle of the virtual screen.
pub fn grab_rect(rect: Rect, opts: &GrabOptions) -> Result<Image, CaptureError> {
    if rect.is_empty() {
        return Err(CaptureError::Empty);
    }
    let t0 = qpc_now();
    let order: &[Backend] = match opts.engine {
        CaptureEngine::Wgc => &[Backend::Wgc],
        CaptureEngine::Dxgi => &[Backend::Dxgi],
        CaptureEngine::Gdi => &[Backend::Gdi],
        CaptureEngine::Auto => {
            let hdr = d3d::outputs().iter().any(|o| o.hdr && !o.rect.intersect(&rect).is_empty());
            if hdr {
                &AUTO_HDR_ORDER
            } else if worker::dxgi_is_warm() {
                &AUTO_STILL_ORDER_WARM
            } else {
                worker::prewarm_dxgi();
                &AUTO_STILL_ORDER_COLD
            }
        }
    };
    let mut last = CaptureError::Failed("no capture backend available".into());
    for &b in order {
        match grab_with(b, rect, opts.include_cursor) {
            Ok(img) => {
                log::debug!("grab_rect {rect:?} via {} in {:.1} ms", b.name(), qpc_ms(t0, qpc_now()));
                return Ok(img);
            }
            Err(e) => {
                log::warn!("{} capture of {rect:?} failed: {e}", b.name());
                last = e;
            }
        }
    }
    Err(last)
}

/// Capture `rect` with one specific backend.
pub fn grab_with(b: Backend, rect: Rect, include_cursor: bool) -> Result<Image, CaptureError> {
    if rect.is_empty() {
        return Err(CaptureError::Empty);
    }
    match b {
        Backend::Gdi => gdi::grab(rect, include_cursor),
        Backend::Dxgi => {
            let mut img = worker::dxgi_grab(rect)?;
            if include_cursor {
                cursor::composite(&mut img, rect.x, rect.y);
            }
            Ok(img)
        }
        Backend::Wgc => grab_wgc_rect(rect, include_cursor),
    }
}

/// WGC capture of a screen rect: one monitor item per monitor the rect touches.
fn grab_wgc_rect(rect: Rect, include_cursor: bool) -> Result<Image, CaptureError> {
    let outs: Vec<_> = d3d::outputs().into_iter().filter(|o| !o.rect.intersect(&rect).is_empty()).collect();
    if outs.is_empty() {
        return Err(CaptureError::Failed("WGC: area is not on any monitor".into()));
    }
    let mut img = Image::filled(rect.w as u32, rect.h as u32, [0, 0, 0, 255]);
    for o in &outs {
        let part = o.rect.intersect(&rect);
        let local = part.offset(-o.rect.x, -o.rect.y);
        let white = o.hdr.then(|| hdr::sdr_white_level(&o.device));
        let mut sub = worker::wgc_monitor(o.hmonitor, local, include_cursor, white)?;
        sub.set_opaque();
        if outs.len() == 1 && part == rect && sub.width == rect.w as u32 && sub.height == rect.h as u32 {
            return Ok(sub);
        }
        img.blit(&sub, part.x - rect.x, part.y - rect.y);
    }
    Ok(img)
}

fn window_alive(hwnd: isize) -> bool {
    // SAFETY: plain query.
    unsafe { IsWindow(win::hwnd_from_isize(hwnd)) }.as_bool()
}

/// Capture a single window (even if partly covered, where WGC allows).
/// Returns the image and the window's visible bounds.
pub fn grab_window(hwnd: isize, opts: &GrabOptions) -> Result<(Image, Rect), CaptureError> {
    if !window_alive(hwnd) {
        return Err(CaptureError::WindowGone);
    }
    let h = win::hwnd_from_isize(hwnd);
    let bounds = win::window_bounds(h);
    if bounds.is_empty() {
        return Err(CaptureError::Empty);
    }
    // SAFETY: plain query.
    let minimized = unsafe { IsIconic(h) }.as_bool();
    if matches!(opts.engine, CaptureEngine::Auto | CaptureEngine::Wgc) && !minimized {
        let t0 = qpc_now();
        match grab_window_wgc(hwnd, bounds, opts.include_cursor) {
            Ok(img) => {
                log::debug!("grab_window via wgc in {:.1} ms", qpc_ms(t0, qpc_now()));
                return Ok((img, bounds));
            }
            Err(e) => {
                if !window_alive(hwnd) {
                    return Err(CaptureError::WindowGone);
                }
                log::warn!("WGC window capture failed, falling back to the screen: {e}");
            }
        }
    }
    let fallback = GrabOptions {
        engine: if opts.engine == CaptureEngine::Wgc { CaptureEngine::Auto } else { opts.engine },
        include_cursor: opts.include_cursor,
    };
    grab_rect(bounds, &fallback).map(|img| (img, bounds))
}

fn grab_window_wgc(hwnd: isize, bounds: Rect, include_cursor: bool) -> Result<Image, CaptureError> {
    let mon = win::monitor_at(bounds.center());
    let out = mon.and_then(|m| d3d::outputs().into_iter().find(|o| o.hmonitor == m.handle));
    let white = out.filter(|o| o.hdr).map(|o| hdr::sdr_white_level(&o.device));
    let mut img = worker::wgc_window(hwnd, include_cursor, white)?;
    img.set_opaque();
    let mut wr = Default::default();
    // SAFETY: valid out pointer.
    let _ = unsafe { GetWindowRect(win::hwnd_from_isize(hwnd), &mut wr) };
    let wr = win::rect_from_win(&wr);
    Ok(img.crop(window_crop(img.width as i32, img.height as i32, wr, bounds)))
}

/// Where the visible bounds lie inside a WGC window frame of size `cw`×`ch`.
/// WGC frames cover the full window rect (with the invisible resize borders);
/// if the size matches neither rect (resize in flight), centre the bounds.
pub fn window_crop(cw: i32, ch: i32, window_rect: Rect, bounds: Rect) -> Rect {
    if (cw, ch) == (window_rect.w, window_rect.h) {
        Rect::new(bounds.x - window_rect.x, bounds.y - window_rect.y, bounds.w, bounds.h)
    } else if (cw, ch) == (bounds.w, bounds.h) {
        Rect::new(0, 0, bounds.w, bounds.h)
    } else {
        Rect::new(((cw - bounds.w) / 2).max(0), ((ch - bounds.h) / 2).max(0), bounds.w, bounds.h)
    }
}

/// Visible, non-cloaked top-level windows in z-order (front first), including menus
/// and tooltips; excludes `exclude` (our own windows) and the desktop/shell background.
pub fn snapshot_windows(exclude: &[isize]) -> Vec<WindowEntry> {
    windows_enum::snapshot(exclude)
}

/// Nested element rectangles under `p` inside top-level window `top_hwnd`, from the
/// outermost (the window itself) to the innermost (child controls, then UI Automation
/// elements where available). Used by the window/object overlay (wheel changes level).
pub fn element_chain_at(top_hwnd: isize, p: Point) -> Vec<Rect> {
    uia::element_chain_at(top_hwnd, p)
}

/// Release cached GPU/duplication resources (called when the app goes idle).
pub fn release_idle_resources() {
    worker::release();
    d3d::invalidate_outputs();
}

/// Time each backend capturing `rect` (for `--bench`). Returns (name, ms or error).
/// "cold" includes creating the device/duplication; "warm" is the best of three repeats.
pub fn bench_engines(rect: Rect) -> Vec<(&'static str, Result<f64, String>)> {
    const NAMES: [(Backend, &str, &str); 3] = [
        (Backend::Gdi, "gdi-cold", "gdi-warm"),
        (Backend::Dxgi, "dxgi-cold", "dxgi-warm"),
        (Backend::Wgc, "wgc-cold", "wgc-warm"),
    ];
    let mut out = Vec::new();
    for (b, cold_name, warm_name) in NAMES {
        release_idle_resources();
        let time = || {
            let t0 = qpc_now();
            grab_with(b, rect, false).map(|_| qpc_ms(t0, qpc_now())).map_err(|e| e.to_string())
        };
        let cold = time();
        let failed = cold.is_err();
        out.push((cold_name, cold));
        if failed {
            out.push((warm_name, Err("skipped (cold capture failed)".into())));
            continue;
        }
        let mut best: Result<f64, String> = Err("no runs".into());
        for _ in 0..3 {
            match (time(), &best) {
                (Ok(ms), Ok(b)) if ms >= *b => {}
                (Ok(ms), _) => best = Ok(ms),
                (Err(e), Err(_)) => best = Err(e),
                (Err(_), Ok(_)) => {}
            }
        }
        out.push((warm_name, best));
    }
    release_idle_resources();
    out
}

#[cfg(test)]
mod tests;
