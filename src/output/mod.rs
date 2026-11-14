//! Output destinations: clipboard, files, printer, email, Office, upload, external program,
//! and the date/time stamp.
//!
//! CONTRACT: functions are blocking. The app calls everything except `clipboard` and
//! `print` (which show UI / need the UI thread) on its I/O worker thread.

use crate::common::Image;
use crate::settings::{ImageFormat, UploadSettings};
use anyhow::{bail, Result};
use std::path::Path;

pub mod clipboard {
    use super::*;
    /// Put the image on the clipboard as CF_DIBV5 (with alpha) plus the "PNG" format.
    pub fn copy_image(img: &Image) -> Result<()> {
        let _ = img;
        bail!("not implemented")
    }
    /// Put text on the clipboard (upload URLs, color values…).
    pub fn copy_text(text: &str) -> Result<()> {
        let _ = text;
        bail!("not implemented")
    }
    /// Image currently on the clipboard, if any (DIB/DIBV5/PNG).
    pub fn paste_image() -> Option<Image> {
        None
    }
}

pub mod save {
    use super::*;
    /// Encode to bytes. `quality` (1–100) applies to JPEG and WebP.
    /// Formats without alpha are composited over white.
    pub fn encode(img: &Image, format: ImageFormat, quality: u8) -> Result<Vec<u8>> {
        let _ = (img, format, quality);
        bail!("not implemented")
    }
    /// Save to `path`; format from the extension, quality from settings. Creates folders.
    /// Writes to a temp file then renames (no half-written files).
    pub fn save_image(img: &Image, path: &Path) -> Result<()> {
        let _ = (img, path);
        bail!("not implemented")
    }
    /// Decode any supported file (png, jpg, bmp, gif, tif, tga, pcx, webp, ico) to an Image.
    pub fn load_image(path: &Path) -> Result<Image> {
        let _ = path;
        bail!("not implemented")
    }
}

pub mod print {
    use super::*;
    /// Show the print dialog (owner window `owner`, may be 0) and print the image fitted
    /// to the page, centred, keeping the aspect ratio. Ok(false) if the user cancelled.
    pub fn print_image(img: &Image, owner: isize, title: &str) -> Result<bool> {
        let _ = (img, owner, title);
        bail!("not implemented")
    }
}

pub mod email {
    use super::*;
    /// Open the default mail client with a new message that has `path` attached (Simple MAPI).
    pub fn email_file(path: &Path, subject: &str) -> Result<()> {
        let _ = (path, subject);
        bail!("not implemented")
    }
}

pub mod office {
    use super::*;
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OfficeApp {
        Word,
        Excel,
        PowerPoint,
        OneNote,
    }
    /// Whether the app is installed (ProgID registered).
    pub fn is_installed(app: OfficeApp) -> bool {
        let _ = app;
        false
    }
    /// Insert the image file into the running (or a newly started) Office app via COM.
    pub fn send_to(app: OfficeApp, path: &Path) -> Result<()> {
        let _ = (app, path);
        bail!("not implemented")
    }
}

pub mod upload {
    use super::*;
    /// Upload `path` via FTP or SFTP. Returns the public URL when `url_prefix` is set.
    pub fn upload_file(path: &Path, settings: &UploadSettings) -> Result<Option<String>> {
        let _ = (path, settings);
        bail!("not implemented")
    }
}

pub mod external {
    use super::*;
    /// Launch `program` with the file path as its argument.
    pub fn open_with(program: &str, path: &Path) -> Result<()> {
        let _ = (program, path);
        bail!("not implemented")
    }
}

pub mod stamp {
    use super::*;
    /// Draw `text` (date/time) into the bottom-right corner, legible on any background.
    pub fn stamp(img: &mut Image, text: &str) {
        let _ = (img, text);
    }
}
