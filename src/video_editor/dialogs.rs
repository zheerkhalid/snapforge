//! File dialogs for the video editor: thin wrappers over the image editor's
//! IFileOpenDialog / IFileSaveDialog helpers (`editor::dialogs`). Modal on the UI thread;
//! the window just doesn't repaint while one is up.

use std::path::{Path, PathBuf};

pub const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "m4v", "wmv", "mov", "avi", "mkv"];

pub fn is_video(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn is_project(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(super::project::EXTENSION))
}

/// A dialog filter: label and `*.ext;*.ext` pattern.
pub struct Filter {
    pub name: String,
    pub pattern: String,
}

impl Filter {
    pub fn new(name: impl Into<String>, exts: &[&str]) -> Self {
        let pattern = exts.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
        Self { name: format!("{} ({})", name.into(), pattern.replace(';', ", ")), pattern }
    }

    fn item(&self) -> (String, String) {
        (self.name.clone(), self.pattern.clone())
    }
}

pub fn open_file(owner: isize, folder: Option<&Path>, filters: &[Filter]) -> Option<PathBuf> {
    let items: Vec<(String, String)> = filters.iter().map(Filter::item).collect();
    crate::editor::dialogs::open_files_with(owner, folder, &items, false).into_iter().next()
}

/// Save dialog with one filter; the extension is appended when missing or different.
pub fn save_file(owner: isize, folder: Option<&Path>, name: &str, filter: &Filter, ext: &str) -> Option<PathBuf> {
    let path = crate::editor::dialogs::save_file_with(owner, folder, name, &[filter.item()], ext)?;
    Some(with_extension(path, ext))
}

fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        path
    } else {
        let mut s = path.into_os_string();
        s.push(".");
        s.push(ext);
        PathBuf::from(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_paths() {
        assert!(is_video(Path::new(r"C:\a\b.MP4")));
        assert!(is_video(Path::new("x.wmv")));
        assert!(!is_video(Path::new("x.png")));
        assert!(is_project(Path::new("p.sfvid")));
        assert_eq!(Filter::new("Video", &["mp4", "wmv"]).pattern, "*.mp4;*.wmv");
    }

    #[test]
    fn appends_missing_extension() {
        assert_eq!(with_extension(PathBuf::from("a.mp4"), "mp4"), PathBuf::from("a.mp4"));
        assert_eq!(with_extension(PathBuf::from("a.MP4"), "mp4"), PathBuf::from("a.MP4"));
        assert_eq!(with_extension(PathBuf::from("a.v1"), "gif"), PathBuf::from("a.v1.gif"));
    }
}
