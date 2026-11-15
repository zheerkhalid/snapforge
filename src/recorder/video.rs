//! Video capture thread.
//!
//! Pipeline per output frame, all on one D3D11 device:
//! 1. Source: Windows.Graphics.Capture (monitor or window, cursor off, no border), or
//!    DXGI Desktop Duplication as the fallback. New frames are copied on the GPU into a
//!    "mirror" texture; when nothing changed the mirror keeps the last image, so idle
//!    screens produce duplicate frames at a constant rate.
//! 2. Compose with Direct2D into the output-sized canvas: the region (or the animated
//!    F5 zoom rectangle) of the mirror is drawn scaled into the canvas, then the cursor,
//!    click rings and the webcam picture-in-picture are drawn on top.
//! 3. Convert BGRA -> NV12 (BT.709, limited range) with the D3D11 video processor into a
//!    pooled NV12 texture that is handed to the encoder as an `MFCreateDXGISurfaceBuffer`
//!    sample (hardware encoders read it without a copy). A tracked-sample callback
//!    returns the texture to the pool when the encoder is done with it.
//!    Fallbacks: NV12 read back to system memory (software encoders, WMV), or a CPU
//!    conversion when the video processor is not available.

use super::clock::{self, frame_at_or_after, frame_time};
use super::cursor;
use super::mf;
use super::wasapi::Shared;
use super::webcam::Webcam;
use crate::common::{win, Point, Rect};
use crate::settings::recorder::{Corner, WebcamSettings, WebcamShape};
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::{implement, Interface};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{BOOL, E_NOTIMPL, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

/// COM pointers moved between threads (Media Foundation / D3D objects are free-threaded).
pub struct SendCom<T>(pub T);
// SAFETY: only used for free-threaded MF/D3D objects (device manager, samples).
unsafe impl<T> Send for SendCom<T> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A monitor; the recorded area is `Live::region` (inside the monitor).
    Monitor { handle: isize, rect: Rect },
    /// A top-level window, followed as it moves.
    Window { hwnd: isize },
}

#[derive(Debug, Clone, Copy)]
pub struct Click {
    pub pos: Point,
    pub at: i64,
    pub right: bool,
}

/// State shared with the control bar / region frame (UI thread side).
pub struct Live {
    /// Recorded area in virtual-screen pixels (moves when dragged / following the mouse).
    pub region: Mutex<Rect>,
    pub zoom_on: AtomicBool,
    pub clicks: Mutex<VecDeque<Click>>,
    /// Region frame window to move when the region follows the mouse (0 = none).
    pub frame_hwnd: AtomicIsize,
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
    pub duplicated: AtomicU64,
}

impl Live {
    pub fn new(region: Rect) -> Self {
        Self {
            region: Mutex::new(region),
            zoom_on: AtomicBool::new(false),
            clicks: Mutex::new(VecDeque::new()),
            frame_hwnd: AtomicIsize::new(0),
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            duplicated: AtomicU64::new(0),
        }
    }

    pub fn push_click(&self, c: Click) {
        if let Ok(mut q) = self.clicks.lock() {
            if q.len() >= 32 {
                q.pop_front();
            }
            q.push_back(c);
        }
    }
}

pub struct VideoParams {
    pub target: Target,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub show_cursor: bool,
    /// Straight-alpha RGBA ring colour and diameter.
    pub highlight: Option<([u8; 4], u32)>,
    pub follow_mouse: bool,
    pub zoom_factor: f32,
    pub webcam: Option<WebcamSettings>,
    /// Try to hand GPU textures to the encoder.
    pub want_gpu_input: bool,
}

pub struct VideoInit {
    pub gpu_input: bool,
    pub devmgr: Option<SendCom<IMFDXGIDeviceManager>>,
    pub backend: &'static str,
}

pub enum EncMsg {
    Video(SendCom<IMFSample>),
    Audio(Vec<super::dsp::Frame>),
}

pub struct Control {
    pub stop: AtomicBool,
    /// Media time at which video ends (set on stop); `i64::MAX` while recording.
    pub stop_media: AtomicI64,
}

/// Frame source copying into the mirror texture.
enum Source {
    Wgc(Wgc),
    Dup(Dup),
}

struct Wgc {
    pool: Direct3D11CaptureFramePool,
    _session: GraphicsCaptureSession,
    _item: GraphicsCaptureItem,
    device: IDirect3DDevice,
    size: SizeInt32,
}

struct Dup {
    output: IDXGIOutput1,
    dup: Option<IDXGIOutputDuplication>,
}

fn d3d_device_for(hmon: Option<HMONITOR>) -> Result<(ID3D11Device, ID3D11DeviceContext, Option<IDXGIOutput1>)> {
    // SAFETY: DXGI enumeration and device creation with valid out pointers.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut adapter: Option<IDXGIAdapter1> = None;
        let mut output: Option<IDXGIOutput1> = None;
        if let Some(hm) = hmon {
            'outer: for i in 0.. {
                let Ok(a) = factory.EnumAdapters1(i) else { break };
                for j in 0.. {
                    let Ok(o) = a.EnumOutputs(j) else { break };
                    if let Ok(d) = o.GetDesc() {
                        if d.Monitor == hm {
                            output = o.cast().ok();
                            adapter = Some(a.clone());
                            break 'outer;
                        }
                    }
                }
            }
        }
        let base = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        for flags in [base | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, base] {
            let mut dev = None;
            let mut ctx = None;
            let drv = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
            let ad: Option<IDXGIAdapter> = adapter.as_ref().and_then(|a| a.cast().ok());
            let r = D3D11CreateDevice(
                ad.as_ref(),
                drv,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            );
            if r.is_ok() {
                if let (Some(d), Some(c)) = (dev, ctx) {
                    return Ok((d, c, output));
                }
            }
        }
        Err(anyhow!("D3D11CreateDevice failed"))
    }
}

fn texture(
    dev: &ID3D11Device,
    w: u32,
    h: u32,
    fmt: DXGI_FORMAT,
    bind: D3D11_BIND_FLAG,
    staging: bool,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: fmt,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        BindFlags: if staging { 0 } else { bind.0 as u32 },
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: 0,
    };
    let mut t = None;
    // SAFETY: valid descriptor and out pointer.
    unsafe { dev.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.ok_or_else(|| anyhow!("CreateTexture2D returned null"))
}

impl Wgc {
    fn new(dev: &ID3D11Device, target: &Target) -> Result<Self> {
        // SAFETY: WinRT interop calls with valid handles.
        unsafe {
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
            let item: GraphicsCaptureItem = match *target {
                Target::Monitor { handle, .. } => interop.CreateForMonitor(HMONITOR(handle as *mut _))?,
                Target::Window { hwnd } => interop.CreateForWindow(win::hwnd_from_isize(hwnd))?,
            };
            let dxgi: IDXGIDevice = dev.cast()?;
            let device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
            let size = item.Size()?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                3,
                size,
            )?;
            let session = pool.CreateCaptureSession(&item)?;
            let _ = session.SetIsCursorCaptureEnabled(false);
            let _ = session.SetIsBorderRequired(false);
            session.StartCapture()?;
            Ok(Self { pool, _session: session, _item: item, device, size })
        }
    }

    /// Copy the newest frame (if any) into `mirror`.
    fn poll(&mut self, ctx: &ID3D11DeviceContext, mirror: &ID3D11Texture2D, mw: u32, mh: u32) -> Result<bool> {
        let mut newest = None;
        while let Ok(f) = self.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(f) {
                let _ = old.Close();
            }
        }
        let Some(frame) = newest else { return Ok(false) };
        let cs = frame.ContentSize()?;
        // SAFETY: the frame surface is a D3D11 texture on our device; the copy box is
        // clamped to both textures.
        unsafe {
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let tex: ID3D11Texture2D = access.GetInterface()?;
            let w = (cs.Width.max(0) as u32).min(mw).min(self.size.Width.max(0) as u32);
            let h = (cs.Height.max(0) as u32).min(mh).min(self.size.Height.max(0) as u32);
            if w > 0 && h > 0 {
                let b = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
                ctx.CopySubresourceRegion(mirror, 0, 0, 0, 0, &tex, 0, Some(&b));
            }
        }
        let _ = frame.Close();
        if cs.Width != self.size.Width || cs.Height != self.size.Height {
            // Window resized: recreate the pool at the new size.
            if cs.Width > 0 && cs.Height > 0 {
                self.size = cs;
                let _ = self.pool.Recreate(&self.device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 3, cs);
            }
        }
        Ok(true)
    }
}

impl Dup {
    fn new(dev: &ID3D11Device, output: IDXGIOutput1) -> Result<Self> {
        // SAFETY: valid output and device on the same adapter.
        let dup = unsafe { output.DuplicateOutput(dev)? };
        Ok(Self { output, dup: Some(dup) })
    }

    fn poll(&mut self, dev: &ID3D11Device, ctx: &ID3D11DeviceContext, mirror: &ID3D11Texture2D) -> Result<bool> {
        if self.dup.is_none() {
            // SAFETY: valid output and device.
            self.dup = unsafe { self.output.DuplicateOutput(dev).ok() };
        }
        let Some(dup) = self.dup.as_ref() else { return Ok(false) };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: valid out pointers; every acquired frame is released.
        unsafe {
            match dup.AcquireNextFrame(0, &mut info, &mut res) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    self.dup = None;
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            }
            let mut copied = false;
            if info.LastPresentTime != 0 {
                if let Some(tex) = res.as_ref().and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                    ctx.CopyResource(mirror, &tex);
                    copied = true;
                }
            }
            let _ = dup.ReleaseFrame();
            Ok(copied)
        }
    }
}

/// Calls back when the encoder releases a pooled sample.
#[implement(IMFAsyncCallback)]
struct SlotRelease {
    free: Arc<AtomicBool>,
}

impl IMFAsyncCallback_Impl for SlotRelease_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn Invoke(&self, _result: Option<&IMFAsyncResult>) -> windows::core::Result<()> {
        self.free.store(true, Ordering::Release);
        Ok(())
    }
}

struct Slot {
    tex: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
    free: Arc<AtomicBool>,
}

struct Vp {
    vctx: ID3D11VideoContext,
    proc_: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    vdev: ID3D11VideoDevice,
    en: ID3D11VideoProcessorEnumerator,
}

impl Vp {
    fn new(
        dev: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        canvas: &ID3D11Texture2D,
        w: u32,
        h: u32,
        fps: u32,
    ) -> Result<Self> {
        // SAFETY: D3D11 video API calls with valid descriptors and objects.
        unsafe {
            let vdev: ID3D11VideoDevice = dev.cast()?;
            let vctx: ID3D11VideoContext = ctx.cast()?;
            let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: w,
                InputHeight: h,
                OutputFrameRate: rate,
                OutputWidth: w,
                OutputHeight: h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let en = vdev.CreateVideoProcessorEnumerator(&desc)?;
            let proc_ = vdev.CreateVideoProcessor(&en, 0)?;
            // Input: full-range RGB. Output: BT.709, studio range (16-235).
            let cs_in = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 2 };
            let cs_out = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            vctx.VideoProcessorSetStreamColorSpace(&proc_, 0, &cs_in);
            vctx.VideoProcessorSetOutputColorSpace(&proc_, &cs_out);
            vctx.VideoProcessorSetStreamFrameFormat(&proc_, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamAutoProcessingMode(&proc_, 0, false);
            let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            let mut input = None;
            vdev.CreateVideoProcessorInputView(canvas, &en, &ivd, Some(&mut input))?;
            let input = input.ok_or_else(|| anyhow!("no VP input view"))?;
            Ok(Self { vctx, proc_, input, vdev, en })
        }
    }

    fn output_view(&self, tex: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView> {
        let ovd = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut v = None;
        // SAFETY: valid texture/enumerator/descriptor.
        unsafe { self.vdev.CreateVideoProcessorOutputView(tex, &self.en, &ovd, Some(&mut v))? };
        v.ok_or_else(|| anyhow!("no VP output view"))
    }

    fn blt(&self, out: &ID3D11VideoProcessorOutputView) -> Result<()> {
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        };
        let streams = [stream];
        // SAFETY: valid processor, views and stream array.
        let r = unsafe { self.vctx.VideoProcessorBlt(&self.proc_, out, 0, &streams) };
        let [mut s] = streams;
        // SAFETY: release the input view reference taken for the stream struct.
        unsafe { std::mem::ManuallyDrop::drop(&mut s.pInputSurface) };
        r.context("VideoProcessorBlt")
    }
}

enum Output {
    /// NV12 textures handed to the encoder.
    Gpu { slots: Vec<Slot> },
    /// NV12 via the video processor, read back to memory.
    Nv12Readback { tex: ID3D11Texture2D, view: ID3D11VideoProcessorOutputView, staging: ID3D11Texture2D },
    /// BGRA read back and converted on the CPU.
    Cpu { staging: ID3D11Texture2D },
}

struct CursorSprite {
    bmp: ID2D1Bitmap1,
    hotspot: Point,
    w: f32,
    h: f32,
}

struct Pip {
    cam: Webcam,
    settings: WebcamSettings,
    seq: u64,
    bmp: Option<(ID2D1Bitmap1, u32, u32)>,
}

struct Pipeline {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    mt: Option<ID3D11Multithread>,
    d2d_factory: ID2D1Factory1,
    d2d: ID2D1DeviceContext,
    source: Source,
    backend: &'static str,
    /// Virtual-screen origin of the mirror texture's (0, 0).
    mirror_origin: Point,
    mirror_w: u32,
    mirror_h: u32,
    mirror: ID3D11Texture2D,
    mirror_bmp: ID2D1Bitmap1,
    canvas: ID3D11Texture2D,
    vp: Option<Vp>,
    output: Output,
    w: u32,
    h: u32,
    cursors: HashMap<isize, Option<CursorSprite>>,
    brush: ID2D1SolidColorBrush,
    pip: Option<Pip>,
    /// Smoothed zoom (1 = off) and zoom centre.
    zoom: f32,
    zoom_center: (f32, f32),
}

fn bitmap_props(alpha: D2D1_ALPHA_MODE, options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: alpha },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    }
}

fn rectf(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn color(c: [u8; 4], alpha_mul: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c[0] as f32 / 255.0,
        g: c[1] as f32 / 255.0,
        b: c[2] as f32 / 255.0,
        a: c[3] as f32 / 255.0 * alpha_mul,
    }
}

impl Pipeline {
    fn new(p: &VideoParams) -> Result<(Self, Option<IMFDXGIDeviceManager>)> {
        let hmon = match p.target {
            Target::Monitor { handle, .. } => Some(HMONITOR(handle as *mut _)),
            Target::Window { .. } => None,
        };
        let (dev, ctx, output1) = d3d_device_for(hmon)?;
        // The device lock is only needed when Media Foundation shares this device
        // (zero-copy samples). Otherwise this thread is the only user of the immediate
        // context, and taking the lock deadlocked against WGC's free-threaded frame pool,
        // which holds it while it waits for a free buffer.
        let mt: Option<ID3D11Multithread> = if p.want_gpu_input { dev.cast().ok() } else { None };
        if let Some(m) = &mt {
            // SAFETY: plain call; required when MF uses the device from other threads.
            unsafe {
                let _ = m.SetMultithreadProtected(true);
            }
        }
        // Source: WGC first, DXGI duplication as the fallback (monitor targets only).
        let (source, backend) = match Wgc::new(&dev, &p.target) {
            Ok(w) => (Source::Wgc(w), "wgc"),
            Err(e) => {
                log::warn!("recorder: WGC unavailable ({e:#}); trying DXGI duplication");
                let o = output1.clone().ok_or_else(|| anyhow!("no DXGI output for the target"))?;
                (Source::Dup(Dup::new(&dev, o)?), "dxgi")
            }
        };
        let (mirror_origin, mirror_w, mirror_h) = match (&p.target, &source) {
            (_, Source::Wgc(w)) => {
                let origin = match p.target {
                    Target::Monitor { rect, .. } => Point::new(rect.x, rect.y),
                    Target::Window { hwnd } => {
                        let r = win::window_bounds(win::hwnd_from_isize(hwnd));
                        Point::new(r.x, r.y)
                    }
                };
                (origin, w.size.Width.max(1) as u32, w.size.Height.max(1) as u32)
            }
            (Target::Monitor { rect, .. }, Source::Dup(_)) => {
                (Point::new(rect.x, rect.y), rect.w as u32, rect.h as u32)
            }
            (Target::Window { .. }, Source::Dup(_)) => return Err(anyhow!("window capture needs WGC")),
        };
        let mirror = texture(&dev, mirror_w, mirror_h, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_SHADER_RESOURCE, false)?;
        let canvas = texture(
            &dev,
            p.width,
            p.height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0),
            false,
        )?;
        // SAFETY: D2D creation on our DXGI device; bitmaps wrap our textures.
        let (d2d_factory, d2d, mirror_bmp, brush) = unsafe {
            let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let dxgi: IDXGIDevice = dev.cast()?;
            let d2dev = f.CreateDevice(&dxgi)?;
            let dc = d2dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let ms: IDXGISurface = mirror.cast()?;
            let mb = dc.CreateBitmapFromDxgiSurface(
                &ms,
                Some(&bitmap_props(D2D1_ALPHA_MODE_IGNORE, D2D1_BITMAP_OPTIONS_NONE)),
            )?;
            let cs: IDXGISurface = canvas.cast()?;
            let target = dc.CreateBitmapFromDxgiSurface(
                &cs,
                Some(&bitmap_props(
                    D2D1_ALPHA_MODE_PREMULTIPLIED,
                    D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                )),
            )?;
            dc.SetTarget(&target);
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = dc.CreateSolidColorBrush(&color([255, 255, 255, 255], 1.0), None)?;
            (f, dc, mb, brush)
        };
        let vp = match Vp::new(&dev, &ctx, &canvas, p.width, p.height, p.fps) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("recorder: no video processor ({e:#}); converting on the CPU");
                None
            }
        };
        let mut devmgr = None;
        let output = match &vp {
            Some(v) if p.want_gpu_input => {
                let mut slots = Vec::new();
                for _ in 0..8 {
                    let tex = texture(
                        &dev,
                        p.width,
                        p.height,
                        DXGI_FORMAT_NV12,
                        D3D11_BIND_FLAG(D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0),
                        false,
                    )?;
                    let view = v.output_view(&tex)?;
                    slots.push(Slot { tex, view, free: Arc::new(AtomicBool::new(true)) });
                }
                let mut token = 0u32;
                let mut m: Option<IMFDXGIDeviceManager> = None;
                // SAFETY: valid out pointers; the manager gets our device.
                unsafe {
                    MFCreateDXGIDeviceManager(&mut token, &mut m)?;
                    if let Some(m) = &m {
                        m.ResetDevice(&dev, token)?;
                    }
                }
                devmgr = m;
                Output::Gpu { slots }
            }
            Some(v) => {
                let tex = texture(&dev, p.width, p.height, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET, false)?;
                let view = v.output_view(&tex)?;
                let staging = texture(&dev, p.width, p.height, DXGI_FORMAT_NV12, D3D11_BIND_FLAG(0), true)?;
                Output::Nv12Readback { tex, view, staging }
            }
            None => Output::Cpu {
                staging: texture(&dev, p.width, p.height, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_FLAG(0), true)?,
            },
        };
        let pip = p.webcam.as_ref().filter(|w| w.enabled).map(|w| Pip {
            cam: Webcam::start(&w.device_id),
            settings: w.clone(),
            seq: 0,
            bmp: None,
        });
        Ok((
            Self {
                dev,
                ctx,
                mt,
                d2d_factory,
                d2d,
                source,
                backend,
                mirror_origin,
                mirror_w,
                mirror_h,
                mirror,
                mirror_bmp,
                canvas,
                vp,
                output,
                w: p.width,
                h: p.height,
                cursors: HashMap::new(),
                brush,
                pip,
                zoom: 1.0,
                zoom_center: (0.0, 0.0),
            },
            devmgr,
        ))
    }

    /// Switch from GPU samples to read-back (the encoder refused DXGI input).
    fn use_readback(&mut self) -> Result<()> {
        if let (Output::Gpu { .. }, Some(v)) = (&self.output, &self.vp) {
            let tex = texture(&self.dev, self.w, self.h, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET, false)?;
            let view = v.output_view(&tex)?;
            let staging = texture(&self.dev, self.w, self.h, DXGI_FORMAT_NV12, D3D11_BIND_FLAG(0), true)?;
            self.output = Output::Nv12Readback { tex, view, staging };
        }
        Ok(())
    }

    fn poll_source(&mut self) -> Result<bool> {
        match &mut self.source {
            Source::Wgc(w) => w.poll(&self.ctx, &self.mirror, self.mirror_w, self.mirror_h),
            Source::Dup(d) => d.poll(&self.dev, &self.ctx, &self.mirror),
        }
    }

    fn cursor_sprite(&mut self, h: isize) -> Option<&CursorSprite> {
        if !self.cursors.contains_key(&h) {
            let sprite = cursor::shape(h).and_then(|s| {
                // SAFETY: the sprite data outlives the call; size and pitch match.
                unsafe {
                    self.d2d
                        .CreateBitmap(
                            D2D_SIZE_U { width: s.image.width, height: s.image.height },
                            Some(s.image.data.as_ptr() as *const _),
                            s.image.width * 4,
                            &bitmap_props(D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BITMAP_OPTIONS_NONE),
                        )
                        .ok()
                }
                .map(|bmp| CursorSprite {
                    bmp,
                    hotspot: s.hotspot,
                    w: s.image.width as f32,
                    h: s.image.height as f32,
                })
            });
            if self.cursors.len() > 64 {
                self.cursors.clear();
            }
            self.cursors.insert(h, sprite);
        }
        self.cursors.get(&h).and_then(|s| s.as_ref())
    }

    /// Draw the frame into the canvas. `region` is in virtual-screen pixels.
    fn compose(&mut self, region: Rect, p: &VideoParams, live: &Live, now: i64) -> Result<()> {
        let (w, h) = (self.w as f32, self.h as f32);
        // Region in mirror coordinates.
        let rx = (region.x - self.mirror_origin.x) as f32;
        let ry = (region.y - self.mirror_origin.y) as f32;
        let cur = cursor::current();
        // Zoom animation (about 250 ms each way) around the smoothed cursor position.
        let target_zoom = if live.zoom_on.load(Ordering::Relaxed) { p.zoom_factor.clamp(1.0, 8.0) } else { 1.0 };
        let dt = 1.0 / p.fps.max(1) as f32;
        let k = (dt / 0.25 * 4.0).min(1.0);
        self.zoom += (target_zoom - self.zoom) * k;
        if (self.zoom - target_zoom).abs() < 0.002 {
            self.zoom = target_zoom;
        }
        let c = cur.map(|(_, pt)| ((pt.x - self.mirror_origin.x) as f32, (pt.y - self.mirror_origin.y) as f32));
        if let Some(c) = c {
            if self.zoom <= 1.0 {
                self.zoom_center = c;
            } else {
                self.zoom_center.0 += (c.0 - self.zoom_center.0) * 0.2;
                self.zoom_center.1 += (c.1 - self.zoom_center.1) * 0.2;
            }
        }
        let z = self.zoom.max(1.0);
        let (vw, vh) = (w / z, h / z);
        let vx = (self.zoom_center.0 - vw / 2.0).clamp(rx, rx + w - vw);
        let vy = (self.zoom_center.1 - vh / 2.0).clamp(ry, ry + h - vh);
        let to_out = |x: f32, y: f32| ((x - vx) * z, (y - vy) * z);

        let d2d = self.d2d.clone();
        // SAFETY: D2D drawing on our device context between BeginDraw/EndDraw.
        unsafe {
            d2d.BeginDraw();
            d2d.Clear(Some(&color([0, 0, 0, 255], 1.0)));
            d2d.DrawBitmap(
                &self.mirror_bmp,
                Some(&rectf(0.0, 0.0, w, h)),
                1.0,
                if z > 1.0 {
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
                } else {
                    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                },
                Some(&rectf(vx, vy, vx + vw, vy + vh)),
                None,
            );
            // Click rings (rendered into the video only).
            if let Some((col, size)) = p.highlight {
                let life = clock::HNS_PER_SEC * 6 / 10;
                let clicks: Vec<Click> = live
                    .clicks
                    .lock()
                    .map(|mut q| {
                        q.retain(|c| now - c.at < life);
                        q.iter().copied().collect()
                    })
                    .unwrap_or_default();
                for ck in clicks {
                    let e = ((now - ck.at) as f32 / life as f32).clamp(0.0, 1.0);
                    let (x, y) =
                        to_out((ck.pos.x - self.mirror_origin.x) as f32, (ck.pos.y - self.mirror_origin.y) as f32);
                    let r = size as f32 * 0.5 * (0.35 + 0.65 * e) * z.sqrt();
                    let el = D2D1_ELLIPSE { point: D2D_POINT_2F { x, y }, radiusX: r, radiusY: r };
                    self.brush.SetColor(&color(col, (1.0 - e) * 0.35));
                    d2d.FillEllipse(&el, &self.brush);
                    self.brush.SetColor(&color(col, 1.0 - e));
                    d2d.DrawEllipse(&el, &self.brush, if ck.right { 2.0 } else { 3.5 }, None);
                }
            }
        }
        if p.show_cursor {
            if let Some((hc, pt)) = cur {
                let origin = self.mirror_origin;
                if let Some(s) = self.cursor_sprite(hc) {
                    let (x, y) = to_out((pt.x - origin.x - s.hotspot.x) as f32, (pt.y - origin.y - s.hotspot.y) as f32);
                    // SAFETY: drawing inside the BeginDraw/EndDraw pair.
                    unsafe {
                        d2d.DrawBitmap(
                            &s.bmp,
                            Some(&rectf(x, y, x + s.w * z, y + s.h * z)),
                            1.0,
                            D2D1_INTERPOLATION_MODE_LINEAR,
                            None,
                            None,
                        );
                    }
                }
            }
        }
        self.draw_pip(&d2d);
        // SAFETY: matches the BeginDraw above.
        unsafe { d2d.EndDraw(None, None).context("D2D EndDraw") }
    }

    fn draw_pip(&mut self, d2d: &ID2D1DeviceContext) {
        let Some(pip) = self.pip.as_mut() else { return };
        let (seq, frame) = match pip.cam.latest.lock() {
            Ok(l) => (l.seq, l.frame.clone()),
            Err(_) => return,
        };
        if seq != pip.seq {
            if let Some(img) = frame {
                pip.seq = seq;
                let same = matches!(&pip.bmp, Some((_, w, h)) if *w == img.width && *h == img.height);
                // SAFETY: the image buffer outlives the calls; size and pitch match.
                unsafe {
                    if same {
                        if let Some((b, _, _)) = &pip.bmp {
                            let _ = b.CopyFromMemory(None, img.data.as_ptr() as *const _, img.width * 4);
                        }
                    } else {
                        pip.bmp = d2d
                            .CreateBitmap(
                                D2D_SIZE_U { width: img.width, height: img.height },
                                Some(img.data.as_ptr() as *const _),
                                img.width * 4,
                                &bitmap_props(D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BITMAP_OPTIONS_NONE),
                            )
                            .ok()
                            .map(|b| (b, img.width, img.height));
                    }
                }
            }
        }
        let Some((bmp, cw, ch)) = pip.bmp.as_ref() else { return };
        let s = &pip.settings;
        let (w, h) = (self.w as f32, self.h as f32);
        let pw = (w * s.size_percent.clamp(5, 60) as f32 / 100.0).round();
        let ph = match s.shape {
            WebcamShape::Circle => pw,
            _ => (pw * 3.0 / 4.0).round(),
        };
        let m = s.margin as f32;
        let (x, y) = match s.corner {
            Corner::TopLeft => (m, m),
            Corner::TopRight => (w - pw - m, m),
            Corner::BottomLeft => (m, h - ph - m),
            Corner::BottomRight => (w - pw - m, h - ph - m),
        };
        // Scale the camera image to cover the box (centre crop), mirrored if requested.
        let (cw, ch) = (*cw as f32, *ch as f32);
        let sc = (pw / cw).max(ph / ch);
        let ox = x + (pw - cw * sc) / 2.0;
        let oy = y + (ph - ch * sc) / 2.0;
        let xf = if s.mirror {
            Matrix3x2 { M11: -sc, M12: 0.0, M21: 0.0, M22: sc, M31: ox + cw * sc, M32: oy }
        } else {
            Matrix3x2 { M11: sc, M12: 0.0, M21: 0.0, M22: sc, M31: ox, M32: oy }
        };
        // SAFETY: D2D calls inside the open BeginDraw; geometry/brush are local.
        unsafe {
            let Ok(brush) = d2d.CreateBitmapBrush(bmp, None, None) else { return };
            brush.SetTransform(&xf);
            let r = rectf(x, y, x + pw, y + ph);
            let geom: Option<ID2D1Geometry> = match s.shape {
                WebcamShape::Circle => self
                    .d2d_factory
                    .CreateEllipseGeometry(&D2D1_ELLIPSE {
                        point: D2D_POINT_2F { x: x + pw / 2.0, y: y + ph / 2.0 },
                        radiusX: pw / 2.0,
                        radiusY: ph / 2.0,
                    })
                    .ok()
                    .and_then(|g| g.cast().ok()),
                WebcamShape::RoundedRect => self
                    .d2d_factory
                    .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                        rect: r,
                        radiusX: pw * 0.08,
                        radiusY: pw * 0.08,
                    })
                    .ok()
                    .and_then(|g| g.cast().ok()),
                WebcamShape::Rect => self.d2d_factory.CreateRectangleGeometry(&r).ok().and_then(|g| g.cast().ok()),
            };
            if let Some(g) = geom {
                d2d.FillGeometry(&g, &brush, None);
                self.brush.SetColor(&color([255, 255, 255, 230], 1.0));
                d2d.DrawGeometry(&g, &self.brush, 3.0, None);
            }
        }
    }

    /// Convert the canvas and wrap it in a sample.
    fn emit(&mut self, time: i64, dur: i64) -> Result<Option<IMFSample>> {
        let (w, h) = (self.w, self.h);
        match &self.output {
            Output::Gpu { slots } => {
                let Some(slot) = slots.iter().find(|s| s.free.load(Ordering::Acquire)) else {
                    return Ok(None); // encoder is behind: drop this frame
                };
                let vp = self.vp.as_ref().ok_or_else(|| anyhow!("no video processor"))?;
                vp.blt(&slot.view)?;
                slot.free.store(false, Ordering::Release);
                // SAFETY: MF wraps our texture; the tracked sample reports when it is released.
                let r: Result<IMFSample> = unsafe {
                    (|| {
                        let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &slot.tex, 0, false)?;
                        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                            buf.SetCurrentLength(b2.GetContiguousLength()?)?;
                        }
                        let tracked = MFCreateTrackedSample()?;
                        let s: IMFSample = tracked.cast()?;
                        s.AddBuffer(&buf)?;
                        s.SetSampleTime(time)?;
                        s.SetSampleDuration(dur)?;
                        let cb: IMFAsyncCallback = SlotRelease { free: slot.free.clone() }.into();
                        tracked.SetAllocator(&cb, None)?;
                        Ok(s)
                    })()
                };
                if r.is_err() {
                    slot.free.store(true, Ordering::Release);
                }
                r.map(Some)
            }
            Output::Nv12Readback { tex, view, staging } => {
                let vp = self.vp.as_ref().ok_or_else(|| anyhow!("no video processor"))?;
                vp.blt(view)?;
                let mut data = vec![0u8; (w * h * 3 / 2) as usize];
                // SAFETY: staging copy then a read map; rows are copied within the mapped pitch.
                unsafe {
                    self.ctx.CopyResource(staging, tex);
                    let m = self.map_read(staging)?;
                    let pitch = m.RowPitch as usize;
                    let base = m.pData as *const u8;
                    for y in 0..h as usize {
                        let src = std::slice::from_raw_parts(base.add(y * pitch), w as usize);
                        data[y * w as usize..(y + 1) * w as usize].copy_from_slice(src);
                    }
                    let uv = base.add(pitch * h as usize);
                    let yoff = (w * h) as usize;
                    for y in 0..(h / 2) as usize {
                        let src = std::slice::from_raw_parts(uv.add(y * pitch), w as usize);
                        data[yoff + y * w as usize..yoff + (y + 1) * w as usize].copy_from_slice(src);
                    }
                    self.ctx.Unmap(staging, 0);
                }
                Ok(Some(mf::memory_sample(&data, time, dur)?))
            }
            Output::Cpu { staging } => {
                let mut data = vec![0u8; (w * h * 3 / 2) as usize];
                // SAFETY: staging copy then a read map of a BGRA texture of size w x h.
                unsafe {
                    self.ctx.CopyResource(staging, &self.canvas);
                    let m = self.map_read(staging)?;
                    let src = std::slice::from_raw_parts(m.pData as *const u8, m.RowPitch as usize * h as usize);
                    bgra_to_nv12(src, m.RowPitch as usize, w as usize, h as usize, &mut data);
                    self.ctx.Unmap(staging, 0);
                }
                Ok(Some(mf::memory_sample(&data, time, dur)?))
            }
        }
    }

    /// Map a staging texture for reading without blocking while we hold the device lock:
    /// waiting inside the lock can deadlock with the WGC frame pool, which needs it to
    /// finish the frame our copy depends on.
    fn map_read(&self, res: &ID3D11Texture2D) -> Result<D3D11_MAPPED_SUBRESOURCE> {
        // SAFETY: valid context; the copy into `res` was queued before.
        unsafe { self.ctx.Flush() };
        let start = std::time::Instant::now();
        loop {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: valid staging texture; DO_NOT_WAIT returns WAS_STILL_DRAWING instead of blocking.
            match unsafe { self.ctx.Map(res, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut m)) } {
                Ok(()) => return Ok(m),
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    if start.elapsed() > Duration::from_secs(2) {
                        return Err(anyhow!("GPU read-back timed out"));
                    }
                    self.unlock();
                    std::thread::sleep(Duration::from_micros(500));
                    self.lock();
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn lock(&self) {
        if let Some(m) = &self.mt {
            // SAFETY: balanced with `unlock`; serialises our context use with MF's.
            unsafe { m.Enter() };
        }
    }

    fn unlock(&self) {
        if let Some(m) = &self.mt {
            // SAFETY: balances `lock`.
            unsafe { m.Leave() };
        }
    }
}

/// The region to record this frame (virtual-screen pixels).
fn current_region(p: &VideoParams, live: &Live) -> Rect {
    let mut r = live.region.lock().map(|r| *r).unwrap_or_default();
    match p.target {
        Target::Window { hwnd } => {
            let b = win::window_bounds(win::hwnd_from_isize(hwnd));
            r = Rect::new(b.x, b.y, r.w, r.h);
        }
        Target::Monitor { rect, .. } => {
            if p.follow_mouse {
                let c = win::cursor_pos();
                let cx = r.x as f32 + r.w as f32 / 2.0;
                let cy = r.y as f32 + r.h as f32 / 2.0;
                let nx = cx + (c.x as f32 - cx) * 0.25;
                let ny = cy + (c.y as f32 - cy) * 0.25;
                let moved =
                    Rect::new((nx - r.w as f32 / 2.0).round() as i32, (ny - r.h as f32 / 2.0).round() as i32, r.w, r.h)
                        .clamp_inside(&rect);
                if moved != r {
                    r = moved;
                    move_frame(live, r);
                }
            }
            r = r.clamp_inside(&rect);
        }
    }
    if let Ok(mut g) = live.region.lock() {
        *g = r;
    }
    r
}

/// Keep the region frame window around the region (it lives on the control-bar thread).
fn move_frame(live: &Live, r: Rect) {
    let h = live.frame_hwnd.load(Ordering::Acquire);
    if h != 0 {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        let packed = ((r.x as i64 as u64 & 0xFFFF_FFFF) << 32) | (r.y as i64 as u64 & 0xFFFF_FFFF);
        // SAFETY: posting to a window owned by the recorder UI thread.
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), super::bar::WM_REGION_MOVE, WPARAM(packed as usize), LPARAM(0));
        }
    }
}

/// Video thread body.
pub fn run(
    p: VideoParams,
    live: Arc<Live>,
    shared: Arc<Shared>,
    control: Arc<Control>,
    ready: SyncSender<Result<VideoInit>>,
    go: Receiver<bool>,
    tx: SyncSender<EncMsg>,
) {
    let _com = mf::ComGuard::mta();
    let (mut pl, devmgr) = match Pipeline::new(&p) {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let gpu_input = matches!(pl.output, Output::Gpu { .. });
    log::info!(
        "recorder: video {}x{} @ {} fps via {} ({})",
        p.width,
        p.height,
        p.fps,
        pl.backend,
        match pl.output {
            Output::Gpu { .. } => "GPU NV12 samples",
            Output::Nv12Readback { .. } => "GPU NV12, read back",
            Output::Cpu { .. } => "CPU conversion",
        }
    );
    let _ = ready.send(Ok(VideoInit { gpu_input, devmgr: devmgr.map(SendCom), backend: pl.backend }));
    // Wait for the session to start the clock (false = the encoder needs read-back).
    match go.recv() {
        Ok(true) => {}
        Ok(false) => {
            if let Err(e) = pl.use_readback() {
                log::error!("recorder: read-back setup failed: {e:#}");
                return;
            }
        }
        Err(_) => return,
    }
    let fps = p.fps.max(1);
    let mut k: u64 = 0;
    // End of the last frame handed to the encoder: the next frame starts there, so the
    // stream stays gapless even when slots are dropped (the previous frame is not
    // extended; the next one covers the gap).
    let mut next_t: i64 = 0;
    let mut fresh = false;
    let mut errors = 0u32;
    loop {
        let now = clock::qpc_hns();
        let (media, paused) = match shared.clock.lock() {
            Ok(c) => (c.media_now(now), c.is_paused()),
            Err(_) => break,
        };
        let stop_at = control.stop_media.load(Ordering::Acquire);
        if frame_time(k, fps) >= stop_at || (control.stop.load(Ordering::Acquire) && stop_at == i64::MAX) {
            break;
        }
        pl.lock();
        let polled = pl.poll_source();
        pl.unlock();
        match polled {
            Ok(true) => fresh = true,
            Ok(false) => {}
            Err(e) => log::debug!("recorder: source poll: {e}"),
        }
        if paused {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let due = frame_time(k, fps);
        if media < due {
            let wait = (due - media).min(clock::HNS_PER_SEC / 200);
            std::thread::sleep(Duration::from_nanos(wait as u64 * 100));
            continue;
        }
        // Late by a whole frame or more: skip to the current slot (counted as dropped).
        let current = frame_at_or_after(media + 1, fps).saturating_sub(1);
        if current > k {
            live.dropped.fetch_add(current - k, Ordering::Relaxed);
            k = current;
        }
        if k % 30 == 0 {
            log::debug!("recorder: video frame {k} (media {} ms)", media / 10_000);
        }
        let region = current_region(&p, &live);
        let t = next_t;
        let dur = frame_time(k + 1, fps) - t;
        pl.lock();
        let r = pl.compose(region, &p, &live, now).and_then(|_| pl.emit(t, dur));
        pl.unlock();
        match r {
            Ok(Some(sample)) => {
                if !fresh {
                    live.duplicated.fetch_add(1, Ordering::Relaxed);
                }
                fresh = false;
                match tx.try_send(EncMsg::Video(SendCom(sample))) {
                    Ok(()) => {
                        live.frames.fetch_add(1, Ordering::Relaxed);
                        next_t = t + dur;
                    }
                    Err(TrySendError::Full(_)) => {
                        live.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
                errors = 0;
            }
            Ok(None) => {
                live.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                errors += 1;
                live.dropped.fetch_add(1, Ordering::Relaxed);
                if errors == 1 || errors % 100 == 0 {
                    log::warn!("recorder: frame {k}: {e:#}");
                }
                if errors > 300 {
                    log::error!("recorder: too many video errors; stopping video");
                    break;
                }
            }
        }
        k += 1;
    }
    if let Some(pip) = pl.pip.as_mut() {
        pip.cam.stop();
    }
    log::info!(
        "recorder: video done: {} frames, {} dropped, {} duplicates",
        live.frames.load(Ordering::Relaxed),
        live.dropped.load(Ordering::Relaxed),
        live.duplicated.load(Ordering::Relaxed)
    );
}

// ------------------------------------------------------------------ CPU colour conversion

#[inline]
fn y_of(r: i32, g: i32, b: i32) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16).clamp(0, 255) as u8
}

/// BGRA (any pitch) to NV12, BT.709 limited range. `out` is `w * h * 3 / 2` bytes;
/// `w` and `h` must be even. Rows are split across threads.
pub fn bgra_to_nv12(src: &[u8], pitch: usize, w: usize, h: usize, out: &mut [u8]) {
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let pairs = h / 2;
    let per = pairs.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let mut y_rest = y_plane;
        let mut uv_rest = uv_plane;
        let mut p0 = 0;
        while p0 < pairs {
            let n = per.min(pairs - p0);
            let (yc, yr) = y_rest.split_at_mut(n * 2 * w);
            let (uc, ur) = uv_rest.split_at_mut(n * w);
            y_rest = yr;
            uv_rest = ur;
            let start = p0;
            s.spawn(move || {
                for i in 0..n {
                    let py = (start + i) * 2;
                    let r0 = &src[py * pitch..py * pitch + w * 4];
                    let r1 = &src[(py + 1) * pitch..(py + 1) * pitch + w * 4];
                    for x in (0..w).step_by(2) {
                        let mut sr = 0;
                        let mut sg = 0;
                        let mut sb = 0;
                        for (row, dy) in [(r0, 0), (r1, 1)] {
                            for dx in 0..2 {
                                let o = (x + dx) * 4;
                                let (b, g, r) = (row[o] as i32, row[o + 1] as i32, row[o + 2] as i32);
                                yc[(i * 2 + dy) * w + x + dx] = y_of(r, g, b);
                                sr += r;
                                sg += g;
                                sb += b;
                            }
                        }
                        let (r, g, b) = ((sr + 2) / 4, (sg + 2) / 4, (sb + 2) / 4);
                        uc[i * w + x] = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                        uc[i * w + x + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                    }
                }
            });
            p0 += n;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_conversion_bt709_limited() {
        let (w, h) = (4usize, 4usize);
        let pitch = w * 4 + 8; // padded rows
        let mut src = vec![0u8; pitch * h];
        for y in 0..h {
            for x in 0..w {
                let o = y * pitch + x * 4;
                let px: [u8; 4] = if x < 2 { [255, 255, 255, 255] } else { [0, 0, 255, 255] }; // white | red
                src[o..o + 4].copy_from_slice(&px);
            }
        }
        let mut out = vec![0u8; w * h * 3 / 2];
        bgra_to_nv12(&src, pitch, w, h, &mut out);
        // White -> Y 235, U/V 128; red -> Y 63, U 102, V 240 (BT.709 studio range).
        assert!((out[0] as i32 - 235).abs() <= 1, "{}", out[0]);
        assert!((out[2] as i32 - 63).abs() <= 1, "{}", out[2]);
        let uv = &out[w * h..];
        assert!((uv[0] as i32 - 128).abs() <= 1 && (uv[1] as i32 - 128).abs() <= 1);
        assert!((uv[2] as i32 - 102).abs() <= 2, "U {}", uv[2]);
        assert!((uv[3] as i32 - 240).abs() <= 2, "V {}", uv[3]);
        // Black -> 16.
        let black = vec![0u8; pitch * h];
        bgra_to_nv12(&black, pitch, w, h, &mut out);
        assert_eq!(out[0], 16);
    }
}
