//! Screen recorder (Phase 4).
//!
//! Threads per recording, joined by bounded channels:
//! * `rec-video`: WGC (or DXGI duplication) capture, Direct2D compositing (cursor, click
//!   rings, F5 zoom, webcam), GPU NV12 conversion, constant frame rate ([`video`]).
//! * `rec-mic` / `rec-loopback`: WASAPI capture onto the shared 48 kHz timeline ([`wasapi`]).
//! * `rec-mixer`: mixes the timeline every 10 ms (AGC, limiter, level meter) ([`dsp`]).
//! * `rec-encoder`: the only owner of the `IMFSinkWriter` ([`mf`]).
//! * `rec-ui`: control bar, region frame, F5 hotkey, click hook ([`bar`]).
//!
//! A/V sync: both streams are timestamped from one QPC-based [`clock::MediaClock`];
//! video frame `k` has time `k / fps` and audio sample `n` has time `n / 48000`, and both
//! are placed on that clock (audio packets by their WASAPI QPC position). Pausing shifts
//! the clock, so later timestamps continue without a gap.
//!
//! Integration (UI thread): see the functions below; nothing here blocks the caller for
//! longer than the overlay selection itself.

pub mod bar;
pub mod clock;
pub mod convert;
pub mod cursor;
pub mod dsp;
pub mod journal;
pub mod mf;
pub mod schedule;
pub mod video;
pub mod wasapi;
pub mod webcam;

#[cfg(test)]
mod tests;

use crate::app::RecordTarget;
use crate::common::{win, Rect};
use crate::settings::recorder::{parse_color, RecorderSettings, VideoFormat};
use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use video::{Control, EncMsg, Live, Target, VideoParams};
use wasapi::Shared;

// Public API for the settings window and the "Convert to MP4" tool.
pub use convert::{convert_to_mp4, extract_audio, AudioFormat, ConvertOptions, Resize};
pub use wasapi::{list_inputs as list_audio_inputs, AudioDevice};
pub use webcam::{list_cameras, Camera};

/// What to record, after any selection UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// A rectangle of the virtual screen (kept inside the monitor holding its centre).
    Rect(Rect),
    /// A top-level window.
    Window(isize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Starting,
    Recording,
    Finishing,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Status {
    pub paused: bool,
    /// Recorded time (pauses excluded).
    pub elapsed_secs: u64,
    /// Mixed audio (peak, rms), linear 0..=1.
    pub level: (f32, f32),
    pub frames: u64,
    pub dropped: u64,
}

/// Result of a finished recording.
#[derive(Debug, Clone)]
pub struct Finished {
    pub path: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub hardware_encoder: bool,
    pub backend: &'static str,
}

/// Overrides for tests and scheduled recordings.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub out_path: Option<PathBuf>,
    /// Do not show the control bar (tests).
    pub headless: bool,
    pub settings: Option<RecorderSettings>,
    /// Put the PC to sleep after this recording is finalized.
    pub sleep_after: bool,
}

const ST_IDLE: u8 = 0;
const ST_STARTING: u8 = 1;
const ST_RECORDING: u8 = 2;
const ST_FINISHING: u8 = 3;

static STATE: AtomicU8 = AtomicU8::new(ST_IDLE);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static LEVEL: dsp::Level = dsp::Level::new();
/// Audio buffered before mixing, so late packets still land in place.
const MIX_LATENCY: i64 = clock::HNS_PER_SEC / 10;

struct Session {
    shared: Arc<Shared>,
    control: Arc<Control>,
    live: Arc<Live>,
    audio_stop: Arc<AtomicBool>,
    mixer_done: Arc<AtomicBool>,
    video: Option<std::thread::JoinHandle<()>>,
    audio: Vec<std::thread::JoinHandle<()>>,
    mixer: Option<std::thread::JoinHandle<()>>,
    encoder: Option<std::thread::JoinHandle<Result<bool>>>,
    bar: Option<bar::BarHandle>,
    journal: journal::Journal,
    journal_path: PathBuf,
    format: VideoFormat,
    fps: u32,
    backend: &'static str,
    sleep_after: bool,
}

pub fn state() -> State {
    match STATE.load(Ordering::Acquire) {
        ST_STARTING => State::Starting,
        ST_RECORDING => State::Recording,
        ST_FINISHING => State::Finishing,
        _ => State::Idle,
    }
}

/// True from the moment a recording is requested until its file is finalized.
pub fn is_recording() -> bool {
    state() != State::Idle
}

pub fn status() -> Status {
    let g = SESSION.lock();
    let Ok(g) = g else { return Status::default() };
    let Some(s) = g.as_ref() else { return Status::default() };
    let (elapsed, paused) =
        s.shared.clock.lock().map(|c| (c.media_now(clock::qpc_hns()), c.is_paused())).unwrap_or((0, false));
    Status {
        paused,
        elapsed_secs: (elapsed / clock::HNS_PER_SEC).max(0) as u64,
        level: LEVEL.get(),
        frames: s.live.frames.load(Ordering::Relaxed),
        dropped: s.live.dropped.load(Ordering::Relaxed),
    }
}

/// AppCommand::Record(target), on the UI thread: shows the selection overlay for region
/// and window targets (and the countdown), then starts recording in the background.
pub fn select_and_start(target: RecordTarget) {
    if is_recording() {
        log::info!("recorder: already recording");
        return;
    }
    let area = match target {
        RecordTarget::FullScreen => None,
        RecordTarget::Region | RecordTarget::FixedRegion | RecordTarget::Window => {
            let mode = match target {
                RecordTarget::Region => crate::overlay::OverlayMode::Region,
                RecordTarget::FixedRegion => crate::overlay::OverlayMode::Fixed,
                _ => crate::overlay::OverlayMode::WindowObject,
            };
            let Some(frozen) = freeze() else {
                crate::app::notify(crate::i18n::t("app-name"), crate::i18n::t("rec-error-select"), true);
                return;
            };
            let sel = crate::overlay::select(&frozen, mode);
            drop(frozen);
            match sel {
                None => return,
                Some(crate::overlay::Selection::Window { hwnd, rect }) => {
                    if target == RecordTarget::Window {
                        Some(Area::Window(hwnd))
                    } else {
                        Some(Area::Rect(rect))
                    }
                }
                Some(crate::overlay::Selection::Rect(r)) => Some(Area::Rect(r)),
                Some(crate::overlay::Selection::Multi(v)) => {
                    v.iter().copied().reduce(|a, b| a.union(&b)).map(Area::Rect)
                }
                Some(crate::overlay::Selection::Polygon(pts)) => {
                    let r = pts.iter().fold(None::<Rect>, |acc, p| {
                        let pr = Rect::new(p.x, p.y, 1, 1);
                        Some(acc.map_or(pr, |a| a.union(&pr)))
                    });
                    r.map(Area::Rect)
                }
            }
        }
    };
    let countdown = crate::settings::get().recorder.countdown_secs;
    if countdown > 0 && !crate::overlay::countdown(countdown) {
        return;
    }
    if let Err(e) = start(target, area) {
        log::error!("recorder: {e:#}");
        crate::app::notify(crate::i18n::t("app-name"), format!("{e:#}"), true);
    }
}

/// The frozen screen for the selection overlay: the capture module, falling back to GDI
/// (keeps the recorder usable on its own).
fn freeze() -> Option<crate::overlay::Frozen> {
    let origin = win::virtual_screen();
    let opts = crate::capture::GrabOptions { include_cursor: false, ..crate::capture::GrabOptions::from_settings() };
    let image = match crate::capture::grab_rect(origin, &opts) {
        Ok(i) => i,
        Err(e) => {
            log::debug!("recorder: capture backend unavailable ({e}); using GDI for the overlay");
            gdi_grab(origin)?
        }
    };
    let mut ours = vec![crate::app::app_hwnd(), crate::overlay::overlay_hwnd()];
    ours.retain(|&h| h != 0);
    let windows = crate::capture::snapshot_windows(&ours);
    Some(crate::overlay::Frozen { image, origin, windows })
}

fn gdi_grab(r: Rect) -> Option<crate::common::Image> {
    use windows::Win32::Graphics::Gdi::*;
    // SAFETY: GDI objects created and released here; bits read after GdiFlush.
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: r.w,
                biHeight: -r.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        let mut out = None;
        if let Some(bmp) = bmp {
            let old = SelectObject(dc, bmp);
            if BitBlt(dc, 0, 0, r.w, r.h, screen, r.x, r.y, SRCCOPY | CAPTUREBLT).is_ok() && !bits.is_null() {
                let _ = GdiFlush();
                let mut data = std::slice::from_raw_parts(bits as *const u8, (r.w * r.h * 4) as usize).to_vec();
                data.chunks_exact_mut(4).for_each(|p| p[3] = 255);
                out = Some(crate::common::Image::from_bgra(r.w as u32, r.h as u32, data));
            }
            SelectObject(dc, old);
            let _ = DeleteObject(bmp);
        }
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

/// Start recording `area` (None = the monitor under the cursor) in the background.
/// Returns an error only if a recording is already running; later failures are
/// reported with a notification.
pub fn start(target: RecordTarget, area: Option<Area>) -> Result<()> {
    let _ = target;
    if STATE.compare_exchange(ST_IDLE, ST_STARTING, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err(anyhow!("a recording is already running"));
    }
    std::thread::Builder::new()
        .name("rec-start".into())
        .spawn(move || {
            if let Err(e) = start_session(area, StartOptions::default()) {
                log::error!("recorder: could not start: {e:#}");
                crate::app::notify(
                    crate::i18n::t("app-name"),
                    crate::i18n::tf("rec-error-start", &[("error", &format!("{e:#}"))]),
                    true,
                );
            }
        })
        .map(|_| ())
        .map_err(|e| {
            STATE.store(ST_IDLE, Ordering::Release);
            anyhow!("spawn: {e}")
        })
}

/// Start synchronously with explicit options (tests, scheduler). On error the state is
/// back to idle.
pub fn start_with(area: Option<Area>, opts: StartOptions) -> Result<()> {
    if STATE.compare_exchange(ST_IDLE, ST_STARTING, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return Err(anyhow!("a recording is already running"));
    }
    start_session(area, opts)
}

fn even(v: i32) -> i32 {
    (v.max(2)) & !1
}

/// Resolve the capture target, initial region and output size.
fn resolve(area: Option<Area>) -> Result<(Target, Rect, Rect)> {
    match area {
        Some(Area::Window(hwnd)) => {
            let b = win::window_bounds(win::hwnd_from_isize(hwnd));
            if b.is_empty() {
                return Err(anyhow!("the window has no visible area"));
            }
            let mon = win::monitor_at(b.center()).map(|m| m.rect).unwrap_or_else(win::virtual_screen);
            Ok((Target::Window { hwnd }, Rect::new(b.x, b.y, even(b.w), even(b.h)), mon))
        }
        Some(Area::Rect(r)) => {
            let m = win::monitor_at(r.center()).ok_or_else(|| anyhow!("no monitor"))?;
            let r = r.intersect(&m.rect);
            if r.w < 16 || r.h < 16 {
                return Err(anyhow!("the area is too small"));
            }
            Ok((Target::Monitor { handle: m.handle, rect: m.rect }, Rect::new(r.x, r.y, even(r.w), even(r.h)), m.rect))
        }
        None => {
            let m = win::monitor_at(win::cursor_pos()).ok_or_else(|| anyhow!("no monitor"))?;
            let r = m.rect;
            Ok((Target::Monitor { handle: m.handle, rect: r }, Rect::new(r.x, r.y, even(r.w), even(r.h)), r))
        }
    }
}

fn output_path(s: &RecorderSettings) -> Result<PathBuf> {
    let folder = s.output_folder();
    std::fs::create_dir_all(&folder).with_context(|| format!("create {}", folder.display()))?;
    let ext = s.format.extension();
    if s.filename_template.trim().is_empty() {
        return Ok(crate::settings::filename::next_output_path(&folder, ext));
    }
    let mut fs = crate::settings::get().filename.clone();
    fs.template = s.filename_template.clone();
    let mut rng = crate::common::rng::Rng::from_entropy();
    let now = crate::common::time::DateTime::now_local();
    let (p, next) = crate::settings::filename::build_path(
        &fs,
        &folder,
        ext,
        &now,
        &mut rng,
        &crate::settings::filename::default_env,
        &|p| p.exists() || journal::partial_for(p).exists(),
    );
    crate::settings::update(|st| {
        st.filename.counter = next.counter;
        st.filename.last_date = next.last_date.clone();
    });
    Ok(p)
}

fn start_session(area: Option<Area>, opts: StartOptions) -> Result<()> {
    let r = start_session_inner(area, opts);
    if r.is_err() {
        STATE.store(ST_IDLE, Ordering::Release);
    }
    r
}

fn start_session_inner(area: Option<Area>, opts: StartOptions) -> Result<()> {
    let s = opts.settings.clone().unwrap_or_else(|| crate::settings::get().recorder.clone());
    mf::startup()?;
    let final_path = match &opts.out_path {
        Some(p) => p.clone(),
        None => output_path(&s)?,
    };
    let partial = if s.format == VideoFormat::Mp4 { journal::partial_for(&final_path) } else { final_path.clone() };
    let (target, region, monitor) = resolve(area)?;
    let fps = s.fps_clamped();
    let with_video = s.format != VideoFormat::Mp3AudioOnly;
    let with_audio = s.record_system_audio || s.record_microphone;
    if !with_video && !with_audio {
        return Err(anyhow!("{}", crate::i18n::t("rec-error-no-audio")));
    }

    let shared = Arc::new(Shared {
        mixer: Mutex::new(dsp::Mixer::new(2)),
        clock: Mutex::new(clock::MediaClock::new(i64::MAX / 4)),
    });
    let control = Arc::new(Control { stop: AtomicBool::new(false), stop_media: AtomicI64::new(i64::MAX) });
    let live = Arc::new(Live::new(region));
    // Separate bounded queues: the sink writer may block one stream until the other
    // catches up, so each stream has its own feeding thread.
    let (tx, rx) = sync_channel::<EncMsg>(32);
    let (atx, arx) = sync_channel::<EncMsg>(64);

    // Encoder thread (owns the sink writer). Created before the video pipeline unless
    // it needs that pipeline's D3D device (zero-copy GPU input).
    let want_gpu = with_video
        && s.zero_copy_gpu
        && s.format == VideoFormat::Mp4
        && s.encoder != crate::settings::recorder::EncoderPreference::Software;
    let enc_cfg = (partial.clone(), s.clone(), region.w as u32, region.h as u32, fps, with_audio);
    let mut queues = Some((rx, arx));
    let mut encoder = None;
    let mut enc_info = (false, false);
    if !want_gpu {
        if let Some((rx, arx)) = queues.take() {
            let (h, info) = spawn_encoder(enc_cfg.clone(), None, rx, arx)?;
            encoder = Some(h);
            enc_info = info;
        }
    }

    // 1. Video thread: GPU + source setup, then it waits for "go".
    let mut video_thread = None;
    let mut go_tx = None;
    let mut init = None;
    if with_video {
        let highlight = s
            .highlight_clicks
            .then(|| (parse_color(&s.highlight_color).unwrap_or([255, 200, 0, 192]), s.highlight_size.clamp(16, 200)));
        let params = VideoParams {
            target,
            width: region.w as u32,
            height: region.h as u32,
            fps,
            show_cursor: s.show_cursor,
            highlight,
            follow_mouse: s.follow_mouse && matches!(area, Some(Area::Rect(_))),
            zoom_factor: s.zoom_factor,
            webcam: s.webcam.enabled.then(|| s.webcam.clone()),
            want_gpu_input: want_gpu,
        };
        let (ready_tx, ready_rx) = sync_channel(1);
        let (gtx, grx) = sync_channel::<bool>(1);
        let (l, sh, c, t) = (live.clone(), shared.clone(), control.clone(), tx.clone());
        video_thread = Some(
            std::thread::Builder::new()
                .name("rec-video".into())
                .spawn(move || video::run(params, l, sh, c, ready_tx, grx, t))?,
        );
        go_tx = Some(gtx);
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(i)) => init = Some(i),
            Ok(Err(e)) => return Err(e.context("video capture")),
            Err(_) => return Err(anyhow!("video capture did not start")),
        }
    }

    let backend = init.as_ref().map(|i| i.backend).unwrap_or("none");
    if let Some(i) = &init {
        log::info!(
            "recorder: capture backend {backend}, frames handed over as {}",
            if i.gpu_input { "GPU textures" } else { "system memory" }
        );
    }
    if let Some((rx, arx)) = queues.take() {
        let devmgr = init.as_mut().and_then(|i| i.devmgr.take());
        match spawn_encoder(enc_cfg, devmgr, rx, arx) {
            Ok((h, info)) => {
                encoder = Some(h);
                enc_info = info;
            }
            Err(e) => {
                drop(go_tx);
                drop(tx);
                drop(atx);
                if let Some(v) = video_thread {
                    let _ = v.join();
                }
                return Err(e);
            }
        }
    }
    let encoder = encoder.ok_or_else(|| anyhow!("no encoder"))?;
    let (gpu_ok, hardware) = enc_info;
    log::info!(
        "recorder: encoder ready ({}; {} input) -> {}",
        if hardware { "hardware" } else { "software" },
        if gpu_ok { "GPU" } else { "system memory" },
        partial.display()
    );

    // 3. Journal for crash recovery.
    let jdir = journal::journal_dir();
    let j = journal::Journal {
        partial: partial.clone(),
        final_path: final_path.clone(),
        format: s.format.extension().to_string(),
        started: crate::common::time::DateTime::now_local().display(),
        pid: std::process::id(),
    };
    let journal_path = j.write(&jdir).unwrap_or_else(|e| {
        log::warn!("recorder: journal not written: {e:#}");
        j.path_in(&jdir)
    });

    // 4. Start the clock, audio and mixer, then release the video thread.
    if let Ok(mut c) = shared.clock.lock() {
        *c = clock::MediaClock::new(clock::qpc_hns());
    }
    let audio_stop = Arc::new(AtomicBool::new(false));
    let mut audio = Vec::new();
    let mixer_done = Arc::new(AtomicBool::new(false));
    let mut mixer = None;
    if with_audio {
        if let Ok(mut m) = shared.mixer.lock() {
            m.inputs[0].gain = s.system_volume.min(200) as f32 / 100.0;
            m.inputs[1].gain = s.mic_volume.min(200) as f32 / 100.0;
            if s.auto_boost {
                m.inputs[1].agc = Some(dsp::Agc::default());
            }
        }
        if s.record_system_audio {
            audio.push(wasapi::spawn(
                wasapi::Source::Loopback { follow: s.auto_switch_output },
                0,
                shared.clone(),
                audio_stop.clone(),
            )?);
        }
        if s.record_microphone {
            audio.push(wasapi::spawn(
                wasapi::Source::Mic(s.mic_device_id.clone()),
                1,
                shared.clone(),
                audio_stop.clone(),
            )?);
        }
        let (sh, c, d, t) = (shared.clone(), control.clone(), mixer_done.clone(), atx.clone());
        mixer = Some(std::thread::Builder::new().name("rec-mixer".into()).spawn(move || mixer_thread(sh, c, d, t))?);
    }
    drop(tx);
    drop(atx);
    if let Some(g) = go_tx {
        let _ = g.send(gpu_ok);
    }

    // 5. Control bar.
    let bar = if opts.headless {
        None
    } else {
        let frame = matches!(area, Some(Area::Rect(_))).then_some(region).filter(|r| *r != monitor);
        bar::spawn(
            bar::BarConfig {
                colors: s.control_bar.clone(),
                frame,
                monitor,
                click_sounds: s.click_sounds,
                click_volume: s.click_volume,
            },
            live.clone(),
        )
    };
    LEVEL.reset();
    if let Ok(mut g) = SESSION.lock() {
        *g = Some(Session {
            shared,
            control,
            live,
            audio_stop,
            mixer_done,
            video: video_thread,
            audio,
            mixer,
            encoder: Some(encoder),
            bar,
            journal: j,
            journal_path,
            format: s.format,
            fps,
            backend,
            sleep_after: opts.sleep_after,
        });
    }
    STATE.store(ST_RECORDING, Ordering::Release);
    log::info!("recorder: recording {}x{} @ {fps} fps, region {:?}", region.w, region.h, region);
    Ok(())
}

type EncCfg = (PathBuf, RecorderSettings, u32, u32, u32, bool);
type EncHandle = std::thread::JoinHandle<Result<bool>>;

/// Start the encoder thread and wait until the sink writer is ready.
/// Returns (GPU input accepted, hardware encoder).
fn spawn_encoder(
    cfg: EncCfg,
    devmgr: Option<video::SendCom<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>>,
    rx: Receiver<EncMsg>,
    arx: Receiver<EncMsg>,
) -> Result<(EncHandle, (bool, bool))> {
    let (ready_tx, ready_rx) = sync_channel::<Result<(bool, bool)>>(1);
    let h = std::thread::Builder::new()
        .name("rec-encoder".into())
        .spawn(move || encoder_thread(cfg, devmgr, rx, arx, ready_tx))?;
    match ready_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(v)) => Ok((h, v)),
        Ok(Err(e)) => {
            let _ = h.join();
            Err(e)
        }
        Err(_) => Err(anyhow!("the encoder did not start")),
    }
}

fn encoder_thread(
    cfg: EncCfg,
    devmgr: Option<video::SendCom<windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>>,
    rx: Receiver<EncMsg>,
    arx: Receiver<EncMsg>,
    ready: std::sync::mpsc::SyncSender<Result<(bool, bool)>>,
) -> Result<bool> {
    let _com = mf::ComGuard::mta();
    let (path, s, w, h, fps, audio) = cfg;
    let make = |d3d: Option<&windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager>| {
        mf::Encoder::new(&mf::EncoderConfig {
            path: &path,
            format: s.format,
            fragmented: true,
            width: w,
            height: h,
            fps,
            video_bitrate: s.video_bitrate(w, h),
            audio_kbps: s.audio_bitrate_kbps,
            video: true,
            audio,
            encoder: s.encoder,
            keyframe_secs: s.keyframe_interval_secs,
            d3d,
        })
    };
    let (enc, gpu) = match devmgr.as_ref().map(|d| make(Some(&d.0))) {
        Some(Ok(e)) => (e, true),
        Some(Err(e)) => {
            log::warn!("recorder: GPU input refused ({e:#}); using system memory");
            let _ = std::fs::remove_file(&path);
            match make(None) {
                Ok(e) => (e, false),
                Err(e) => {
                    let _ = ready.send(Err(e));
                    return Ok(false);
                }
            }
        }
        None => match make(None) {
            Ok(e) => (e, false),
            Err(e) => {
                let _ = ready.send(Err(e));
                return Ok(false);
            }
        },
    };
    let hw = enc.hardware;
    let _ = ready.send(Ok((gpu, hw)));
    // Writes until both queues are closed; after an error, keeps draining so producers
    // never block.
    let pump = |q: Receiver<EncMsg>| -> Result<()> {
        let mut failed: Option<anyhow::Error> = None;
        for msg in q {
            if failed.is_some() {
                continue;
            }
            let r = match msg {
                EncMsg::Video(sample) => enc.write_video(&sample.0),
                EncMsg::Audio(frames) => enc.write_audio(&frames),
            };
            if let Err(e) = r {
                log::error!("recorder: encoder: {e:#}");
                failed = Some(e);
            }
        }
        failed.map_or(Ok(()), Err)
    };
    let (rv, ra) = std::thread::scope(|sc| {
        let audio = std::thread::Builder::new().name("rec-audio-writer".into()).spawn_scoped(sc, || {
            let _com = mf::ComGuard::mta();
            pump(arx)
        });
        let rv = pump(rx);
        let ra = match audio {
            Ok(h) => h.join().unwrap_or_else(|_| Err(anyhow!("audio writer panicked"))),
            Err(e) => Err(anyhow!("spawn audio writer: {e}")),
        };
        (rv, ra)
    });
    let fin = enc.finish();
    rv?;
    ra?;
    fin.map(|_| hw)
}

fn mixer_thread(
    shared: Arc<Shared>,
    control: Arc<Control>,
    done: Arc<AtomicBool>,
    tx: std::sync::mpsc::SyncSender<EncMsg>,
) {
    loop {
        let finishing = done.load(Ordering::Acquire);
        let stop_media = control.stop_media.load(Ordering::Acquire);
        let media_now = shared.clock.lock().map(|c| c.media_now(clock::qpc_hns())).unwrap_or(0);
        let target_t = if finishing { stop_media } else { (media_now - MIX_LATENCY).min(stop_media) };
        let target = clock::sample_index(target_t.max(0), dsp::RATE);
        let frames = match shared.mixer.lock() {
            Ok(mut m) => m.mix_until(target),
            Err(_) => break,
        };
        if !frames.is_empty() {
            LEVEL.store(&frames[frames.len().saturating_sub(4800)..]);
            // Blocking send: bounded queue, the encoder is the only consumer.
            if tx.send(EncMsg::Audio(frames)).is_err() {
                break;
            }
        }
        if finishing {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pause or resume the running recording.
pub fn toggle_pause() {
    if let Ok(g) = SESSION.lock() {
        if let Some(s) = g.as_ref() {
            if let Ok(mut c) = s.shared.clock.lock() {
                let now = clock::qpc_hns();
                if c.is_paused() {
                    c.resume(now);
                    log::info!("recorder: resumed");
                } else {
                    c.pause(now);
                    LEVEL.reset();
                    log::info!("recorder: paused");
                }
            }
        }
    }
}

/// Stop and finalize in the background; a notification reports the file.
pub fn stop() {
    if STATE.compare_exchange(ST_RECORDING, ST_FINISHING, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("rec-finish".into()).spawn(|| match finish() {
        Ok(f) => {
            let open_after = crate::settings::get().recorder.open_after;
            // Clicking the balloon opens the recording in the video editor.
            let key = if crate::video_editor::accepts(&f.path) { "rec-saved-edit" } else { "rec-saved" };
            crate::app::notify_file(
                crate::i18n::t("app-name"),
                crate::i18n::tf(key, &[("path", &f.path.display().to_string())]),
                f.path.clone(),
            );
            if open_after {
                win::reveal_in_explorer(&f.path);
            }
        }
        Err(e) => {
            log::error!("recorder: finishing failed: {e:#}");
            crate::app::notify(
                crate::i18n::t("app-name"),
                crate::i18n::tf("rec-error-finish", &[("error", &format!("{e:#}"))]),
                true,
            );
        }
    });
}

/// Stop and finalize on the calling thread (tests, scheduler).
pub fn stop_blocking() -> Result<Finished> {
    match STATE.compare_exchange(ST_RECORDING, ST_FINISHING, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => finish(),
        Err(_) => Err(anyhow!("not recording")),
    }
}

fn finish() -> Result<Finished> {
    let r = finish_inner();
    STATE.store(ST_IDLE, Ordering::Release);
    r
}

fn finish_inner() -> Result<Finished> {
    let Some(mut s) = SESSION.lock().ok().and_then(|mut g| g.take()) else {
        return Err(anyhow!("no session"));
    };
    // The end of the recording on the media clock; both streams stop exactly here.
    let now = s.shared.clock.lock().map(|c| c.media_now(clock::qpc_hns())).unwrap_or(0);
    // Round up to a frame boundary so the last video frame and the audio end together.
    let end = clock::frame_time(clock::frame_at_or_after(now, s.fps), s.fps);
    s.control.stop_media.store(end, Ordering::Release);
    s.control.stop.store(true, Ordering::Release);
    if let Some(b) = s.bar.take() {
        b.close();
    }
    log::debug!("recorder: stopping at {} ms", end / 10_000);
    if let Some(v) = s.video.take() {
        let _ = v.join();
    }
    log::debug!("recorder: video joined");
    s.audio_stop.store(true, Ordering::Release);
    for a in s.audio.drain(..) {
        let _ = a.join();
    }
    log::debug!("recorder: audio joined");
    s.mixer_done.store(true, Ordering::Release);
    if let Some(m) = s.mixer.take() {
        let _ = m.join();
    }
    log::debug!("recorder: mixer joined");
    let hardware = match s.encoder.take().map(|e| e.join()) {
        Some(Ok(r)) => r?,
        Some(Err(_)) => return Err(anyhow!("encoder thread panicked")),
        None => false,
    };
    LEVEL.reset();
    let j = s.journal.clone();
    let path = finalize_file(&j, s.format)?;
    let _ = std::fs::remove_file(&s.journal_path);
    let f = Finished {
        path,
        frames: s.live.frames.load(Ordering::Relaxed),
        dropped: s.live.dropped.load(Ordering::Relaxed),
        duplicated: s.live.duplicated.load(Ordering::Relaxed),
        hardware_encoder: hardware,
        backend: s.backend,
    };
    log::info!("recorder: saved {} ({} frames, {} dropped)", f.path.display(), f.frames, f.dropped);
    if s.sleep_after {
        schedule::sleep_now();
    }
    Ok(f)
}

/// Turn the partial file into the final one (remux fragmented MP4 into a regular MP4).
fn finalize_file(j: &journal::Journal, format: VideoFormat) -> Result<PathBuf> {
    if format != VideoFormat::Mp4 || j.partial == j.final_path {
        return Ok(j.final_path.clone());
    }
    let dst = journal::unique_path(&j.final_path);
    match mf::remux_to_mp4(&j.partial, &dst) {
        Ok(()) => {
            let _ = std::fs::remove_file(&j.partial);
            Ok(dst)
        }
        Err(e) => {
            log::warn!("recorder: remux failed ({e:#}); keeping the fragmented MP4");
            let _ = std::fs::remove_file(&dst);
            std::fs::rename(&j.partial, &dst)?;
            Ok(dst)
        }
    }
}

/// Startup: finalize recordings interrupted by a crash or power loss (background thread).
/// Posts a notification for each recovered file.
pub fn recover_pending() {
    let _ = std::thread::Builder::new().name("rec-recover".into()).spawn(|| {
        let _com = mf::ComGuard::mta();
        for p in recover_pending_in(&journal::journal_dir()) {
            let text = crate::i18n::tf("rec-recovered", &[("path", &p.display().to_string())]);
            crate::app::notify_file(crate::i18n::t("app-name"), text, p);
        }
    });
}

/// Recover every pending journal in `dir`; returns the recovered files.
pub fn recover_pending_in(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (jpath, j) in journal::scan_pending(dir, std::process::id()) {
        let exists = j.partial.exists();
        let complete =
            if exists && j.format == "mp4" { journal::fmp4_complete_len_of(&j.partial).ok().flatten() } else { None };
        let plan = journal::plan(&j, exists, complete);
        log::info!("recorder: recovering {} -> {:?}", j.partial.display(), plan);
        let r: Result<Option<PathBuf>> = (|| match plan {
            journal::RecoveryPlan::DropJournal => Ok(None),
            journal::RecoveryPlan::Discard => {
                let bad = j.partial.with_extension("unrecoverable.mp4");
                let _ = std::fs::rename(&j.partial, &bad);
                Ok(None)
            }
            journal::RecoveryPlan::Rename => {
                if j.partial != j.final_path {
                    let dst = journal::unique_path(&j.final_path);
                    std::fs::rename(&j.partial, &dst)?;
                    Ok(Some(dst))
                } else {
                    Ok(Some(j.final_path.clone()))
                }
            }
            journal::RecoveryPlan::TruncateAndRemux { len } => {
                let f = std::fs::OpenOptions::new().write(true).open(&j.partial)?;
                f.set_len(len)?;
                drop(f);
                finalize_file(&j, VideoFormat::Mp4).map(Some)
            }
        })();
        match r {
            Ok(Some(p)) => out.push(p),
            Ok(None) => {}
            Err(e) => log::warn!("recorder: recovery of {} failed: {e:#}", j.partial.display()),
        }
        let _ = std::fs::remove_file(&jpath);
    }
    out
}
