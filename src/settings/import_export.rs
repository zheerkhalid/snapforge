//! Settings import / export as TOML (Settings window). Owned by the settings UI module.
//!
//! Exported files never carry the FTP password: it is DPAPI-encrypted for one Windows
//! user and useless anywhere else. On import an empty password keeps the current one when
//! the upload host and user name are unchanged.

use super::Settings;
use std::path::Path;

/// Serialize settings for export (the stored password is removed).
pub fn to_export_toml(s: &Settings) -> anyhow::Result<String> {
    let mut copy = s.clone();
    copy.output.upload.password_protected.clear();
    Ok(toml::to_string_pretty(&copy)?)
}

/// Parse an exported file. Missing keys take their defaults; `current` supplies the
/// password when the imported file has none for the same host and user.
pub fn from_import_toml(text: &str, current: &Settings) -> anyhow::Result<Settings> {
    let mut s: Settings = toml::from_str(text)?;
    let (new, cur) = (&mut s.output.upload, &current.output.upload);
    if new.password_protected.is_empty() && new.host == cur.host && new.username == cur.username {
        new.password_protected = cur.password_protected.clone();
    }
    Ok(s)
}

pub fn export_to(path: &Path, s: &Settings) -> anyhow::Result<()> {
    let text = to_export_toml(s)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn import_from(path: &Path, current: &Settings) -> anyhow::Result<Settings> {
    let text = std::fs::read_to_string(path)?;
    from_import_toml(&text, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Destination, ImageFormat, Size};

    #[test]
    fn roundtrip_keeps_everything_but_the_password() {
        let mut s = Settings::default();
        s.output.destination = Destination::Clipboard;
        s.output.format = ImageFormat::Webp;
        s.capture.fixed_presets.push(Size { w: 333, h: 222 });
        s.hotkeys.region = "Ctrl+Shift+F7".into();
        s.filename.template = "shot_###".into();
        s.output.upload.host = "ftp.example.com".into();
        s.output.upload.username = "me".into();
        s.output.upload.password_protected = "SECRET".into();
        let text = to_export_toml(&s).expect("export");
        assert!(!text.contains("SECRET"));

        let back = from_import_toml(&text, &Settings::default()).expect("import");
        let mut expected = s.clone();
        expected.output.upload.password_protected.clear();
        assert_eq!(back, expected);

        // Same host and user: the current password is kept.
        let back = from_import_toml(&text, &s).expect("import");
        assert_eq!(back, s);
    }

    #[test]
    fn rejects_garbage_and_fills_defaults() {
        assert!(from_import_toml("this is = = not toml", &Settings::default()).is_err());
        let s = from_import_toml("[general]\nshow_notifications = false\n", &Settings::default()).expect("partial");
        assert!(!s.general.show_notifications);
        assert_eq!(s.hotkeys, Settings::default().hotkeys);
    }

    #[test]
    fn file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("snapforge-ie-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("s.toml");
        let mut s = Settings::default();
        s.general.show_notifications = false;
        export_to(&p, &s).expect("write");
        assert_eq!(import_from(&p, &s).expect("read"), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
