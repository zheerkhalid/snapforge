//! Modern Windows file dialogs (IFileOpenDialog / IFileSaveDialog).
//!
//! The dialogs are modal and run on the editor thread (their own message loop keeps the
//! rest of Windows responsive); the editor window simply doesn't repaint while one is up.

use super::document::format_for_path;
use crate::common::win::{hwnd_from_isize, pcwstr, wide};
use crate::settings::ImageFormat;
use std::cell::Cell;
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
    SHCreateItemFromParsingName, FDAP_TOP, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

/// Every extension we can decode (for the open filter and drag & drop).
pub const OPEN_EXTENSIONS: [&str; 12] =
    ["png", "jpg", "jpeg", "jpe", "bmp", "gif", "tif", "tiff", "tga", "pcx", "webp", "ico"];

pub fn is_supported_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| OPEN_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Dialog filter label and pattern for a save format.
pub fn format_filter(f: ImageFormat) -> (&'static str, &'static str) {
    match f {
        ImageFormat::Png => ("PNG", "*.png"),
        ImageFormat::Jpeg => ("JPEG", "*.jpg;*.jpeg;*.jpe"),
        ImageFormat::Bmp => ("BMP", "*.bmp"),
        ImageFormat::Gif => ("GIF", "*.gif"),
        ImageFormat::Tiff => ("TIFF", "*.tif;*.tiff"),
        ImageFormat::Tga => ("TGA", "*.tga"),
        ImageFormat::Pcx => ("PCX", "*.pcx"),
        ImageFormat::Webp => ("WebP", "*.webp"),
    }
}

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// Initialise COM (STA) on this thread once. winit may already have done it (OLE drag &
/// drop); an existing MTA (`RPC_E_CHANGED_MODE`) is tolerated, the dialogs still work.
fn ensure_com() {
    if COM_READY.with(|c| c.get()) {
        return;
    }
    // SAFETY: plain COM initialisation for the calling thread; never uninitialised because
    // the editor thread lives until process exit.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if hr == RPC_E_CHANGED_MODE {
        log::debug!("editor: COM already initialised with another apartment model");
    } else if let Err(e) = hr.ok() {
        log::warn!("editor: CoInitializeEx failed: {e}");
    }
    COM_READY.with(|c| c.set(true));
}

struct Filters {
    // Keep the UTF-16 buffers alive while the spec structs point into them.
    _bufs: Vec<(Vec<u16>, Vec<u16>)>,
    specs: Vec<COMDLG_FILTERSPEC>,
}

fn filters(items: &[(String, String)]) -> Filters {
    let bufs: Vec<(Vec<u16>, Vec<u16>)> = items.iter().map(|(n, s)| (wide(n), wide(s))).collect();
    let specs = bufs.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: pcwstr(n), pszSpec: pcwstr(s) }).collect();
    Filters { _bufs: bufs, specs }
}

fn shell_item_path(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: GetDisplayName returns a CoTaskMem string that we free after copying.
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
    // SAFETY: valid NUL-terminated path; the item is released when dropped.
    unsafe {
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(pcwstr(&w), None) {
            let _ = dlg.SetFolder(&item);
        }
    }
}

fn log_show_error(e: &windows::core::Error) {
    if e.code() != ERROR_CANCELLED.to_hresult() {
        log::warn!("editor: file dialog failed: {e}");
    }
}

/// Add the SnapForge output folder to the dialog's navigation pane (a "custom place").
fn add_output_place(dlg: &IFileDialog) {
    let dir = crate::settings::get().output_folder();
    if !dir.is_dir() {
        return;
    }
    let w = wide(&dir.to_string_lossy());
    // SAFETY: valid NUL-terminated path; the item is released when dropped.
    unsafe {
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(pcwstr(&w), None) {
            let _ = dlg.AddPlace(&item, FDAP_TOP);
        }
    }
}

/// Open-dialog filters: all images, one per format, all files.
pub fn open_filter_items(all_images: &str, all_files: &str) -> Vec<(String, String)> {
    let pattern = OPEN_EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
    let mut items = vec![(all_images.to_string(), pattern)];
    for f in ImageFormat::ALL {
        let (n, p) = format_filter(f);
        items.push((format!("{n} ({})", p.replace(';', ", ")), p.to_string()));
    }
    items.push(("ICO (*.ico)".into(), "*.ico".into()));
    items.push((all_files.to_string(), "*.*".to_string()));
    items
}

/// Folder picker. `None` when cancelled.
pub fn pick_folder(owner: isize, folder: Option<&Path>) -> Option<PathBuf> {
    ensure_com();
    // SAFETY: standard COM usage; all pointers passed in outlive the calls.
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, folder);
            add_output_place(&base);
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_show_error(&e);
            return None;
        }
        shell_item_path(&dlg.GetResult().ok()?)
    }
}

/// Pick a program (.exe) for "Edit With".
pub fn pick_program(owner: isize) -> Option<PathBuf> {
    ensure_com();
    let f = filters(&[("Programs (*.exe)".into(), "*.exe".into()), ("All files".into(), "*.*".into())]);
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
    // SAFETY: standard COM usage; all pointers passed in outlive the calls.
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileTypes(&f.specs);
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, pf.as_deref());
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_show_error(&e);
            return None;
        }
        shell_item_path(&dlg.GetResult().ok()?)
    }
}

/// Multi-select open dialog. Returns an empty list when cancelled.
pub fn open_files(owner: isize, folder: Option<&Path>, all_images: &str, all_files: &str) -> Vec<PathBuf> {
    ensure_com();
    let f = filters(&open_filter_items(all_images, all_files));
    // SAFETY: standard COM usage; all pointers passed in outlive the calls.
    unsafe {
        let dlg: IFileOpenDialog = match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
            Ok(d) => d,
            Err(e) => {
                log::error!("editor: cannot create the open dialog: {e}");
                return Vec::new();
            }
        };
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileTypes(&f.specs);
        let _ = dlg.SetFileTypeIndex(1);
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, folder);
            add_output_place(&base);
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_show_error(&e);
            return Vec::new();
        }
        let Ok(results) = dlg.GetResults() else { return Vec::new() };
        let n = results.GetCount().unwrap_or(0);
        (0..n).filter_map(|i| results.GetItemAt(i).ok()).filter_map(|it| shell_item_path(&it)).collect()
    }
}

/// Save dialog with one filter per [`ImageFormat`]. The chosen format follows the file's
/// extension (falling back to the selected filter, whose extension is then appended).
pub fn save_file(
    owner: isize,
    folder: Option<&Path>,
    file_name: &str,
    format: ImageFormat,
) -> Option<(PathBuf, ImageFormat)> {
    ensure_com();
    let items: Vec<(String, String)> = ImageFormat::ALL
        .iter()
        .map(|&f| {
            let (n, p) = format_filter(f);
            (format!("{n} ({})", p.replace(';', ", ")), p.to_string())
        })
        .collect();
    let f = filters(&items);
    let index = ImageFormat::ALL.iter().position(|&x| x == format).unwrap_or(0);
    let name = wide(file_name);
    let ext = wide(format.extension());
    // SAFETY: standard COM usage; all pointers passed in outlive the calls.
    unsafe {
        let dlg: IFileSaveDialog = match CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) {
            Ok(d) => d,
            Err(e) => {
                log::error!("editor: cannot create the save dialog: {e}");
                return None;
            }
        };
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileTypes(&f.specs);
        let _ = dlg.SetFileTypeIndex(index as u32 + 1);
        let _ = dlg.SetDefaultExtension(pcwstr(&ext));
        let _ = dlg.SetFileName(PCWSTR(name.as_ptr()));
        if let Ok(base) = dlg.cast::<IFileDialog>() {
            set_folder(&base, folder);
            add_output_place(&base);
        }
        if let Err(e) = dlg.Show(hwnd_from_isize(owner)) {
            log_show_error(&e);
            return None;
        }
        let path = shell_item_path(&dlg.GetResult().ok()?)?;
        if let Some(fmt) = format_for_path(&path) {
            return Some((path, fmt));
        }
        // Unknown extension: use the selected filter's format.
        let sel = dlg.GetFileTypeIndex().unwrap_or(index as u32 + 1) as usize;
        let fmt = ImageFormat::ALL.get(sel.saturating_sub(1)).copied().unwrap_or(format);
        let mut s = path.into_os_string();
        s.push(".");
        s.push(fmt.extension());
        Some((PathBuf::from(s), fmt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_save_format_has_a_filter_matching_its_extension() {
        for f in ImageFormat::ALL {
            let (_, pat) = format_filter(f);
            assert!(pat.split(';').any(|p| p == format!("*.{}", f.extension())), "{f:?}");
            assert!(OPEN_EXTENSIONS.contains(&f.extension()));
        }
    }

    #[test]
    fn open_filters_list_every_format() {
        let items = open_filter_items("All images", "All files");
        assert_eq!(items.first().map(|i| i.0.as_str()), Some("All images"));
        assert_eq!(items.last().map(|i| i.1.as_str()), Some("*.*"));
        assert_eq!(items.len(), ImageFormat::ALL.len() + 3);
        assert!(items[0].1.contains("*.webp"));
    }

    #[test]
    fn supported_images_by_extension() {
        assert!(is_supported_image(Path::new(r"C:\a\b.PNG")));
        assert!(is_supported_image(Path::new("x.jpeg")));
        assert!(!is_supported_image(Path::new("x.txt")));
        assert!(!is_supported_image(Path::new("noext")));
    }
}
