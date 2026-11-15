//! Background analysis of a source video: filmstrip thumbnails and the audio waveform (plus the
//! decoded PCM, kept for preview playback when it fits in memory). Two worker threads; results
//! arrive through a channel the UI drains each frame.

use super::compose::scale_region;
use super::mf::{self, AudioReader, VideoReader, HNS_PER_MS};
use crate::draw::RectF;
use eframe::egui::{Color32, ColorImage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Thumbnail height in pixels.
pub const THUMB_H: u32 = 72;
/// Waveform resolution: one peak per this many milliseconds.
pub const PEAK_MS: u64 = 10;
/// Keep decoded PCM for playback up to this many samples (~40 min of 48 kHz stereo).
const MAX_PCM_SAMPLES: usize = 48_000 * 2 * 60 * 40;

/// Decoded audio for preview playback.
pub struct Pcm {
    pub rate: u32,
    pub channels: u32,
    pub samples: Vec<i16>,
}

pub enum Msg {
    Thumb {
        time_ms: u64,
        image: ColorImage,
    },
    ThumbsDone,
    /// Peak amplitude (0..=255) per [`PEAK_MS`] of source time, and the PCM if kept.
    Waveform {
        peaks: Vec<u8>,
        pcm: Option<Arc<Pcm>>,
    },
    NoAudio,
    AudioProgress(f32),
    Error(String),
}

pub struct Analysis {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Analysis {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Number of thumbnails for a duration: about one per 2 s, 12..=160.
pub fn thumb_count(duration_ms: u64) -> usize {
    ((duration_ms / 2000) as usize).clamp(12, 160)
}

impl Analysis {
    pub fn start(path: PathBuf, duration_ms: u64, wake: impl Fn() + Send + Clone + 'static) -> Self {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let (p, tx, c, w) = (path.clone(), tx.clone(), cancel.clone(), wake.clone());
            let r = std::thread::Builder::new()
                .name("video-thumbs".into())
                .spawn(move || thumbs_worker(p, duration_ms, tx, c, w));
            if let Err(e) = r {
                log::error!("video editor: cannot start the thumbnail thread: {e}");
            }
        }
        {
            let c = cancel.clone();
            let r =
                std::thread::Builder::new().name("video-audio".into()).spawn(move || audio_worker(path, tx, c, wake));
            if let Err(e) = r {
                log::error!("video editor: cannot start the audio thread: {e}");
            }
        }
        Self { rx, cancel }
    }
}

fn thumb_image(img: &crate::common::Image) -> ColorImage {
    let w = ((img.width as u64 * THUMB_H as u64) / img.height.max(1) as u64).max(1) as u32;
    let t = scale_region(img, RectF::new(0.0, 0.0, img.width as f32, img.height as f32), w, THUMB_H);
    let pixels = t.data.chunks_exact(4).map(|p| Color32::from_rgba_premultiplied(p[2], p[1], p[0], p[3])).collect();
    ColorImage { size: [t.width as usize, t.height as usize], pixels }
}

fn thumbs_worker(path: PathBuf, duration_ms: u64, tx: Sender<Msg>, cancel: Arc<AtomicBool>, wake: impl Fn()) {
    let run = || -> anyhow::Result<()> {
        let _mf = mf::init()?;
        let mut r = VideoReader::open(&path, true)?;
        let n = thumb_count(duration_ms);
        let step = duration_ms.max(1) as f64 / n as f64;
        let times: Vec<u64> = (0..n).map(|i| ((i as f64 + 0.5) * step) as u64).collect();
        if step <= 1000.0 {
            // Dense: one sequential decode pass.
            let mut next = 0usize;
            while next < times.len() {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let Some(f) = r.next_frame()? else { break };
                if f.pts_hns + f.dur_hns > times[next] as i64 * HNS_PER_MS {
                    let image = thumb_image(&f.image);
                    while next < times.len() && f.pts_hns + f.dur_hns > times[next] as i64 * HNS_PER_MS {
                        let _ = tx.send(Msg::Thumb { time_ms: times[next], image: image.clone() });
                        next += 1;
                    }
                    wake();
                }
            }
        } else {
            for &t in &times {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if let Some(f) = r.frame_at(t)? {
                    let _ = tx.send(Msg::Thumb { time_ms: t, image: thumb_image(&f.image) });
                    wake();
                }
            }
        }
        Ok(())
    };
    if let Err(e) = run() {
        let _ = tx.send(Msg::Error(format!("{e:#}")));
    }
    let _ = tx.send(Msg::ThumbsDone);
    wake();
}

/// Fold interleaved PCM into peak buckets (max |sample| over all channels, 0..=255).
pub fn accumulate_peaks(peaks: &mut Vec<u8>, samples: &[i16], first_frame: u64, rate: u32, channels: u32) {
    let ch = channels.max(1) as usize;
    let per_bucket = (rate as u64 * PEAK_MS / 1000).max(1);
    for (i, frame) in samples.chunks_exact(ch).enumerate() {
        let b = ((first_frame + i as u64) / per_bucket) as usize;
        if b >= peaks.len() {
            peaks.resize(b + 1, 0);
        }
        let m = frame.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let v = (m as u32 * 255 / 32768).min(255) as u8;
        if v > peaks[b] {
            peaks[b] = v;
        }
    }
}

fn audio_worker(path: PathBuf, tx: Sender<Msg>, cancel: Arc<AtomicBool>, wake: impl Fn()) {
    let run = || -> anyhow::Result<()> {
        let _mf = mf::init()?;
        let Some(mut a) = AudioReader::open(&path)? else {
            let _ = tx.send(Msg::NoAudio);
            return Ok(());
        };
        let (rate, ch) = (a.rate, a.channels);
        let mut peaks = Vec::new();
        let mut pcm: Option<Vec<i16>> = Some(Vec::new());
        let mut frames_seen = 0u64;
        let mut last = std::time::Instant::now();
        while let Some(c) = a.next_chunk()? {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let start = (c.pts_hns.max(0) as u64 * rate as u64 / mf::HNS_PER_SEC as u64).max(frames_seen);
            accumulate_peaks(&mut peaks, &c.samples, start, rate, ch);
            if let Some(p) = pcm.as_mut() {
                // Align by timestamp (fill gaps with silence).
                let want = start as usize * ch as usize;
                if want > p.len() && want - p.len() < rate as usize * ch as usize * 10 {
                    p.resize(want, 0);
                }
                p.extend_from_slice(&c.samples);
                if p.len() > MAX_PCM_SAMPLES {
                    log::info!("video editor: audio too long to keep for preview playback");
                    pcm = None;
                }
            }
            frames_seen = start + (c.samples.len() / ch as usize) as u64;
            if last.elapsed().as_millis() > 200 && a.duration_ms > 0 {
                let ms = frames_seen * 1000 / rate as u64;
                let _ = tx.send(Msg::AudioProgress((ms as f32 / a.duration_ms as f32).min(1.0)));
                wake();
                last = std::time::Instant::now();
            }
        }
        let pcm = pcm.map(|samples| Arc::new(Pcm { rate, channels: ch, samples }));
        let _ = tx.send(Msg::Waveform { peaks, pcm });
        Ok(())
    };
    if let Err(e) = run() {
        let _ = tx.send(Msg::Error(format!("{e:#}")));
    }
    wake();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_bucket_by_time() {
        let mut p = Vec::new();
        // 1000 Hz, mono: 10 frames per 10 ms bucket.
        let mut s = vec![0i16; 30];
        s[5] = 16384;
        s[25] = -32768;
        accumulate_peaks(&mut p, &s, 0, 1000, 1);
        assert_eq!(p, vec![127, 0, 255]);
        // Stereo, starting later.
        accumulate_peaks(&mut p, &[0, 3276, 0, 0], 40, 1000, 2);
        assert_eq!(p.len(), 5);
        assert_eq!(p[4], 25);
        assert_eq!(thumb_count(1_000), 12);
        assert_eq!(thumb_count(10_000_000), 160);
    }
}
