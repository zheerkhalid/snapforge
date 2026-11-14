//! Windows.Graphics.Capture backend for monitors and windows.
//!
//! Runs on the capture worker thread (initialized for the multithreaded WinRT apartment).
//! Each grab creates a free-threaded frame pool and a session, waits for one frame,
//! reads the requested region and closes everything again; only the D3D device is reused.

use super::d3d::{self, win_err};
use super::CaptureError;
use crate::common::{Image, Rect};
use std::sync::mpsc;
use std::sync::{Once, OnceLock};
use std::time::Duration;
use windows::core::{IInspectable, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

const FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

pub struct WgcDevice {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    winrt: IDirect3DDevice,
}

impl WgcDevice {
    pub fn new(device: ID3D11Device, ctx: ID3D11DeviceContext) -> Result<Self, CaptureError> {
        let dxgi: IDXGIDevice = device.cast().map_err(win_err("IDXGIDevice"))?;
        // SAFETY: plain interop call with a live DXGI device.
        let insp: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .map_err(win_err("CreateDirect3D11DeviceFromDXGIDevice"))?;
        let winrt: IDirect3DDevice = insp.cast().map_err(win_err("IDirect3DDevice"))?;
        Ok(Self { device, ctx, winrt })
    }
}

/// Whether WGC is available (queried once; the call itself costs ~25 ms).
pub fn is_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| GraphicsCaptureSession::IsSupported().unwrap_or(false))
}

/// Creates capture items. Holding on to the activation factory (and to monitor items)
/// avoids ~60 ms of WinRT activation per grab.
pub struct ItemFactory {
    interop: IGraphicsCaptureItemInterop,
    monitors: Vec<(isize, GraphicsCaptureItem)>,
}

impl ItemFactory {
    pub fn new() -> Result<Self, CaptureError> {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(win_err("capture item factory"))?;
        Ok(Self { interop, monitors: Vec::new() })
    }

    pub fn monitor(&mut self, hmonitor: isize) -> Result<GraphicsCaptureItem, CaptureError> {
        if let Some((_, item)) = self.monitors.iter().find(|(h, _)| *h == hmonitor) {
            return Ok(item.clone());
        }
        // SAFETY: the handle comes from DXGI/EnumDisplayMonitors; an invalid one only errors.
        let item: GraphicsCaptureItem = unsafe { self.interop.CreateForMonitor(HMONITOR(hmonitor as *mut _)) }
            .map_err(win_err("CreateForMonitor"))?;
        self.monitors.push((hmonitor, item.clone()));
        Ok(item)
    }

    pub fn window(&self, hwnd: isize) -> Result<GraphicsCaptureItem, CaptureError> {
        // SAFETY: an invalid window handle only produces an error.
        unsafe { self.interop.CreateForWindow(HWND(hwnd as *mut _)) }.map_err(win_err("CreateForWindow"))
    }
}

/// Ask once for borderless capture (needed on some builds before `IsBorderRequired = false`).
fn request_borderless() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
        match GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless).and_then(|op| op.get()) {
            Ok(status) => log::debug!("WGC borderless access: {status:?}"),
            Err(e) => log::debug!("WGC borderless access request unavailable: {e}"),
        }
    });
}

/// Grab one frame of `item`. `region` (item coordinates) limits what is read back;
/// `None` = everything. `hdr_white`: capture FP16 and tone-map with this SDR white.
/// Returns the image and the item's full content size.
pub fn grab(
    dev: &WgcDevice,
    item: &GraphicsCaptureItem,
    region: Option<Rect>,
    include_cursor: bool,
    hdr_white: Option<f32>,
) -> Result<(Image, (i32, i32)), CaptureError> {
    request_borderless();
    let size = item.Size().map_err(win_err("item size"))?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(CaptureError::Empty);
    }
    let format = if hdr_white.is_some() {
        DirectXPixelFormat::R16G16B16A16Float
    } else {
        DirectXPixelFormat::B8G8R8A8UIntNormalized
    };
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&dev.winrt, format, 1, size)
        .map_err(win_err("CreateFreeThreaded"))?;
    let result = (|| {
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
            let _ = tx.try_send(());
            Ok(())
        }))
        .map_err(win_err("FrameArrived"))?;
        let session = pool.CreateCaptureSession(item).map_err(win_err("CreateCaptureSession"))?;
        // Both are optional features (Windows 10 2004 / Windows 11); ignore if missing.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(include_cursor);
        let r = (|| {
            session.StartCapture().map_err(win_err("StartCapture"))?;
            let frame = loop {
                if let Ok(f) = pool.TryGetNextFrame() {
                    break f;
                }
                match rx.recv_timeout(FRAME_TIMEOUT) {
                    Ok(()) => continue,
                    Err(_) => return Err(CaptureError::Failed("WGC: no frame arrived".into())),
                }
            };
            let r = (|| {
                let content = frame.ContentSize().map_err(win_err("ContentSize"))?;
                let surface = frame.Surface().map_err(win_err("Surface"))?;
                let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(win_err("DxgiInterfaceAccess"))?;
                // SAFETY: plain COM call returning the frame's texture.
                let tex: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(win_err("frame texture"))?;
                let full = Rect::new(0, 0, content.Width, content.Height);
                let want = region.map(|r| r.intersect(&full)).unwrap_or(full);
                let img = d3d::read_region(&dev.device, &dev.ctx, &tex, want, hdr_white)?;
                Ok((img, (content.Width, content.Height)))
            })();
            let _ = frame.Close();
            r
        })();
        let _ = session.Close();
        r
    })();
    let _ = pool.Close();
    result
}
