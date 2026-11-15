//! Webcam picture-in-picture source: an MF source reader on the selected camera, on its
//! own thread, converting to RGB32. The newest frame is kept for the video compositor.

use super::mf::{self, ComGuard};
use crate::common::Image;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::PWSTR;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    /// Symbolic link (stable device id).
    pub id: String,
    pub name: String,
}

fn get_string(a: &IMFActivate, key: &windows::core::GUID) -> String {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: GetAllocatedString returns a CoTaskMem string that we free.
    unsafe {
        if a.GetAllocatedString(key, &mut p, &mut len).is_err() || p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn enum_devices() -> Result<Vec<IMFActivate>> {
    mf::startup()?;
    let attrs = mf::attributes(1)?;
    // SAFETY: attribute setter; MFEnumDeviceSources returns a CoTaskMem array of
    // activation objects that we take ownership of and free.
    unsafe {
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFEnumDeviceSources(&attrs, &mut arr, &mut n)?;
        let mut v = Vec::new();
        if !arr.is_null() {
            for i in 0..n as usize {
                if let Some(a) = std::ptr::read(arr.add(i)) {
                    v.push(a);
                }
            }
            CoTaskMemFree(Some(arr as *const _));
        }
        Ok(v)
    }
}

pub fn list_cameras() -> Vec<Camera> {
    let _com = ComGuard::mta();
    match enum_devices() {
        Ok(v) => v
            .iter()
            .map(|a| Camera {
                id: get_string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK),
                name: get_string(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME),
            })
            .collect(),
        Err(e) => {
            log::warn!("recorder: list cameras: {e:#}");
            Vec::new()
        }
    }
}

/// Latest camera frame plus a sequence number that changes with every new frame.
#[derive(Default)]
pub struct Latest {
    pub seq: u64,
    pub frame: Option<Arc<Image>>,
}

pub struct Webcam {
    pub latest: Arc<Mutex<Latest>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Webcam {
    pub fn start(device_id: &str) -> Webcam {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s, id) = (latest.clone(), stop.clone(), device_id.to_string());
        let thread = std::thread::Builder::new()
            .name("rec-webcam".into())
            .spawn(move || {
                let _com = ComGuard::mta();
                if let Err(e) = run(&id, &l, &s) {
                    log::warn!("recorder: webcam: {e:#}");
                }
            })
            .ok();
        Webcam { latest, stop, thread }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Webcam {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(device_id: &str, latest: &Mutex<Latest>, stop: &AtomicBool) -> Result<()> {
    let devices = enum_devices()?;
    let act = devices
        .iter()
        .find(|a| {
            device_id.is_empty() || get_string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK) == device_id
        })
        .or_else(|| devices.first())
        .ok_or_else(|| anyhow!("no camera"))?;
    // SAFETY: COM calls on valid objects.
    let (source, reader) = unsafe {
        let source: IMFMediaSource = act.ActivateObject()?;
        let attrs = mf::attributes(2)?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)?;
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &t)?;
        (source, reader)
    };
    // SAFETY: plain getters on the negotiated type.
    let (w, h, stride) = unsafe {
        let t = reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)?;
        let (w, h) = mf::unpack(t.GetUINT64(&MF_MT_FRAME_SIZE)?);
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);
        (w, h, stride)
    };
    log::info!("recorder: webcam {w}x{h}");
    while !stop.load(Ordering::Acquire) {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: synchronous read with valid out pointers.
        unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                Some(&mut flags),
                None,
                Some(&mut sample),
            )?;
        }
        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 | MF_SOURCE_READERF_ERROR.0) as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        if let Some(img) = sample_to_image(&sample, w, h, stride) {
            if let Ok(mut l) = latest.lock() {
                l.seq += 1;
                l.frame = Some(Arc::new(img));
            }
        }
    }
    // SAFETY: shut the camera down so its light turns off.
    unsafe {
        let _ = source.Shutdown();
    }
    Ok(())
}

fn sample_to_image(sample: &IMFSample, w: u32, h: u32, stride: i32) -> Option<Image> {
    // SAFETY: the buffer is locked while we read `h` rows of `w * 4` bytes at `stride`.
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ok()?;
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len)).ok()?;
        let row = w as usize * 4;
        let abs = stride.unsigned_abs() as usize;
        let mut img = None;
        if !p.is_null() && abs >= row && len as usize >= abs * (h as usize - 1) + row {
            let mut data = Vec::with_capacity(row * h as usize);
            for y in 0..h as usize {
                let src_y = if stride < 0 { h as usize - 1 - y } else { y };
                let s = std::slice::from_raw_parts(p.add(src_y * abs), row);
                data.extend_from_slice(s);
            }
            // RGB32 has an undefined alpha byte: make it opaque.
            for px in data.chunks_exact_mut(4) {
                px[3] = 255;
            }
            img = Some(Image::from_bgra(w, h, data));
        }
        let _ = buf.Unlock();
        img
    }
}
