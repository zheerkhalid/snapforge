//! Documents (one per tab) and the tab list. Pure state: no egui or I/O here except the
//! optional GPU textures slot.
//!
//! All pixel edits go through [`Document::apply_image_edit`], [`Document::apply_region_edit`],
//! [`Document::commit_image`] or [`Document::commit_region`]: they record undo steps (see
//! [`super::history`]) and mark the changed area so the canvas re-uploads only those
//! texture pixels.

use super::ext::DocOverlay;
use super::history::{changed_bounds, Budget, History, Patch, Payload, Step, StepKind};
use super::selection::{FloatKind, Floating, Selection};
use super::texture::ImageTextures;
use super::view::ViewState;
use crate::common::{Image, Rect};
use crate::settings::{Destination, ImageFormat};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

pub type DocId = u64;

/// Repaint request used by background work that belongs to a document.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Which texture pixels are stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexDirty {
    Clean,
    /// Re-upload this rectangle only (same image size).
    Region(Rect),
    /// Re-create all textures.
    All,
}

impl TexDirty {
    fn add(self, r: Rect) -> TexDirty {
        match self {
            TexDirty::Clean => TexDirty::Region(r),
            TexDirty::Region(o) => TexDirty::Region(o.union(&r)),
            TexDirty::All => TexDirty::All,
        }
    }
}

/// A compressed undo/redo step being decoded on a worker.
struct PendingRestore {
    step: Step,
    redo: bool,
    rx: Receiver<Result<Image, String>>,
}

pub struct Document {
    pub id: DocId,
    /// Shared with background jobs (save / copy) without copying pixels.
    pub image: Arc<Image>,
    pub path: Option<PathBuf>,
    pub title: String,
    /// Content state id: every edit gets a fresh id, undo / redo restore earlier ids.
    /// A save of state `s` makes the document clean while `revision == s`.
    pub revision: u64,
    pub dirty: bool,
    /// State id of the last save (`None`: never saved, e.g. a capture).
    saved_revision: Option<u64>,
    pub continue_to: Option<Destination>,
    pub view: ViewState,
    /// GPU textures, created lazily by the canvas.
    pub textures: Option<ImageTextures>,
    pub tex_dirty: TexDirty,
    pub has_alpha: bool,
    /// A save job is running for this document.
    pub saving: bool,
    /// Bench: QPC of the `open_image` call, cleared once the image has been presented.
    pub opened_qpc: Option<i64>,
    pub history: History,
    pending: Option<PendingRestore>,
    pub waker: Option<Waker>,
    pub selection: Option<Selection>,
    pub floating: Option<Floating>,
    /// Phase 3 annotation layer (objects drawn over the pixels), if any.
    pub overlay: Option<Box<dyn DocOverlay>>,
    /// Workspace this tab belongs to.
    pub workspace: u64,
    /// The `.sfcap` project this document was opened from or last saved to.
    pub project: Option<PathBuf>,
    /// Pixels before the live brush stroke in progress.
    live_before: Option<Arc<Image>>,
    /// Bumped whenever the selection changes (statistics refresh).
    pub selection_version: u64,
}

/// What "Save" should do for a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// Write straight to this file.
    Path(PathBuf, ImageFormat),
    /// Unnamed (or not a writable format): show the Save As dialog.
    Prompt,
}

/// Format implied by a path's extension, if it is one we can write.
pub fn format_for_path(p: &Path) -> Option<ImageFormat> {
    p.extension().and_then(|e| e.to_str()).and_then(|e| ImageFormat::from_extension(&e.to_ascii_lowercase()))
}

fn file_title(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

static NEXT_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn fresh_state() -> u64 {
    NEXT_STATE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Document {
    /// A capture or any in-memory image. Unnamed images start dirty (never saved).
    pub fn new(
        id: DocId,
        image: Image,
        title: String,
        path: Option<PathBuf>,
        continue_to: Option<Destination>,
    ) -> Self {
        let title = match (&path, title.trim().is_empty()) {
            (Some(p), true) => file_title(p),
            (None, true) => "Untitled".to_string(),
            _ => title,
        };
        let has_alpha = image.has_transparency();
        let revision = fresh_state();
        Self {
            id,
            dirty: path.is_none(),
            saved_revision: path.as_ref().map(|_| revision),
            image: Arc::new(image),
            path,
            title,
            revision,
            continue_to,
            view: ViewState::default(),
            textures: None,
            tex_dirty: TexDirty::All,
            has_alpha,
            saving: false,
            opened_qpc: None,
            history: History::default(),
            pending: None,
            waker: None,
            selection: None,
            floating: None,
            overlay: None,
            workspace: 0,
            project: None,
            live_before: None,
            selection_version: 0,
        }
    }

    /// An image loaded from disk: clean, titled after the file.
    pub fn from_file(id: DocId, image: Image, path: PathBuf) -> Self {
        Self::new(id, image, String::new(), Some(path), None)
    }

    pub fn set_history_budget(&mut self, b: Budget) {
        self.history.budget = b;
    }

    pub fn set_waker(&mut self, w: Waker) {
        self.history.set_waker(w.clone());
        self.waker = Some(w);
    }

    /// Replace the pixels without an undo step. Drops stale textures.
    #[cfg(test)]
    pub fn set_image(&mut self, image: Image) {
        self.replace_pixels(Arc::new(image));
        self.mark_edited();
    }

    fn replace_pixels(&mut self, image: Arc<Image>) {
        self.has_alpha = image.has_transparency();
        if image.width != self.image.width || image.height != self.image.height {
            self.view.initialized = false;
            self.set_selection(None);
        }
        self.image = image;
        self.tex_dirty = TexDirty::All;
    }

    /// A new content state (fresh id); updates the dirty flag.
    #[cfg(test)]
    pub fn mark_edited(&mut self) {
        self.revision = fresh_state();
        self.refresh_dirty();
    }

    fn refresh_dirty(&mut self) {
        self.dirty = self.saved_revision != Some(self.revision);
    }

    /// No edit may start (a compressed undo step is being restored).
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    // ------------------------------------------------------------ edits (public API)

    /// Replace the whole image with `f(current)`, recorded as one undo step called
    /// `name`. Only the changed rectangle is stored when the size is unchanged.
    /// Returns false (and records nothing) if nothing changed or the document is busy.
    /// Floating pixels are merged first.
    pub fn apply_image_edit(&mut self, name: &str, f: impl FnOnce(&Image) -> Image) -> bool {
        if self.busy() {
            return false;
        }
        self.drop_floating_commit();
        let new = f(&self.image);
        self.commit_image(name, new)
    }

    /// Record and install an already computed image (e.g. one an effect computed on a
    /// worker thread from [`Document::image`]).
    pub fn commit_image(&mut self, name: &str, new: Image) -> bool {
        if self.busy() || new.is_empty() {
            return false;
        }
        let old = self.image.clone();
        let patch = if (new.width, new.height) == (old.width, old.height) {
            let Some(r) = changed_bounds(&old, &new) else { return false };
            Patch { rect: r, size: (old.width, old.height), pixels: Payload::Raw(Arc::new(old.crop(r))) }
        } else {
            Patch { rect: old.bounds(), size: (old.width, old.height), pixels: Payload::Raw(old.clone()) }
        };
        let region = (!patch.is_full()).then_some(patch.rect);
        self.record(name, StepKind::Pixels(patch));
        match region {
            Some(r) => self.install_region(Arc::new(new), r),
            None => self.replace_pixels(Arc::new(new)),
        }
        true
    }

    /// Edit the pixels inside `rect` in place: `f` receives a copy of that region and
    /// modifies it (keeping its size); the result is written back. One undo step.
    pub fn apply_region_edit(&mut self, name: &str, rect: Rect, f: impl FnOnce(&mut Image)) -> bool {
        if self.busy() {
            return false;
        }
        self.drop_floating_commit();
        let r = rect.intersect(&self.image.bounds());
        if r.is_empty() {
            return false;
        }
        let before = self.image.crop(r);
        let mut region = before.clone();
        f(&mut region);
        if (region.width, region.height) != (before.width, before.height) || region == before {
            return false;
        }
        let patch =
            Patch { rect: r, size: (self.image.width, self.image.height), pixels: Payload::Raw(Arc::new(before)) };
        self.record(name, StepKind::Pixels(patch));
        Arc::make_mut(&mut self.image).blit(&region, r.x, r.y);
        self.after_region_change(r);
        true
    }

    /// Write `pixels` (same size as `rect`) into `rect`, honouring an optional coverage
    /// mask (freehand selections: only pixels with a non-zero mask change). One undo step.
    pub fn commit_region(&mut self, name: &str, rect: Rect, pixels: &Image, mask: Option<&[u8]>) -> bool {
        if (pixels.width as i32, pixels.height as i32) != (rect.w, rect.h) {
            return false;
        }
        let clip = rect.intersect(&self.image.bounds());
        let (ox, oy) = (clip.x - rect.x, clip.y - rect.y);
        self.apply_region_edit(name, clip, |reg| {
            for y in 0..reg.height {
                for x in 0..reg.width {
                    let (sx, sy) = (x + ox as u32, y + oy as u32);
                    let m = mask.map(|m| m.get((sy * pixels.width + sx) as usize).copied().unwrap_or(0)).unwrap_or(255);
                    if m != 0 {
                        reg.set_pixel(x, y, pixels.pixel(sx, sy));
                    }
                }
            }
        })
    }

    /// Pixels to modify live during a brush stroke (clone stamp). Call
    /// [`Document::mark_region_dirty`] for what changed and [`Document::end_live_edit`] when
    /// the stroke ends: the whole stroke becomes one undo step.
    pub fn live_pixels(&mut self) -> &mut Image {
        if self.live_before.is_none() {
            self.drop_floating_commit();
            self.live_before = Some(self.image.clone());
        }
        Arc::make_mut(&mut self.image)
    }

    pub fn mark_region_dirty(&mut self, r: Rect) {
        self.tex_dirty = self.tex_dirty.add(r.intersect(&self.image.bounds()));
    }

    /// Record the live stroke as one undo step (false if nothing changed).
    pub fn end_live_edit(&mut self, name: &str) -> bool {
        let Some(before) = self.live_before.take() else { return false };
        let after = std::mem::replace(&mut self.image, before);
        let ok = self.commit_image(name, (*after).clone());
        if !ok {
            self.image = after;
        }
        ok
    }

    /// The annotation layer changed; record a marker so Ctrl+Z reaches it.
    pub fn record_overlay_step(&mut self, name: &str) {
        self.record(name, StepKind::Overlay);
    }

    fn record(&mut self, name: &str, kind: StepKind) {
        let before = self.revision;
        let after = fresh_state();
        self.history.push(name, kind, before, after);
        self.revision = after;
        self.refresh_dirty();
    }

    fn install_region(&mut self, new: Arc<Image>, r: Rect) {
        self.image = new;
        self.after_region_change(r);
    }

    fn after_region_change(&mut self, r: Rect) {
        self.tex_dirty = self.tex_dirty.add(r);
        if !self.has_alpha {
            self.has_alpha = self.image.crop(r).has_transparency();
        }
    }

    // ------------------------------------------------------------ undo / redo

    pub fn can_undo(&self) -> bool {
        !self.busy() && (self.history.can_undo() || self.floating.is_some())
    }
    pub fn can_redo(&self) -> bool {
        !self.busy() && self.history.can_redo() && self.floating.is_none()
    }

    pub fn undo(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        if let Some(f) = self.floating.take() {
            // Undo of an uncommitted move / paste: drop it and put the source back.
            if let Some((src, _)) = &f.lifted {
                let r = *src;
                let mut img = self.image.clone();
                f.restore_source(Arc::make_mut(&mut img));
                self.install_region(img, r);
            }
            self.has_alpha = self.image.has_transparency();
            self.set_selection(match f.kind {
                FloatKind::Paste => None,
                _ => Some(f.selection_at_source()),
            });
            return true;
        }
        match self.history.pop_undo() {
            Some(step) => {
                self.restore(step, false);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.busy() || self.floating.is_some() {
            return false;
        }
        match self.history.pop_redo() {
            Some(step) => {
                self.restore(step, true);
                true
            }
            None => false,
        }
    }

    /// Join the step just recorded to the one before it, so one Undo takes both.
    pub fn link_last_step(&mut self) {
        self.history.link_top();
    }

    /// Undo (`n > 0`) or redo (`n < 0`) up to `|n|` steps and return how many are left:
    /// it stops early while a compressed step decodes on a worker (call again when the
    /// document is no longer busy). Linked steps count as one.
    pub fn step_many(&mut self, mut n: isize) -> isize {
        while n != 0 && !self.busy() {
            let pair = n > 0 && self.floating.is_none() && self.history.undo_top_linked();
            let ok = if n > 0 { self.undo() } else { self.redo() };
            if !ok {
                return 0;
            }
            let more = if n > 0 { pair } else { self.history.redo_top_linked() };
            if !more {
                n -= n.signum();
            }
        }
        n
    }

    fn restore(&mut self, step: Step, redo: bool) {
        let pixels = match &step.kind {
            StepKind::Overlay => {
                if let Some(o) = self.overlay.as_mut() {
                    if redo {
                        o.redo();
                    } else {
                        o.undo();
                    }
                }
                self.finish_restore(step, None, redo);
                return;
            }
            StepKind::Pixels(p) => p.pixels.clone(),
        };
        match pixels {
            Payload::Raw(img) => self.finish_restore(step, Some(img), redo),
            Payload::Packed(packed) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let waker = self.waker.clone();
                let spawned = std::thread::Builder::new().name("editor-undo-unpack".into()).spawn(move || {
                    let _ = tx.send(packed.decompress());
                    if let Some(w) = waker {
                        w();
                    }
                });
                match spawned {
                    Ok(_) => self.pending = Some(PendingRestore { step, redo, rx }),
                    Err(e) => {
                        log::warn!("editor: cannot start the undo decoder: {e}");
                        self.push_back(step, redo);
                    }
                }
            }
        }
    }

    /// Put a step back where it came from (the restore failed).
    fn push_back(&mut self, step: Step, redo: bool) {
        if redo {
            self.history.push_redo(step);
        } else {
            self.history.push_undo_keep_redo(step);
        }
    }

    fn finish_restore(&mut self, step: Step, pixels: Option<Arc<Image>>, redo: bool) {
        let Step { id, name, kind, state_before, state_after } = step;
        let kind = match (kind, pixels) {
            (StepKind::Pixels(patch), Some(px)) => {
                let inverse = patch.inverse_for(&self.image);
                let full = patch.is_full() || (self.image.width, self.image.height) != patch.size;
                let mut img = self.image.clone();
                patch.apply(&mut img, px);
                if full {
                    self.replace_pixels(img);
                } else {
                    self.install_region(img, patch.rect);
                    self.has_alpha = self.image.has_transparency();
                }
                StepKind::Pixels(inverse)
            }
            (k, _) => k,
        };
        let step = Step { id, name, kind, state_before, state_after };
        self.revision = if redo { state_after } else { state_before };
        self.refresh_dirty();
        if redo {
            self.history.push_undo_keep_redo(step);
        } else {
            self.history.push_redo(step);
        }
    }

    /// Per-frame housekeeping: finished background decodes and compressions.
    pub fn poll(&mut self) {
        self.history.poll();
        let Some(p) = &self.pending else { return };
        let res = match p.rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("undo decoder stopped".into()),
        };
        let Some(PendingRestore { step, redo, .. }) = self.pending.take() else { return };
        match res {
            Ok(img) => self.finish_restore(step, Some(Arc::new(img)), redo),
            Err(e) => {
                log::warn!("editor: cannot restore undo step: {e}");
                self.push_back(step, redo);
            }
        }
    }

    // ------------------------------------------------------------ selection & floating

    pub fn set_selection(&mut self, sel: Option<Selection>) {
        if self.selection != sel {
            self.selection = sel;
            self.selection_version += 1;
        }
    }

    /// Lift the selection into floating pixels (a move clears the source to `bg`).
    pub fn lift_selection(&mut self, copy: bool, bg: [u8; 4]) -> bool {
        if self.busy() || self.floating.is_some() {
            return false;
        }
        let Some(sel) = self.selection.clone() else { return false };
        let mut img = self.image.clone();
        let Some(f) = Floating::lift(Arc::make_mut(&mut img), &sel, copy, bg) else { return false };
        if copy {
            self.image = img;
        } else {
            let r = sel.bounds.intersect(&img.bounds());
            self.install_region(img, r);
        }
        self.floating = Some(f);
        true
    }

    /// Paste pixels as a floating selection at (x, y).
    pub fn paste_floating(&mut self, img: Image, x: i32, y: i32) {
        self.drop_floating_commit();
        let f = Floating::pasted(img, x, y);
        self.set_selection(Some(f.selection.clone()));
        self.floating = Some(f);
    }

    pub fn move_floating(&mut self, dx: i32, dy: i32) {
        if let Some(f) = self.floating.as_mut() {
            f.move_by(dx, dy);
            let s = f.selection.clone();
            self.set_selection(Some(s));
        }
    }

    /// Merge floating pixels into the image as one undo step.
    pub fn commit_floating(&mut self) -> bool {
        let Some(f) = self.floating.take() else { return false };
        let (region, before) = f.commit_region(&self.image);
        if region.is_empty() {
            // Dragged completely off the image: a move still cleared its source.
            if let Some((src, pixels)) = &f.lifted {
                let patch = Patch {
                    rect: *src,
                    size: (self.image.width, self.image.height),
                    pixels: Payload::Raw(Arc::new(pixels.clone())),
                };
                self.record("Move Selection", StepKind::Pixels(patch));
            }
            self.clip_selection();
            return false;
        }
        let mut img = self.image.clone();
        f.stamp(Arc::make_mut(&mut img));
        let name = match f.kind {
            FloatKind::Move => "Move Selection",
            FloatKind::Copy => "Copy Selection",
            FloatKind::Paste => "Paste",
        };
        let changed = img.crop(region) != before;
        if changed {
            let patch = Patch { rect: region, size: (img.width, img.height), pixels: Payload::Raw(Arc::new(before)) };
            self.record(name, StepKind::Pixels(patch));
        }
        self.install_region(img, region);
        self.has_alpha = self.image.has_transparency();
        self.clip_selection();
        changed
    }

    /// Keep the selection inside the picture (merged pixels may have hung over an edge).
    fn clip_selection(&mut self) {
        if let Some(sel) = &self.selection {
            let clipped = sel.clipped(self.image.bounds());
            self.set_selection(clipped);
        }
    }

    /// Merge floating pixels (if any), keeping the selection where they landed.
    pub fn merge_floating(&mut self) {
        self.drop_floating_commit();
    }

    fn drop_floating_commit(&mut self) {
        if self.floating.is_some() {
            self.commit_floating();
        }
    }

    /// Cancel floating pixels (Esc): a move puts the source back.
    pub fn cancel_floating(&mut self) {
        if self.floating.is_some() {
            self.undo();
        }
    }

    /// Discard the selection (merging floating pixels first).
    pub fn deselect(&mut self) {
        self.drop_floating_commit();
        self.set_selection(None);
    }

    /// What save / copy / print / pin need, without flattening on this thread: the pixels
    /// (with floating pixels stamped) plus the layer's detached flattener, optionally
    /// limited to the selection. [`ExportJob::run`] does the work on a worker.
    pub fn export_job(&self, selection_only: bool) -> ExportJob {
        let base = match &self.floating {
            Some(f) => {
                let mut img = (*self.image).clone();
                f.stamp(&mut img);
                Arc::new(img)
            }
            None => self.image.clone(),
        };
        let select = if selection_only { self.selection.clone() } else { None };
        ExportJob { base, flatten: self.overlay.as_ref().and_then(|o| o.flattener()), select, crop: None }
    }

    /// The annotation layer has objects.
    pub fn has_annotations(&self) -> bool {
        self.overlay.as_ref().is_some_and(|o| !o.is_empty())
    }

    /// A project (.sfcap) save of state `revision` finished.
    pub fn on_project_saved(&mut self, path: PathBuf, revision: u64) {
        if self.path.is_none() {
            self.title = file_title(&path);
            self.saved_revision = Some(revision);
            self.refresh_dirty();
        }
        self.project = Some(path);
    }

    /// The pixels to save / copy: the image plus floating pixels and any annotation layer.
    pub fn composited(&self) -> Arc<Image> {
        if self.overlay.is_none() && self.floating.is_none() {
            return self.image.clone();
        }
        let mut img = (*self.image).clone();
        if let Some(f) = &self.floating {
            f.stamp(&mut img);
        }
        if let Some(o) = &self.overlay {
            img = o.flatten(&img);
        }
        Arc::new(img)
    }

    /// The selected pixels (freehand outside transparent), or the whole image.
    pub fn selection_or_image(&self) -> Image {
        if let Some(f) = &self.floating {
            return (*f.image).clone();
        }
        match &self.selection {
            Some(s) if self.has_annotations() => s.extract(&self.composited()),
            Some(s) => s.extract(&self.image),
            None => (*self.composited()).clone(),
        }
    }

    pub fn save_target(&self) -> SaveTarget {
        match &self.path {
            Some(p) => match format_for_path(p) {
                Some(f) => SaveTarget::Path(p.clone(), f),
                None => SaveTarget::Prompt,
            },
            None => SaveTarget::Prompt,
        }
    }

    /// A save of state `revision` to `path` finished successfully.
    pub fn on_saved(&mut self, path: PathBuf, revision: u64) {
        self.title = file_title(&path);
        self.path = Some(path);
        self.saved_revision = Some(revision);
        self.refresh_dirty();
    }

    /// Suggested file name for Save As.
    pub fn suggested_name(&self, format: ImageFormat) -> String {
        let stem = match &self.path {
            Some(p) => p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            None => sanitize_file_stem(&self.title),
        };
        let stem = if stem.is_empty() { "Untitled".to_string() } else { stem };
        format!("{stem}.{}", format.extension())
    }

    pub fn size(&self) -> [f32; 2] {
        [self.image.width as f32, self.image.height as f32]
    }
}

/// Pixels for an export, flattened on a worker thread.
#[derive(Clone)]
pub struct ExportJob {
    pub base: Arc<Image>,
    pub flatten: Option<super::ext::Flattener>,
    /// Limit to this selection (freehand outside transparent).
    pub select: Option<Selection>,
    /// Limit to this rectangle (floating pixels).
    pub crop: Option<Rect>,
}

impl ExportJob {
    /// Flatten (and crop). May take a while for big annotated images: call off the UI thread.
    pub fn run(&self) -> Arc<Image> {
        let full = match &self.flatten {
            Some(f) => Arc::new(f(&self.base)),
            None => self.base.clone(),
        };
        if let Some(r) = self.crop {
            return Arc::new(full.crop(r.intersect(&full.bounds())));
        }
        match &self.select {
            Some(s) => Arc::new(s.extract(&full)),
            None => full,
        }
    }
}

fn sanitize_file_stem(s: &str) -> String {
    let s: String = s.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.');
    // Drop an image extension the title may already carry.
    match s.rsplit_once('.') {
        Some((stem, ext)) if ImageFormat::from_extension(&ext.to_ascii_lowercase()).is_some() => stem.to_string(),
        _ => s.to_string(),
    }
}

/// The open tabs, in display order.
#[derive(Default)]
pub struct Tabs {
    pub docs: Vec<Document>,
    pub active: usize,
    next_id: DocId,
    /// Only tabs of this workspace are shown (workspaces enabled).
    filter: Option<u64>,
}

impl Tabs {
    pub fn next_id(&mut self) -> DocId {
        self.next_id += 1;
        self.next_id
    }

    /// Append and activate.
    pub fn push(&mut self, doc: Document) {
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        self.fix_active();
    }

    /// No visible tabs.
    pub fn is_empty(&self) -> bool {
        self.visible().next().is_none()
    }

    fn shows(&self, d: &Document) -> bool {
        self.filter.map_or(true, |w| d.workspace == w)
    }

    /// Tabs of the shown workspace, in order.
    pub fn visible(&self) -> impl Iterator<Item = &Document> {
        self.docs.iter().filter(move |d| self.shows(d))
    }

    pub fn set_filter(&mut self, f: Option<u64>) {
        self.filter = f;
        self.fix_active();
    }

    /// Make sure the active index points at a visible tab when there is one.
    fn fix_active(&mut self) {
        if self.docs.get(self.active).is_some_and(|d| self.shows(d)) {
            return;
        }
        // Prefer the nearest visible tab to the left, then to the right.
        let left = (0..self.active.min(self.docs.len())).rev().find(|&i| self.shows(&self.docs[i]));
        let right = (self.active..self.docs.len()).find(|&i| self.shows(&self.docs[i]));
        if let Some(i) = right.or(left) {
            self.active = i;
        }
    }

    pub fn active(&self) -> Option<&Document> {
        self.docs.get(self.active).filter(|d| self.shows(d))
    }

    pub fn active_mut(&mut self) -> Option<&mut Document> {
        let filter = self.filter;
        self.docs.get_mut(self.active).filter(|d| filter.map_or(true, |w| d.workspace == w))
    }

    pub fn active_id(&self) -> Option<DocId> {
        self.active().map(|d| d.id)
    }

    pub fn index_of(&self, id: DocId) -> Option<usize> {
        self.docs.iter().position(|d| d.id == id)
    }

    pub fn get_mut(&mut self, id: DocId) -> Option<&mut Document> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    pub fn activate(&mut self, id: DocId) {
        if let Some(i) = self.index_of(id) {
            self.active = i;
        }
    }

    /// Remove a tab. The active tab stays the same document if possible; closing the
    /// active tab activates its right neighbour (or the left one at the end).
    pub fn close(&mut self, id: DocId) -> Option<Document> {
        let i = self.index_of(id)?;
        let doc = self.docs.remove(i);
        if i < self.active || (i == self.active && self.active >= self.docs.len()) {
            self.active = self.active.saturating_sub(1);
        }
        self.fix_active();
        Some(doc)
    }

    /// The tab showing `path`, if it is already open.
    pub fn find_path(&self, path: &Path) -> Option<DocId> {
        self.docs
            .iter()
            .find(|d| d.path.as_deref().is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(path.as_os_str())))
            .map(|d| d.id)
    }
}

/// Insert `path` at the front of the recent list (case-insensitive dedupe), capped.
pub fn push_recent(list: &mut Vec<String>, path: &str, max: usize) {
    list.retain(|p| !p.eq_ignore_ascii_case(path));
    list.insert(0, path.to_string());
    list.truncate(max);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img() -> Image {
        Image::filled(4, 3, [1, 2, 3, 255])
    }

    #[test]
    fn capture_starts_dirty_and_prompts() {
        let d = Document::new(1, img(), "Capture 1".into(), None, Some(Destination::Clipboard));
        assert!(d.dirty);
        assert_eq!(d.save_target(), SaveTarget::Prompt);
        assert_eq!(d.suggested_name(ImageFormat::Jpeg), "Capture 1.jpg");
        assert!(!d.has_alpha);
    }

    #[test]
    fn file_starts_clean_and_saves_in_place() {
        let p = PathBuf::from(r"C:\shots\a.PNG");
        let d = Document::from_file(1, img(), p.clone());
        assert!(!d.dirty);
        assert_eq!(d.title, "a.PNG");
        assert_eq!(d.save_target(), SaveTarget::Path(p, ImageFormat::Png));
        // A read-only format (no encoder) falls back to Save As.
        let d = Document::from_file(2, img(), PathBuf::from(r"C:\x\icon.ico"));
        assert_eq!(d.save_target(), SaveTarget::Prompt);
    }

    #[test]
    fn dirty_transitions() {
        let mut d = Document::from_file(1, img(), PathBuf::from(r"C:\a.png"));
        d.mark_edited();
        assert!(d.dirty);
        let rev = d.revision;
        // Save completes with the current revision → clean.
        d.on_saved(PathBuf::from(r"C:\b.webp"), rev);
        assert!(!d.dirty);
        assert_eq!(d.title, "b.webp");
        assert_eq!(d.save_target(), SaveTarget::Path(PathBuf::from(r"C:\b.webp"), ImageFormat::Webp));
        // Edit during a save: the save finishing must not clear the newer edit.
        d.mark_edited();
        let rev = d.revision;
        d.mark_edited();
        d.on_saved(PathBuf::from(r"C:\b.webp"), rev);
        assert!(d.dirty);
    }

    #[test]
    fn set_image_marks_dirty_and_resets_view_on_resize() {
        let mut d = Document::from_file(1, img(), PathBuf::from(r"C:\a.png"));
        d.view.initialized = true;
        d.set_image(Image::new(4, 3));
        assert!(d.dirty && d.has_alpha && d.view.initialized);
        d.set_image(Image::new(5, 3));
        assert!(!d.view.initialized);
    }

    #[test]
    fn suggested_name_sanitizes() {
        let d = Document::new(1, img(), "a/b:c.png".into(), None, None);
        assert_eq!(d.suggested_name(ImageFormat::Png), "a_b_c.png");
        let d = Document::new(1, img(), "  ".into(), None, None);
        assert_eq!(d.title, "Untitled");
    }

    #[test]
    fn tabs_close_keeps_sensible_active() {
        let mut t = Tabs::default();
        let ids: Vec<DocId> = (0..4)
            .map(|_| {
                let id = t.next_id();
                t.push(Document::new(id, img(), format!("{id}"), None, None));
                id
            })
            .collect();
        assert_eq!(t.active, 3);
        t.activate(ids[1]);
        // Closing the active tab activates the right neighbour.
        t.close(ids[1]);
        assert_eq!(t.active_id(), Some(ids[2]));
        // Closing a tab left of the active one keeps the same document active.
        t.close(ids[0]);
        assert_eq!(t.active_id(), Some(ids[2]));
        // Closing the last (active) tab activates the left neighbour.
        t.activate(ids[3]);
        t.close(ids[3]);
        assert_eq!(t.active_id(), Some(ids[2]));
        t.close(ids[2]);
        assert!(t.is_empty() && t.active().is_none());
        assert!(t.close(99).is_none());
    }

    #[test]
    fn find_path_is_case_insensitive() {
        let mut t = Tabs::default();
        let id = t.next_id();
        t.push(Document::from_file(id, img(), PathBuf::from(r"C:\Shots\A.png")));
        assert_eq!(t.find_path(Path::new(r"c:\shots\a.PNG")), Some(id));
        assert_eq!(t.find_path(Path::new(r"c:\shots\b.png")), None);
    }

    #[test]
    fn recent_list_dedupes_and_caps() {
        let mut v: Vec<String> = (0..15).map(|i| format!("f{i}")).collect();
        push_recent(&mut v, "F3", 15);
        assert_eq!(v[0], "F3");
        assert_eq!(v.len(), 15);
        assert!(!v.iter().any(|p| p == "f3"));
        push_recent(&mut v, "new", 15);
        assert_eq!(v.len(), 15);
        assert_eq!(v[0], "new");
    }

    fn numbered(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [x as u8, y as u8, 7, 255]);
            }
        }
        img
    }

    fn file_doc() -> Document {
        let mut d = Document::from_file(1, numbered(20, 10), PathBuf::from(r"C:\a.png"));
        d.history = History::new_sync(Budget::default());
        d
    }

    #[test]
    fn image_edit_undo_redo_and_saved_state() {
        let mut d = file_doc();
        let orig = (*d.image).clone();
        assert!(!d.dirty);
        // Pretend the textures were uploaded.
        d.tex_dirty = TexDirty::Clean;
        // A same-size edit stores only the changed rectangle.
        assert!(d.apply_image_edit("Paint", |img| {
            let mut o = img.clone();
            o.set_pixel(3, 4, [0, 0, 255, 255]);
            o
        }));
        assert!(d.dirty);
        assert_eq!(d.history.total_bytes(), 4);
        assert_eq!(d.tex_dirty, TexDirty::Region(Rect::new(3, 4, 1, 1)));
        // No-op edits record nothing.
        assert!(!d.apply_image_edit("Nothing", |img| img.clone()));
        assert_eq!(d.history.undo_len(), 1);
        // Size change: full snapshot.
        assert!(d.apply_image_edit("Crop", |img| img.crop(Rect::new(0, 0, 5, 5))));
        assert_eq!((d.image.width, d.image.height), (5, 5));
        assert_eq!(d.history.undo_names().collect::<Vec<_>>(), ["Crop", "Paint"]);
        assert!(d.undo() && d.undo());
        assert_eq!(*d.image, orig);
        assert!(!d.dirty, "back at the saved state");
        assert!(d.redo());
        assert_eq!(d.image.pixel(3, 4), [0, 0, 255, 255]);
        assert!(d.dirty);
        assert!(d.redo());
        assert_eq!((d.image.width, d.image.height), (5, 5));
        assert!(!d.redo());
        // A new edit after undo drops the redo branch.
        assert!(d.undo());
        assert!(d.apply_region_edit("Fill", Rect::new(0, 0, 2, 2), |r| *r = Image::filled(2, 2, [1, 1, 1, 255])));
        assert!(!d.can_redo());
    }

    #[test]
    fn region_edit_and_commit_region_with_mask() {
        let mut d = file_doc();
        let before = (*d.image).clone();
        // Clipped to the image.
        assert!(d.apply_region_edit("Fill", Rect::new(18, 8, 10, 10), |r| {
            assert_eq!((r.width, r.height), (2, 2));
            *r = Image::filled(2, 2, [9, 9, 9, 255]);
        }));
        assert_eq!(d.image.pixel(19, 9), [9, 9, 9, 255]);
        assert!(d.undo());
        assert_eq!(*d.image, before);
        // Mask: only the diagonal changes.
        let px = Image::filled(2, 2, [5, 5, 5, 255]);
        assert!(d.commit_region("Masked", Rect::new(0, 0, 2, 2), &px, Some(&[255, 0, 0, 255])));
        assert_eq!(d.image.pixel(0, 0), [5, 5, 5, 255]);
        assert_eq!(d.image.pixel(1, 0), before.pixel(1, 0));
        assert_eq!(d.image.pixel(1, 1), [5, 5, 5, 255]);
        // Wrong size is rejected.
        assert!(!d.commit_region("Bad", Rect::new(0, 0, 3, 3), &px, None));
    }

    #[test]
    fn floating_move_is_one_undo_step() {
        let mut d = file_doc();
        let orig = (*d.image).clone();
        d.set_selection(Selection::rect(Rect::new(1, 1, 3, 3)));
        assert!(d.lift_selection(false, [255, 255, 255, 255]));
        d.move_floating(5, 2);
        d.move_floating(1, 0);
        assert_eq!(d.selection.as_ref().map(|s| s.bounds), Some(Rect::new(7, 3, 3, 3)));
        assert!(d.composited().pixel(7, 3) == orig.pixel(1, 1));
        assert!(d.commit_floating());
        assert_eq!(d.history.undo_len(), 1);
        assert_eq!(d.history.undo_names().next(), Some("Move Selection"));
        assert_eq!(d.image.pixel(1, 1), [255, 255, 255, 255]);
        assert_eq!(d.image.pixel(8, 4), orig.pixel(2, 2));
        assert!(d.undo());
        assert_eq!(*d.image, orig);
        // Ctrl+drag copy, then Esc cancels it without a step.
        d.set_selection(Selection::rect(Rect::new(0, 0, 2, 2)));
        assert!(d.lift_selection(true, [0, 0, 0, 0]));
        d.move_floating(3, 3);
        d.cancel_floating();
        assert!(d.floating.is_none());
        assert_eq!(*d.image, orig);
        assert_eq!(d.selection.as_ref().map(|s| s.bounds), Some(Rect::new(0, 0, 2, 2)));
        // Paste of a 1×1 transparent pixel, committed by an edit.
        d.paste_floating(Image::new(1, 1), 4, 4);
        assert!(d.floating.is_some());
        d.apply_image_edit("Paint", |img| {
            let mut o = img.clone();
            o.set_pixel(0, 0, [1, 2, 3, 255]);
            o
        });
        assert!(d.floating.is_none());
        // The transparent paste blends to nothing, so only "Paint" is recorded.
        // (The undone move sat on the redo stack, which the new edit cleared.)
        assert_eq!(d.history.undo_names().collect::<Vec<_>>(), ["Paint"]);
    }

    #[test]
    fn merged_paste_leaves_the_selection_inside_the_picture() {
        let mut d = file_doc();
        let (w, h) = (d.image.width as i32, d.image.height as i32);
        // A paste that hangs over the right and bottom edges.
        d.paste_floating(Image::filled(6, 6, [9, 9, 9, 255]), w - 2, h - 3);
        assert_eq!(d.selection.as_ref().map(|s| s.bounds), Some(Rect::new(w - 2, h - 3, 6, 6)));
        d.merge_floating();
        let sel = d.selection.clone().expect("selection kept");
        assert_eq!(sel.bounds, Rect::new(w - 2, h - 3, 2, 3));
        // The pixels an effect would be handed match the selection again.
        let crop = d.image.crop(sel.bounds);
        assert_eq!((crop.width as i32, crop.height as i32), (sel.bounds.w, sel.bounds.h));
        assert_eq!(d.image.pixel(w as u32 - 1, h as u32 - 1), [9, 9, 9, 255]);
        // Dropped completely outside: nothing is selected afterwards.
        d.paste_floating(Image::filled(2, 2, [1, 1, 1, 255]), w + 5, 0);
        d.merge_floating();
        assert!(d.selection.is_none());
    }

    #[test]
    fn linked_steps_undo_and_redo_as_one() {
        // A crop of an annotated picture records the pixel step and then a step that
        // moves the annotations. One Undo must take both, and the list shows one entry.
        let mut d = file_doc();
        let orig = (*d.image).clone();
        d.apply_image_edit("Paint", |img| {
            let mut o = img.clone();
            o.set_pixel(0, 0, [1, 2, 3, 255]);
            o
        });
        let painted = (*d.image).clone();
        assert!(d.apply_image_edit("Crop", |img| img.crop(Rect::new(1, 1, 4, 4))));
        d.record_overlay_step("Crop");
        d.link_last_step();
        let cropped = (*d.image).clone();
        assert_eq!(d.history.undo_len(), 3);
        assert_eq!(d.history.undo_names().collect::<Vec<_>>(), ["Crop", "Paint"]);
        // One undo: both halves of the crop.
        assert_eq!(d.step_many(1), 0);
        assert_eq!(*d.image, painted);
        assert_eq!(d.history.undo_len(), 1);
        assert_eq!(d.history.redo_names().collect::<Vec<_>>(), ["Crop"]);
        // One redo brings both back.
        assert_eq!(d.step_many(-1), 0);
        assert_eq!(*d.image, cropped);
        assert_eq!(d.history.undo_len(), 3);
        assert!(!d.can_redo());
        // "Undo 2 steps" from the history list: the crop and the paint.
        assert_eq!(d.step_many(2), 0);
        assert_eq!(*d.image, orig);
        assert!(!d.can_undo());
        // More than there is: stops cleanly.
        assert_eq!(d.step_many(5), 0);
        assert_eq!(d.step_many(-9), 0);
        assert_eq!(*d.image, cropped);
        // Ordinary steps are unaffected.
        assert_eq!(d.step_many(1), 0);
        assert_eq!(*d.image, painted);
    }

    #[test]
    fn compressed_steps_restore_through_poll() {
        let mut d = file_doc();
        d.history = History::new_sync(Budget { raw: 0, total: usize::MAX });
        let orig = (*d.image).clone();
        assert!(d.apply_image_edit("Big", |img| img.crop(Rect::new(0, 0, 4, 4))));
        assert!(d.apply_image_edit("Bigger", |img| crate::editor::selection::resize_canvas(
            img,
            Rect::new(0, 0, 8, 8),
            [0; 4]
        )));
        assert_eq!(d.history.packed_steps(), 2);
        assert!(d.undo());
        assert!(d.busy(), "decoding on a worker");
        assert!(!d.undo(), "no second undo while busy");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while d.busy() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
            d.poll();
        }
        assert_eq!((d.image.width, d.image.height), (4, 4));
        assert!(d.undo());
        while d.busy() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
            d.poll();
        }
        assert_eq!(*d.image, orig);
        assert!(!d.dirty);
    }

    #[test]
    fn workspace_filter_hides_tabs() {
        let mut t = Tabs::default();
        for ws in [1u64, 2, 1] {
            let id = t.next_id();
            let mut d = Document::new(id, img(), format!("{id}"), None, None);
            d.workspace = ws;
            t.push(d);
        }
        t.set_filter(Some(2));
        assert_eq!(t.visible().count(), 1);
        assert_eq!(t.active_id(), Some(2));
        t.set_filter(Some(1));
        assert_eq!(t.visible().map(|d| d.id).collect::<Vec<_>>(), [1, 3]);
        assert!(t.active_id().is_some_and(|id| id != 2));
        t.set_filter(Some(9));
        assert!(t.is_empty() && t.active().is_none());
        t.set_filter(None);
        assert_eq!(t.visible().count(), 3);
    }
}
