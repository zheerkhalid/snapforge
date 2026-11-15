//! DXGI Desktop Duplication backend (one duplication per output, composed for spanning rects).
//!
//! Lives on the capture worker thread. Devices, duplications and one staging texture per
//! output (holding the latest desktop image) are cached between grabs and dropped by the
//! worker when idle. Rotated outputs are not supported here (the caller falls back).

use super::d3d::{self, win_err, Mapped, OutputMeta};
use super::CaptureError;
use crate::common::{Image, Rect};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};

/// How long the first grab of a fresh duplication waits for a full frame.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

struct AdapterDevice {
    adapter_index: u32,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
}

struct DupOutput {
    meta: OutputMeta,
    dev: usize,
    dup: IDXGIOutputDuplication,
    /// Copy of the latest desktop image, valid when `have_frame`.
    staging: Option<ID3D11Texture2D>,
    have_frame: bool,
}

#[derive(Default)]
pub struct DxgiCache {
    devices: Vec<AdapterDevice>,
    outputs: Vec<DupOutput>,
}

enum Refresh {
    Ok,
    AccessLost,
}

impl DxgiCache {
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.outputs.is_empty()
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
        self.devices.clear();
    }

    fn device_index(&mut self, adapter_index: u32) -> Result<usize, CaptureError> {
        if let Some(i) = self.devices.iter().position(|d| d.adapter_index == adapter_index) {
            return Ok(i);
        }
        let factory = d3d::factory()?;
        let adapter = d3d::adapter(&factory, adapter_index)?;
        let (device, ctx) = d3d::create_device(Some(&adapter))?;
        self.devices.push(AdapterDevice { adapter_index, device, ctx });
        Ok(self.devices.len() - 1)
    }

    fn output_index(&mut self, meta: &OutputMeta) -> Result<usize, CaptureError> {
        if let Some(i) = self.outputs.iter().position(|o| o.meta.device == meta.device && o.meta.rect == meta.rect) {
            return Ok(i);
        }
        if meta.rotated {
            return Err(CaptureError::Failed("DXGI: rotated outputs are not supported".into()));
        }
        let dev = self.device_index(meta.adapter_index)?;
        let factory = d3d::factory()?;
        let adapter = d3d::adapter(&factory, meta.adapter_index)?;
        // SAFETY: plain COM calls on live objects.
        let dup = unsafe {
            let output = adapter.EnumOutputs(meta.output_index).map_err(win_err("EnumOutputs"))?;
            let output1: IDXGIOutput1 = output.cast().map_err(win_err("IDXGIOutput1"))?;
            output1.DuplicateOutput(&self.devices[dev].device).map_err(win_err("DuplicateOutput"))?
        };
        self.outputs.push(DupOutput { meta: meta.clone(), dev, dup, staging: None, have_frame: false });
        Ok(self.outputs.len() - 1)
    }

    /// Create devices and duplications for every (unrotated) output, without grabbing.
    pub fn prewarm(&mut self) -> Result<(), CaptureError> {
        for meta in d3d::outputs().iter().filter(|m| !m.rotated) {
            self.output_index(meta)?;
        }
        Ok(())
    }

    /// Bring output `i`'s staging copy up to date with the desktop.
    fn refresh(&mut self, i: usize) -> Result<Refresh, CaptureError> {
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        loop {
            let o = &mut self.outputs[i];
            let dev = &self.devices[o.dev];
            let timeout = if o.have_frame { 0 } else { 50 };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            // SAFETY: valid out pointers; every successful acquire is paired with ReleaseFrame.
            let r = unsafe { o.dup.AcquireNextFrame(timeout, &mut info, &mut res) };
            match r {
                Ok(()) => {
                    let mut result = Ok(());
                    if info.LastPresentTime != 0 {
                        result = (|| {
                            let res = res.ok_or_else(|| CaptureError::Failed("no desktop resource".into()))?;
                            let tex: ID3D11Texture2D = res.cast().map_err(win_err("desktop texture"))?;
                            let desc = d3d::texture_desc(&tex);
                            let stale = o.staging.as_ref().map(|s| {
                                let sd = d3d::texture_desc(s);
                                sd.Width != desc.Width || sd.Height != desc.Height
                            });
                            if stale.unwrap_or(true) {
                                o.staging = Some(d3d::staging_texture(&dev.device, desc.Width, desc.Height, false)?);
                            }
                            if let Some(s) = &o.staging {
                                // SAFETY: same device, same size and format (BGRA8).
                                unsafe { dev.ctx.CopyResource(s, &tex) };
                                o.have_frame = true;
                            }
                            Ok(())
                        })();
                    }
                    // SAFETY: matches the successful AcquireNextFrame above.
                    unsafe {
                        let _ = o.dup.ReleaseFrame();
                    }
                    result?;
                    if o.have_frame {
                        return Ok(Refresh::Ok);
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if o.have_frame {
                        return Ok(Refresh::Ok);
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Ok(Refresh::AccessLost),
                Err(e) => return Err(CaptureError::Failed(format!("AcquireNextFrame: {e}"))),
            }
            if Instant::now() >= deadline {
                return Err(CaptureError::Failed("DXGI: no desktop frame arrived".into()));
            }
        }
    }

    /// Capture `rect` (virtual-screen coordinates), composing every output it touches.
    pub fn grab(&mut self, rect: Rect) -> Result<Image, CaptureError> {
        if rect.is_empty() {
            return Err(CaptureError::Empty);
        }
        let metas = d3d::outputs();
        let parts: Vec<OutputMeta> = metas.into_iter().filter(|m| !m.rect.intersect(&rect).is_empty()).collect();
        if parts.is_empty() {
            return Err(CaptureError::Failed("DXGI: area is not on any monitor".into()));
        }
        let mut img = Image::filled(rect.w as u32, rect.h as u32, [0, 0, 0, 255]);
        for meta in &parts {
            let mut attempt = 0;
            let i = loop {
                let i = self.output_index(meta)?;
                match self.refresh(i)? {
                    Refresh::Ok => break i,
                    Refresh::AccessLost if attempt < 2 => {
                        // Mode change, desktop switch or fullscreen app: recreate the duplication.
                        log::debug!("DXGI access lost on {}; recreating", meta.device);
                        self.outputs.remove(i);
                        attempt += 1;
                    }
                    Refresh::AccessLost => {
                        self.outputs.remove(i);
                        d3d::invalidate_outputs();
                        return Err(CaptureError::Failed("DXGI: access lost".into()));
                    }
                }
            };
            let o = &self.outputs[i];
            let dev = &self.devices[o.dev];
            let Some(staging) = &o.staging else {
                return Err(CaptureError::Failed("DXGI: no staging texture".into()));
            };
            let part = meta.rect.intersect(&rect);
            let m = Mapped::new(&dev.ctx, staging)?;
            m.copy_bgra_into(part.offset(-meta.rect.x, -meta.rect.y), &mut img, part.x - rect.x, part.y - rect.y);
        }
        Ok(img)
    }
}
