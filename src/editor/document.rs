//! Documents (one per tab) and the tab list. Pure state: no egui or I/O here except the
//! optional GPU textures slot.

use super::texture::ImageTextures;
use super::view::ViewState;
use crate::common::Image;
use crate::settings::{Destination, ImageFormat};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type DocId = u64;

pub struct Document {
    pub id: DocId,
    /// Shared with background jobs (save / copy) without copying pixels.
    pub image: Arc<Image>,
    pub path: Option<PathBuf>,
    pub title: String,
    /// Bumped on every edit; a save only clears `dirty` if no edit happened meanwhile.
    pub revision: u64,
    pub dirty: bool,
    pub continue_to: Option<Destination>,
    pub view: ViewState,
    /// GPU textures, created lazily by the canvas.
    pub textures: Option<ImageTextures>,
    pub has_alpha: bool,
    /// A save job is running for this document.
    pub saving: bool,
    /// Bench: QPC of the `open_image` call, cleared once the image has been presented.
    pub opened_qpc: Option<i64>,
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
        Self {
            id,
            dirty: path.is_none(),
            image: Arc::new(image),
            path,
            title,
            revision: 0,
            continue_to,
            view: ViewState::default(),
            textures: None,
            has_alpha,
            saving: false,
            opened_qpc: None,
        }
    }

    /// An image loaded from disk: clean, titled after the file.
    pub fn from_file(id: DocId, image: Image, path: PathBuf) -> Self {
        Self::new(id, image, String::new(), Some(path), None)
    }

    /// Replace the pixels (Phase 2 edits go through here). Drops stale textures.
    pub fn set_image(&mut self, image: Image) {
        self.has_alpha = image.has_transparency();
        if image.width != self.image.width || image.height != self.image.height {
            self.view.initialized = false;
        }
        self.image = Arc::new(image);
        self.textures = None;
        self.mark_edited();
    }

    pub fn mark_edited(&mut self) {
        self.revision += 1;
        self.dirty = true;
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

    /// A save of `revision` to `path` finished successfully.
    pub fn on_saved(&mut self, path: PathBuf, revision: u64) {
        self.title = file_title(&path);
        self.path = Some(path);
        if revision == self.revision {
            self.dirty = false;
        }
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
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn active(&self) -> Option<&Document> {
        self.docs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Document> {
        self.docs.get_mut(self.active)
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
}
