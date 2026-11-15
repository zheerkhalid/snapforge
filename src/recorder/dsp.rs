//! Audio processing: sample-format decoding, channel mapping and resampling to 48 kHz
//! stereo float, timeline placement of capture packets (silence gaps, drift), mixing
//! with gain / AGC, a soft limiter and the level meter.

use std::collections::VecDeque;

pub const RATE: u32 = 48_000;
pub type Frame = [f32; 2];

// ------------------------------------------------------------------ input decoding

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    I16,
    /// 24-bit packed in 3 bytes.
    I24,
    /// 24-bit in a 32-bit container, or 32-bit integer.
    I32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputFormat {
    pub rate: u32,
    pub channels: u16,
    pub kind: SampleKind,
}

impl InputFormat {
    pub fn bytes_per_frame(&self) -> usize {
        let b = match self.kind {
            SampleKind::F32 | SampleKind::I32 => 4,
            SampleKind::I16 => 2,
            SampleKind::I24 => 3,
        };
        b * self.channels as usize
    }
}

fn read_sample(kind: SampleKind, b: &[u8]) -> f32 {
    match kind {
        SampleKind::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        SampleKind::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        SampleKind::I24 => {
            let v = ((b[2] as i32) << 24 | (b[1] as i32) << 16 | (b[0] as i32) << 8) >> 8;
            v as f32 / 8_388_608.0
        }
        SampleKind::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
    }
}

/// Decode interleaved device bytes into stereo frames (mono is duplicated; for more
/// than two channels the centre is folded into left and right, the rest is dropped).
pub fn decode_to_stereo(fmt: &InputFormat, bytes: &[u8], out: &mut Vec<Frame>) {
    let ch = fmt.channels.max(1) as usize;
    let bpf = fmt.bytes_per_frame();
    let bps = bpf / ch;
    if bpf == 0 {
        return;
    }
    out.reserve(bytes.len() / bpf);
    for f in bytes.chunks_exact(bpf) {
        let s = |i: usize| read_sample(fmt.kind, &f[i * bps..(i + 1) * bps]);
        let fr = match ch {
            1 => {
                let v = s(0);
                [v, v]
            }
            2 => [s(0), s(1)],
            _ => {
                let c = s(2) * std::f32::consts::FRAC_1_SQRT_2;
                [s(0) + c, s(1) + c]
            }
        };
        out.push(fr);
    }
}

// ------------------------------------------------------------------ resampler

/// Streaming Catmull-Rom (cubic) resampler for stereo frames.
pub struct Resampler {
    step: f64,
    pos: f64,
    hist: Vec<Frame>,
    passthrough: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let mut r = Self {
            step: in_rate.max(1) as f64 / out_rate.max(1) as f64,
            pos: 1.0,
            hist: Vec::new(),
            passthrough: false,
        };
        r.passthrough = in_rate == out_rate;
        // One frame of left context so the first output can interpolate.
        r.hist.push([0.0; 2]);
        r
    }

    pub fn process(&mut self, input: &[Frame], out: &mut Vec<Frame>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.hist.extend_from_slice(input);
        let n = self.hist.len();
        // Needs frames i-1, i, i+1, i+2.
        while self.pos + 2.0 < n as f64 {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            let p0 = self.hist[i.saturating_sub(1)];
            let p1 = self.hist[i];
            let p2 = self.hist[i + 1];
            let p3 = self.hist[i + 2];
            let mut o = [0.0f32; 2];
            for c in 0..2 {
                o[c] = catmull_rom(p0[c], p1[c], p2[c], p3[c], t);
            }
            out.push(o);
            self.pos += self.step;
        }
        // Drop consumed frames, keeping one frame of left context.
        let keep_from = (self.pos.floor() as usize).saturating_sub(1).min(n);
        if keep_from > 0 {
            self.hist.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
}

// ------------------------------------------------------------------ timeline track

/// Jitter tolerated before a packet's timestamp is trusted over the running position:
/// smaller deviations are ignored (clock jitter), larger ones insert silence (loopback
/// delivers nothing while the system is silent) or drop samples (device clock drift).
pub const SNAP_TOLERANCE: i64 = (RATE / 1000 * 15) as i64; // 15 ms

/// One input on the 48 kHz timeline. `buf[0]` sits at sample index `base`.
#[derive(Default)]
pub struct Track {
    buf: VecDeque<Frame>,
    base: i64,
    started: bool,
    /// Samples inserted (silence) and dropped to follow timestamps; for diagnostics.
    pub padded: u64,
    pub dropped: u64,
}

impl Track {
    pub fn new() -> Self {
        Self::default()
    }

    fn end(&self) -> i64 {
        self.base + self.buf.len() as i64
    }

    /// Place `frames` whose first sample belongs at timeline index `at`.
    /// `floor` is the first index the mixer has not emitted yet; older samples are dropped.
    pub fn push_at(&mut self, at: i64, frames: &[Frame], floor: i64) {
        if frames.is_empty() {
            return;
        }
        if !self.started || self.buf.is_empty() && self.base < floor {
            self.started = true;
            self.buf.clear();
            self.base = at;
        }
        let end = self.end();
        let mut frames = frames;
        let mut at = at;
        if at > end + SNAP_TOLERANCE {
            let gap = (at - end) as usize;
            self.padded += gap as u64;
            self.buf.extend(std::iter::repeat([0.0; 2]).take(gap));
        } else if at < end - SNAP_TOLERANCE {
            let overlap = ((end - at) as usize).min(frames.len());
            self.dropped += overlap as u64;
            frames = &frames[overlap..];
            at += overlap as i64;
            let _ = at;
        }
        self.buf.extend(frames.iter().copied());
        // Anything already emitted by the mixer is useless.
        if self.base < floor {
            let n = ((floor - self.base) as usize).min(self.buf.len());
            self.buf.drain(..n);
            self.base += n as i64;
        }
    }

    /// Add `[start, start + out.len())` of this track (times `gain`) into `out`.
    /// Missing samples count as silence. Consumes everything before the end of the range.
    pub fn mix_into(&mut self, start: i64, out: &mut [Frame], gain: f32) {
        let end = start + out.len() as i64;
        for (i, o) in out.iter_mut().enumerate() {
            let idx = start + i as i64 - self.base;
            if idx >= 0 {
                if let Some(f) = self.buf.get(idx as usize) {
                    o[0] += f[0] * gain;
                    o[1] += f[1] * gain;
                }
            }
        }
        if self.base < end {
            let n = ((end - self.base) as usize).min(self.buf.len());
            self.buf.drain(..n);
            self.base += n as i64;
            if self.buf.is_empty() {
                self.base = self.base.max(end);
            }
        }
    }

    #[cfg(test)]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

// ------------------------------------------------------------------ AGC and limiter

/// Slow automatic gain control that only boosts (up to `max_gain`) quiet speech.
pub struct Agc {
    gain: f32,
    target_rms: f32,
    max_gain: f32,
    noise_floor: f32,
}

impl Default for Agc {
    fn default() -> Self {
        Self { gain: 1.0, target_rms: 0.1, max_gain: 8.0, noise_floor: 0.004 }
    }
}

impl Agc {
    #[cfg(test)]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Apply in place; the gain ramps linearly across the block (no zipper noise).
    pub fn process(&mut self, block: &mut [Frame]) {
        if block.is_empty() {
            return;
        }
        let rms = rms(block);
        let start = self.gain;
        if rms > self.noise_floor {
            let desired = (self.target_rms / rms).clamp(1.0, self.max_gain);
            // Fast attack (reduce gain quickly), slow release (raise slowly).
            if rms * self.gain > 0.5 {
                // Suddenly loud: drop at once instead of driving the limiter.
                self.gain = desired;
            } else {
                let k = if desired < self.gain { 0.5 } else { 0.05 };
                self.gain += (desired - self.gain) * k;
            }
        }
        let n = block.len() as f32;
        for (i, f) in block.iter_mut().enumerate() {
            let g = start + (self.gain - start) * (i as f32 / n);
            f[0] *= g;
            f[1] *= g;
        }
    }
}

/// Soft knee above 0.9 full scale, never exceeds 1.0.
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.9 {
        x
    } else {
        let y = 0.9 + 0.1 * ((a - 0.9) / 0.1).tanh();
        y.copysign(x)
    }
}

pub fn rms(block: &[Frame]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let s: f64 = block.iter().map(|f| (f[0] as f64 * f[0] as f64 + f[1] as f64 * f[1] as f64) * 0.5).sum();
    (s / block.len() as f64).sqrt() as f32
}

pub fn peak(block: &[Frame]) -> f32 {
    block.iter().fold(0.0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()))
}

// ------------------------------------------------------------------ mixer

pub struct Input {
    pub track: Track,
    pub gain: f32,
    pub agc: Option<Agc>,
}

/// Mixes the inputs on the shared timeline. `emitted` is the next index to produce.
pub struct Mixer {
    pub inputs: Vec<Input>,
    pub emitted: i64,
}

impl Mixer {
    pub fn new(n_inputs: usize) -> Self {
        let inputs = (0..n_inputs).map(|_| Input { track: Track::new(), gain: 1.0, agc: None }).collect();
        Self { inputs, emitted: 0 }
    }

    /// Produce samples up to (excluding) timeline index `target`.
    pub fn mix_until(&mut self, target: i64) -> Vec<Frame> {
        let n = (target - self.emitted).max(0) as usize;
        let mut out = vec![[0.0f32; 2]; n];
        if n == 0 {
            return out;
        }
        let mut tmp = vec![[0.0f32; 2]; n];
        for inp in &mut self.inputs {
            tmp.iter_mut().for_each(|f| *f = [0.0; 2]);
            inp.track.mix_into(self.emitted, &mut tmp, inp.gain);
            if let Some(agc) = inp.agc.as_mut() {
                agc.process(&mut tmp);
            }
            for (o, t) in out.iter_mut().zip(&tmp) {
                o[0] += t[0];
                o[1] += t[1];
            }
        }
        for f in &mut out {
            f[0] = soft_limit(f[0]);
            f[1] = soft_limit(f[1]);
        }
        self.emitted = target.max(self.emitted);
        out
    }
}

pub fn to_i16(frames: &[Frame]) -> Vec<u8> {
    let mut v = Vec::with_capacity(frames.len() * 4);
    for f in frames {
        for c in f {
            let s = (c.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            v.extend_from_slice(&s.to_le_bytes());
        }
    }
    v
}

// ------------------------------------------------------------------ level meter

/// Peak / RMS of the last mixed block, as atomics for the control bar.
pub struct Level {
    peak: std::sync::atomic::AtomicU32,
    rms: std::sync::atomic::AtomicU32,
}

impl Level {
    pub const fn new() -> Self {
        Self { peak: std::sync::atomic::AtomicU32::new(0), rms: std::sync::atomic::AtomicU32::new(0) }
    }

    pub fn store(&self, block: &[Frame]) {
        use std::sync::atomic::Ordering::Relaxed;
        self.peak.store(peak(block).to_bits(), Relaxed);
        self.rms.store(rms(block).to_bits(), Relaxed);
    }

    pub fn reset(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.peak.store(0, Relaxed);
        self.rms.store(0, Relaxed);
    }

    /// (peak, rms), linear 0..=1.
    pub fn get(&self) -> (f32, f32) {
        use std::sync::atomic::Ordering::Relaxed;
        (f32::from_bits(self.peak.load(Relaxed)), f32::from_bits(self.rms.load(Relaxed)))
    }
}

/// Map a linear level to a 0..=1 meter position over a 60 dB range.
pub fn meter_position(linear: f32) -> f32 {
    if linear <= 1e-6 {
        return 0.0;
    }
    let db = 20.0 * linear.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f32, n: usize, amp: f32) -> Vec<Frame> {
        (0..n)
            .map(|i| {
                let v = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn decode_formats() {
        let fmt = InputFormat { rate: 48_000, channels: 1, kind: SampleKind::I16 };
        let mut out = Vec::new();
        decode_to_stereo(&fmt, &[0x00, 0x40, 0x00, 0xC0], &mut out);
        assert_eq!(out, vec![[0.5, 0.5], [-0.5, -0.5]]);
        let fmt = InputFormat { rate: 48_000, channels: 2, kind: SampleKind::I24 };
        out.clear();
        decode_to_stereo(&fmt, &[0, 0, 0x40, 0, 0, 0xC0], &mut out);
        assert_eq!(out, vec![[0.5, -0.5]]);
        let fmt = InputFormat { rate: 48_000, channels: 2, kind: SampleKind::F32 };
        out.clear();
        let mut b = Vec::new();
        b.extend_from_slice(&0.25f32.to_le_bytes());
        b.extend_from_slice(&(-1.0f32).to_le_bytes());
        decode_to_stereo(&fmt, &b, &mut out);
        assert_eq!(out, vec![[0.25, -1.0]]);
    }

    #[test]
    fn resampler_rate_and_quality() {
        // 44.1 kHz -> 48 kHz, fed in uneven chunks: output count and frequency preserved.
        let input = sine(44_100, 1000.0, 44_100, 0.5);
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(441 + 7) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 3, "len {}", out.len());
        // Compare against the ideal 1 kHz sine at 48 kHz (skip the first few samples).
        let ideal = sine(48_000, 1000.0, out.len(), 0.5);
        let err: f32 = out[10..].iter().zip(&ideal[10..]).map(|(a, b)| (a[0] - b[0]).abs()).fold(0.0, f32::max);
        assert!(err < 0.01, "max error {err}");
        // Passthrough at equal rates.
        let mut p = Resampler::new(48_000, 48_000);
        let mut o2 = Vec::new();
        p.process(&input[..100], &mut o2);
        assert_eq!(o2, input[..100].to_vec());
    }

    #[test]
    fn resampler_downsample() {
        let input = sine(96_000, 500.0, 96_000, 0.5);
        let mut r = Resampler::new(96_000, 48_000);
        let mut out = Vec::new();
        for chunk in input.chunks(960) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 3);
    }

    #[test]
    fn track_gap_is_filled_with_silence() {
        let mut t = Track::new();
        t.push_at(0, &[[1.0; 2]; 480], 0);
        // Loopback was silent for 100 ms: the next packet is far ahead.
        t.push_at(480 + 4800, &[[1.0; 2]; 480], 0);
        assert_eq!(t.padded, 4800);
        let mut out = vec![[0.0f32; 2]; 480 + 4800 + 480];
        t.mix_into(0, &mut out, 1.0);
        assert_eq!(out[0], [1.0; 2]);
        assert_eq!(out[480 + 100], [0.0; 2]);
        assert_eq!(out[480 + 4800 + 10], [1.0; 2]);
    }

    #[test]
    fn track_jitter_ignored_and_overlap_dropped() {
        let mut t = Track::new();
        t.push_at(1000, &[[0.5; 2]; 480], 0);
        // 2 ms of jitter: appended contiguously.
        t.push_at(1480 + 96, &[[0.5; 2]; 480], 0);
        assert_eq!((t.padded, t.dropped), (0, 0));
        assert_eq!(t.buffered(), 960);
        // 50 ms early (clock drift): the overlapping part is dropped.
        t.push_at(1960 - 2400, &[[0.5; 2]; 4800], 0);
        assert_eq!(t.dropped, 2400);
        assert_eq!(t.buffered(), 960 + 2400);
    }

    #[test]
    fn track_late_samples_before_floor_are_discarded() {
        let mut t = Track::new();
        t.push_at(0, &[[0.5; 2]; 100], 50);
        assert_eq!(t.buffered(), 50);
    }

    #[test]
    fn mixer_sums_and_limits() {
        let mut m = Mixer::new(2);
        m.inputs[0].track.push_at(0, &[[0.4; 2]; 100], 0);
        m.inputs[1].track.push_at(50, &[[0.4; 2]; 100], 0);
        m.inputs[1].gain = 2.0;
        let out = m.mix_until(200);
        assert_eq!(out.len(), 200);
        assert!((out[10][0] - 0.4).abs() < 1e-6);
        // 0.4 + 0.8 = 1.2 is limited below 1.0.
        assert!(out[60][0] < 1.0 && out[60][0] > 0.9);
        assert!((out[120][0] - 0.8).abs() < 1e-6);
        assert_eq!(out[190], [0.0; 2]);
        assert_eq!(m.emitted, 200);
        // Nothing more to emit for an old target.
        assert!(m.mix_until(150).is_empty());
    }

    #[test]
    fn agc_boosts_quiet_speech_and_ignores_silence() {
        let mut agc = Agc::default();
        let mut silence = vec![[0.0005f32; 2]; 960];
        agc.process(&mut silence);
        assert_eq!(agc.gain(), 1.0);
        for _ in 0..200 {
            let mut b = sine(48_000, 300.0, 960, 0.02);
            agc.process(&mut b);
        }
        assert!(agc.gain() > 3.0, "gain {}", agc.gain());
        let mut loud = sine(48_000, 300.0, 960, 0.5);
        agc.process(&mut loud);
        // Loud input pulls the gain down quickly.
        assert!(agc.gain() < 3.0);
    }

    #[test]
    fn limiter_and_meter() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert!(soft_limit(5.0) <= 1.0);
        assert!(soft_limit(-5.0) >= -1.0);
        assert_eq!(meter_position(1.0), 1.0);
        assert_eq!(meter_position(0.0), 0.0);
        assert!((meter_position(0.001) - 0.0).abs() < 1e-6);
        let b = to_i16(&[[1.0, -1.0]]);
        assert_eq!(b, vec![0xFF, 0x7F, 0x01, 0x80]);
    }
}
