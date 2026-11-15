//! File dialogs for the video editor (IFileOpenDialog / IFileSaveDialog). Modal on the UI
//! thread, like the image editor's dialogs; the window just doesn't repaint while one is up.

use crate::common::win::{hwnd_from_isize, pcwstr, wide};
use std::path::{Path, PathBuf};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
    SHCreateItemFromParsingName, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST,
    SIGDN_FILESYSPATH,
};

pub const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "m4v", "wmv", "mov", "avi", "mkv"];

pub fn is_video(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub fn is_project(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(super::project::EXTENSION))
}

fn ensure_com() {
    // SAFETY: per-thread COM init; an existing apartment (RPC_E_CHANGED_MODE or S_FALSE) is fine.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        log::warn!("video editor: CoInitializeEx failed: {hr:?}");
    }
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
}

struct Specs {
    _bufs: Vec<(Vec<u16>, Vec<u16>)>,
    specs: Vec<COMDLG_FILTERSPEC>,
}

fn specs(items: &[Filter]) -> Specs {
    let bufs: Vec<(Vec<u16>, Vec<u16>)> = items.iter().map(|f| (wide(&f.name), wide(&f.pattern))).collect();
    let specs = bufs.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: pcwstr(n), pszSpec: pcwstr(s) }).collect();
    Specs { _bufs: bufs, specs }
}

fn shell_item_path(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: the CoTaskMem string is freed after copying.
    unsafe {
        let p: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

fn set_folder(dlg: &IFileDialog, folder: Option<&Path>) {
    let Some(folder) = folder.filter(|f| f.is_dir()) else { return };
    let w = wide(&folder.to_string_lossy());
    // SAFETY: valid NUL-terminated path.
    unsafe {
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(pcwstr(&w), None) {
            let _ = dlg.SetFolder(&item);
        }
    }
}

fn log_err(e: &windows::core::Error) {
    if e.code() != ERROR_CANCELLED.to_hresult() {
        log::warn!("video editor: file dialog failed: {e}");
    }
}

pub fn open_file(owner: isize, folder: Option<&Path>, filters: &[Filter]) -> Option<PathBuf> {
    ensure_com();
    let f = specs(filters);
    // SAFETY: standard COM usage; pointers outlive the calls.
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| log::error!("video editor: cannot create the open dialog: {e}"))
            .ok()?;
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileTypes(&f.specs);
        let _ = dlg.SetFileTypeIndex(1);
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, folder);
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_err(&e);
            return None;
        }
        shell_item_path(&dlg.GetResult().ok()?)
    }
}

/// Save dialog with one filter; the extension is appended when missing.
pub fn save_file(owner: isize, folder: Option<&Path>, name: &str, filter: &Filter, ext: &str) -> Option<PathBuf> {
    ensure_com();
    let f = specs(std::slice::from_ref(filter));
    let name_w = wide(name);
    let ext_w = wide(ext);
    // SAFETY: standard COM usage; pointers outlive the calls.
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| log::error!("video editor: cannot create the save dialog: {e}"))
            .ok()?;
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileTypes(&f.specs);
        let _ = dlg.SetDefaultExtension(pcwstr(&ext_w));
        let _ = dlg.SetFileName(PCWSTR(name_w.as_ptr()));
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, folder);
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_err(&e);
            return None;
        }
        let path = shell_item_path(&dlg.GetResult().ok()?)?;
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            Some(path)
        } else {
            let mut s = path.into_os_string();
            s.push(".");
            s.push(ext);
            Some(PathBuf::from(s))
        }
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
}
