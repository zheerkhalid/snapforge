//! "Convert to MP4" and audio extraction, with Media Foundation only.
//!
//! The source reader decodes the input (anything Windows can play: WMV, AVI, MOV,
//! MKV with the store extensions, MP4...) to NV12 video and float audio; video is
//! re-encoded to H.264 and audio is resampled to 48 kHz stereo and encoded to AAC
//! (MP4 / M4A) or MP3. These functions block: call them from a worker thread.

use super::clock;
use super::dsp::{self, InputFormat, Resampler, SampleKind};
use super::mf::{self, ComGuard, Encoder, EncoderConfig};
use crate::settings::recorder::{EncoderPreference, VideoFormat, VideoQuality};
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
}

/// Output frame size of a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resize {
    #[default]
    Original,
    /// An exact size in pixels.
    Size { width: u32, height: u32 },
    /// A percentage of the source size.
    Percent(u32),
}

/// Largest output side (H.264 level 5.2 handles 8192×4320).
pub const MAX_SIDE: u32 = 8192;

impl Resize {
    /// The output size for a `w` × `h` source: rounded down to even numbers (H.264 4:2:0),
    /// at least 2 and at most [`MAX_SIDE`] per side.
    pub fn apply(self, w: u32, h: u32) -> (u32, u32) {
        let (w, h) = match self {
            Resize::Original => (w, h),
            Resize::Size { width, height } => (width, height),
            Resize::Percent(p) => {
                let p = p.clamp(1, 400) as u64;
                ((w as u64 * p / 100) as u32, (h as u64 * p / 100) as u32)
            }
        };
        (w.clamp(2, MAX_SIDE) & !1, h.clamp(2, MAX_SIDE) & !1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    /// Bits per second; `None` = derived from size, frame rate and [`Self::quality`].
    pub video_bitrate: Option<u32>,
    /// Used when `video_bitrate` is `None` (`Custom` counts as medium).
    pub quality: VideoQuality,
    pub audio_kbps: u32,
    pub encoder: EncoderPreference,
    pub resize: Resize,
    /// Output frame rate; `None` keeps the source's frames and timing.
    pub fps: Option<u32>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            video_bitrate: None,
            quality: VideoQuality::Medium,
            audio_kbps: 192,
            encoder: EncoderPreference::Auto,
            resize: Resize::Original,
            fps: None,
        }
    }
}

impl ConvertOptions {
    /// H.264 bit rate for a `w` × `h` output at `fps`.
    pub fn bitrate_for(&self, w: u32, h: u32, fps: u32) -> u32 {
        let quality = if self.quality == VideoQuality::Custom { VideoQuality::Medium } else { self.quality };
        let rates =
            crate::settings::recorder::RecorderSettings { fps: fps.clamp(1, 60), quality, ..Default::default() };
        self.video_bitrate.unwrap_or_else(|| rates.video_bitrate(w, h))
    }
}

/// Progress callback: fraction 0..=1; return `false` to cancel.
pub type Progress<'a> = &'a mut dyn FnMut(f32) -> bool;

struct VideoIn {
    w: u32,
    h: u32,
    fps: u32,
}

struct AudioIn {
    fmt: InputFormat,
    resampler: Resampler,
    /// Media time of the first decoded sample (silence is inserted before it).
    started: bool,
}

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const FIRST_AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

fn open_reader(input: &Path, video: bool) -> Result<IMFSourceReader> {
    let attrs = mf::attributes(2)?;
    // SAFETY: attribute setters on a valid store.
    unsafe {
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
    }
    let reader = mf::source_reader(input, Some(&attrs))?;
    // SAFETY: valid reader; stream selection by well-known indices.
    unsafe {
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        if video {
            let _ = reader.SetStreamSelection(FIRST_VIDEO, true);
        }
        let _ = reader.SetStreamSelection(FIRST_AUDIO, true);
    }
    Ok(reader)
}

fn setup_video(reader: &IMFSourceReader) -> Option<VideoIn> {
    // SAFETY: media type setters/getters on valid objects.
    unsafe {
        let t = MFCreateMediaType().ok()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).ok()?;
        reader.SetCurrentMediaType(FIRST_VIDEO, None, &t).ok()?;
        let cur = reader.GetCurrentMediaType(FIRST_VIDEO).ok()?;
        let (w, h) = mf::unpack(cur.GetUINT64(&MF_MT_FRAME_SIZE).ok()?);
        let (num, den) = mf::unpack(cur.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or((30u64 << 32) | 1));
        let fps = ((num as f64 / den.max(1) as f64).round() as u32).clamp(1, 240);
        (w >= 2 && h >= 2).then_some(VideoIn { w, h, fps })
    }
}

fn setup_audio(reader: &IMFSourceReader) -> Option<AudioIn> {
    // SAFETY: media type setters/getters on valid objects.
    unsafe {
        let t = MFCreateMediaType().ok()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).ok()?;
        reader.SetCurrentMediaType(FIRST_AUDIO, None, &t).ok()?;
        let cur = reader.GetCurrentMediaType(FIRST_AUDIO).ok()?;
        let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).ok()?;
        let ch = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).ok()?;
        let fmt = InputFormat { rate, channels: ch as u16, kind: SampleKind::F32 };
        Some(AudioIn { fmt, resampler: Resampler::new(rate, dsp::RATE), started: false })
    }
}

fn sample_bytes(s: &IMFSample) -> Option<Vec<u8>> {
    // SAFETY: buffers are locked/copied within their reported lengths.
    unsafe {
        let buf = s.ConvertToContiguousBuffer().ok()?;
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let n = b2.GetContiguousLength().ok()? as usize;
            let mut v = vec![0u8; n];
            if b2.ContiguousCopyTo(&mut v).is_ok() {
                return Some(v);
            }
        }
        let mut p: *mut u8 = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len)).ok()?;
        let v = if p.is_null() { Vec::new() } else { std::slice::from_raw_parts(p, len as usize).to_vec() };
        let _ = buf.Unlock();
        Some(v)
    }
}

/// Repack NV12 of any stride into a tightly packed `ow` x `oh` frame (crop to even size).
fn repack_nv12(src: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Option<Vec<u8>> {
    let rows = h as usize * 3 / 2;
    if rows == 0 || src.len() < rows {
        return None;
    }
    let stride = src.len() / (h as usize + (h as usize).div_ceil(2));
    if stride < w as usize {
        return None;
    }
    let (ow, oh) = (ow as usize, oh as usize);
    let mut out = vec![0u8; ow * oh * 3 / 2];
    for y in 0..oh {
        out[y * ow..(y + 1) * ow].copy_from_slice(&src[y * stride..y * stride + ow]);
    }
    let uv = stride * h as usize;
    for y in 0..oh / 2 {
        let s = uv + y * stride;
        if s + ow > src.len() {
            return None;
        }
        out[ow * oh + y * ow..ow * oh + (y + 1) * ow].copy_from_slice(&src[s..s + ow]);
    }
    Some(out)
}

/// Bilinear scale of one 8-bit plane with `ch` interleaved channels (tightly packed rows).
/// Pixel centres are aligned, like most video scalers; 16.16 fixed point.
fn scale_plane(src: &[u8], sw: usize, sh: usize, ch: usize, dst: &mut [u8], dw: usize, dh: usize) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < sw * sh * ch || dst.len() < dw * dh * ch {
        return;
    }
    // Source coordinate (16.16) of output index i: (i + 0.5) * s / d - 0.5, clamped.
    let coords = |d: usize, s: usize| -> Vec<(usize, usize, u32)> {
        (0..d)
            .map(|i| {
                let num = ((2 * i + 1) as i64 * s as i64 * 65536) / (2 * d as i64) - 32768;
                let pos = num.clamp(0, ((s - 1) as i64) << 16);
                let i0 = (pos >> 16) as usize;
                let i1 = (i0 + 1).min(s - 1);
                (i0, i1, (pos & 0xFFFF) as u32)
            })
            .collect()
    };
    let xs = coords(dw, sw);
    let ys = coords(dh, sh);
    for (y, &(y0, y1, fy)) in ys.iter().enumerate() {
        let r0 = &src[y0 * sw * ch..(y0 + 1) * sw * ch];
        let r1 = &src[y1 * sw * ch..(y1 + 1) * sw * ch];
        let out = &mut dst[y * dw * ch..(y + 1) * dw * ch];
        for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
            for c in 0..ch {
                let (a, b) = (r0[x0 * ch + c] as u32, r0[x1 * ch + c] as u32);
                let (p, q) = (r1[x0 * ch + c] as u32, r1[x1 * ch + c] as u32);
                let top = a * (65536 - fx) + b * fx;
                let bottom = p * (65536 - fx) + q * fx;
                let v = (top as u64 * (65536 - fy) as u64 + bottom as u64 * fy as u64 + (1u64 << 31)) >> 32;
                out[x * ch + c] = v as u8;
            }
        }
    }
}

/// Scale a tightly packed NV12 frame (even sizes) to `dw` × `dh` (even sizes).
fn scale_nv12(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![128u8; dw * dh * 3 / 2];
    if src.len() < sw * sh * 3 / 2 {
        return out;
    }
    let (sy, suv) = src.split_at(sw * sh);
    let (dy, duv) = out.split_at_mut(dw * dh);
    scale_plane(sy, sw, sh, 1, dy, dw, dh);
    scale_plane(suv, sw / 2, sh / 2, 2, duv, dw / 2, dh / 2);
    out
}

/// Output frame slots (at `fps`) that a source frame covering `[ts, ts + dur)` fills,
/// starting at `next` (the first slot not written yet). Slots in a gap before the frame are
/// filled with it too, so the output has a constant rate.
fn output_slots(ts: i64, dur: i64, fps: u32, next: u64) -> std::ops::Range<u64> {
    next..clock::frame_at_or_after(ts.saturating_add(dur.max(1)), fps).max(next)
}

/// How decoded frames become output frames: size and frame rate.
struct VideoOut {
    /// Decoded size and the even size it is cropped to before scaling.
    src: (u32, u32),
    crop: (u32, u32),
    size: (u32, u32),
    src_fps: u32,
    fps: Option<u32>,
    next: u64,
}

impl VideoOut {
    fn new(v: &VideoIn, opts: &ConvertOptions) -> Self {
        let crop = (v.w & !1, v.h & !1);
        let size = opts.resize.apply(v.w, v.h);
        let fps = opts.fps.map(|f| f.clamp(1, 120)).filter(|&f| f != v.fps);
        Self { src: (v.w, v.h), crop, size, src_fps: v.fps, fps, next: 0 }
    }

    fn out_fps(&self) -> u32 {
        self.fps.unwrap_or(self.src_fps)
    }

    fn write(&mut self, enc: &Encoder, bytes: &[u8], ts: i64, dur: Option<i64>) -> Result<()> {
        let Some(frame) = repack_nv12(bytes, self.src.0, self.src.1, self.crop.0, self.crop.1) else {
            return Ok(());
        };
        let frame = if self.size == self.crop {
            frame
        } else {
            scale_nv12(&frame, self.crop.0, self.crop.1, self.size.0, self.size.1)
        };
        let dur = dur.unwrap_or(clock::HNS_PER_SEC / self.src_fps.max(1) as i64);
        match self.fps {
            None => enc.write_video(&mf::memory_sample(&frame, ts, dur)?),
            Some(f) => {
                for k in output_slots(ts, dur, f, self.next) {
                    let t = clock::frame_time(k, f);
                    enc.write_video(&mf::memory_sample(&frame, t, clock::frame_time(k + 1, f) - t)?)?;
                    self.next = k + 1;
                }
                Ok(())
            }
        }
    }
}

fn read_loop(
    reader: &IMFSourceReader,
    enc: &Encoder,
    mut video: Option<VideoOut>,
    mut audio: Option<AudioIn>,
    duration: i64,
    progress: Progress,
) -> Result<()> {
    let mut video_done = video.is_none();
    let mut audio_done = audio.is_none();
    let mut stereo = Vec::new();
    let mut resampled = Vec::new();
    while !(video_done && audio_done) {
        let (mut idx, mut flags, mut ts) = (0u32, 0u32, 0i64);
        let mut sample: Option<IMFSample> = None;
        // SAFETY: synchronous read with valid out pointers.
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
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err(anyhow!("decoding failed"));
        }
        // SAFETY: plain query of the stream's major type.
        let is_video = unsafe { reader.GetCurrentMediaType(idx) }
            .and_then(|t| unsafe { t.GetGUID(&MF_MT_MAJOR_TYPE) })
            .map(|g| g == MFMediaType_Video)
            .unwrap_or(false);
        if let Some(s) = sample {
            if is_video {
                if let (Some(v), Some(bytes)) = (video.as_mut(), sample_bytes(&s)) {
                    // SAFETY: plain getter.
                    let dur = unsafe { s.GetSampleDuration() }.ok().filter(|&d| d > 0);
                    v.write(enc, &bytes, ts, dur)?;
                }
            } else if let Some(a) = audio.as_mut() {
                if let Some(bytes) = sample_bytes(&s) {
                    if !a.started {
                        a.started = true;
                        // Keep sync with video when the audio starts late.
                        let gap = super::clock::sample_index(ts.max(0), dsp::RATE) as usize;
                        if gap > 0 {
                            enc.write_audio(&vec![[0.0f32; 2]; gap])?;
                        }
                    }
                    stereo.clear();
                    dsp::decode_to_stereo(&a.fmt, &bytes, &mut stereo);
                    resampled.clear();
                    a.resampler.process(&stereo, &mut resampled);
                    enc.write_audio(&resampled)?;
                }
            }
            if duration > 0 && !progress((ts as f64 / duration as f64).clamp(0.0, 1.0) as f32) {
                return Err(anyhow!("cancelled"));
            }
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            if is_video {
                video_done = true;
            } else {
                audio_done = true;
            }
        }
    }
    Ok(())
}

/// Re-encode `input` to H.264/AAC MP4 at `output`.
pub fn convert_to_mp4(input: &Path, output: &Path, opts: &ConvertOptions, progress: Progress) -> Result<()> {
    let _com = ComGuard::mta();
    mf::startup()?;
    let reader = open_reader(input, true)?;
    let duration = mf::duration_of(&reader);
    let video = setup_video(&reader).ok_or_else(|| anyhow!("{} has no video", input.display()))?;
    let audio = setup_audio(&reader);
    let out = VideoOut::new(&video, opts);
    let (w, h) = out.size;
    let fps = out.out_fps();
    let bitrate = opts.bitrate_for(w, h, fps);
    log::info!("convert: {}x{} @ {} -> {w}x{h} @ {fps}, {} kbit/s", video.w, video.h, video.fps, bitrate / 1000);
    let enc = Encoder::new(&EncoderConfig {
        path: output,
        format: VideoFormat::Mp4,
        fragmented: false,
        width: w,
        height: h,
        fps,
        video_bitrate: bitrate,
        audio_kbps: opts.audio_kbps,
        video: true,
        audio: audio.is_some(),
        encoder: opts.encoder,
        keyframe_secs: 2,
        d3d: None,
    })
    .context("create the MP4 encoder")?;
    let r = read_loop(&reader, &enc, Some(out), audio, duration, progress);
    let fin = enc.finish();
    if r.is_err() {
        let _ = std::fs::remove_file(output);
    }
    r?;
    fin?;
    progress(1.0);
    Ok(())
}

/// Extract the audio track of `input` to MP3 or M4A (AAC) at `output`.
pub fn extract_audio(input: &Path, output: &Path, format: AudioFormat, kbps: u32, progress: Progress) -> Result<()> {
    let _com = ComGuard::mta();
    mf::startup()?;
    let reader = open_reader(input, false)?;
    let duration = mf::duration_of(&reader);
    let audio = setup_audio(&reader).ok_or_else(|| anyhow!("{} has no audio", input.display()))?;
    let enc = Encoder::new(&EncoderConfig {
        path: output,
        format: match format {
            AudioFormat::Mp3 => VideoFormat::Mp3AudioOnly,
            AudioFormat::M4a => VideoFormat::Mp4,
        },
        fragmented: false,
        width: 0,
        height: 0,
        fps: 30,
        video_bitrate: 0,
        audio_kbps: kbps,
        video: false,
        audio: true,
        encoder: EncoderPreference::Auto,
        keyframe_secs: 2,
        d3d: None,
    })
    .context("create the audio encoder")?;
    let r = read_loop(&reader, &enc, None, Some(audio), duration, progress);
    let fin = enc.finish();
    if r.is_err() {
        let _ = std::fs::remove_file(output);
    }
    r?;
    fin?;
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repack_crops_and_removes_stride() {
        // 3x2 frame with stride 4: Y rows then one UV row.
        let (w, h) = (3u32, 2u32);
        let stride = 4usize;
        let mut src = vec![0u8; stride * 3];
        for x in 0..stride {
            src[x] = x as u8; // Y row 0
            src[stride + x] = 10 + x as u8; // Y row 1
            src[2 * stride + x] = 100 + x as u8; // UV row
        }
        let out = repack_nv12(&src, w, h, 2, 2).expect("repack");
        assert_eq!(out, vec![0, 1, 10, 11, 100, 101]);
    }

    #[test]
    fn resize_rounds_to_even_and_clamps() {
        assert_eq!(Resize::Original.apply(1921, 1081), (1920, 1080));
        assert_eq!(Resize::Percent(50).apply(1920, 1080), (960, 540));
        assert_eq!(Resize::Percent(0).apply(1920, 1080), (18, 10));
        assert_eq!(Resize::Size { width: 641, height: 0 }.apply(1920, 1080), (640, 2));
        assert_eq!(Resize::Size { width: 99_999, height: 480 }.apply(1, 1), (MAX_SIDE, 480));
    }

    #[test]
    fn bitrate_follows_quality_unless_explicit() {
        let mut o = ConvertOptions { quality: VideoQuality::Low, ..Default::default() };
        let low = o.bitrate_for(1920, 1080, 30);
        o.quality = VideoQuality::High;
        assert!(o.bitrate_for(1920, 1080, 30) > low);
        o.video_bitrate = Some(1_234_000);
        assert_eq!(o.bitrate_for(1920, 1080, 30), 1_234_000);
    }

    #[test]
    fn scaling_keeps_flat_planes_and_sizes() {
        let (sw, sh, dw, dh) = (8u32, 6u32, 4u32, 2u32);
        let mut src = vec![77u8; (sw * sh) as usize];
        src.extend(std::iter::repeat_n([30u8, 200u8], (sw * sh / 4) as usize).flatten());
        let out = scale_nv12(&src, sw, sh, dw, dh);
        assert_eq!(out.len(), (dw * dh * 3 / 2) as usize);
        assert!(out[..(dw * dh) as usize].iter().all(|&v| v == 77));
        assert!(out[(dw * dh) as usize..].chunks(2).all(|c| c == [30, 200]));
        // Upscaling a horizontal ramp stays monotonic and within range.
        let ramp: Vec<u8> = (0..4).map(|x| x * 80).collect();
        let mut dst = vec![0u8; 8];
        scale_plane(&ramp, 4, 1, 1, &mut dst, 8, 1);
        assert!(dst.windows(2).all(|w| w[0] <= w[1]), "{dst:?}");
        assert_eq!((dst[0], dst[7]), (0, 240));
    }

    #[test]
    fn frame_rate_conversion_slots() {
        let f = |ms: i64| ms * 10_000;
        // 60 -> 30 fps: every other source frame fills a slot.
        let mut next = 0;
        let mut written = 0;
        for k in 0..60i64 {
            let r = output_slots(clock::frame_time(k as u64, 60), f(1000) / 60, 30, next);
            next = r.end.max(next);
            written += r.count();
        }
        assert_eq!(written, 30);
        // 15 -> 30 fps: each source frame fills two slots.
        assert_eq!(output_slots(0, f(1000) / 15, 30, 0), 0..2);
        // A frame after a gap also fills the skipped slots.
        assert_eq!(output_slots(f(100), f(33), 30, 1), 1..4);
        // Nothing to write when the frame ends before the next slot.
        assert_eq!(output_slots(f(10), f(5), 30, 1), 1..1);
    }
}
