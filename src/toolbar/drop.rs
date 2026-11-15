//! OLE drop target for the toolbar: dragging files over it slides it down (no mouse
//! messages arrive during a drag, so the hover path cannot do it), and dropping
//! images, `.sfcap` projects or videos opens them in the editor.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use windows::core::implement;
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// Files the editor or the video editor can open.
pub fn accepts(p: &Path) -> bool {
    crate::editor::dialogs::is_supported_image(p)
        || p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("sfcap"))
        || crate::video_editor::accepts(p)
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The dragged file paths (empty when the data is not a file list).
fn dropped_files(data: &IDataObject) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // SAFETY: standard CF_HDROP retrieval; the medium is released below.
    unsafe {
        let Ok(mut medium) = data.GetData(&hdrop_format()) else { return out };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

fn has_files(data: Option<&IDataObject>) -> bool {
    // SAFETY: QueryGetData only inspects the format.
    data.is_some_and(|d| unsafe { d.QueryGetData(&hdrop_format()) }.is_ok())
}

#[implement(IDropTarget)]
struct ToolbarDrop {
    files: Cell<bool>,
}

fn set_effect(p: *mut DROPEFFECT, e: DROPEFFECT) {
    if !p.is_null() {
        // SAFETY: OLE passes a valid in/out pointer.
        unsafe { *p = e };
    }
}

impl IDropTarget_Impl for ToolbarDrop_Impl {
    fn DragEnter(
        &self,
        data: Option<&IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = has_files(data);
        self.files.set(files);
        super::drag_over(true);
        set_effect(effect, if files { DROPEFFECT_COPY } else { DROPEFFECT_NONE });
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        set_effect(effect, if self.files.get() { DROPEFFECT_COPY } else { DROPEFFECT_NONE });
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.files.set(false);
        super::drag_over(false);
        Ok(())
    }

    fn Drop(
        &self,
        data: Option<&IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        self.files.set(false);
        super::drag_over(false);
        let all = data.map(dropped_files).unwrap_or_default();
        let (ok, skipped): (Vec<PathBuf>, Vec<PathBuf>) = all.into_iter().partition(|p| accepts(p));
        if !skipped.is_empty() {
            log::info!("toolbar: ignored {} dropped file(s) that cannot be opened", skipped.len());
        }
        set_effect(effect, if ok.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY });
        if !ok.is_empty() {
            log::info!("toolbar: opening {} dropped file(s)", ok.len());
            crate::editor::open_files(ok);
        }
        Ok(())
    }
}

/// Register the toolbar window as a drop target (UI thread). Failure is logged only.
pub fn register(hwnd: HWND) {
    // SAFETY: OLE init for the UI thread (STA); harmless if already initialized.
    if let Err(e) = unsafe { OleInitialize(None) } {
        log::warn!("toolbar: OleInitialize failed ({e}); drag and drop is disabled");
        return;
    }
    let target: IDropTarget = ToolbarDrop { files: Cell::new(false) }.into();
    // SAFETY: our own window; OLE keeps a reference to the target until revoked.
    if let Err(e) = unsafe { RegisterDragDrop(hwnd, &target) } {
        log::warn!("toolbar: RegisterDragDrop failed: {e}");
    }
}

pub fn revoke(hwnd: HWND) {
    // SAFETY: undoing our own registration (an error just means it was not registered).
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_images_projects_and_videos_only() {
        assert!(accepts(Path::new("C:/a/shot.PNG")));
        assert!(accepts(Path::new("C:/a/photo.jpeg")));
        assert!(accepts(Path::new("C:/a/notes.sfcap")));
        assert!(accepts(Path::new("C:/a/clip.mp4")));
        assert!(!accepts(Path::new("C:/a/readme.txt")));
        assert!(!accepts(Path::new("C:/a/folder")));
    }
}
