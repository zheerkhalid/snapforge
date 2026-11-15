//! Export (MP4, animated GIF) and still-frame rendering. Everything here runs on a worker
//! thread; progress and cancellation go through [`Progress`].

use super::compose::{self, compose_video_frame, render_title};
use super::gif_export::{GifOptions, GifWriter};
use super::mf::{self, FrameRate, Mp4Writer, PcmCursor, VideoFrame, VideoReader, WriterConfig, HNS_PER_MS};
use super::model::EditState;
use super::timeline::{self, ItemKind, Pos, Timeline};
use crate::common::Image;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

/// Shared progress (0..=1000) and cancel flag.
#[derive(Default)]
pub struct Progress {
    permille: AtomicU32,
    cancel: AtomicBool,
}

impl Progress {
    pub fn set(&self, done: u64, total: u64) {
        let p = (done * 1000).checked_div(total).unwrap_or(0).min(1000) as u32;
        self.permille.store(p, Ordering::Relaxed);
    }
    pub fn fraction(&self) -> f32 {
        self.permille.load(Ordering::Relaxed) as f32 / 1000.0
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Mp4 {
        /// Output width (height follows the aspect ratio).
        width: u32,
        fps: f64,
        bitrate: u32,
    },
    Gif {
        width: u32,
        opts: GifOptions,
    },
}

#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub source: PathBuf,
    pub edit: EditState,
    pub format: Format,
    pub path: PathBuf,
    pub hardware: bool,
}

#[derive(Debug)]
pub enum Outcome {
    Done(PathBuf),
    Cancelled,
}

/// A running export.
pub struct ExportJob {
    pub progress: Arc<Progress>,
    rx: Receiver<Result<Outcome, String>>,
}

impl ExportJob {
    /// The result once finished.
    pub fn poll(&self) -> Option<Result<Outcome, String>> {
        self.rx.try_recv().ok()
    }
}

/// Start an export on a worker thread. `wake` is called on progress and when it finishes.
pub fn spawn(req: ExportRequest, wake: impl Fn() + Send + 'static) -> ExportJob {
    let progress = Arc::new(Progress::default());
    let (tx, rx) = channel();
    let p = progress.clone();
    let res = std::thread::Builder::new().name("video-export".into()).spawn(move || {
        let out = run(&req, &p, &wake).map_err(|e| format!("{e:#}"));
        if !matches!(out, Ok(Outcome::Done(_))) {
            let _ = std::fs::remove_file(&req.path);
        }
        let _ = tx.send(out);
        wake();
    });
    if let Err(e) = res {
        log::error!("video editor: cannot start the export thread: {e}");
    }
    ExportJob { progress, rx }
}

/// Run an export synchronously (on the calling thread).
pub fn run(req: &ExportRequest, progress: &Progress, wake: &dyn Fn()) -> Result<Outcome> {
    let _mf = mf::init()?;
    match req.format {
        Format::Mp4 { width, fps, bitrate } => export_mp4(req, width, fps, bitrate, progress, wake),
        Format::Gif { width, opts } => export_gif(req, width, opts, progress, wake),
    }
}

/// Sequential access to "the frame shown at source time t", seeking only on jumps.
pub struct FrameSource {
    reader: VideoReader,
    cur: Option<VideoFrame>,
    next: Option<VideoFrame>,
    eof: bool,
}

impl FrameSource {
    pub fn new(reader: VideoReader) -> Self {
        Self { reader, cur: None, next: None, eof: false }
    }
    pub fn info(&self) -> mf::MediaInfo {
        self.reader.info
    }

    pub fn get(&mut self, src_ms: u64) -> Result<Option<&Image>> {
        let t = src_ms as i64 * HNS_PER_MS;
        let need_seek = match &self.cur {
            None => true,
            Some(c) => t < c.pts_hns || t > c.pts_hns + 2_000 * HNS_PER_MS,
        };
        if need_seek {
            self.reader.seek(src_ms)?;
            self.cur = None;
            self.next = None;
            self.eof = false;
        }
        loop {
            if self.next.is_none() && !self.eof {
                self.next = self.reader.next_frame()?;
                self.eof = self.next.is_none();
            }
            match &self.next {
                Some(n) if n.pts_hns <= t || self.cur.is_none() => self.cur = self.next.take(),
                _ => break,
            }
        }
        Ok(self.cur.as_ref().map(|f| &f.image))
    }
}

/// Composes output frames for any output time.
struct Composer<'a> {
    edit: &'a EditState,
    tl: Timeline,
    frames: FrameSource,
    titles: HashMap<usize, Image>,
    w: u32,
    h: u32,
    last: Option<Image>,
}

impl<'a> Composer<'a> {
    fn frame(&mut self, out_ms: u64) -> Result<Image> {
        let img = match self.tl.locate(out_ms) {
            Pos::Clip { src_ms, .. } => match self.frames.get(src_ms)? {
                Some(f) => compose_video_frame(f, src_ms, self.edit, self.w, self.h),
                None => self.last.clone().unwrap_or_else(|| Image::filled(self.w, self.h, [0, 0, 0, 255])),
            },
            Pos::Title { index, .. } => {
                let (w, h) = (self.w, self.h);
                let edit = self.edit;
                self.titles
                    .entry(index)
                    .or_insert_with(|| {
                        edit.titles.get(index).map(|c| render_title(c, w, h)).unwrap_or_else(|| Image::new(w, h))
                    })
                    .clone()
            }
            Pos::End => self.last.clone().unwrap_or_else(|| Image::filled(self.w, self.h, [0, 0, 0, 255])),
        };
        self.last = Some(img.clone());
        Ok(img)
    }
}

fn open_composer<'a>(req: &'a ExportRequest, width: u32, even: bool) -> Result<Composer<'a>> {
    let reader = VideoReader::open(&req.source, req.hardware)?;
    let info = reader.info;
    let (w, h) = compose::fit_width(info.width, info.height, width, even);
    let tl = timeline::build(&req.edit.segments, &req.edit.titles);
    if tl.duration_ms == 0 {
        bail!("nothing to export");
    }
    Ok(Composer { edit: &req.edit, tl, frames: FrameSource::new(reader), titles: HashMap::new(), w, h, last: None })
}

/// Output-timeline audio for output frames `[a, b)` (samples per channel).
fn output_audio(tl: &Timeline, pcm: &mut PcmCursor, a: i64, b: i64) -> Result<Vec<i16>> {
    let rate = pcm.rate() as i64;
    let ch = pcm.channels() as usize;
    let mut out = vec![0i16; (b - a).max(0) as usize * ch];
    for it in &tl.items {
        let os = it.out_start as i64 * rate / 1000;
        let oe = it.out_end() as i64 * rate / 1000;
        let (lo, hi) = (a.max(os), b.min(oe));
        if lo >= hi {
            continue;
        }
        if let ItemKind::Clip { src_start, .. } = it.kind {
            let s0 = src_start as i64 * rate / 1000 + (lo - os);
            let s = pcm.read(s0, s0 + (hi - lo))?;
            let d0 = (lo - a) as usize * ch;
            out[d0..d0 + s.len()].copy_from_slice(&s);
        }
    }
    Ok(out)
}

fn export_mp4(
    req: &ExportRequest,
    width: u32,
    fps: f64,
    bitrate: u32,
    progress: &Progress,
    wake: &dyn Fn(),
) -> Result<Outcome> {
    let mut comp = open_composer(req, width, true)?;
    let info = comp.frames.info();
    let rate = if fps > 0.0 { FrameRate::from_fps(fps) } else { info.frame_rate };
    let rate = if rate.fps() > 120.0 { FrameRate::new(60, 1) } else { rate };
    let mut pcm = if info.has_audio {
        match mf::AudioReader::open(&req.source) {
            Ok(Some(a)) => Some(PcmCursor::new(a)),
            Ok(None) => None,
            Err(e) => {
                log::warn!("video editor: audio not exported: {e:#}");
                None
            }
        }
    } else {
        None
    };
    let cfg = WriterConfig {
        width: comp.w,
        height: comp.h,
        frame_rate: rate,
        bitrate: if bitrate > 0 { bitrate } else { WriterConfig::default_bitrate(comp.w, comp.h, rate.fps()) },
        audio: pcm.as_ref().map(|p| (p.rate(), p.channels())),
        hardware: req.hardware,
    };
    let mut writer = Mp4Writer::create(&req.path, cfg)?;
    if !writer.has_audio() {
        pcm = None;
    }
    let total = ((comp.tl.duration_ms as f64) * rate.fps() / 1000.0).ceil().max(1.0) as u64;
    let mut audio_done = 0i64;
    let mut last_wake = std::time::Instant::now();
    for n in 0..total {
        if progress.cancelled() {
            drop(writer);
            return Ok(Outcome::Cancelled);
        }
        let out_ms = (rate.frame_time(n) / HNS_PER_MS) as u64;
        let img = comp.frame(out_ms)?;
        writer.write_frame(&img, n)?;
        if let Some(p) = pcm.as_mut() {
            let r = p.rate() as i64;
            let end_ms = (rate.frame_time(n + 1) / HNS_PER_MS).min(comp.tl.duration_ms as i64);
            let target = end_ms * r / 1000;
            if target > audio_done {
                let chunk = output_audio(&comp.tl, p, audio_done, target)?;
                writer.write_audio(&chunk)?;
                audio_done = target;
            }
        }
        progress.set(n + 1, total);
        if last_wake.elapsed().as_millis() > 100 {
            wake();
            last_wake = std::time::Instant::now();
        }
    }
    writer.finish()?;
    Ok(Outcome::Done(req.path.clone()))
}

fn export_gif(
    req: &ExportRequest,
    width: u32,
    opts: GifOptions,
    progress: &Progress,
    wake: &dyn Fn(),
) -> Result<Outcome> {
    let mut comp = open_composer(req, width, false)?;
    let fps = if opts.fps.is_finite() { opts.fps.clamp(1.0, 50.0) } else { 10.0 } as f64;
    let total = ((comp.tl.duration_ms as f64) * fps / 1000.0).ceil().max(1.0) as u64;
    let tmp = req.path.with_extension("gif.part");
    let file = std::fs::File::create(&tmp).with_context(|| format!("could not create {}", tmp.display()))?;
    let result = (|| -> Result<Outcome> {
        let mut g = GifWriter::new(std::io::BufWriter::new(file), comp.w, comp.h, opts)?;
        let mut last_wake = std::time::Instant::now();
        for n in 0..total {
            if progress.cancelled() {
                return Ok(Outcome::Cancelled);
            }
            let out_ms = (n as f64 * 1000.0 / fps) as u64;
            g.add_frame(&comp.frame(out_ms)?)?;
            progress.set(n + 1, total);
            if last_wake.elapsed().as_millis() > 100 {
                wake();
                last_wake = std::time::Instant::now();
            }
        }
        let mut w = g.finish()?;
        std::io::Write::flush(&mut w)?;
        Ok(Outcome::Done(req.path.clone()))
    })();
    match result {
        Ok(Outcome::Done(p)) => {
            let _ = std::fs::remove_file(&p);
            std::fs::rename(&tmp, &p).with_context(|| format!("could not write {}", p.display()))?;
            Ok(Outcome::Done(p))
        }
        other => {
            let _ = std::fs::remove_file(&tmp);
            other
        }
    }
}

/// The composed output frame at output time `out_ms`, at the source resolution (for "Save
/// Frame as Image"). Runs MF on the calling thread.
pub fn render_still(source: &Path, edit: &EditState, out_ms: u64) -> Result<Image> {
    let _mf = mf::init()?;
    let req = ExportRequest {
        source: source.to_path_buf(),
        edit: edit.clone(),
        format: Format::Gif { width: u32::MAX, opts: GifOptions::default() },
        path: PathBuf::new(),
        hardware: true,
    };
    let mut comp = open_composer(&req, u32::MAX, false)?;
    let out_ms = out_ms.min(comp.tl.duration_ms.saturating_sub(1));
    comp.frame(out_ms)
}

#[cfg(test)]
mod tests {
    use super::super::model::{Anchor, TitleCard};
    use super::*;

    #[test]
    #[ignore = "needs Media Foundation with an H.264 encoder (run with --ignored)"]
    fn mp4_export_round_trip_cut_half_second() {
        let _mf = mf::init().expect("mf");
        let dir = std::env::temp_dir();
        let src = dir.join(format!("sf-ve-src-{}.mp4", std::process::id()));
        let dst = dir.join(format!("sf-ve-out-{}.mp4", std::process::id()));
        mf::tests::write_test_clip(&src, 320, 240, 30, 2, true).expect("write source");
        let info = mf::probe(&src).expect("probe");
        assert!((1_900..=2_100).contains(&info.duration_ms), "source {}", info.duration_ms);
        let mut edit = EditState::new(info.duration_ms);
        assert!(timeline::cut(&mut edit.segments, &mut edit.titles, 500, 1_000));
        let req = ExportRequest {
            source: src.clone(),
            edit,
            format: Format::Mp4 { width: 320, fps: 0.0, bitrate: 0 },
            path: dst.clone(),
            hardware: true,
        };
        let progress = Progress::default();
        let out = run(&req, &progress, &|| {}).expect("export");
        assert!(matches!(out, Outcome::Done(_)));
        assert!((progress.fraction() - 1.0).abs() < 1e-6);
        let got = mf::VideoReader::open(&dst, false).expect("open export");
        let d = got.info.duration_ms as i64;
        assert!((d - (info.duration_ms as i64 - 500)).abs() <= 120, "exported {d} ms");
        assert_eq!((got.info.width, got.info.height), (320, 240));
        assert!(got.info.has_audio);
        drop(got);

        // Same with a title card and GIF output.
        let mut edit = EditState::new(info.duration_ms);
        edit.titles.push(TitleCard {
            anchor: Anchor::Start,
            duration_ms: 500,
            title: "Hi".into(),
            ..TitleCard::default()
        });
        let gif_path = dir.join(format!("sf-ve-out-{}.gif", std::process::id()));
        let req = ExportRequest {
            source: src.clone(),
            edit,
            format: Format::Gif { width: 160, opts: GifOptions { fps: 10.0, dither: true, looping: true } },
            path: gif_path.clone(),
            hardware: false,
        };
        assert!(matches!(run(&req, &Progress::default(), &|| {}).expect("gif"), Outcome::Done(_)));
        let bytes = std::fs::read(&gif_path).expect("read gif");
        let mut d = gif::DecodeOptions::new().read_info(bytes.as_slice()).expect("decode gif");
        assert_eq!((d.width(), d.height()), (160, 120));
        let mut cs = 0u32;
        while let Some(f) = d.read_next_frame().expect("frame") {
            cs += f.delay as u32;
        }
        assert!((240..=260).contains(&cs), "gif length {cs} cs");

        // Cancel removes the partial file.
        let p = Progress::default();
        p.cancel();
        assert!(matches!(run(&req, &p, &|| {}).expect("cancel"), Outcome::Cancelled));
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
        let _ = std::fs::remove_file(&gif_path);
    }
}
