//! Preview audio: plays the edited timeline's audio (decoded PCM, cuts applied, silence over
//! title cards) through WASAPI shared mode on its own thread. The engine converts our 16-bit PCM
//! to the mix format (`AUTOCONVERTPCM`).

use super::analysis::Pcm;
use super::timeline::{ItemKind, Timeline};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

/// Fill `out` (interleaved, `pcm.channels`) with output-timeline audio starting at output frame
/// `first` (frames at `pcm.rate`).
pub fn fill(tl: &Timeline, pcm: &Pcm, first: i64, out: &mut [i16]) {
    out.fill(0);
    let rate = pcm.rate as i64;
    let ch = pcm.channels.max(1) as usize;
    let n = (out.len() / ch) as i64;
    let (a, b) = (first, first + n);
    for it in &tl.items {
        let os = it.out_start as i64 * rate / 1000;
        let oe = it.out_end() as i64 * rate / 1000;
        let (lo, hi) = (a.max(os), b.min(oe));
        if lo >= hi {
            continue;
        }
        if let ItemKind::Clip { src_start, .. } = it.kind {
            let s0 = src_start as i64 * rate / 1000 + (lo - os);
            let src_lo = (s0.max(0) as usize) * ch;
            let src_hi = (((s0 + hi - lo).max(0)) as usize * ch).min(pcm.samples.len());
            if src_lo >= src_hi {
                continue;
            }
            let d0 = (lo - a) as usize * ch;
            let len = src_hi - src_lo;
            out[d0..d0 + len].copy_from_slice(&pcm.samples[src_lo..src_hi]);
        }
    }
}

/// A playing preview; stops when dropped.
pub struct AudioPlayer {
    stop: Arc<AtomicBool>,
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl AudioPlayer {
    /// Start playing the timeline from output time `from_ms`, as of `started` (the moment the
    /// video clock started; device start-up time is skipped so audio stays in sync).
    pub fn start(pcm: Arc<Pcm>, tl: Timeline, from_ms: u64, started: Instant) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let r = std::thread::Builder::new().name("video-audio-out".into()).spawn(move || {
            if let Err(e) = play(&pcm, &tl, from_ms, started, &s) {
                log::warn!("video editor: preview audio unavailable: {e:#}");
            }
        });
        if let Err(e) = r {
            log::warn!("video editor: cannot start the audio thread: {e}");
        }
        Self { stop }
    }
}

fn play(pcm: &Pcm, tl: &Timeline, from_ms: u64, started: Instant, stop: &AtomicBool) -> anyhow::Result<()> {
    // SAFETY: COM/WASAPI calls on objects owned by this thread; buffers are sized by the API.
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _com = scopeguard(hr.is_ok());
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let ch = pcm.channels.clamp(1, 8) as u16;
        let wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: ch,
            nSamplesPerSec: pcm.rate,
            nAvgBytesPerSec: pcm.rate * ch as u32 * 2,
            nBlockAlign: ch * 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1_000_000, // 100 ms
            0,
            &wfx,
            None,
        )?;
        let size = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        let rate = pcm.rate as i64;
        // Skip what the video clock already played while the device was starting.
        let skip = started.elapsed().as_millis() as i64;
        let mut pos = (from_ms as i64 + skip) * rate / 1000;
        let end = tl.duration_ms as i64 * rate / 1000;
        let mut scratch = vec![0i16; size as usize * ch as usize];
        let mut write = |frames: u32, pos: &mut i64| -> anyhow::Result<()> {
            if frames == 0 {
                return Ok(());
            }
            let p = render.GetBuffer(frames)?;
            let n = frames as usize * ch as usize;
            fill(tl, pcm, *pos, &mut scratch[..n]);
            std::ptr::copy_nonoverlapping(scratch.as_ptr() as *const u8, p, n * 2);
            render.ReleaseBuffer(frames, 0)?;
            *pos += frames as i64;
            Ok(())
        };
        write(size, &mut pos)?;
        client.Start()?;
        while !stop.load(Ordering::Relaxed) && pos < end + rate / 5 {
            std::thread::sleep(Duration::from_millis(10));
            let pad = client.GetCurrentPadding()?;
            write(size.saturating_sub(pad), &mut pos)?;
        }
        let _ = client.Stop();
        Ok(())
    }
}

struct ComGuard(bool);

fn scopeguard(init: bool) -> ComGuard {
    ComGuard(init)
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::Segment;
    use super::super::timeline::build;
    use super::*;

    #[test]
    fn fill_applies_cuts() {
        // 1 kHz mono, sample value = source frame index.
        let pcm = Pcm { rate: 1000, channels: 1, samples: (0..3000).map(|i| i as i16).collect() };
        let tl = build(&[Segment::new(0, 1000), Segment::new(2000, 3000)], &[]);
        let mut out = vec![0i16; 10];
        fill(&tl, &pcm, 995, &mut out);
        assert_eq!(out, vec![995, 996, 997, 998, 999, 2000, 2001, 2002, 2003, 2004]);
        // Past the end: silence.
        fill(&tl, &pcm, 1995, &mut out);
        assert_eq!(&out[..5], &[2995, 2996, 2997, 2998, 2999]);
        assert_eq!(&out[5..], &[0; 5]);
    }
}
