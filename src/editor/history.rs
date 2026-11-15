//! Per-document undo/redo of pixel edits.
//!
//! Every step stores the pixels needed to go back: only the changed rectangle when the
//! image size stayed the same, the whole previous image when it changed. Undoing a step
//! swaps those pixels with the current ones, so the inverse becomes the redo step.
//!
//! Memory: steps start as raw [`Image`]s. Once the raw total exceeds the budget, the
//! oldest raw steps are compressed (lossless PNG, fast setting) on a worker thread; a
//! hard cap on the total drops the very oldest steps. Restoring a compressed step decodes
//! it on a worker too (see [`super::document::Document::undo`]), so frames never wait.
//!
//! Annotation layers (Phase 3) keep their own object history; they record a
//! [`StepKind::Overlay`] marker here so Ctrl+Z walks one combined timeline.

use crate::common::{Image, Rect};
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub const MB: usize = 1024 * 1024;
/// Raw (uncompressed) bytes kept before older steps are compressed.
pub const DEFAULT_RAW_BUDGET: usize = 384 * MB;
/// Hard cap on all history memory of one document (oldest steps dropped beyond it).
pub const DEFAULT_TOTAL_CAP: usize = 1536 * MB;

/// A losslessly compressed image.
pub struct Packed {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl Packed {
    pub fn compress(img: &Image) -> Option<Packed> {
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};
        use image::{ExtendedColorType, ImageEncoder};
        if img.is_empty() {
            return None;
        }
        let mut bytes = Vec::with_capacity(img.data.len() / 8);
        // The premultiplied BGRA bytes are stored verbatim as "RGBA": lossless either way.
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Sub)
            .write_image(&img.data, img.width, img.height, ExtendedColorType::Rgba8)
            .ok()?;
        bytes.shrink_to_fit();
        Some(Packed { width: img.width, height: img.height, bytes })
    }

    pub fn decompress(&self) -> Result<Image, String> {
        use image::codecs::png::PngDecoder;
        use image::ImageDecoder;
        let mut dec = PngDecoder::new(Cursor::new(&self.bytes)).map_err(|e| e.to_string())?;
        dec.set_limits(image::Limits::no_limits()).map_err(|e| e.to_string())?;
        let mut data = vec![0u8; dec.total_bytes() as usize];
        dec.read_image(&mut data).map_err(|e| e.to_string())?;
        if data.len() != self.width as usize * self.height as usize * 4 {
            return Err("history: corrupt compressed step".into());
        }
        Ok(Image::from_bgra(self.width, self.height, data))
    }
}

#[derive(Clone)]
pub enum Payload {
    Raw(Arc<Image>),
    Packed(Arc<Packed>),
}

impl Payload {
    pub fn bytes(&self) -> usize {
        match self {
            Payload::Raw(i) => i.data.len(),
            Payload::Packed(p) => p.bytes.len(),
        }
    }
    pub fn is_raw(&self) -> bool {
        matches!(self, Payload::Raw(_))
    }
}

/// Pixels that restore one state: `pixels` goes at `rect` of an image sized `size`.
/// When `rect` covers the whole `size`, the whole image is replaced (size may change).
#[derive(Clone)]
pub struct Patch {
    pub rect: Rect,
    pub size: (u32, u32),
    pub pixels: Payload,
}

impl Patch {
    pub fn is_full(&self) -> bool {
        self.rect == Rect::new(0, 0, self.size.0 as i32, self.size.1 as i32)
    }

    /// The patch that undoes applying `self` to `current`.
    pub fn inverse_for(&self, current: &Image) -> Patch {
        let same_size = (current.width, current.height) == self.size;
        if self.is_full() || !same_size {
            Patch {
                rect: current.bounds(),
                size: (current.width, current.height),
                pixels: Payload::Raw(Arc::new(current.clone())),
            }
        } else {
            Patch { rect: self.rect, size: self.size, pixels: Payload::Raw(Arc::new(current.crop(self.rect))) }
        }
    }

    /// Apply decoded `pixels` to `current`, returning the new image.
    pub fn apply(&self, current: &mut Arc<Image>, pixels: Arc<Image>) {
        let same_size = (current.width, current.height) == self.size;
        if self.is_full() || !same_size {
            *current = pixels;
        } else {
            Arc::make_mut(current).blit(&pixels, self.rect.x, self.rect.y);
        }
    }
}

pub enum StepKind {
    Pixels(Patch),
    /// The document's annotation layer recorded a change in its own history.
    Overlay,
}

pub struct Step {
    pub id: u64,
    pub name: String,
    pub kind: StepKind,
    /// Document state ids before / after this step (for the "unsaved" flag).
    pub state_before: u64,
    pub state_after: u64,
}

impl Step {
    pub fn bytes(&self) -> usize {
        match &self.kind {
            StepKind::Pixels(p) => p.pixels.bytes(),
            StepKind::Overlay => 0,
        }
    }
    fn raw_bytes(&self) -> usize {
        match &self.kind {
            StepKind::Pixels(p) if p.pixels.is_raw() => p.pixels.bytes(),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub raw: usize,
    pub total: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { raw: DEFAULT_RAW_BUDGET, total: DEFAULT_TOTAL_CAP }
    }
}

/// Undo / redo stacks with memory accounting.
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    next_id: u64,
    pub budget: Budget,
    /// Compress on the calling thread (tests) instead of a worker.
    sync: bool,
    in_flight: HashSet<u64>,
    tx: Sender<(u64, Option<Packed>)>,
    rx: Receiver<(u64, Option<Packed>)>,
    /// Called when a background compression finished (repaint request).
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new(Budget::default())
    }
}

impl History {
    pub fn new(budget: Budget) -> Self {
        let (tx, rx) = channel();
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 0,
            budget,
            sync: false,
            in_flight: HashSet::new(),
            tx,
            rx,
            waker: None,
        }
    }

    /// Compress synchronously (deterministic tests).
    pub fn new_sync(budget: Budget) -> Self {
        Self { sync: true, ..Self::new(budget) }
    }

    pub fn set_waker(&mut self, w: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(w);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo_names(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().rev().map(|s| s.name.as_str())
    }
    pub fn redo_names(&self) -> impl Iterator<Item = &str> {
        self.redo.iter().rev().map(|s| s.name.as_str())
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn raw_bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(Step::raw_bytes).sum()
    }
    pub fn total_bytes(&self) -> usize {
        self.undo.iter().chain(&self.redo).map(Step::bytes).sum()
    }
    pub fn packed_steps(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .filter(|s| matches!(&s.kind, StepKind::Pixels(p) if !p.pixels.is_raw()))
            .count()
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Record a new step; clears the redo stack.
    pub fn push(&mut self, name: &str, kind: StepKind, state_before: u64, state_after: u64) {
        let id = self.alloc_id();
        self.redo.clear();
        self.undo.push(Step { id, name: name.to_string(), kind, state_before, state_after });
        self.maintain();
    }

    pub fn pop_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }
    pub fn pop_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    /// Push the inverse of an undone step (keeps the redo stack).
    pub fn push_redo(&mut self, step: Step) {
        self.redo.push(step);
        self.maintain();
    }
    /// Push the inverse of a redone step (keeps the redo stack).
    pub fn push_undo_keep_redo(&mut self, step: Step) {
        self.undo.push(step);
        self.maintain();
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Collect finished background compressions.
    pub fn poll(&mut self) {
        while let Ok((id, packed)) = self.rx.try_recv() {
            self.in_flight.remove(&id);
            if let Some(p) = packed {
                self.replace_payload(id, Payload::Packed(Arc::new(p)));
            }
        }
        // Keep within the total cap even when nothing new was pushed.
        self.enforce_total_cap();
    }

    fn replace_payload(&mut self, id: u64, payload: Payload) {
        if let Some(step) = self.undo.iter_mut().chain(self.redo.iter_mut()).find(|s| s.id == id) {
            if let StepKind::Pixels(p) = &mut step.kind {
                if p.pixels.is_raw() {
                    p.pixels = payload;
                }
            }
        }
    }

    /// Steps ordered from least to most likely to be needed soon.
    fn age_order(&self) -> Vec<u64> {
        let n = self.undo.len();
        let mut v: Vec<u64> = self.undo.iter().take(n.saturating_sub(1)).map(|s| s.id).collect();
        v.extend(self.redo.iter().take(self.redo.len().saturating_sub(1)).map(|s| s.id));
        // The next undo / redo steps go last: compressed only if they alone blow the budget.
        v.extend(self.redo.last().map(|s| s.id));
        v.extend(self.undo.last().map(|s| s.id));
        v
    }

    /// Compress old raw steps over the raw budget; drop the oldest beyond the total cap.
    pub fn maintain(&mut self) {
        let mut raw: usize =
            self.undo.iter().chain(&self.redo).filter(|s| !self.in_flight.contains(&s.id)).map(Step::raw_bytes).sum();
        if raw > self.budget.raw {
            let mut jobs: Vec<(u64, Arc<Image>)> = Vec::new();
            for id in self.age_order() {
                if raw <= self.budget.raw {
                    break;
                }
                if self.in_flight.contains(&id) {
                    continue;
                }
                let Some(step) = self.undo.iter().chain(&self.redo).find(|s| s.id == id) else { continue };
                if let StepKind::Pixels(Patch { pixels: Payload::Raw(img), .. }) = &step.kind {
                    raw = raw.saturating_sub(img.data.len());
                    jobs.push((id, img.clone()));
                }
            }
            self.compress(jobs);
        }
        self.enforce_total_cap();
    }

    fn compress(&mut self, jobs: Vec<(u64, Arc<Image>)>) {
        if jobs.is_empty() {
            return;
        }
        if self.sync {
            for (id, img) in jobs {
                if let Some(p) = Packed::compress(&img) {
                    self.replace_payload(id, Payload::Packed(Arc::new(p)));
                }
            }
            return;
        }
        for (id, _) in &jobs {
            self.in_flight.insert(*id);
        }
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name("editor-undo-pack".into()).spawn(move || {
            for (id, img) in jobs {
                let packed = Packed::compress(&img);
                drop(img);
                if tx.send((id, packed)).is_err() {
                    return;
                }
            }
            if let Some(w) = waker {
                w();
            }
        });
        if let Err(e) = spawned {
            log::warn!("editor: cannot start the undo compressor: {e}");
            self.in_flight.clear();
        }
    }

    fn enforce_total_cap(&mut self) {
        // Always keep the most recent undo step, whatever its size.
        while self.total_bytes() > self.budget.total {
            if self.redo.len() > 1 {
                self.redo.remove(0);
            } else if self.undo.len() > 1 {
                let s = self.undo.remove(0);
                log::debug!("editor: undo step \"{}\" dropped (history memory cap)", s.name);
            } else {
                break;
            }
        }
    }
}

/// Bounding box of the pixels that differ between two same-size images.
pub fn changed_bounds(a: &Image, b: &Image) -> Option<Rect> {
    if (a.width, a.height) != (b.width, b.height) || a.is_empty() {
        return None;
    }
    let stride = a.width as usize * 4;
    let rows_differ = |y: usize| a.data[y * stride..(y + 1) * stride] != b.data[y * stride..(y + 1) * stride];
    let h = a.height as usize;
    let top = (0..h).find(|&y| rows_differ(y))?;
    let bottom = (top..h).rev().find(|&y| rows_differ(y)).unwrap_or(top);
    let (mut left, mut right) = (usize::MAX, 0usize);
    for y in top..=bottom {
        let ra = &a.data[y * stride..(y + 1) * stride];
        let rb = &b.data[y * stride..(y + 1) * stride];
        if ra == rb {
            continue;
        }
        let px = |x: usize| ra[x * 4..x * 4 + 4] != rb[x * 4..x * 4 + 4];
        let w = a.width as usize;
        if let Some(l) = (0..left.min(w)).find(|&x| px(x)) {
            left = l;
        }
        if let Some(r) = (right..w).rev().find(|&x| px(x)) {
            right = right.max(r);
        }
    }
    if left == usize::MAX {
        return None;
    }
    Some(Rect::from_ltrb(left as i32, top as i32, right as i32 + 1, bottom as i32 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: u32, h: u32, seed: u32) -> Image {
        let mut img = Image::new(w, h);
        let mut s = seed.wrapping_mul(2654435761).max(1);
        for px in img.data.chunks_exact_mut(4) {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            px.copy_from_slice(&[s as u8, (s >> 8) as u8, (s >> 16) as u8, 255]);
        }
        img
    }

    fn full_patch(img: &Image) -> StepKind {
        StepKind::Pixels(Patch {
            rect: img.bounds(),
            size: (img.width, img.height),
            pixels: Payload::Raw(Arc::new(img.clone())),
        })
    }

    #[test]
    fn changed_bounds_finds_exact_rect() {
        let a = Image::filled(50, 40, [1, 2, 3, 255]);
        let mut b = a.clone();
        assert_eq!(changed_bounds(&a, &b), None);
        b.set_pixel(7, 9, [0, 0, 0, 255]);
        b.set_pixel(20, 30, [0, 0, 0, 255]);
        assert_eq!(changed_bounds(&a, &b), Some(Rect::from_ltrb(7, 9, 21, 31)));
        let mut c = a.clone();
        c.set_pixel(49, 39, [9, 9, 9, 9]);
        assert_eq!(changed_bounds(&a, &c), Some(Rect::new(49, 39, 1, 1)));
        assert_eq!(changed_bounds(&a, &Image::new(10, 10)), None);
    }

    #[test]
    fn packed_roundtrip_is_lossless() {
        let mut img = noisy(37, 11, 3);
        img.set_pixel(0, 0, [0, 0, 0, 0]);
        let p = Packed::compress(&img).expect("pack");
        assert_eq!(p.decompress().expect("unpack"), img);
        assert!(Packed::compress(&Image::new(0, 5)).is_none());
        let flat = Image::filled(512, 512, [200, 100, 50, 255]);
        let p = Packed::compress(&flat).expect("pack");
        assert!(p.bytes.len() < flat.data.len() / 50, "flat image compresses well: {}", p.bytes.len());
    }

    #[test]
    fn memory_accounting_compresses_old_steps() {
        // Each full step is 100*100*4 = 40_000 bytes raw.
        let budget = Budget { raw: 100_000, total: usize::MAX };
        let mut h = History::new_sync(budget);
        for i in 0..5u32 {
            h.push(&format!("edit {i}"), full_patch(&noisy(100, 100, i)), i as u64, i as u64 + 1);
        }
        assert_eq!(h.undo_len(), 5);
        assert!(h.raw_bytes() <= budget.raw, "raw {} over budget", h.raw_bytes());
        assert_eq!(h.packed_steps(), 3);
        // The newest step is still raw (instant undo).
        let newest = h.pop_undo().expect("step");
        assert!(matches!(&newest.kind, StepKind::Pixels(p) if p.pixels.is_raw()));
        assert_eq!(newest.name, "edit 4");
        // Names come newest first.
        assert_eq!(h.undo_names().next(), Some("edit 3"));
    }

    #[test]
    fn total_cap_drops_oldest_but_keeps_latest() {
        let budget = Budget { raw: usize::MAX, total: 100_000 };
        let mut h = History::new_sync(budget);
        for i in 0..5 {
            h.push(&format!("e{i}"), full_patch(&noisy(100, 100, i)), i as u64, i as u64 + 1);
        }
        assert!(h.total_bytes() <= 100_000);
        assert_eq!(h.undo_len(), 2);
        assert_eq!(h.undo_names().collect::<Vec<_>>(), ["e4", "e3"]);
        // A single step bigger than the cap is still kept.
        let mut h = History::new_sync(Budget { raw: usize::MAX, total: 10 });
        h.push("big", full_patch(&noisy(10, 10, 1)), 0, 1);
        assert_eq!(h.undo_len(), 1);
    }

    #[test]
    fn background_compression_arrives_via_poll() {
        let mut h = History::new(Budget { raw: 50_000, total: usize::MAX });
        for i in 0..4 {
            h.push("e", full_patch(&noisy(100, 100, i)), 0, 0);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.packed_steps() < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.poll();
        }
        assert_eq!(h.packed_steps(), 3);
        assert!(h.raw_bytes() <= 50_000);
    }

    #[test]
    fn push_clears_redo_and_overlay_steps_are_free() {
        let mut h = History::new_sync(Budget::default());
        h.push("a", StepKind::Overlay, 0, 1);
        let s = h.pop_undo().expect("a");
        h.push_redo(s);
        assert!(h.can_redo());
        h.push("b", StepKind::Overlay, 0, 2);
        assert!(!h.can_redo());
        assert_eq!(h.total_bytes(), 0);
    }

    #[test]
    fn patch_inverse_roundtrip() {
        let orig = noisy(20, 20, 9);
        let mut cur = Arc::new(orig.clone());
        // Region edit: paint a block, then restore via a region patch.
        let r = Rect::new(5, 6, 4, 3);
        let before = Patch { rect: r, size: (20, 20), pixels: Payload::Raw(Arc::new(orig.crop(r))) };
        Arc::make_mut(&mut cur).blit(&Image::filled(4, 3, [0, 0, 255, 255]), 5, 6);
        let edited = (*cur).clone();
        let inv = before.inverse_for(&cur);
        let Payload::Raw(px) = before.pixels.clone() else { unreachable!() };
        before.apply(&mut cur, px);
        assert_eq!(*cur, orig);
        let Payload::Raw(px) = inv.pixels.clone() else { unreachable!() };
        inv.apply(&mut cur, px);
        assert_eq!(*cur, edited);
        // Size change: full replacement.
        let small =
            Patch { rect: Rect::new(0, 0, 3, 2), size: (3, 2), pixels: Payload::Raw(Arc::new(Image::new(3, 2))) };
        let inv = small.inverse_for(&cur);
        assert!(inv.is_full() && inv.size == (20, 20));
        let Payload::Raw(px) = small.pixels.clone() else { unreachable!() };
        small.apply(&mut cur, px);
        assert_eq!((cur.width, cur.height), (3, 2));
    }
}
