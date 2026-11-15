//! Small Media Foundation helpers: a video reader (RGB32 frames via the source reader's video
//! processor, hardware decoding when available), a PCM audio reader, and an MP4 writer
//! (H.264 + AAC through the sink writer, hardware encoding when available).
//!
//! Every object here belongs to the thread that created it. Call [`init`] first on that thread
//! and keep the guard alive longer than the readers and writers (declare it first).

use crate::common::Image;
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use windows::core::{Interface, GUID, PROPVARIANT};
use windows::Win32::Foundation::{HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// 100-ns units per millisecond / second.
pub const HNS_PER_MS: i64 = 10_000;
pub const HNS_PER_SEC: i64 = 10_000_000;

const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO_STREAM: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
const MEDIASOURCE: u32 = MF_SOURCE_READER_MEDIASOURCE.0 as u32;

/// COM (MTA) + `MFStartup` for the current thread; undone on drop.
pub struct MfGuard {
    com: bool,
}

pub fn init() -> Result<MfGuard> {
    // SAFETY: plain per-thread COM/MF initialisation, balanced in Drop.
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        let com = hr.is_ok();
        if hr.is_err() && hr != RPC_E_CHANGED_MODE {
            bail!("CoInitializeEx failed: {hr:?}");
        }
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            if com {
                CoUninitialize();
            }
            return Err(anyhow!("Media Foundation is not available: {e}"));
        }
        Ok(MfGuard { com })
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        // SAFETY: balances init().
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// A frame rate as a fraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub fn new(num: u32, den: u32) -> Self {
        if num == 0 || den == 0 {
            Self { num: 30, den: 1 }
        } else {
            Self { num, den }
        }
    }
    pub fn from_fps(fps: f64) -> Self {
        let fps = if fps.is_finite() { fps.clamp(1.0, 240.0) } else { 30.0 };
        Self::new((fps * 1000.0).round() as u32, 1000)
    }
    pub fn fps(&self) -> f64 {
        self.num as f64 / self.den as f64
    }
    /// Timestamp of frame `n` in 100-ns units (no accumulated rounding).
    pub fn frame_time(&self, n: u64) -> i64 {
        (n as i128 * HNS_PER_SEC as i128 * self.den as i128 / self.num as i128) as i64
    }
    pub fn pack(&self) -> u64 {
        ((self.num as u64) << 32) | self.den as u64
    }
}

/// Facts about an opened video.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub has_audio: bool,
}

fn pv_u64(p: &PROPVARIANT) -> Option<u64> {
    u64::try_from(p).ok().or_else(|| i64::try_from(p).ok().map(|v| v.max(0) as u64))
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

unsafe fn duration_ms(reader: &IMFSourceReader) -> u64 {
    reader
        .GetPresentationAttribute(MEDIASOURCE, &MF_PD_DURATION)
        .ok()
        .and_then(|p| pv_u64(&p))
        .map(|hns| hns / HNS_PER_MS as u64)
        .unwrap_or(0)
}

unsafe fn has_stream(reader: &IMFSourceReader, stream: u32) -> bool {
    reader.GetNativeMediaType(stream, 0).is_ok()
}

fn path_wide(path: &Path) -> Vec<u16> {
    crate::common::win::wide(&path.to_string_lossy())
}

/// D3D11 device + DXGI device manager for hardware decoding / encoding.
struct Dxgi {
    _device: ID3D11Device,
    manager: IMFDXGIDeviceManager,
}

fn create_dxgi() -> Result<Dxgi> {
    // SAFETY: standard D3D11 device creation; out-pointers are valid locals.
    unsafe {
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.ok_or_else(|| anyhow!("no D3D11 device"))?;
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| anyhow!("no DXGI device manager"))?;
        manager.ResetDevice(&device, token)?;
        Ok(Dxgi { _device: device, manager })
    }
}

// ---------------------------------------------------------------------------------------------
// Video reader
// ---------------------------------------------------------------------------------------------

pub struct VideoFrame {
    pub pts_hns: i64,
    pub dur_hns: i64,
    /// Opaque premultiplied BGRA at the display size.
    pub image: Image,
}

impl VideoFrame {
    pub fn pts_ms(&self) -> u64 {
        (self.pts_hns.max(0) / HNS_PER_MS) as u64
    }
}

pub struct VideoReader {
    reader: IMFSourceReader,
    _dxgi: Option<Dxgi>,
    pub info: MediaInfo,
    /// Buffer layout of the current output type.
    frame_w: u32,
    frame_h: u32,
    stride: i32,
    pub hardware: bool,
}

impl VideoReader {
    /// Open the first video stream of `path`. Tries hardware decoding first (DXGI device
    /// manager + advanced video processing), then software.
    pub fn open(path: &Path, prefer_hardware: bool) -> Result<Self> {
        if prefer_hardware {
            match create_dxgi().and_then(|d| Self::open_with(path, Some(d))) {
                Ok(r) => return Ok(r),
                Err(e) => log::info!("video editor: hardware decoding unavailable ({e:#}); using software"),
            }
        }
        Self::open_with(path, None)
    }

    fn open_with(path: &Path, dxgi: Option<Dxgi>) -> Result<Self> {
        let url = path_wide(path);
        // SAFETY: COM calls on objects owned by this thread; buffers outlive the calls.
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 4)?;
            let attrs = attrs.ok_or_else(|| anyhow!("no attributes"))?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            match &dxgi {
                Some(d) => {
                    attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &d.manager)?;
                    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
                }
                None => attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?,
            }
            let reader = MFCreateSourceReaderFromURL(windows::core::PCWSTR(url.as_ptr()), &attrs)
                .with_context(|| format!("could not open {}", path.display()))?;
            if !has_stream(&reader, VIDEO_STREAM) {
                bail!("{} has no video stream", path.display());
            }
            let has_audio = has_stream(&reader, AUDIO_STREAM);
            reader.SetStreamSelection(ALL_STREAMS, false)?;
            reader.SetStreamSelection(VIDEO_STREAM, true)?;
            let native = reader.GetNativeMediaType(VIDEO_STREAM, 0)?;
            let (num, den) = native.GetUINT64(&MF_MT_FRAME_RATE).map(unpack).unwrap_or((30, 1));
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            reader.SetCurrentMediaType(VIDEO_STREAM, None, &out).context("the video cannot be converted to RGB")?;
            let mut r = Self {
                reader,
                hardware: dxgi.is_some(),
                _dxgi: dxgi,
                info: MediaInfo {
                    duration_ms: 0,
                    width: 0,
                    height: 0,
                    frame_rate: FrameRate::new(num, den),
                    has_audio,
                },
                frame_w: 0,
                frame_h: 0,
                stride: 0,
            };
            r.refresh_type()?;
            // Display size from the native type (minimum display aperture, else frame size).
            let (nw, nh) = native.GetUINT64(&MF_MT_FRAME_SIZE).map(unpack).unwrap_or((r.frame_w, r.frame_h));
            let (dw, dh) = aperture(&native).unwrap_or((nw, nh));
            r.info.width = dw.min(r.frame_w).max(1);
            r.info.height = dh.min(r.frame_h).max(1);
            r.info.duration_ms = duration_ms(&r.reader);
            Ok(r)
        }
    }

    unsafe fn refresh_type(&mut self) -> Result<()> {
        let cur = self.reader.GetCurrentMediaType(VIDEO_STREAM)?;
        let (w, h) = unpack(cur.GetUINT64(&MF_MT_FRAME_SIZE)?);
        if w == 0 || h == 0 {
            bail!("the video has no frame size");
        }
        let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);
        self.frame_w = w;
        self.frame_h = h;
        self.stride = if stride == 0 { w as i32 * 4 } else { stride };
        if self.info.width == 0 || self.info.width > w || self.info.height > h {
            self.info.width = self.info.width.min(w).max(1);
            self.info.height = self.info.height.min(h).max(1);
        }
        Ok(())
    }

    /// Seek so the next frame read is the keyframe at or before `ms`.
    pub fn seek(&mut self, ms: u64) -> Result<()> {
        let pv = PROPVARIANT::from(ms as i64 * HNS_PER_MS);
        // SAFETY: valid GUID and PROPVARIANT for the call's duration.
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &pv)? };
        Ok(())
    }

    /// Next decoded frame, `None` at the end of the stream.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        let default_dur = self.info.frame_rate.frame_time(1).max(1);
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: out-pointers are valid locals.
            unsafe {
                self.reader.ReadSample(VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                bail!("the video decoder reported an error");
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                // SAFETY: as above.
                unsafe { self.refresh_type()? };
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
            let Some(sample) = sample else { continue };
            // SAFETY: the sample and its buffer are valid; we copy out under Lock/Unlock.
            let image = unsafe { self.copy_sample(&sample)? };
            let dur = unsafe { sample.GetSampleDuration() }.ok().filter(|d| *d > 0).unwrap_or(default_dur);
            return Ok(Some(VideoFrame { pts_hns: ts, dur_hns: dur, image }));
        }
    }

    /// Decode forward to the frame shown at `ms` (seek first if needed).
    pub fn frame_at(&mut self, ms: u64) -> Result<Option<VideoFrame>> {
        self.seek(ms)?;
        let target = ms as i64 * HNS_PER_MS;
        let mut last = None;
        while let Some(f) = self.next_frame()? {
            if f.pts_hns + f.dur_hns > target {
                return Ok(Some(f));
            }
            last = Some(f);
        }
        Ok(last)
    }

    unsafe fn copy_sample(&self, sample: &IMFSample) -> Result<Image> {
        let buf = sample.ConvertToContiguousBuffer()?;
        let (w, h) = (self.info.width, self.info.height);
        let row_bytes = w as usize * 4;
        let mut img = Image::new(w, h);
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut p: *mut u8 = std::ptr::null_mut();
            let mut pitch = 0i32;
            if b2.Lock2D(&mut p, &mut pitch).is_ok() {
                let res = copy_rows(&mut img, p, pitch as isize, None, row_bytes);
                let _ = b2.Unlock2D();
                res?;
                return Ok(img);
            }
        }
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len))?;
        let (start, pitch) = if self.stride < 0 {
            let abs = (-self.stride) as usize;
            ((self.frame_h as usize).saturating_sub(1) * abs, self.stride as isize)
        } else {
            (0, self.stride as isize)
        };
        // Bounds: every row we read must lie inside [p, p + len).
        let need = (self.frame_h as usize).saturating_sub(1) * pitch.unsigned_abs() + row_bytes;
        let res = if (len as usize) < need || h > self.frame_h {
            Err(anyhow!("short video buffer ({len} < {need})"))
        } else {
            copy_rows(&mut img, p.add(start), pitch, Some(len as usize), row_bytes)
        };
        let _ = buf.Unlock();
        res?;
        Ok(img)
    }
}

/// Copy `img.height` rows of `row_bytes` from `p` (row step `pitch`) and make them opaque.
unsafe fn copy_rows(img: &mut Image, p: *const u8, pitch: isize, _len: Option<usize>, row_bytes: usize) -> Result<()> {
    if p.is_null() || pitch.unsigned_abs() < row_bytes {
        bail!("invalid video buffer");
    }
    for y in 0..img.height as usize {
        let src = std::slice::from_raw_parts(p.offset(y as isize * pitch), row_bytes);
        let dst = &mut img.data[y * row_bytes..(y + 1) * row_bytes];
        dst.copy_from_slice(src);
        for px in dst.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Ok(())
}

unsafe fn aperture(t: &IMFMediaType) -> Option<(u32, u32)> {
    let mut area = MFVideoArea::default();
    let bytes = std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, size_of::<MFVideoArea>());
    t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).ok()?;
    (area.Area.cx > 0 && area.Area.cy > 0).then_some((area.Area.cx as u32, area.Area.cy as u32))
}

/// Probe a file without keeping it open.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    Ok(VideoReader::open(path, false)?.info)
}

// ---------------------------------------------------------------------------------------------
// Audio reader
// ---------------------------------------------------------------------------------------------

/// Interleaved 16-bit PCM from the first audio stream.
pub struct AudioReader {
    reader: IMFSourceReader,
    pub rate: u32,
    pub channels: u32,
    pub duration_ms: u64,
}

pub struct AudioChunk {
    pub pts_hns: i64,
    pub samples: Vec<i16>,
}

impl AudioReader {
    /// `Ok(None)` when the file has no audio. Asks for 48 kHz stereo (the source reader
    /// resamples); falls back to the native rate and channel count.
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let url = path_wide(path);
        // SAFETY: COM calls on objects owned by this thread.
        unsafe {
            let reader = MFCreateSourceReaderFromURL(windows::core::PCWSTR(url.as_ptr()), None)
                .with_context(|| format!("could not open {}", path.display()))?;
            if !has_stream(&reader, AUDIO_STREAM) {
                return Ok(None);
            }
            reader.SetStreamSelection(ALL_STREAMS, false)?;
            reader.SetStreamSelection(AUDIO_STREAM, true)?;
            let want = |rate: Option<u32>, ch: Option<u32>| -> Result<IMFMediaType> {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                if let (Some(rate), Some(ch)) = (rate, ch) {
                    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch)?;
                    t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ch * 2)?;
                    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * ch * 2)?;
                }
                Ok(t)
            };
            let ok =
                want(Some(48_000), Some(2)).and_then(|t| Ok(reader.SetCurrentMediaType(AUDIO_STREAM, None, &t)?));
            if ok.is_err() {
                let t = want(None, None)?;
                reader.SetCurrentMediaType(AUDIO_STREAM, None, &t).context("the audio cannot be decoded to PCM")?;
            }
            let cur = reader.GetCurrentMediaType(AUDIO_STREAM)?;
            let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000).max(1);
            let channels = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2).max(1);
            let bits = cur.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).unwrap_or(16);
            if bits != 16 {
                bail!("unsupported audio sample size ({bits} bits)");
            }
            let duration_ms = duration_ms(&reader);
            Ok(Some(Self { reader, rate, channels, duration_ms }))
        }
    }

    pub fn seek(&mut self, ms: u64) -> Result<()> {
        let pv = PROPVARIANT::from(ms as i64 * HNS_PER_MS);
        // SAFETY: valid arguments for the call's duration.
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &pv)? };
        Ok(())
    }

    pub fn next_chunk(&mut self) -> Result<Option<AudioChunk>> {
        loop {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: out-pointers are valid locals; buffer copied under Lock/Unlock.
            unsafe {
                self.reader.ReadSample(AUDIO_STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                    bail!("the audio decoder reported an error");
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Ok(None);
                }
                let Some(sample) = sample else { continue };
                let buf = sample.ConvertToContiguousBuffer()?;
                let mut p: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buf.Lock(&mut p, None, Some(&mut len))?;
                let n = len as usize / 2;
                let mut samples = vec![0i16; n];
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(p, samples.as_mut_ptr() as *mut u8, n * 2);
                }
                let _ = buf.Unlock();
                return Ok(Some(AudioChunk { pts_hns: ts, samples }));
            }
        }
    }
}

/// Random access to source PCM by sample index, decoding sequentially and seeking when a
/// request jumps. Used by export (kept segments only) and anything else that reads ranges.
pub struct PcmCursor {
    reader: AudioReader,
    buf: std::collections::VecDeque<i16>,
    /// Source frame index (per channel) of `buf[0]`.
    buf_start: i64,
    eof: bool,
}

impl PcmCursor {
    pub fn new(reader: AudioReader) -> Self {
        Self { reader, buf: Default::default(), buf_start: 0, eof: false }
    }
    pub fn rate(&self) -> u32 {
        self.reader.rate
    }
    pub fn channels(&self) -> u32 {
        self.reader.channels
    }

    fn buf_end(&self) -> i64 {
        self.buf_start + (self.buf.len() / self.reader.channels as usize) as i64
    }

    /// Interleaved samples for source frames `[a, b)`, zero-padded where there's no audio.
    pub fn read(&mut self, a: i64, b: i64) -> Result<Vec<i16>> {
        let ch = self.reader.channels as usize;
        let rate = self.reader.rate as i64;
        let mut out = vec![0i16; (b - a).max(0) as usize * ch];
        if b <= a {
            return Ok(out);
        }
        // Jump backwards or far ahead: seek.
        if a < self.buf_start || a > self.buf_end() + rate {
            self.reader.seek((a.max(0) * 1000 / rate) as u64)?;
            self.buf.clear();
            self.buf_start = a.max(0);
            self.eof = false;
            let mut first = true;
            // Re-anchor on the first chunk's timestamp.
            while let Some(c) = self.reader.next_chunk()? {
                let start = c.pts_hns * rate / HNS_PER_SEC;
                if first {
                    self.buf_start = start;
                    first = false;
                }
                self.buf.extend(c.samples);
                if self.buf_end() > a {
                    break;
                }
            }
        }
        while self.buf_end() < b && !self.eof {
            match self.reader.next_chunk()? {
                Some(c) => {
                    let start = c.pts_hns * rate / HNS_PER_SEC;
                    // Fill a gap with silence; drop overlap.
                    let end = self.buf_end();
                    if start > end && start - end < rate * 10 {
                        self.buf.resize(self.buf.len() + (start - end) as usize * ch, 0);
                    }
                    self.buf.extend(c.samples);
                }
                None => self.eof = true,
            }
        }
        // Copy the overlap of [a, b) with the buffer.
        let lo = a.max(self.buf_start);
        let hi = b.min(self.buf_end());
        if hi > lo {
            let src0 = (lo - self.buf_start) as usize * ch;
            let dst0 = (lo - a) as usize * ch;
            let n = (hi - lo) as usize * ch;
            for i in 0..n {
                out[dst0 + i] = self.buf[src0 + i];
            }
        }
        // Drop what we have consumed (keep a little for small backward steps).
        let keep_from = (b - rate / 10).max(self.buf_start);
        let drop = ((keep_from - self.buf_start) as usize * ch).min(self.buf.len());
        self.buf.drain(..drop);
        self.buf_start += (drop / ch) as i64;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// MP4 writer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriterConfig {
    /// Must be even (H.264 4:2:0).
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    /// Video bit rate in bits per second.
    pub bitrate: u32,
    /// `(sample rate, channels)` of the PCM that will be written, or no audio track.
    pub audio: Option<(u32, u32)>,
    pub hardware: bool,
}

impl WriterConfig {
    /// A reasonable H.264 bit rate for screen content: ~0.12 bits per pixel per frame,
    /// 2–40 Mbit/s.
    pub fn default_bitrate(w: u32, h: u32, fps: f64) -> u32 {
        let b = w as f64 * h as f64 * fps.clamp(1.0, 120.0) * 0.12;
        b.clamp(2_000_000.0, 40_000_000.0) as u32
    }
}

pub struct Mp4Writer {
    writer: IMFSinkWriter,
    video: u32,
    audio: Option<(u32, u32, u32)>, // (stream, rate, channels)
    cfg: WriterConfig,
    audio_frames: i64,
    pub hardware: bool,
}

impl Mp4Writer {
    /// Create `path` (overwritten). Falls back to software encoding if hardware setup fails.
    pub fn create(path: &Path, cfg: WriterConfig) -> Result<Self> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width % 2 != 0 || cfg.height % 2 != 0 {
            bail!("the output size must be even ({}×{})", cfg.width, cfg.height);
        }
        if cfg.hardware {
            match Self::create_with(path, cfg, true) {
                Ok(w) => return Ok(w),
                Err(e) => log::info!("video editor: hardware encoding unavailable ({e:#}); using software"),
            }
        }
        Self::create_with(path, cfg, false)
    }

    fn create_with(path: &Path, cfg: WriterConfig, hardware: bool) -> Result<Self> {
        let url = path_wide(path);
        let _ = std::fs::remove_file(path);
        // SAFETY: COM calls on objects owned by this thread.
        unsafe {
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 3)?;
            let attrs = attrs.ok_or_else(|| anyhow!("no attributes"))?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            let writer = MFCreateSinkWriterFromURL(windows::core::PCWSTR(url.as_ptr()), None, &attrs)
                .with_context(|| format!("could not create {}", path.display()))?;

            let size = ((cfg.width as u64) << 32) | cfg.height as u64;
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate.max(100_000))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            out.SetUINT64(&MF_MT_FRAME_RATE, cfg.frame_rate.pack())?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
            let video = writer.AddStream(&out)?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
            input.SetUINT64(&MF_MT_FRAME_RATE, cfg.frame_rate.pack())?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            // Positive stride = top-down rows (our Image layout).
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, cfg.width * 4)?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            writer.SetInputMediaType(video, &input, None).context("the H.264 encoder rejected the input format")?;

            let mut audio = None;
            if let Some((rate, ch)) = cfg.audio {
                // AAC supports 44.1/48 kHz, mono or stereo, 16-bit.
                if (rate == 44_100 || rate == 48_000) && (ch == 1 || ch == 2) {
                    let aout = MFCreateMediaType()?;
                    aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                    aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch)?;
                    aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24_000)?; // 192 kbit/s
                    aout.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                    let stream = writer.AddStream(&aout)?;
                    let ain = MFCreateMediaType()?;
                    ain.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                    ain.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                    ain.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                    ain.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
                    ain.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch)?;
                    ain.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, ch * 2)?;
                    ain.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * ch * 2)?;
                    writer
                        .SetInputMediaType(stream, &ain, None)
                        .context("the AAC encoder rejected the input format")?;
                    audio = Some((stream, rate, ch));
                } else {
                    log::warn!("video editor: audio {rate} Hz × {ch} can't be AAC-encoded; exporting without audio");
                }
            }
            writer.BeginWriting().context("could not start writing the MP4")?;
            Ok(Self { writer, video, audio, cfg, audio_frames: 0, hardware })
        }
    }

    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// Write frame number `n` (timestamps come from the frame rate). `img` must be the
    /// configured size.
    pub fn write_frame(&mut self, img: &Image, n: u64) -> Result<()> {
        if img.width != self.cfg.width || img.height != self.cfg.height {
            bail!("frame size {}×{} does not match the output", img.width, img.height);
        }
        let t0 = self.cfg.frame_rate.frame_time(n);
        let t1 = self.cfg.frame_rate.frame_time(n + 1);
        // SAFETY: the buffer is locked while we copy exactly its length.
        unsafe {
            let sample = make_sample(&img.data, t0, t1 - t0)?;
            self.writer.WriteSample(self.video, &sample)?;
        }
        Ok(())
    }

    /// Append interleaved PCM to the audio track (no-op without one).
    pub fn write_audio(&mut self, pcm: &[i16]) -> Result<()> {
        let Some((stream, rate, ch)) = self.audio else { return Ok(()) };
        if pcm.is_empty() {
            return Ok(());
        }
        let frames = (pcm.len() / ch as usize) as i64;
        let t0 = self.audio_frames * HNS_PER_SEC / rate as i64;
        let t1 = (self.audio_frames + frames) * HNS_PER_SEC / rate as i64;
        // SAFETY: plain byte view of the i16 slice.
        unsafe {
            let bytes = std::slice::from_raw_parts(pcm.as_ptr() as *const u8, std::mem::size_of_val(pcm));
            let sample = make_sample(bytes, t0, t1 - t0)?;
            self.writer.WriteSample(stream, &sample)?;
        }
        self.audio_frames += frames;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        // SAFETY: finalize the file; the writer is dropped afterwards.
        unsafe { self.writer.Finalize().context("could not finish the MP4 file") }
    }
}

unsafe fn make_sample(bytes: &[u8], time: i64, dur: i64) -> Result<IMFSample> {
    let len = u32::try_from(bytes.len()).map_err(|_| anyhow!("sample too large"))?;
    let buf = MFCreateMemoryBuffer(len)?;
    let mut p: *mut u8 = std::ptr::null_mut();
    buf.Lock(&mut p, None, None)?;
    if p.is_null() {
        let _ = buf.Unlock();
        bail!("could not lock a media buffer");
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
    buf.Unlock()?;
    buf.SetCurrentLength(len)?;
    let sample = MFCreateSample()?;
    sample.AddBuffer(&buf)?;
    sample.SetSampleTime(time)?;
    sample.SetSampleDuration(dur)?;
    Ok(sample)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Write a synthetic clip: a moving bar over a colour that changes every second, top half
    /// red-ish marker row, plus a 440 Hz tone. Returns false when MF/H.264 is unavailable.
    pub fn write_test_clip(path: &Path, w: u32, h: u32, fps: u32, secs: u32, audio: bool) -> Result<()> {
        let cfg = WriterConfig {
            width: w,
            height: h,
            frame_rate: FrameRate::new(fps, 1),
            bitrate: 2_000_000,
            audio: audio.then_some((48_000, 2)),
            hardware: false,
        };
        let mut wr = Mp4Writer::create(path, cfg)?;
        let frames = fps * secs;
        let per_frame = 48_000 / fps as usize;
        for n in 0..frames {
            let sec = n / fps;
            let bg = [(sec * 60 % 256) as u8, 80, 200 - (sec * 50 % 200) as u8, 255];
            let mut img = Image::filled(w, h, bg);
            // Top quarter pure red (orientation check).
            for y in 0..h / 4 {
                for x in 0..w {
                    img.set_pixel(x, y, [0, 0, 255, 255]);
                }
            }
            let bx = (n * 4) % w.max(1);
            for y in h / 2..h {
                for x in bx..(bx + 8).min(w) {
                    img.set_pixel(x, y, [255, 255, 255, 255]);
                }
            }
            wr.write_frame(&img, n as u64)?;
            if audio {
                let mut pcm = Vec::with_capacity(per_frame * 2);
                for i in 0..per_frame {
                    let t = (n as usize * per_frame + i) as f32 / 48_000.0;
                    let v = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
                    pcm.push(v);
                    pcm.push(v);
                }
                wr.write_audio(&pcm)?;
            }
        }
        wr.finish()
    }

    #[test]
    fn frame_rate_math() {
        let r = FrameRate::new(30_000, 1001);
        assert_eq!(r.frame_time(0), 0);
        assert_eq!(r.frame_time(30_000), 1001 * HNS_PER_SEC);
        assert_eq!(FrameRate::new(0, 0), FrameRate::new(30, 1));
        assert_eq!(FrameRate::from_fps(25.0).fps(), 25.0);
    }

    #[test]
    #[ignore = "needs Media Foundation with an H.264 encoder (run with --ignored)"]
    fn write_then_read_back() {
        let _mf = init().expect("mf");
        let path = std::env::temp_dir().join(format!("sf-mf-rw-{}.mp4", std::process::id()));
        write_test_clip(&path, 320, 240, 30, 2, true).expect("write");
        let mut r = VideoReader::open(&path, true).expect("open");
        assert_eq!((r.info.width, r.info.height), (320, 240));
        assert!((1900..=2100).contains(&r.info.duration_ms), "{}", r.info.duration_ms);
        assert!(r.info.has_audio);
        let f = r.next_frame().expect("read").expect("frame");
        // Top-left is red, bottom-left is the background (not red): rows are top-down.
        let tl = f.image.pixel(4, 4);
        assert!(tl[2] > 200 && tl[1] < 60 && tl[0] < 60, "{tl:?}");
        let bl = f.image.pixel(100, 200);
        assert!((60..110).contains(&bl[1]) && bl[0] < 40, "{bl:?}");
        let f = r.frame_at(1_500).expect("seek").expect("frame");
        assert!((1_450..=1_550).contains(&f.pts_ms()), "{}", f.pts_ms());
        let mut a = AudioReader::open(&path).expect("audio").expect("has audio");
        assert_eq!((a.rate, a.channels), (48_000, 2));
        let mut total = 0usize;
        while let Some(c) = a.next_chunk().expect("chunk") {
            total += c.samples.len();
        }
        let secs = total as f64 / 96_000.0;
        assert!((1.9..2.2).contains(&secs), "{secs}");
        drop(r);
        drop(a);
        let _ = std::fs::remove_file(&path);
    }
}
