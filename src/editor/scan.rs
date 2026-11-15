//! Scanner acquisition through WIA 2.0 (`IWiaDevMgr2::GetImageDlg`).
//!
//! Threading: [`scan_once`] is blocking and shows a modal system dialog (device choice,
//! scan settings, progress). Call it from a dedicated worker thread, never from the egui
//! frame: it initialises COM as a single-threaded apartment on the calling thread (and
//! balances that on return), and the dialog pumps its own messages. The owner window may
//! belong to another thread (the editor); Windows disables it while the dialog is up.

use crate::common::win::hwnd_from_isize;
use std::path::{Path, PathBuf};
use windows::core::{BSTR, HRESULT};
use windows::Win32::Devices::ImageAcquisition::{
    IWiaDevMgr2, IWiaItem2, WiaDevMgr2, WIA_ERROR_OFFLINE, WIA_S_NO_DEVICE_AVAILABLE,
};
use windows::Win32::Foundation::{ERROR_CANCELLED, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
    COINIT_DISABLE_OLE1DDE,
};

#[derive(Debug)]
pub enum ScanError {
    /// No scanner is installed or connected (or the WIA service has none to offer).
    NoDevice,
    /// The user closed the dialog without scanning.
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::NoDevice => write!(f, "no scanner is available"),
            ScanError::Cancelled => write!(f, "scan cancelled"),
            ScanError::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// Balances a successful `CoInitializeEx` on drop.
struct ComGuard(bool);

impl ComGuard {
    fn sta() -> Self {
        // SAFETY: plain per-thread COM initialisation; balanced in Drop when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr == RPC_E_CHANGED_MODE {
            // Already an MTA thread: WIA still works through the proxy; don't uninitialise.
            return ComGuard(false);
        }
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx in `sta`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Classify a failed HRESULT from the WIA calls.
fn classify(code: HRESULT, what: &str, e: &windows::core::Error) -> ScanError {
    if code == WIA_S_NO_DEVICE_AVAILABLE || code == WIA_ERROR_OFFLINE {
        ScanError::NoDevice
    } else if code == ERROR_CANCELLED.to_hresult() {
        ScanError::Cancelled
    } else {
        ScanError::Failed(format!("{what}: {e}"))
    }
}

/// Show the WIA device + scan dialog (modal to `owner`, an HWND as isize) and scan once
/// into `folder` (created if missing) with base name `file_stem`. Returns the scanned file
/// paths (a document feeder may return several).
///
/// Initialises COM as STA on the calling thread itself (an existing MTA is tolerated).
/// Blocking: run it on a worker thread. Never panics.
pub fn scan_once(owner: isize, folder: &Path, file_stem: &str) -> Result<Vec<PathBuf>, ScanError> {
    std::fs::create_dir_all(folder)
        .map_err(|e| ScanError::Failed(format!("cannot create {}: {e}", folder.display())))?;
    let _com = ComGuard::sta();
    let stem = if file_stem.trim().is_empty() { "Scan" } else { file_stem.trim() };
    // SAFETY: standard COM usage. All BSTRs outlive the call; the returned array is read
    // exactly `n` times (taking ownership of each BSTR, freed on drop) and then released
    // with CoTaskMemFree, as GetImageDlg documents.
    unsafe {
        let mgr: IWiaDevMgr2 = CoCreateInstance(&WiaDevMgr2, None, CLSCTX_LOCAL_SERVER)
            .map_err(|e| classify(e.code(), "cannot start the Windows Image Acquisition service", &e))?;
        let folder_b = BSTR::from(folder.to_string_lossy().as_ref());
        let stem_b = BSTR::from(stem);
        let mut n: i32 = 0;
        let mut paths: *mut BSTR = std::ptr::null_mut();
        let mut item: Option<IWiaItem2> = None;
        let res =
            mgr.GetImageDlg(0, &BSTR::new(), hwnd_from_isize(owner), &folder_b, &stem_b, &mut n, &mut paths, &mut item);
        drop(item);
        let mut out = Vec::new();
        if !paths.is_null() {
            for i in 0..n.max(0) as usize {
                let b: BSTR = std::ptr::read(paths.add(i));
                let s = b.to_string();
                if !s.is_empty() {
                    out.push(PathBuf::from(s));
                }
            }
            CoTaskMemFree(Some(paths as *const _));
        }
        match res {
            Err(e) => Err(classify(e.code(), "scanning failed", &e)),
            // S_FALSE (user cancelled) also maps to Ok in the windows crate.
            Ok(()) if out.is_empty() => Err(ScanError::Cancelled),
            Ok(()) => Ok(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_hresults() {
        let e = windows::core::Error::from(WIA_S_NO_DEVICE_AVAILABLE);
        assert!(matches!(classify(e.code(), "x", &e), ScanError::NoDevice));
        let e = windows::core::Error::from(ERROR_CANCELLED.to_hresult());
        assert!(matches!(classify(e.code(), "x", &e), ScanError::Cancelled));
        let e = windows::core::Error::from(HRESULT(0x80004005_u32 as i32));
        assert!(matches!(classify(e.code(), "x", &e), ScanError::Failed(_)));
    }

    /// Manual: `cargo test scan_dialog -- --ignored` shows the real dialog.
    #[test]
    #[ignore]
    fn scan_dialog() {
        let dir = std::env::temp_dir().join("snapforge-scan-test");
        println!("{:?}", scan_once(0, &dir, "Scan"));
    }
}
