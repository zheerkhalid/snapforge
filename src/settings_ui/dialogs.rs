//! Common Item Dialogs (IFileOpenDialog / IFileSaveDialog). Modal; call on the UI thread.
//!
//! Every function returns `Ok(None)` when the user cancels.

use crate::common::win::{pcwstr, wide};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
    SHCreateItemFromParsingName, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

/// A file-type filter: (label, pattern such as "*.toml").
pub type Filter = (String, String);

/// Balances a successful CoInitializeEx (the thread may already be STA, which is fine).
struct ComGuard(bool);

impl ComGuard {
    fn new() -> Self {
        // SAFETY: plain COM initialisation on this thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful CoInitializeEx in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

fn is_cancel(e: &windows::core::Error) -> bool {
    e.code() == ERROR_CANCELLED.to_hresult()
}

fn shell_item(path: &Path) -> Option<IShellItem> {
    let w = wide(&path.display().to_string());
    // SAFETY: valid NUL-terminated path.
    unsafe { SHCreateItemFromParsingName(pcwstr(&w), None).ok() }
}

/// Shared setup; `filters` must outlive `Show` (the wide buffers are kept by the caller).
unsafe fn setup(
    dlg: &IFileDialog,
    title: &[u16],
    specs: &[COMDLG_FILTERSPEC],
    initial: Option<&Path>,
) -> windows::core::Result<()> {
    dlg.SetTitle(pcwstr(title))?;
    if !specs.is_empty() {
        dlg.SetFileTypes(specs)?;
    }
    if let Some(item) = initial.filter(|p| p.is_dir()).and_then(shell_item) {
        dlg.SetFolder(&item)?;
    }
    Ok(())
}

unsafe fn result_path(dlg: &IFileDialog) -> windows::core::Result<PathBuf> {
    let item = dlg.GetResult()?;
    let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
    let s = p.to_string();
    CoTaskMemFree(Some(p.0 as *const c_void));
    Ok(PathBuf::from(s.map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?))
}

fn finish(r: windows::core::Result<PathBuf>) -> anyhow::Result<Option<PathBuf>> {
    match r {
        Ok(p) => Ok(Some(p)),
        Err(e) if is_cancel(&e) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{}", e.message())),
    }
}

fn owner() -> HWND {
    // SAFETY: plain query; the settings window is in front when a button is clicked.
    unsafe { GetForegroundWindow() }
}

struct Specs {
    _names: Vec<Vec<u16>>,
    _patterns: Vec<Vec<u16>>,
    specs: Vec<COMDLG_FILTERSPEC>,
}

fn specs(filters: &[Filter]) -> Specs {
    let names: Vec<Vec<u16>> = filters.iter().map(|f| wide(&f.0)).collect();
    let patterns: Vec<Vec<u16>> = filters.iter().map(|f| wide(&f.1)).collect();
    let specs = names
        .iter()
        .zip(&patterns)
        .map(|(n, p)| COMDLG_FILTERSPEC { pszName: pcwstr(n), pszSpec: pcwstr(p) })
        .collect();
    Specs { _names: names, _patterns: patterns, specs }
}

/// Pick a folder (FOS_PICKFOLDERS).
pub fn pick_folder(title: &str, initial: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let _com = ComGuard::new();
    let title = wide(title);
    // SAFETY: COM calls on an object created here; all buffers outlive the calls.
    let r = unsafe {
        (|| {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let opts = dlg.GetOptions()?;
            dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
            let base: IFileDialog = dlg.cast()?;
            setup(&base, &title, &[], initial)?;
            dlg.Show(owner())?;
            result_path(&base)
        })()
    };
    finish(r)
}

/// Choose an existing file.
pub fn open_file(title: &str, filters: &[Filter], initial: Option<&Path>) -> anyhow::Result<Option<PathBuf>> {
    let _com = ComGuard::new();
    let title = wide(title);
    let sp = specs(filters);
    // SAFETY: as above.
    let r = unsafe {
        (|| {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let opts = dlg.GetOptions()?;
            dlg.SetOptions(opts | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            let base: IFileDialog = dlg.cast()?;
            setup(&base, &title, &sp.specs, initial)?;
            dlg.Show(owner())?;
            result_path(&base)
        })()
    };
    finish(r)
}

/// Choose a file name to save to (asks before overwriting).
pub fn save_file(
    title: &str,
    filters: &[Filter],
    default_name: &str,
    default_ext: &str,
    initial: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    let _com = ComGuard::new();
    let title = wide(title);
    let name = wide(default_name);
    let ext = wide(default_ext);
    let sp = specs(filters);
    // SAFETY: as above.
    let r = unsafe {
        (|| {
            let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            let opts = dlg.GetOptions()?;
            dlg.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)?;
            let base: IFileDialog = dlg.cast()?;
            setup(&base, &title, &sp.specs, initial)?;
            dlg.SetFileName(pcwstr(&name))?;
            dlg.SetDefaultExtension(PCWSTR(ext.as_ptr()))?;
            dlg.Show(owner())?;
            result_path(&base)
        })()
    };
    finish(r)
}
