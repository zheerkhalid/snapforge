//! "Convert to MP4" and audio extraction, with Media Foundation only.
//!
//! The source reader decodes the input (anything Windows can play: WMV, AVI, MOV,
//! MKV with the store extensions, MP4...) to NV12 video and float audio; video is
//! re-encoded to H.264 and audio is resampled to 48 kHz stereo and encoded to AAC
//! (MP4 / M4A) or MP3. These functions block: call them from a worker thread.

use super::dsp::{self, InputFormat, Resampler, SampleKind};
use super::mf::{self, ComGuard, Encoder, EncoderConfig};
use crate::settings::recorder::{EncoderPreference, VideoFormat};
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
}

#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    /// Bits per second; `None` = derived from size and frame rate (medium quality).
    pub video_bitrate: Option<u32>,
    pub audio_kbps: u32,
    pub encoder: EncoderPreference,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self { video_bitrate: None, audio_kbps: 192, encoder: EncoderPreference::Auto }
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

fn read_loop(
    reader: &IMFSourceReader,
    enc: &Encoder,
    video: Option<&VideoIn>,
    mut audio: Option<AudioIn>,
    duration: i64,
    progress: Progress,
) -> Result<()> {
    let (ow, oh) = video.map(|v| (v.w & !1, v.h & !1)).unwrap_or((0, 0));
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
                if let (Some(v), Some(bytes)) = (video, sample_bytes(&s)) {
                    if let Some(frame) = repack_nv12(&bytes, v.w, v.h, ow, oh) {
                        // SAFETY: plain getter.
                        let dur = unsafe { s.GetSampleDuration() }.unwrap_or(super::clock::HNS_PER_SEC / v.fps as i64);
                        let out = mf::memory_sample(&frame, ts, dur)?;
                        enc.write_video(&out)?;
                    }
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
    let (w, h) = (video.w & !1, video.h & !1);
    let defaults = crate::settings::recorder::RecorderSettings { fps: video.fps.min(60), ..Default::default() };
    let bitrate = opts.video_bitrate.unwrap_or_else(|| defaults.video_bitrate(w, h));
    let enc = Encoder::new(&EncoderConfig {
        path: output,
        format: VideoFormat::Mp4,
        fragmented: false,
        width: w,
        height: h,
        fps: video.fps,
        video_bitrate: bitrate,
        audio_kbps: opts.audio_kbps,
        video: true,
        audio: audio.is_some(),
        encoder: opts.encoder,
        keyframe_secs: 2,
        d3d: None,
    })
    .context("create the MP4 encoder")?;
    let r = read_loop(&reader, &enc, Some(&video), audio, duration, progress);
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
}
