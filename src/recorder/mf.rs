//! Media Foundation plumbing: startup, media types, the sink-writer based encoder
//! (H.264/AAC in MP4 or fragmented MP4, WMV9/WMA in ASF, MP3), and reading files back.

use crate::common::win::{pcwstr, wide};
use crate::settings::recorder::{EncoderPreference, VideoFormat};
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::sync::OnceLock;
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// COM (MTA) for the current thread; uninitialised on drop when we initialised it.
pub struct ComGuard(bool);

impl ComGuard {
    pub fn mta() -> Self {
        // SAFETY: plain COM init; balanced in Drop only when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx above on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// MFStartup once per process (never shut down: the process exits instead).
pub fn startup() -> Result<()> {
    static R: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    R.get_or_init(|| {
        // SAFETY: plain call.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string())
    })
    .clone()
    .map_err(|e| anyhow!("MFStartup failed: {e}"))
}

pub fn attributes(n: u32) -> Result<IMFAttributes> {
    let mut a: Option<IMFAttributes> = None;
    // SAFETY: valid out pointer.
    unsafe { MFCreateAttributes(&mut a, n)? };
    a.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

pub fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

pub fn video_type(subtype: &GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    // SAFETY: plain attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

/// 16-bit PCM type.
pub fn pcm_type(rate: u32, channels: u32) -> Result<IMFMediaType> {
    // SAFETY: plain attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * channels)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 2 * channels * rate)?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        Ok(t)
    }
}

fn aac_type(rate: u32, kbps: u32) -> Result<IMFMediaType> {
    // The MS AAC encoder accepts 12000, 16000, 20000 or 24000 bytes per second.
    let bytes = match kbps {
        0..=111 => 12_000,
        112..=143 => 16_000,
        144..=175 => 20_000,
        _ => 24_000,
    };
    // SAFETY: plain attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes)?;
        t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
        Ok(t)
    }
}

fn mp3_type(rate: u32, kbps: u32) -> Result<IMFMediaType> {
    let kbps = [128u32, 160, 192, 224, 256, 320].into_iter().min_by_key(|k| k.abs_diff(kbps)).unwrap_or(192);
    // SAFETY: plain attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_MP3)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1)?;
        Ok(t)
    }
}

/// Pick a WMA 9 output type close to 48 kHz stereo at `kbps` from the encoder's list.
fn wma_type(kbps: u32) -> Result<IMFMediaType> {
    // SAFETY: COM calls on valid objects; elements are media types per the API contract.
    unsafe {
        let col = MFTranscodeGetAudioOutputAvailableTypes(&MFAudioFormat_WMAudioV8, MFT_ENUM_FLAG_ALL.0 as u32, None)?;
        let n = col.GetElementCount()?;
        let mut best: Option<(i64, IMFMediaType)> = None;
        for i in 0..n {
            let Ok(t) = col.GetElement(i).and_then(|u| u.cast::<IMFMediaType>()) else { continue };
            let rate = t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0);
            let ch = t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0);
            let bps = t.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND).unwrap_or(0) as i64 * 8 / 1000;
            if ch != 2 || (rate != 48_000 && rate != 44_100) {
                continue;
            }
            let score = (bps - kbps as i64).abs() + if rate == 48_000 { 0 } else { 1000 };
            if best.as_ref().map_or(true, |(s, _)| score < *s) {
                best = Some((score, t));
            }
        }
        best.map(|(_, t)| t).ok_or_else(|| anyhow!("no WMA output type for 48 kHz stereo"))
    }
}

/// Sample rate the WMA type chose (the mixer always runs at 48 kHz; WMA may need 44.1).
fn type_rate(t: &IMFMediaType) -> u32 {
    // SAFETY: plain getter.
    unsafe { t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000) }
}

pub struct EncoderConfig<'a> {
    pub path: &'a Path,
    pub format: VideoFormat,
    /// Fragmented MP4 (crash-safe) instead of a regular MP4.
    pub fragmented: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_bitrate: u32,
    pub audio_kbps: u32,
    /// Add a video stream (ignored for MP3).
    pub video: bool,
    pub audio: bool,
    pub encoder: EncoderPreference,
    pub keyframe_secs: u32,
    /// GPU input: samples carry DXGI surfaces of this device.
    pub d3d: Option<&'a IMFDXGIDeviceManager>,
}

/// A sink writer with an optional video stream and an optional audio stream.
///
/// Video and audio may be written from two different threads: with throttling on, the
/// sink writer blocks `WriteSample` on a stream that runs ahead until the other stream
/// catches up, so each stream needs its own feeding thread.
pub struct Encoder {
    writer: IMFSinkWriter,
    video: Option<u32>,
    audio: Option<u32>,
    audio_rate: u32,
    audio_state: std::sync::Mutex<AudioState>,
    pub hardware: bool,
}

struct AudioState {
    /// Samples (per channel) written to the audio stream so far.
    pos: i64,
    /// Rate conversion when the encoder needs 44.1 kHz (some WMA profiles).
    resampler: Option<super::dsp::Resampler>,
}

// SAFETY: Media Foundation objects are free-threaded and the sink writer serialises
// calls internally; the only mutable Rust state is behind a Mutex.
unsafe impl Send for Encoder {}
// SAFETY: see above.
unsafe impl Sync for Encoder {}

impl Encoder {
    pub fn new(cfg: &EncoderConfig) -> Result<Self> {
        startup()?;
        let attrs = attributes(4)?;
        let container = match cfg.format {
            VideoFormat::Mp4 if cfg.fragmented => MFTranscodeContainerType_FMPEG4,
            VideoFormat::Mp4 => MFTranscodeContainerType_MPEG4,
            VideoFormat::Wmv => MFTranscodeContainerType_ASF,
            VideoFormat::Mp3AudioOnly => MFTranscodeContainerType_MP3,
        };
        let want_hw = cfg.encoder != EncoderPreference::Software;
        // SAFETY: attribute setters on a valid store; the sink writer copies what it needs.
        unsafe {
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &container)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, want_hw as u32)?;
            if let Some(dm) = cfg.d3d {
                attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, dm)?;
            }
        }
        let url = wide(&cfg.path.to_string_lossy());
        // SAFETY: valid NUL-terminated path and attributes.
        let writer = unsafe { MFCreateSinkWriterFromURL(pcwstr(&url), None, &attrs) }
            .with_context(|| format!("create sink writer for {}", cfg.path.display()))?;

        let mut video = None;
        if cfg.video && cfg.format != VideoFormat::Mp3AudioOnly {
            let sub = if cfg.format == VideoFormat::Wmv { MFVideoFormat_WMV3 } else { MFVideoFormat_H264 };
            let out = video_type(&sub, cfg.width, cfg.height, cfg.fps)?;
            let inp = video_type(&MFVideoFormat_NV12, cfg.width, cfg.height, cfg.fps)?;
            // SAFETY: attribute setters and sink-writer calls with valid objects.
            unsafe {
                out.SetUINT32(&MF_MT_AVG_BITRATE, cfg.video_bitrate)?;
                if cfg.format == VideoFormat::Mp4 {
                    out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
                }
                for t in [&out, &inp] {
                    t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
                    t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
                }
                if cfg.d3d.is_none() {
                    inp.SetUINT32(&MF_MT_DEFAULT_STRIDE, cfg.width)?;
                }
                let idx = writer.AddStream(&out).context("add video stream")?;
                let params = attributes(2)?;
                let gop = (cfg.fps * cfg.keyframe_secs.max(1)).max(1);
                params.SetUINT32(&CODECAPI_AVEncMPVGOPSize, gop)?;
                // No B-frames: decode order = presentation order, which the fragmented
                // MP4 sink needs for correct fragment start times (and lower latency).
                params.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;
                writer.SetInputMediaType(idx, &inp, &params).context("video input type (NV12)")?;
                video = Some(idx);
            }
        }

        let mut audio = None;
        let mut audio_rate = super::dsp::RATE;
        if cfg.audio {
            let out = match cfg.format {
                VideoFormat::Mp4 => aac_type(super::dsp::RATE, cfg.audio_kbps)?,
                VideoFormat::Wmv => wma_type(cfg.audio_kbps)?,
                VideoFormat::Mp3AudioOnly => mp3_type(super::dsp::RATE, cfg.audio_kbps)?,
            };
            audio_rate = type_rate(&out);
            let inp = pcm_type(audio_rate, 2)?;
            // SAFETY: sink-writer calls with valid media types.
            unsafe {
                let idx = writer.AddStream(&out).context("add audio stream")?;
                writer.SetInputMediaType(idx, &inp, None).context("audio input type (PCM)")?;
                audio = Some(idx);
            }
        }
        // SAFETY: valid writer with all streams configured.
        unsafe { writer.BeginWriting().context("begin writing")? };
        let hardware = want_hw && video.is_some() && encoder_is_hardware(&writer, video);
        let resampler =
            (audio_rate != super::dsp::RATE).then(|| super::dsp::Resampler::new(super::dsp::RATE, audio_rate));
        let audio_state = std::sync::Mutex::new(AudioState { pos: 0, resampler });
        Ok(Self { writer, video, audio, audio_rate, audio_state, hardware })
    }

    pub fn write_video(&self, sample: &IMFSample) -> Result<()> {
        let Some(idx) = self.video else { return Ok(()) };
        // SAFETY: valid writer and sample.
        unsafe { self.writer.WriteSample(idx, sample).context("write video sample") }
    }

    /// Append 48 kHz stereo frames; timestamps follow from the running sample count, so
    /// the audio stream is gap-free (silence is mixed in upstream).
    pub fn write_audio(&self, frames: &[super::dsp::Frame]) -> Result<()> {
        let Some(idx) = self.audio else { return Ok(()) };
        if frames.is_empty() {
            return Ok(());
        }
        let mut st = self.audio_state.lock().map_err(|_| anyhow!("audio state poisoned"))?;
        let pcm = match st.resampler.as_mut() {
            Some(r) => {
                let mut o = Vec::with_capacity(frames.len());
                r.process(frames, &mut o);
                super::dsp::to_i16(&o)
            }
            None => super::dsp::to_i16(frames),
        };
        let n = (pcm.len() / 4) as i64;
        if n == 0 {
            return Ok(());
        }
        let t0 = super::clock::sample_time(st.pos, self.audio_rate);
        let t1 = super::clock::sample_time(st.pos + n, self.audio_rate);
        let sample = memory_sample(&pcm, t0, t1 - t0)?;
        st.pos += n;
        drop(st);
        // SAFETY: valid writer and sample.
        unsafe { self.writer.WriteSample(idx, &sample).context("write audio sample") }
    }

    /// Media time covered by the audio written so far.
    pub fn finish(self) -> Result<()> {
        // SAFETY: valid writer; finalizes and closes the file.
        unsafe { self.writer.Finalize().context("finalize") }
    }
}

/// Best-effort: was a hardware MFT chosen for the video stream?
fn encoder_is_hardware(writer: &IMFSinkWriter, video: Option<u32>) -> bool {
    let Some(idx) = video else { return false };
    let mut p: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: GetServiceForStream returns an AddRef'd IMFTransform in `p` on success.
    unsafe {
        if writer.GetServiceForStream(idx, &GUID::zeroed(), &IMFTransform::IID, &mut p).is_err() || p.is_null() {
            return false;
        }
        let t = IMFTransform::from_raw(p);
        let Ok(a) = t.GetAttributes() else { return false };
        // MFT_ENUM_HARDWARE_URL_Attribute is only set on hardware MFTs.
        a.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute).is_ok()
    }
}

/// A sample backed by system memory holding a copy of `data`.
pub fn memory_sample(data: &[u8], time: i64, duration: i64) -> Result<IMFSample> {
    // SAFETY: the buffer is locked while we copy exactly `data.len()` <= max length bytes.
    unsafe {
        let buf = MFCreateMemoryBuffer(data.len() as u32)?;
        let mut p: *mut u8 = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        buf.Unlock()?;
        buf.SetCurrentLength(data.len() as u32)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        s.SetSampleTime(time)?;
        s.SetSampleDuration(duration)?;
        Ok(s)
    }
}

// ------------------------------------------------------------------ reading back

#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub audio: bool,
    pub subtype: GUID,
    pub samples: u64,
    pub first_ts: i64,
    /// End of the last sample (timestamp + duration).
    pub end_ts: i64,
    pub width: u32,
    pub height: u32,
    pub rate: u32,
}

/// What [`probe`] reads back (the recorder's hardware tests check timing with it).
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    pub duration: i64,
    pub streams: Vec<StreamInfo>,
}

#[cfg(test)]
impl MediaInfo {
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| !s.audio)
    }
    pub fn audio(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.audio)
    }
}

pub fn source_reader(path: &Path, attrs: Option<&IMFAttributes>) -> Result<IMFSourceReader> {
    startup()?;
    let url = wide(&path.to_string_lossy());
    // SAFETY: valid NUL-terminated path.
    unsafe { MFCreateSourceReaderFromURL(pcwstr(&url), attrs) }.with_context(|| format!("open {}", path.display()))
}

pub fn duration_of(reader: &IMFSourceReader) -> i64 {
    // SAFETY: valid reader; the PROPVARIANT holds a VT_UI8 duration.
    unsafe {
        reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| u64::try_from(&v).ok())
            .unwrap_or(0) as i64
    }
}

/// Stream indices and native (compressed) types of a reader, selecting audio/video only.
pub fn native_streams(reader: &IMFSourceReader) -> Vec<(u32, IMFMediaType, bool)> {
    let mut v = Vec::new();
    for i in 0..16u32 {
        // SAFETY: valid reader; errors end the enumeration.
        let Ok(t) = (unsafe { reader.GetNativeMediaType(i, 0) }) else { break };
        // SAFETY: plain getter.
        let major = unsafe { t.GetGUID(&MF_MT_MAJOR_TYPE) }.unwrap_or_default();
        let audio = major == MFMediaType_Audio;
        let selected = audio || major == MFMediaType_Video;
        // SAFETY: valid reader and stream index.
        unsafe {
            let _ = reader.SetStreamSelection(i, selected);
        }
        if selected {
            v.push((i, t, audio));
        }
    }
    v
}

/// Read every compressed sample of `path` and report per-stream timing.
#[cfg(test)]
pub fn probe(path: &Path) -> Result<MediaInfo> {
    let reader = source_reader(path, None)?;
    let duration = duration_of(&reader);
    let streams = native_streams(&reader);
    let mut infos: Vec<StreamInfo> = streams
        .iter()
        .map(|(_, t, audio)| {
            // SAFETY: plain getters.
            unsafe {
                let (w, h) = unpack(t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0));
                StreamInfo {
                    audio: *audio,
                    subtype: t.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default(),
                    first_ts: i64::MAX,
                    width: w,
                    height: h,
                    rate: t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0),
                    ..Default::default()
                }
            }
        })
        .collect();
    loop {
        let (mut idx, mut flags, mut ts) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: valid out pointers.
        unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut idx),
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )?;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 && sample.is_none() {
            // One stream ended; keep reading until all have.
            if let Some(pos) = streams.iter().position(|(i, _, _)| *i == idx) {
                // SAFETY: deselect the finished stream.
                unsafe {
                    let _ = reader.SetStreamSelection(idx, false);
                }
                let _ = pos;
            }
            if streams.iter().all(|(i, _, _)| {
                // SAFETY: plain query.
                !unsafe { reader.GetStreamSelection(*i) }.map(|b| b.as_bool()).unwrap_or(false)
            }) {
                break;
            }
            continue;
        }
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(anyhow!("read error"));
        }
        let Some(s) = sample else { continue };
        let Some(pos) = streams.iter().position(|(i, _, _)| *i == idx) else { continue };
        // SAFETY: plain getter.
        let dur = unsafe { s.GetSampleDuration() }.unwrap_or(0);
        let inf = &mut infos[pos];
        inf.samples += 1;
        inf.first_ts = inf.first_ts.min(ts);
        inf.end_ts = inf.end_ts.max(ts + dur);
    }
    for i in &mut infos {
        if i.first_ts == i64::MAX {
            i.first_ts = 0;
        }
    }
    Ok(MediaInfo { duration, streams: infos })
}

/// Copy the compressed streams of `src` into a regular MP4 at `dst` without re-encoding.
///
/// Sample times are rebuilt as the running sum of sample durations per stream: the MF
/// fragmented-MP4 reader derives fragment start times itself (the MF fMP4 sink writes no
/// `tfdt`) and gets them wrong by a few frames when audio and video fragments differ in
/// length. Our recordings are gapless by construction, so the durations are exact.
pub fn remux_to_mp4(src: &Path, dst: &Path) -> Result<()> {
    let reader = source_reader(src, None)?;
    let streams = native_streams(&reader);
    if streams.is_empty() {
        return Err(anyhow!("no audio or video in {}", src.display()));
    }
    let attrs = attributes(1)?;
    // SAFETY: attribute setter on a valid store.
    unsafe { attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)? };
    let url = wide(&dst.to_string_lossy());
    // SAFETY: valid path and attributes.
    let writer = unsafe { MFCreateSinkWriterFromURL(pcwstr(&url), None, &attrs)? };
    let mut map: Vec<(u32, u32, Option<i64>)> = Vec::new();
    for (i, t, _) in &streams {
        // SAFETY: passthrough stream (no input type => samples are written as they are).
        let out = unsafe { writer.AddStream(t)? };
        map.push((*i, out, None));
    }
    // SAFETY: valid writer.
    unsafe { writer.BeginWriting()? };
    let mut written = 0u64;
    let mut active: Vec<u32> = streams.iter().map(|s| s.0).collect();
    while !active.is_empty() {
        let (mut idx, mut flags, mut ts) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: valid out pointers.
        let r = unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut idx),
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
        };
        if let Err(e) = r {
            // A truncated tail: keep what was read so far.
            log::warn!("recorder: remux read stopped: {e}");
            break;
        }
        if let Some(s) = sample {
            if let Some((_, out, next)) = map.iter_mut().find(|(i, _, _)| *i == idx) {
                // SAFETY: valid writer and sample from the matching stream.
                unsafe {
                    let dur = s.GetSampleDuration().unwrap_or(0);
                    let t = next.unwrap_or(ts);
                    if dur > 0 {
                        s.SetSampleTime(t)?;
                        *next = Some(t + dur);
                    }
                    writer.WriteSample(*out, &s)?;
                }
                written += 1;
            }
        }
        if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 | MF_SOURCE_READERF_ERROR.0) as u32 != 0 {
            active.retain(|&i| i != idx);
            // SAFETY: deselect the finished stream.
            unsafe {
                let _ = reader.SetStreamSelection(idx, false);
            }
        }
    }
    if written == 0 {
        return Err(anyhow!("nothing to remux in {}", src.display()));
    }
    // SAFETY: valid writer.
    unsafe { writer.Finalize()? };
    Ok(())
}

#[cfg(test)]
pub fn subtype_name(g: &GUID) -> &'static str {
    match *g {
        x if x == MFVideoFormat_H264 => "H.264",
        x if x == MFVideoFormat_WMV3 => "WMV9",
        x if x == MFAudioFormat_AAC => "AAC",
        x if x == MFAudioFormat_MP3 => "MP3",
        x if x == MFAudioFormat_WMAudioV8 => "WMA",
        x if x == MFAudioFormat_PCM => "PCM",
        x if x == MFAudioFormat_Float => "Float",
        x if x == MFVideoFormat_NV12 => "NV12",
        _ => "other",
    }
}
