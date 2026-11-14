//! The capture worker thread: owns every D3D/DXGI/WGC object.
//!
//! COM/WinRT objects stay on one thread (initialized for the multithreaded apartment), and
//! callers on any thread send jobs and block on the reply. After [`IDLE_RELEASE`] without
//! work the cached device, duplications and staging textures are dropped, so the idle
//! process holds no GPU memory.

use super::dxgi::DxgiCache;
use super::wgc::{self, ItemFactory, WgcDevice};
use super::{d3d, CaptureError};
use crate::common::{Image, Rect};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const IDLE_RELEASE: Duration = Duration::from_secs(10);
/// Upper bound for one job; a hung driver must not hang the caller forever.
const JOB_TIMEOUT: Duration = Duration::from_secs(5);

/// True while DXGI duplications are cached (the next DXGI grab is a few ms).
static DXGI_WARM: AtomicBool = AtomicBool::new(false);

type Reply = Result<(Image, (i32, i32)), CaptureError>;

enum Job {
    Dxgi { rect: Rect },
    WgcMonitor { hmonitor: isize, region: Rect, cursor: bool, hdr_white: Option<f32> },
    WgcWindow { hwnd: isize, cursor: bool, hdr_white: Option<f32> },
    PrewarmDxgi,
    Release,
}

struct Request {
    job: Job,
    reply: mpsc::SyncSender<Reply>,
}

fn sender() -> Result<Sender<Request>, CaptureError> {
    static TX: OnceLock<Mutex<Option<Sender<Request>>>> = OnceLock::new();
    let slot = TX.get_or_init(|| Mutex::new(None));
    let mut g = slot.lock().map_err(|_| CaptureError::Failed("capture worker lock poisoned".into()))?;
    if let Some(tx) = g.as_ref() {
        return Ok(tx.clone());
    }
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("snapforge-capture".into())
        .stack_size(256 * 1024)
        .spawn(move || run(rx))
        .map_err(|e| CaptureError::Failed(format!("spawn capture worker: {e}")))?;
    *g = Some(tx.clone());
    Ok(tx)
}

fn call(job: Job) -> Reply {
    let (rtx, rrx) = mpsc::sync_channel(1);
    sender()?.send(Request { job, reply: rtx }).map_err(|_| CaptureError::Failed("capture worker stopped".into()))?;
    match rrx.recv_timeout(JOB_TIMEOUT) {
        Ok(r) => r,
        Err(RecvTimeoutError::Timeout) => Err(CaptureError::Failed("capture worker timed out".into())),
        Err(RecvTimeoutError::Disconnected) => Err(CaptureError::Failed("capture worker failed".into())),
    }
}

pub fn dxgi_grab(rect: Rect) -> Result<Image, CaptureError> {
    call(Job::Dxgi { rect }).map(|r| r.0)
}

/// Grab `region` (monitor-local coordinates) of a monitor.
pub fn wgc_monitor(hmonitor: isize, region: Rect, cursor: bool, hdr_white: Option<f32>) -> Result<Image, CaptureError> {
    call(Job::WgcMonitor { hmonitor, region, cursor, hdr_white }).map(|r| r.0)
}

/// Grab a whole window. Returns the image (full WGC content) and the content size.
pub fn wgc_window(hwnd: isize, cursor: bool, hdr_white: Option<f32>) -> Result<Image, CaptureError> {
    call(Job::WgcWindow { hwnd, cursor, hdr_white }).map(|r| r.0)
}

fn post(job: Job) {
    if let Ok(tx) = sender() {
        let (rtx, _rrx) = mpsc::sync_channel(1);
        let _ = tx.send(Request { job, reply: rtx });
    }
}

pub fn release() {
    post(Job::Release);
}

pub fn dxgi_is_warm() -> bool {
    DXGI_WARM.load(Ordering::Acquire)
}

/// Create the DXGI device and duplications in the background (no waiting).
pub fn prewarm_dxgi() {
    post(Job::PrewarmDxgi);
}

#[derive(Default)]
struct State {
    dxgi: DxgiCache,
    wgc: Option<WgcDevice>,
    items: Option<ItemFactory>,
}

impl State {
    fn has_resources(&self) -> bool {
        !self.dxgi.is_empty() || self.wgc.is_some() || self.items.is_some()
    }

    fn release(&mut self) {
        if self.has_resources() {
            log::debug!("capture worker: releasing GPU resources");
        }
        self.dxgi.clear();
        self.wgc = None;
        self.items = None;
    }

    fn items(&mut self) -> Result<&mut ItemFactory, CaptureError> {
        if self.items.is_none() {
            self.items = Some(ItemFactory::new()?);
        }
        self.items.as_mut().ok_or_else(|| CaptureError::Failed("no capture item factory".into()))
    }

    fn wgc_device(&mut self) -> Result<&WgcDevice, CaptureError> {
        if self.wgc.is_none() {
            let (device, ctx) = d3d::create_device(None)?;
            self.wgc = Some(WgcDevice::new(device, ctx)?);
        }
        self.wgc.as_ref().ok_or_else(|| CaptureError::Failed("no WGC device".into()))
    }

    fn handle(&mut self, job: Job) -> Reply {
        match job {
            Job::Dxgi { rect } => {
                let r = self.dxgi.grab(rect);
                if r.is_err() {
                    // Don't keep half-broken duplications around.
                    self.dxgi.clear();
                }
                r.map(|img| {
                    let s = (img.width as i32, img.height as i32);
                    (img, s)
                })
            }
            Job::WgcMonitor { hmonitor, region, cursor, hdr_white } => {
                if !wgc::is_supported() {
                    return Err(CaptureError::Failed("WGC is not supported".into()));
                }
                let item = self.items()?.monitor(hmonitor)?;
                let r = wgc::grab(self.wgc_device()?, &item, Some(region), cursor, hdr_white);
                if r.is_err() {
                    // Monitor handles change with display configuration; start fresh.
                    self.wgc = None;
                    self.items = None;
                }
                r
            }
            Job::WgcWindow { hwnd, cursor, hdr_white } => {
                if !wgc::is_supported() {
                    return Err(CaptureError::Failed("WGC is not supported".into()));
                }
                let item = self.items()?.window(hwnd)?;
                wgc::grab(self.wgc_device()?, &item, None, cursor, hdr_white)
            }
            Job::PrewarmDxgi => {
                if let Err(e) = self.dxgi.prewarm() {
                    log::debug!("DXGI prewarm failed: {e}");
                    self.dxgi.clear();
                }
                Err(CaptureError::Cancelled)
            }
            Job::Release => {
                self.release();
                Err(CaptureError::Cancelled)
            }
        }
    }
}

fn run(rx: Receiver<Request>) {
    // SAFETY: initializes WinRT/COM (MTA) for this thread only; balanced at exit.
    let inited = unsafe {
        windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED).is_ok()
    };
    let mut state = State::default();
    loop {
        let req = if state.has_resources() {
            match rx.recv_timeout(IDLE_RELEASE) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    state.release();
                    DXGI_WARM.store(false, Ordering::Release);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(r) => r,
                Err(_) => break,
            }
        };
        let reply = state.handle(req.job);
        DXGI_WARM.store(!state.dxgi.is_empty(), Ordering::Release);
        let _ = req.reply.try_send(reply);
    }
    state.release();
    drop(state);
    if inited {
        // SAFETY: balances the successful RoInitialize above; all objects are dropped.
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}
