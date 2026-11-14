//! Shared Direct3D 11 / DXGI helpers: devices, output enumeration, texture readback.

use super::{hdr, CaptureError};
use crate::common::{win, Image, Rect};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput6};

pub fn win_err(ctx: &'static str) -> impl Fn(windows::core::Error) -> CaptureError {
    move |e| CaptureError::Failed(format!("{ctx}: {e}"))
}

/// What we need to know about a monitor, as plain data (safe to cache and share).
#[derive(Debug, Clone)]
pub struct OutputMeta {
    pub rect: Rect,
    pub hmonitor: isize,
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub adapter_index: u32,
    pub output_index: u32,
    /// HDR (ST.2084 / BT.2020) is enabled for this monitor.
    pub hdr: bool,
    pub rotated: bool,
}

static OUTPUTS: Mutex<Option<(Instant, Vec<OutputMeta>)>> = Mutex::new(None);

/// Monitor list from DXGI, cached for a short time (enumeration costs a few ms).
pub fn outputs() -> Vec<OutputMeta> {
    const TTL: Duration = Duration::from_secs(3);
    if let Ok(g) = OUTPUTS.lock() {
        if let Some((t, v)) = g.as_ref() {
            if t.elapsed() < TTL {
                return v.clone();
            }
        }
    }
    let v = match enumerate_outputs() {
        Ok(v) => v,
        Err(e) => {
            log::warn!("DXGI output enumeration failed: {e}");
            Vec::new()
        }
    };
    if v.is_empty() {
        // Transient failure (display change in progress): don't cache it.
        return v;
    }
    if let Ok(mut g) = OUTPUTS.lock() {
        *g = Some((Instant::now(), v.clone()));
    }
    v
}

/// Forget the cached monitor list (after a display change or a DXGI access loss).
pub fn invalidate_outputs() {
    if let Ok(mut g) = OUTPUTS.lock() {
        *g = None;
    }
}

pub fn factory() -> Result<IDXGIFactory1, CaptureError> {
    // SAFETY: plain factory creation.
    unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.map_err(win_err("CreateDXGIFactory1"))
}

pub fn adapter(factory: &IDXGIFactory1, index: u32) -> Result<IDXGIAdapter1, CaptureError> {
    // SAFETY: plain COM call.
    unsafe { factory.EnumAdapters1(index) }.map_err(win_err("EnumAdapters1"))
}

fn enumerate_outputs() -> Result<Vec<OutputMeta>, CaptureError> {
    let factory = factory()?;
    let mut out = Vec::new();
    // SAFETY: plain COM enumeration; loops end at DXGI_ERROR_NOT_FOUND.
    unsafe {
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                if let Ok(desc) = output.GetDesc() {
                    if desc.AttachedToDesktop.as_bool() {
                        let hdr = output
                            .cast::<IDXGIOutput6>()
                            .and_then(|o6| o6.GetDesc1())
                            .map(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
                            .unwrap_or(false);
                        out.push(OutputMeta {
                            rect: win::rect_from_win(&desc.DesktopCoordinates),
                            hmonitor: desc.Monitor.0 as isize,
                            device: win::from_wide(&desc.DeviceName),
                            adapter_index: ai,
                            output_index: oi,
                            hdr,
                            rotated: desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
                                && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED,
                        });
                    }
                }
                oi += 1;
            }
            ai += 1;
        }
    }
    Ok(out)
}

/// Create a D3D11 device (BGRA support) on `adapter`, or the default hardware adapter.
pub fn create_device(adapter: Option<&IDXGIAdapter1>) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device = None;
    let mut ctx = None;
    let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
    let adapter: Option<IDXGIAdapter> = adapter.map(|a| a.cast()).transpose().map_err(win_err("IDXGIAdapter"))?;
    // SAFETY: out pointers are valid; on success both are set.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(win_err("D3D11CreateDevice"))?;
    }
    match (device, ctx) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(CaptureError::Failed("D3D11CreateDevice returned no device".into())),
    }
}

/// Create a CPU-readable staging texture of the given size and format.
pub fn staging_texture(
    device: &ID3D11Device,
    w: u32,
    h: u32,
    format_hdr: bool,
) -> Result<ID3D11Texture2D, CaptureError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: if format_hdr { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM },
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor and out pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(win_err("CreateTexture2D(staging)"))?;
    tex.ok_or_else(|| CaptureError::Failed("CreateTexture2D returned null".into()))
}

pub fn texture_desc(tex: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: valid out pointer.
    unsafe { tex.GetDesc(&mut d) };
    d
}

/// A mapped staging texture; unmapped on drop.
pub struct Mapped<'a> {
    ctx: &'a ID3D11DeviceContext,
    tex: &'a ID3D11Texture2D,
    pub data: *const u8,
    pub pitch: usize,
    pub width: u32,
    pub height: u32,
}

impl<'a> Mapped<'a> {
    pub fn new(ctx: &'a ID3D11DeviceContext, tex: &'a ID3D11Texture2D) -> Result<Self, CaptureError> {
        let d = texture_desc(tex);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: tex is a CPU-readable staging texture created on ctx's device.
        unsafe { ctx.Map(tex, 0, D3D11_MAP_READ, 0, Some(&mut m)) }.map_err(win_err("Map"))?;
        if m.pData.is_null() {
            return Err(CaptureError::Failed("Map returned null".into()));
        }
        Ok(Self { ctx, tex, data: m.pData as *const u8, pitch: m.RowPitch as usize, width: d.Width, height: d.Height })
    }

    /// Row `y`, `bytes` long.
    pub fn row(&self, y: u32, bytes: usize) -> &[u8] {
        debug_assert!(y < self.height && bytes <= self.pitch);
        // SAFETY: the mapping covers `height` rows of `pitch` bytes and lives until drop.
        unsafe { std::slice::from_raw_parts(self.data.add(y as usize * self.pitch), bytes) }
    }

    /// Row `y` as half-floats (RGBA16F), `width * 4` values.
    pub fn row_u16(&self, y: u32) -> &[u16] {
        let n = self.width as usize * 4;
        // SAFETY: as in `row`; RowPitch of an FP16 texture is 2-byte aligned (in fact 16).
        unsafe { std::slice::from_raw_parts(self.data.add(y as usize * self.pitch) as *const u16, n) }
    }

    /// Copy the region `r` (texture coordinates, BGRA8) into `dst` at (`dx`, `dy`), opaque.
    pub fn copy_bgra_into(&self, r: Rect, dst: &mut Image, dx: i32, dy: i32) {
        let r = r.intersect(&Rect::new(0, 0, self.width as i32, self.height as i32));
        let dr = Rect::new(dx, dy, r.w, r.h).intersect(&dst.bounds());
        if dr.is_empty() {
            return;
        }
        let (sx, sy) = (r.x + (dr.x - dx), r.y + (dr.y - dy));
        let bytes = dr.w as usize * 4;
        for row in 0..dr.h {
            let src = self.row((sy + row) as u32, (sx as usize + dr.w as usize) * 4);
            let src = &src[sx as usize * 4..];
            let d = ((dr.y + row) as usize * dst.width as usize + dr.x as usize) * 4;
            let out = &mut dst.data[d..d + bytes];
            out.copy_from_slice(&src[..bytes]);
            for p in out.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        // SAFETY: matching Unmap for the Map in `new`.
        unsafe { self.ctx.Unmap(self.tex, 0) };
    }
}

/// Read the region `r` (texture coordinates) of a GPU texture into an [`Image`].
/// `hdr_white`: `Some(sdr_white)` for FP16 scRGB textures (tone-mapped), `None` for BGRA8.
pub fn read_region(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    src: &ID3D11Texture2D,
    r: Rect,
    hdr_white: Option<f32>,
) -> Result<Image, CaptureError> {
    let d = texture_desc(src);
    let r = r.intersect(&Rect::new(0, 0, d.Width as i32, d.Height as i32));
    if r.is_empty() {
        return Err(CaptureError::Empty);
    }
    let staging = staging_texture(device, r.w as u32, r.h as u32, hdr_white.is_some())?;
    let bx = D3D11_BOX {
        left: r.x as u32,
        top: r.y as u32,
        front: 0,
        right: r.right() as u32,
        bottom: r.bottom() as u32,
        back: 1,
    };
    // SAFETY: both textures live on `device`, same format, box within bounds.
    unsafe { ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, src, 0, Some(&bx)) };
    let m = Mapped::new(ctx, &staging)?;
    Ok(match hdr_white {
        Some(white) => hdr::tonemap_rows(m.width, m.height, |y| m.row_u16(y), white),
        None => {
            let mut img = Image::new(m.width, m.height);
            m.copy_bgra_into(Rect::new(0, 0, r.w, r.h), &mut img, 0, 0);
            img
        }
    })
}
