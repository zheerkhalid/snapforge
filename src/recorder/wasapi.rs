//! WASAPI capture: microphone and system-audio loopback, each on its own thread.
//!
//! Every packet is decoded to stereo float, resampled to 48 kHz and placed on the shared
//! timeline by its QPC timestamp (see [`dsp::Track`]), so a silent loopback (which
//! delivers no packets at all) becomes silence and device clock drift is corrected.

use super::clock::{self, MediaClock};
use super::dsp::{self, InputFormat, Mixer, Resampler, SampleKind};
use super::mf::ComGuard;
use crate::common::win::{pcwstr, wide};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::core::{implement, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: standard COM creation (COM is initialised by the caller's thread).
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

fn device_id(d: &IMMDevice) -> String {
    // SAFETY: GetId returns a CoTaskMem string that we free.
    unsafe {
        match d.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

fn device_name(d: &IMMDevice) -> String {
    // SAFETY: property store read of a well-known key.
    unsafe {
        d.OpenPropertyStore(STGM_READ)
            .and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

/// Active capture (microphone) endpoints.
pub fn list_inputs() -> Vec<AudioDevice> {
    let _com = ComGuard::mta();
    let run = || -> Result<Vec<AudioDevice>> {
        let e = enumerator()?;
        // SAFETY: valid enumerator.
        let col = unsafe { e.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)? };
        let mut v = Vec::new();
        // SAFETY: indices below GetCount.
        unsafe {
            for i in 0..col.GetCount()? {
                if let Ok(d) = col.Item(i) {
                    v.push(AudioDevice { id: device_id(&d), name: device_name(&d) });
                }
            }
        }
        Ok(v)
    };
    run().unwrap_or_else(|e| {
        log::warn!("recorder: list audio inputs: {e:#}");
        Vec::new()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Endpoint id; empty = default capture device.
    Mic(String),
    /// Default render device; follows default-device changes when `follow` is set.
    Loopback { follow: bool },
}

/// Sets a flag when the default render device changes.
#[implement(IMMNotificationClient)]
struct DefaultWatcher {
    changed: Arc<AtomicBool>,
}

impl IMMNotificationClient_Impl for DefaultWatcher_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            self.changed.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(
        &self,
        _id: &PCWSTR,
        _key: &windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Parse a WASAPI mix format.
///
/// # Safety
/// `wf` must point to a valid WAVEFORMATEX (or WAVEFORMATEXTENSIBLE when tagged so).
unsafe fn parse_format(wf: *const WAVEFORMATEX) -> Option<InputFormat> {
    let w = &*wf;
    let tag = w.wFormatTag as u32;
    let bits = w.wBitsPerSample;
    let (float, valid_bits) = if tag == WAVE_FORMAT_EXTENSIBLE {
        let x = &*(wf as *const WAVEFORMATEXTENSIBLE);
        let sub = x.SubFormat;
        let valid = x.Samples.wValidBitsPerSample;
        if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            (true, valid)
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            (false, valid)
        } else {
            return None;
        }
    } else if tag == WAVE_FORMAT_IEEE_FLOAT {
        (true, bits)
    } else if tag == WAVE_FORMAT_PCM {
        (false, bits)
    } else {
        return None;
    };
    let _ = valid_bits;
    let kind = match (float, bits) {
        (true, 32) => SampleKind::F32,
        (false, 16) => SampleKind::I16,
        (false, 24) => SampleKind::I24,
        (false, 32) => SampleKind::I32,
        _ => return None,
    };
    Some(InputFormat { rate: w.nSamplesPerSec, channels: w.nChannels, kind })
}

pub struct Shared {
    pub mixer: Mutex<Mixer>,
    pub clock: Mutex<MediaClock>,
}

/// Spawn a capture thread feeding mixer input `input`. Runs until `stop` is set.
pub fn spawn(
    source: Source,
    input: usize,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>> {
    let name = match source {
        Source::Mic(_) => "rec-mic",
        Source::Loopback { .. } => "rec-loopback",
    };
    Ok(std::thread::Builder::new().name(name.into()).spawn(move || {
        let _com = ComGuard::mta();
        let changed = Arc::new(AtomicBool::new(false));
        let enumr = enumerator().ok();
        let watcher: Option<IMMNotificationClient> = match (&source, &enumr) {
            (Source::Loopback { follow: true }, Some(e)) => {
                let w: IMMNotificationClient = DefaultWatcher { changed: changed.clone() }.into();
                // SAFETY: valid enumerator and callback; unregistered below.
                unsafe { e.RegisterEndpointNotificationCallback(&w).ok().map(|_| w) }
            }
            _ => None,
        };
        while !stop.load(Ordering::Acquire) {
            changed.store(false, Ordering::Release);
            match run_stream(&source, input, &shared, &stop, &changed) {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("recorder: {name}: {e:#}; retrying");
                    // Device gone or not available: retry until stopped.
                    for _ in 0..50 {
                        if stop.load(Ordering::Acquire) || changed.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
        if let (Some(w), Some(e)) = (watcher, enumr) {
            // SAFETY: unregister the callback registered above.
            unsafe {
                let _ = e.UnregisterEndpointNotificationCallback(&w);
            }
        }
    })?)
}

fn open_device(source: &Source) -> Result<IMMDevice> {
    let e = enumerator()?;
    // SAFETY: valid enumerator and NUL-terminated id.
    unsafe {
        Ok(match source {
            Source::Mic(id) if !id.is_empty() => {
                let w = wide(id);
                match e.GetDevice(pcwstr(&w)) {
                    Ok(d) => d,
                    Err(err) => {
                        log::warn!("recorder: microphone {id} unavailable ({err}); using the default");
                        e.GetDefaultAudioEndpoint(eCapture, eConsole)?
                    }
                }
            }
            Source::Mic(_) => e.GetDefaultAudioEndpoint(eCapture, eConsole)?,
            Source::Loopback { .. } => e.GetDefaultAudioEndpoint(eRender, eConsole)?,
        })
    }
}

/// Capture until stopped, the device fails, or (loopback) the default device changes.
fn run_stream(source: &Source, input: usize, shared: &Shared, stop: &AtomicBool, changed: &AtomicBool) -> Result<()> {
    let device = open_device(source)?;
    log::info!("recorder: audio {:?} on \"{}\"", source, device_name(&device));
    // SAFETY: COM calls on valid objects; the mix format is freed with CoTaskMemFree.
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let wf = client.GetMixFormat()?;
        let fmt = parse_format(wf);
        let flags = if matches!(source, Source::Loopback { .. }) { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, wf, None);
        CoTaskMemFree(Some(wf as *const _));
        init?;
        let fmt = fmt.ok_or_else(|| anyhow!("unsupported mix format"))?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let mut resampler = Resampler::new(fmt.rate, dsp::RATE);
        let mut stereo = Vec::new();
        let mut out = Vec::new();
        let bpf = fmt.bytes_per_frame();
        let result = loop {
            if stop.load(Ordering::Acquire) {
                break Ok(());
            }
            if changed.load(Ordering::Acquire) {
                log::info!("recorder: default output device changed; switching loopback");
                break Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
            let mut failed = None;
            loop {
                let n = match cap.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                };
                if n == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let (mut frames, mut bflags, mut qpc) = (0u32, 0u32, 0u64);
                if let Err(e) = cap.GetBuffer(&mut data, &mut frames, &mut bflags, None, Some(&mut qpc)) {
                    failed = Some(e);
                    break;
                }
                stereo.clear();
                if bflags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    stereo.resize(frames as usize, [0.0; 2]);
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * bpf);
                    dsp::decode_to_stereo(&fmt, bytes, &mut stereo);
                }
                let _ = cap.ReleaseBuffer(frames);
                let ts = if qpc == 0 || bflags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
                    clock::qpc_hns() - frames as i64 * clock::HNS_PER_SEC / fmt.rate.max(1) as i64
                } else {
                    qpc as i64
                };
                out.clear();
                resampler.process(&stereo, &mut out);
                let media = shared.clock.lock().map(|c| c.media_time(ts)).unwrap_or(None);
                if let Some(t) = media {
                    if let Ok(mut m) = shared.mixer.lock() {
                        let floor = m.emitted;
                        if let Some(inp) = m.inputs.get_mut(input) {
                            inp.track.push_at(clock::sample_index(t, dsp::RATE), &out, floor);
                        }
                    }
                }
            }
            if let Some(e) = failed {
                break Err(anyhow!("capture failed: {e}"));
            }
        };
        let _ = client.Stop();
        result
    }
}

/// Play a sine tone on the default output for `secs` (used by the integration test so
/// loopback has something to capture).
#[cfg(test)]
pub fn play_tone(freq: f32, secs: f32, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<Result<()>> {
    std::thread::spawn(move || -> Result<()> {
        let _com = ComGuard::mta();
        let e = enumerator()?;
        // SAFETY: COM calls on valid objects; buffers are filled within the granted size.
        unsafe {
            let device = e.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let wf = client.GetMixFormat()?;
            let fmt = parse_format(wf);
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, wf, None);
            CoTaskMemFree(Some(wf as *const _));
            init?;
            let fmt = fmt.ok_or_else(|| anyhow!("unsupported render format"))?;
            if fmt.kind != SampleKind::F32 {
                return Err(anyhow!("tone needs a float mix format"));
            }
            let render: IAudioRenderClient = client.GetService()?;
            let size = client.GetBufferSize()?;
            client.Start()?;
            let total = (secs * fmt.rate as f32) as u64;
            let mut pos = 0u64;
            while pos < total && !stop.load(Ordering::Acquire) {
                let pad = client.GetCurrentPadding()?;
                let n = (size - pad).min((total - pos) as u32);
                if n > 0 {
                    let p = render.GetBuffer(n)? as *mut f32;
                    let buf = std::slice::from_raw_parts_mut(p, n as usize * fmt.channels as usize);
                    for (i, fr) in buf.chunks_exact_mut(fmt.channels as usize).enumerate() {
                        let t = (pos + i as u64) as f32 / fmt.rate as f32;
                        let v = 0.3 * (2.0 * std::f32::consts::PI * freq * t).sin();
                        fr.iter_mut().for_each(|s| *s = v);
                    }
                    render.ReleaseBuffer(n, 0)?;
                    pos += n as u64;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(100));
            let _ = client.Stop();
        }
        Ok(())
    })
}
