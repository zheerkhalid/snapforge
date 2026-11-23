//! Capture history: the last N captures, so one that only went to the clipboard or the
//! editor can be found again (Tools ▸ Capture History, `src/editor/history_win.rs`).
//!
//! CONTRACT:
//! * Everything here touches the disk and blocks: call it from worker threads (the
//!   controller's I/O worker, the history window's workers), never from a UI thread.
//! * The store is a folder, `<settings dir>/history/`, with an index file `history.json`
//!   (written atomically) and the PNG files of the captures the store keeps itself
//!   (`owned`). A capture that was already saved somewhere (auto-save, the File
//!   destination) is only linked: the store never copies, changes or deletes that file.
//! * [`generation`] changes whenever the index changed, so a window can reload.

use crate::common::Image;
use crate::settings::{self, CaptureMode, ImageFormat};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

const INDEX_FILE: &str = "history.json";
/// Files the store writes itself are named `cap-<unix time>-<n>.png`.
const OWNED_PREFIX: &str = "cap-";

/// One remembered capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    /// When it was captured: seconds since the Unix epoch (see [`Entry::when`]).
    pub time: u64,
    pub width: u32,
    pub height: u32,
    /// [`CaptureMode::id`] of the capture mode.
    pub mode: String,
    /// The image file (absolute once loaded).
    pub path: PathBuf,
    /// The file lives in the history folder and belongs to the store.
    pub owned: bool,
    /// Recognized text: `None` = not looked at yet, empty = nothing found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Entry {
    /// Local date and time, `YYYY-MM-DD HH:MM`.
    pub fn when(&self) -> String {
        local_time_text(self.time)
    }

    /// `1920x1080`.
    pub fn size_text(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Short localized name of the capture mode ("Region"); the raw id when unknown.
    pub fn mode_label(&self) -> String {
        match CaptureMode::from_id(&self.mode) {
            Some(m) => crate::i18n::t(&format!("history-mode-{}", m.id())),
            None => self.mode.clone(),
        }
    }

    /// Everything the search looks at, lower-cased.
    fn haystack(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            self.file_name(),
            self.when(),
            self.mode,
            self.mode_label(),
            self.size_text(),
            self.text.as_deref().unwrap_or_default()
        )
        .to_lowercase()
    }
}

/// `YYYY-MM-DD HH:MM` in local time for seconds since the Unix epoch (empty on failure).
pub fn local_time_text(unix_secs: u64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME counts 100 ns intervals since 1601-01-01.
    let ticks = unix_secs.saturating_mul(10_000_000).saturating_add(116_444_736_000_000_000);
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out pointers.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc).is_ok() && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
    };
    if !ok {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

/// Indices of the entries matching `query`: case-insensitive, and every whitespace-separated
/// term must appear in the file name, the date/time, the mode, the `WxH` size or the
/// recognized text. An empty query matches everything.
pub fn search(entries: &[Entry], query: &str) -> Vec<usize> {
    let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase().replace('×', "x")).collect();
    if terms.is_empty() {
        return (0..entries.len()).collect();
    }
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let hay = e.haystack();
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------- the store

/// The index as it is on disk (entries oldest first; owned files by name only, so the
/// folder can move with a portable install).
#[derive(Default, Serialize)]
struct Index {
    next_id: u64,
    entries: Vec<Entry>,
}

/// One process-wide lock around every read-modify-write of an index.
static LOCK: Mutex<()> = Mutex::new(());
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn changed() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Bumped on every change of a history index (any store).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().replace('/', "\\").eq_ignore_ascii_case(&b.to_string_lossy().replace('/', "\\"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A history folder. The functions at the end of this module use the one in the settings
/// directory; tests use temporary folders.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    /// Read the index. A missing file is an empty history and so is a damaged one (the
    /// entries that can still be read are kept); only a file that cannot be read at all
    /// right now is an error, so a later write does not wipe it.
    fn load(&self) -> Result<Index> {
        let path = self.index_path();
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Index::default()),
            Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
        };
        let Ok(root) = serde_json::from_slice::<serde_json::Value>(&data) else {
            log::warn!("history: the index file is damaged; starting an empty history");
            return Ok(Index::default());
        };
        let mut entries: Vec<Entry> = root
            .get("entries")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
            .unwrap_or_default();
        for e in &mut entries {
            if e.path.is_relative() {
                e.path = self.dir.join(&e.path);
            }
        }
        let next_id = root.get("next_id").and_then(|v| v.as_u64()).unwrap_or(0);
        let next_id = entries.iter().map(|e| e.id + 1).fold(next_id.max(1), u64::max);
        Ok(Index { next_id, entries })
    }

    fn save(&self, index: &Index) -> Result<()> {
        let disk = Index {
            next_id: index.next_id,
            entries: index
                .entries
                .iter()
                .map(|e| {
                    let mut e = e.clone();
                    if e.owned {
                        if let Some(name) = e.path.file_name() {
                            e.path = PathBuf::from(name);
                        }
                    }
                    e
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&disk).context("could not write the history index")?;
        crate::output::save::write_atomic(&self.index_path(), &bytes)
    }

    /// The file is one the store wrote itself: flagged as owned and inside the folder.
    fn is_own_file(&self, e: &Entry) -> bool {
        e.owned && e.path.parent().is_some_and(|p| same_path(p, &self.dir))
    }

    fn delete_file(&self, e: &Entry) {
        if self.is_own_file(e) {
            let _ = std::fs::remove_file(&e.path);
        }
    }

    /// A free file name in the folder for a capture taken at `time`.
    fn new_file_path(&self, time: u64) -> PathBuf {
        (1u32..)
            .map(|n| self.dir.join(format!("{OWNED_PREFIX}{time}-{n}.png")))
            .find(|p| !p.exists())
            .unwrap_or_else(|| self.dir.join(format!("{OWNED_PREFIX}{time}.png")))
    }

    /// Add a capture and keep only the newest `keep` entries. `saved` = the file the
    /// capture was already written to (linked, not copied); without one (or when that file
    /// does not exist) the image is written into the folder as PNG. Returns the new id.
    pub fn record(&self, image: &Image, mode: &str, saved: Option<&Path>, keep: usize) -> Result<Option<u64>> {
        if image.is_empty() {
            return Ok(None);
        }
        let time = unix_now();
        let linked = saved.filter(|p| p.is_file()).map(Path::to_path_buf);
        // The slow part (PNG encoding) happens before the index is locked.
        let (path, owned) = match linked {
            Some(p) => (p, false),
            None => {
                let p = {
                    let _guard = lock();
                    let p = self.new_file_path(time);
                    // Claim the name so a concurrent record picks another one.
                    crate::output::save::write_atomic(&p, &[])?;
                    p
                };
                if let Err(e) = crate::output::save::save_image_as(image, &p, ImageFormat::Png, 100) {
                    let _ = std::fs::remove_file(&p);
                    return Err(e);
                }
                (p, true)
            }
        };
        let _guard = lock();
        let added = Entry {
            id: 0,
            time,
            width: image.width,
            height: image.height,
            mode: mode.to_string(),
            path,
            owned,
            text: None,
        };
        let mut index = match self.load() {
            Ok(i) => i,
            Err(e) => {
                self.delete_file(&added);
                return Err(e);
            }
        };
        if !owned {
            // The same file captured again (e.g. a fixed output name): one entry is enough.
            index.entries.retain(|e| e.owned || !same_path(&e.path, &added.path));
        }
        let id = index.next_id.max(1);
        index.next_id = id + 1;
        index.entries.push(Entry { id, ..added });
        let excess = index.entries.len().saturating_sub(keep.max(1));
        let dropped: Vec<Entry> = index.entries.drain(..excess).collect();
        if let Err(e) = self.save(&index) {
            if let Some(new) = index.entries.last() {
                self.delete_file(new);
            }
            return Err(e);
        }
        for e in &dropped {
            self.delete_file(e);
        }
        changed();
        Ok(Some(id))
    }

    /// The history, newest first, without the entries whose file no longer exists.
    pub fn entries(&self) -> Vec<Entry> {
        let index = {
            let _guard = lock();
            self.load()
        };
        match index {
            Ok(i) => i.entries.into_iter().rev().filter(|e| e.path.is_file()).collect(),
            Err(e) => {
                log::warn!("history: {e:#}");
                Vec::new()
            }
        }
    }

    /// Forget one capture; its file is deleted only when the store owns it.
    pub fn remove(&self, id: u64) -> Result<bool> {
        let _guard = lock();
        let mut index = self.load()?;
        let Some(pos) = index.entries.iter().position(|e| e.id == id) else { return Ok(false) };
        let entry = index.entries.remove(pos);
        self.save(&index)?;
        self.delete_file(&entry);
        changed();
        Ok(true)
    }

    /// Forget everything: the store's own files and the index. Files saved elsewhere stay.
    pub fn clear(&self) -> Result<()> {
        let _guard = lock();
        let index = self.load()?;
        for e in &index.entries {
            self.delete_file(e);
        }
        // Files of an index that was lost or damaged.
        if let Ok(list) = std::fs::read_dir(&self.dir) {
            for f in list.flatten() {
                let name = f.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with(OWNED_PREFIX) && name.ends_with(".png") {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
        match std::fs::remove_file(self.index_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).context("could not delete the history index");
            }
            _ => {}
        }
        changed();
        Ok(())
    }

    /// Store the recognized text of a capture (empty = nothing found).
    pub fn set_text(&self, id: u64, text: &str) -> Result<bool> {
        let _guard = lock();
        let mut index = self.load()?;
        let Some(e) = index.entries.iter_mut().find(|e| e.id == id) else { return Ok(false) };
        e.text = Some(text.to_string());
        self.save(&index)?;
        changed();
        Ok(true)
    }
}

// ---------------------------------------------------------------- the app's history

/// The app's history folder.
pub fn dir() -> PathBuf {
    settings::dir().join("history")
}

fn store() -> Store {
    Store::new(dir())
}

fn log_err<T: Default>(what: &str, r: Result<T>) -> T {
    r.unwrap_or_else(|e| {
        log::warn!("history: {what} failed: {e:#}");
        T::default()
    })
}

/// Remember a delivered capture (no-op when the history is turned off). Blocking.
pub fn record(image: &Image, mode: &str, saved: Option<&Path>) {
    let (enabled, keep) = {
        let s = settings::get();
        (s.history.enabled, s.history.keep_clamped())
    };
    if enabled {
        log_err("record", store().record(image, mode, saved, keep));
    }
}

/// The history, newest first (entries whose file is gone are left out). Blocking.
pub fn entries() -> Vec<Entry> {
    store().entries()
}

/// Forget one capture (its file is deleted only when the history owns it). Blocking.
pub fn remove(id: u64) {
    log_err("remove", store().remove(id));
}

/// Forget every capture. Blocking.
pub fn clear() {
    log_err("clear", store().clear());
}

/// Store the recognized text of a capture. Blocking.
pub fn set_text(id: u64, text: &str) {
    log_err("set_text", store().set_text(id, text));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary history folder, removed on drop.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("snapforge-caplog-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Temp(dir)
        }

        fn store(&self) -> Store {
            Store::new(self.0.join("history"))
        }

        /// A file "the user saved" outside the history folder.
        fn user_file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            crate::output::save::save_image_as(&img(), &p, ImageFormat::Png, 100).expect("save");
            p
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn img() -> Image {
        Image::filled(8, 6, [10, 20, 30, 255])
    }

    fn entry(id: u64, name: &str, mode: &str, w: u32, h: u32, text: Option<&str>) -> Entry {
        Entry {
            id,
            time: 1_700_000_000 + id,
            width: w,
            height: h,
            mode: mode.into(),
            path: PathBuf::from(format!("C:\\shots\\{name}")),
            owned: false,
            text: text.map(str::to_string),
        }
    }

    #[test]
    fn record_keeps_an_own_png_when_nothing_was_saved() {
        let t = Temp::new("own");
        let s = t.store();
        let id = s.record(&img(), "region", None, 50).expect("record").expect("id");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        let e = &list[0];
        assert_eq!((e.id, e.width, e.height, e.mode.as_str(), e.owned), (id, 8, 6, "region", true));
        assert!(e.text.is_none());
        assert!(e.path.is_absolute());
        assert_eq!(e.path.parent(), Some(t.0.join("history").as_path()));
        let back = crate::output::save::load_image(&e.path).expect("decodes");
        assert_eq!((back.width, back.height), (8, 6));
        // The index stores owned files by name only (the folder can move).
        let raw = std::fs::read_to_string(t.0.join("history").join(INDEX_FILE)).expect("index");
        assert!(!raw.contains("snapforge-caplog"), "owned paths must be relative in the index");
    }

    #[test]
    fn record_links_a_saved_file_without_copying() {
        let t = Temp::new("link");
        let s = t.store();
        let saved = t.user_file("shot.png");
        s.record(&img(), "window", Some(&saved), 50).expect("record");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert!(!list[0].owned);
        assert_eq!(list[0].path, saved);
        let pngs = std::fs::read_dir(t.0.join("history"))
            .expect("dir")
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "png"))
            .count();
        assert_eq!(pngs, 0, "nothing is copied for a linked capture");
    }

    #[test]
    fn a_saved_path_that_does_not_exist_falls_back_to_an_own_copy() {
        let t = Temp::new("fallback");
        let s = t.store();
        s.record(&img(), "region", Some(&t.0.join("never-written.png")), 50).expect("record");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert!(list[0].owned && list[0].path.is_file());
    }

    #[test]
    fn the_same_linked_file_is_listed_once() {
        let t = Temp::new("dedupe");
        let s = t.store();
        let saved = t.user_file("fixed-name.png");
        s.record(&img(), "region", Some(&saved), 50).expect("record");
        s.record(&img(), "fullscreen", Some(&saved), 50).expect("record");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].mode, "fullscreen");
        assert!(saved.is_file());
    }

    #[test]
    fn empty_images_are_not_recorded() {
        let t = Temp::new("empty");
        let s = t.store();
        assert_eq!(s.record(&Image::new(0, 0), "region", None, 50).expect("record"), None);
        assert!(s.entries().is_empty());
    }

    #[test]
    fn entries_are_newest_first_with_growing_ids() {
        let t = Temp::new("order");
        let s = t.store();
        let a = s.record(&img(), "region", None, 50).expect("record").expect("id");
        let b = s.record(&img(), "window", None, 50).expect("record").expect("id");
        let c = s.record(&img(), "fullscreen", None, 50).expect("record").expect("id");
        assert!(a < b && b < c);
        assert_eq!(s.entries().iter().map(|e| e.id).collect::<Vec<_>>(), vec![c, b, a]);
        let paths: std::collections::HashSet<PathBuf> = s.entries().into_iter().map(|e| e.path).collect();
        assert_eq!(paths.len(), 3, "every capture has its own file");
    }

    #[test]
    fn prune_drops_the_oldest_and_deletes_only_own_files() {
        let t = Temp::new("prune");
        let s = t.store();
        let user = t.user_file("mine.png");
        s.record(&img(), "region", Some(&user), 3).expect("record");
        s.record(&img(), "region", None, 3).expect("record");
        let own_old = s.entries()[0].path.clone();
        for _ in 0..3 {
            s.record(&img(), "region", None, 3).expect("record");
        }
        let list = s.entries();
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|e| e.owned && e.path.is_file()));
        assert!(!own_old.exists(), "a pruned own file is deleted");
        assert!(user.is_file(), "a pruned linked file is the user's: never deleted");
        let pngs = std::fs::read_dir(t.0.join("history"))
            .expect("dir")
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "png"))
            .count();
        assert_eq!(pngs, 3);
    }

    #[test]
    fn remove_deletes_own_files_but_not_linked_ones() {
        let t = Temp::new("remove");
        let s = t.store();
        let user = t.user_file("keep-me.png");
        let linked = s.record(&img(), "region", Some(&user), 50).expect("record").expect("id");
        let own = s.record(&img(), "region", None, 50).expect("record").expect("id");
        let own_path = s.entries().into_iter().find(|e| e.id == own).expect("entry").path;
        assert!(s.remove(own).expect("remove"));
        assert!(!own_path.exists());
        assert!(s.remove(linked).expect("remove"));
        assert!(user.is_file());
        assert!(s.entries().is_empty());
        assert!(!s.remove(999).expect("remove"), "unknown id");
    }

    #[test]
    fn an_owned_flag_does_not_reach_outside_the_folder() {
        let t = Temp::new("tamper");
        let s = t.store();
        let user = t.user_file("outside.png");
        let id = s.record(&img(), "region", Some(&user), 50).expect("record").expect("id");
        // A tampered index claims the user's file is the store's.
        let index = t.0.join("history").join(INDEX_FILE);
        let raw = std::fs::read_to_string(&index).expect("index").replace("\"owned\": false", "\"owned\": true");
        std::fs::write(&index, raw).expect("write");
        assert!(s.entries()[0].owned);
        assert!(s.remove(id).expect("remove"));
        assert!(user.is_file(), "only files inside the history folder are ever deleted");
    }

    #[test]
    fn clear_removes_own_files_and_the_index_only() {
        let t = Temp::new("clear");
        let s = t.store();
        let user = t.user_file("stay.png");
        s.record(&img(), "region", Some(&user), 50).expect("record");
        s.record(&img(), "region", None, 50).expect("record");
        s.record(&img(), "region", None, 50).expect("record");
        let other = t.0.join("history").join("notes.txt");
        std::fs::write(&other, "x").expect("write");
        s.clear().expect("clear");
        assert!(s.entries().is_empty());
        assert!(!t.0.join("history").join(INDEX_FILE).exists());
        assert!(user.is_file());
        assert!(other.is_file(), "unrelated files in the folder are left alone");
        let left: Vec<_> = std::fs::read_dir(t.0.join("history")).expect("dir").flatten().collect();
        assert_eq!(left.len(), 1);
        // Clearing an empty / missing history is fine, and recording works again.
        s.clear().expect("clear twice");
        s.record(&img(), "region", None, 50).expect("record");
        assert_eq!(s.entries().len(), 1);
    }

    #[test]
    fn set_text_is_stored_and_empty_text_counts_as_done() {
        let t = Temp::new("text");
        let s = t.store();
        let a = s.record(&img(), "region", None, 50).expect("record").expect("id");
        let b = s.record(&img(), "region", None, 50).expect("record").expect("id");
        assert!(s.set_text(a, "Invoice 2024").expect("set"));
        assert!(s.set_text(b, "").expect("set"));
        assert!(!s.set_text(999, "x").expect("set"));
        let list = s.entries();
        assert_eq!(list.iter().find(|e| e.id == a).and_then(|e| e.text.as_deref()), Some("Invoice 2024"));
        assert_eq!(list.iter().find(|e| e.id == b).and_then(|e| e.text.as_deref()), Some(""));
        assert_eq!(search(&list, "invoice").len(), 1);
    }

    #[test]
    fn a_corrupt_index_is_tolerated() {
        let t = Temp::new("corrupt");
        let s = t.store();
        s.record(&img(), "region", None, 50).expect("record");
        let index = t.0.join("history").join(INDEX_FILE);
        std::fs::write(&index, b"{ this is not json \xff\xfe").expect("write");
        assert!(s.entries().is_empty());
        assert!(!s.remove(1).expect("remove"));
        // Recording starts a fresh index and never reuses the name of a file that is there.
        let id = s.record(&img(), "window", None, 50).expect("record").expect("id");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        let pngs = std::fs::read_dir(t.0.join("history"))
            .expect("dir")
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "png"))
            .count();
        assert_eq!(pngs, 2, "the earlier capture's file was not overwritten");
    }

    #[test]
    fn damaged_entries_are_skipped_and_the_rest_kept() {
        let t = Temp::new("partial");
        let s = t.store();
        let user = t.user_file("good.png");
        let dir = t.0.join("history");
        std::fs::create_dir_all(&dir).expect("dir");
        let good = serde_json::json!({
            "id": 7, "time": 1_700_000_000u64, "width": 8, "height": 6,
            "mode": "region", "path": user, "owned": false
        });
        let raw = serde_json::json!({ "entries": [ { "id": "oops" }, good, 42 ] });
        std::fs::write(dir.join(INDEX_FILE), raw.to_string()).expect("write");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 7);
        // Ids continue after the highest one even without a stored counter.
        let id = s.record(&img(), "region", None, 50).expect("record").expect("id");
        assert_eq!(id, 8);
    }

    #[test]
    fn entries_whose_file_is_gone_are_dropped_from_the_list() {
        let t = Temp::new("missing");
        let s = t.store();
        let user = t.user_file("temp.png");
        s.record(&img(), "region", Some(&user), 50).expect("record");
        s.record(&img(), "region", None, 50).expect("record");
        assert_eq!(s.entries().len(), 2);
        std::fs::remove_file(&user).expect("delete");
        let list = s.entries();
        assert_eq!(list.len(), 1);
        assert!(list[0].owned);
    }

    #[test]
    fn a_missing_folder_is_an_empty_history() {
        let t = Temp::new("nofolder");
        let s = t.store();
        assert!(s.entries().is_empty());
        assert!(!s.remove(1).expect("remove"));
        assert!(!s.set_text(1, "x").expect("set"));
        s.clear().expect("clear");
    }

    #[test]
    fn every_change_bumps_the_generation() {
        let t = Temp::new("gen");
        let s = t.store();
        let g0 = generation();
        let id = s.record(&img(), "region", None, 50).expect("record").expect("id");
        let g1 = generation();
        assert!(g1 > g0);
        s.set_text(id, "abc").expect("set");
        let g2 = generation();
        assert!(g2 > g1);
        s.remove(id).expect("remove");
        assert!(generation() > g2);
    }

    #[test]
    fn search_matches_name_date_mode_size_and_text() {
        let list = vec![
            entry(1, "Invoice March.png", "region", 1920, 1080, Some("Total due: 120 USD")),
            entry(2, "cap-1.png", "fullscreen", 2560, 1440, Some("")),
            entry(3, "ڕاپۆرت.png", "window", 800, 600, None),
        ];
        assert_eq!(search(&list, ""), vec![0, 1, 2]);
        assert_eq!(search(&list, "   "), vec![0, 1, 2]);
        assert_eq!(search(&list, "INVOICE"), vec![0]);
        assert_eq!(search(&list, "march invoice"), vec![0], "every term must match, in any order");
        assert_eq!(search(&list, "invoice april"), Vec::<usize>::new());
        assert_eq!(search(&list, "fullscreen"), vec![1]);
        assert_eq!(search(&list, "2560x1440"), vec![1]);
        assert_eq!(search(&list, "800×600"), vec![2]);
        assert_eq!(search(&list, "total DUE"), vec![0]);
        assert_eq!(search(&list, "ڕاپۆرت"), vec![2]);
        assert_eq!(search(&list, "png"), vec![0, 1, 2]);
        // The date as it is shown.
        let day = list[1].when()[..10].to_string();
        assert!(search(&list, &day).contains(&1));
        // Terms may match different fields.
        assert_eq!(search(&list, "region 1920x1080 usd"), vec![0]);
        assert_eq!(search(&list, "region 2560x1440"), Vec::<usize>::new());
    }

    #[test]
    fn search_finds_the_mode_by_its_shown_name() {
        let list = vec![entry(1, "a.png", "scrolling", 10, 10, None), entry(2, "b.png", "mystery", 10, 10, None)];
        let label = list[0].mode_label();
        assert!(!label.is_empty() && label != "history-mode-scrolling", "the mode has a localized name");
        assert_eq!(search(&list, &label), vec![0]);
        assert_eq!(list[1].mode_label(), "mystery", "an unknown mode shows as it is");
    }

    #[test]
    fn local_time_has_the_shown_shape() {
        let s = local_time_text(1_700_000_000);
        assert_eq!(s.len(), 16, "{s}");
        let b = s.as_bytes();
        assert!(s.starts_with("2023-11-1"), "{s}");
        assert_eq!((b[4], b[7], b[10], b[13]), (b'-', b'-', b' ', b':'));
        let now = local_time_text(unix_now());
        assert_eq!(&now[..10], crate::common::time::DateTime::now_local().date_key());
    }

    #[test]
    fn concurrent_records_do_not_lose_entries() {
        let t = Temp::new("threads");
        let dir = t.0.join("history");
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let s = Store::new(dir);
                    for _ in 0..5 {
                        s.record(&img(), "region", None, 50).expect("record");
                    }
                })
            })
            .collect();
        for th in threads {
            th.join().expect("join");
        }
        let list = t.store().entries();
        assert_eq!(list.len(), 20);
        let ids: std::collections::HashSet<u64> = list.iter().map(|e| e.id).collect();
        let paths: std::collections::HashSet<PathBuf> = list.iter().map(|e| e.path.clone()).collect();
        assert_eq!((ids.len(), paths.len()), (20, 20));
    }
}
