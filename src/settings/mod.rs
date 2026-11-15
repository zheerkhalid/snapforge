//! Settings: a TOML file in `%APPDATA%\SnapForge\` (or next to the EXE in portable mode).
//!
//! Global access: [`get`] for a read snapshot, [`update`] to mutate and persist.

pub mod draw;
pub mod editor_ext;
pub mod filename;
pub mod ocr;
pub mod pin;
pub mod print;
pub mod recorder;
pub mod secret;
pub mod tools;

use crate::common::win;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock, RwLockReadGuard};

// ---------------------------------------------------------------- enums

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEngine {
    /// WGC, then DXGI duplication, then GDI.
    #[default]
    Auto,
    Wgc,
    Dxgi,
    Gdi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    ActiveWindow,
    WindowObject,
    #[default]
    Region,
    Freehand,
    FullScreen,
    Scrolling,
    FixedRegion,
}

impl CaptureMode {
    pub const ALL: [CaptureMode; 7] = [
        CaptureMode::ActiveWindow,
        CaptureMode::WindowObject,
        CaptureMode::Region,
        CaptureMode::Freehand,
        CaptureMode::FullScreen,
        CaptureMode::Scrolling,
        CaptureMode::FixedRegion,
    ];

    /// Stable id used in settings, CLI and toolbar button lists.
    pub fn id(self) -> &'static str {
        match self {
            CaptureMode::ActiveWindow => "active",
            CaptureMode::WindowObject => "window",
            CaptureMode::Region => "region",
            CaptureMode::Freehand => "freehand",
            CaptureMode::FullScreen => "fullscreen",
            CaptureMode::Scrolling => "scrolling",
            CaptureMode::FixedRegion => "fixed",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == s.to_ascii_lowercase())
    }

    /// Fluent message id for the human label.
    pub fn label_key(self) -> &'static str {
        match self {
            CaptureMode::ActiveWindow => "mode-active",
            CaptureMode::WindowObject => "mode-window",
            CaptureMode::Region => "mode-region",
            CaptureMode::Freehand => "mode-freehand",
            CaptureMode::FullScreen => "mode-fullscreen",
            CaptureMode::Scrolling => "mode-scrolling",
            CaptureMode::FixedRegion => "mode-fixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    #[default]
    Editor,
    Draw,
    Clipboard,
    File,
    Printer,
    Email,
    Word,
    Excel,
    PowerPoint,
    OneNote,
    Upload,
    Program,
}

impl Destination {
    pub const ALL: [Destination; 12] = [
        Destination::Editor,
        Destination::Draw,
        Destination::Clipboard,
        Destination::File,
        Destination::Printer,
        Destination::Email,
        Destination::Word,
        Destination::Excel,
        Destination::PowerPoint,
        Destination::OneNote,
        Destination::Upload,
        Destination::Program,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Destination::Editor => "editor",
            Destination::Draw => "draw",
            Destination::Clipboard => "clipboard",
            Destination::File => "file",
            Destination::Printer => "printer",
            Destination::Email => "email",
            Destination::Word => "word",
            Destination::Excel => "excel",
            Destination::PowerPoint => "powerpoint",
            Destination::OneNote => "onenote",
            Destination::Upload => "upload",
            Destination::Program => "program",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == s.to_ascii_lowercase())
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Destination::Editor => "dest-editor",
            Destination::Draw => "dest-draw",
            Destination::Clipboard => "dest-clipboard",
            Destination::File => "dest-file",
            Destination::Printer => "dest-printer",
            Destination::Email => "dest-email",
            Destination::Word => "dest-word",
            Destination::Excel => "dest-excel",
            Destination::PowerPoint => "dest-powerpoint",
            Destination::OneNote => "dest-onenote",
            Destination::Upload => "dest-upload",
            Destination::Program => "dest-program",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
    Bmp,
    Gif,
    Tiff,
    Tga,
    Pcx,
    Webp,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 8] = [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Bmp,
        ImageFormat::Gif,
        ImageFormat::Tiff,
        ImageFormat::Tga,
        ImageFormat::Pcx,
        ImageFormat::Webp,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Gif => "gif",
            ImageFormat::Tiff => "tif",
            ImageFormat::Tga => "tga",
            ImageFormat::Pcx => "pcx",
            ImageFormat::Webp => "webp",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(ImageFormat::Png),
            "jpg" | "jpeg" | "jpe" => Some(ImageFormat::Jpeg),
            "bmp" => Some(ImageFormat::Bmp),
            "gif" => Some(ImageFormat::Gif),
            "tif" | "tiff" => Some(ImageFormat::Tiff),
            "tga" => Some(ImageFormat::Tga),
            "pcx" => Some(ImageFormat::Pcx),
            "webp" => Some(ImageFormat::Webp),
            _ => None,
        }
    }

    pub fn supports_alpha(self) -> bool {
        matches!(self, ImageFormat::Png | ImageFormat::Tiff | ImageFormat::Tga | ImageFormat::Webp | ImageFormat::Gif)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    #[default]
    Vertical,
    Horizontal,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UploadProtocol {
    #[default]
    Ftp,
    Sftp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

// ---------------------------------------------------------------- sections

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub language: String,
    pub start_with_windows: bool,
    /// When started with Windows: true = tray only, false = also show the toolbar.
    pub start_minimized: bool,
    pub theme: Theme,
    pub show_notifications: bool,
    pub log_level: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            language: "en-US".into(),
            start_with_windows: false,
            start_minimized: false,
            theme: Theme::System,
            show_notifications: true,
            log_level: "info".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollSettings {
    pub direction: ScrollDirection,
    /// Safety cap on the number of screens stitched.
    pub max_screens: u32,
    /// Minimum settle time after each scroll step.
    pub step_delay_ms: u32,
}

impl Default for ScrollSettings {
    fn default() -> Self {
        Self { direction: ScrollDirection::Vertical, max_screens: 60, step_delay_ms: 60 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    pub engine: CaptureEngine,
    /// Full-screen capture: all monitors (true) or the monitor under the cursor (false).
    pub fullscreen_all_monitors: bool,
    pub include_cursor: bool,
    /// Delay before capture in seconds (0 = none), with on-screen countdown.
    pub delay_secs: u32,
    /// Region mode: finish the capture when the mouse button is released.
    pub capture_on_release: bool,
    pub show_magnifier: bool,
    pub show_crosshair: bool,
    pub fixed_presets: Vec<Size>,
    pub fixed_index: usize,
    pub auto_capture_enabled: bool,
    pub auto_capture_interval_secs: u32,
    pub auto_capture_mode: CaptureMode,
    /// Empty = use the output folder.
    pub auto_capture_folder: String,
    pub scroll: ScrollSettings,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            engine: CaptureEngine::Auto,
            fullscreen_all_monitors: false,
            include_cursor: false,
            delay_secs: 0,
            capture_on_release: true,
            show_magnifier: true,
            show_crosshair: true,
            fixed_presets: vec![
                Size { w: 640, h: 480 },
                Size { w: 800, h: 600 },
                Size { w: 1024, h: 768 },
                Size { w: 1280, h: 720 },
                Size { w: 1920, h: 1080 },
                Size { w: 500, h: 500 },
            ],
            fixed_index: 3,
            auto_capture_enabled: false,
            auto_capture_interval_secs: 60,
            auto_capture_mode: CaptureMode::FullScreen,
            auto_capture_folder: String::new(),
            scroll: ScrollSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadSettings {
    pub protocol: UploadProtocol,
    pub host: String,
    /// 0 = protocol default (21 / 22).
    pub port: u16,
    pub username: String,
    /// DPAPI-encrypted, base64 (see [`secret`]). Never plain text.
    pub password_protected: String,
    pub remote_dir: String,
    /// Public URL prefix; the uploaded file's URL is copied when set.
    pub url_prefix: String,
    pub keep_local_copy: bool,
    /// SFTP private key file (uses the Windows OpenSSH client).
    pub sftp_key_path: String,
    pub passive: bool,
}

impl Default for UploadSettings {
    fn default() -> Self {
        Self {
            protocol: UploadProtocol::Ftp,
            host: String::new(),
            port: 0,
            username: String::new(),
            password_protected: String::new(),
            remote_dir: "/".into(),
            url_prefix: String::new(),
            keep_local_copy: true,
            sftp_key_path: String::new(),
            passive: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSettings {
    pub destination: Destination,
    /// Show the editor first; the editor offers "Continue" to the destination.
    pub preview_in_editor: bool,
    pub auto_copy: bool,
    pub timestamp: bool,
    /// Same tokens as file names ($Y $M $D $H $N $S).
    pub timestamp_format: String,
    /// Empty = `Pictures\SnapForge`.
    pub folder: String,
    pub folder_history: Vec<String>,
    pub format: ImageFormat,
    pub jpeg_quality: u8,
    pub webp_quality: u8,
    pub external_program: String,
    pub upload: UploadSettings,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            destination: Destination::Editor,
            preview_in_editor: false,
            auto_copy: false,
            timestamp: false,
            timestamp_format: "$Y-$M-$D $H:$N:$S".into(),
            folder: String::new(),
            folder_history: Vec::new(),
            format: ImageFormat::Png,
            jpeg_quality: 90,
            webp_quality: 90,
            external_program: String::new(),
            upload: UploadSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileNameSettings {
    /// Tokens: `$Y $M $D $H $N $S`, `###` counter, `@` random char, `%USERNAME%` etc.
    pub template: String,
    /// Next counter value.
    pub counter: u32,
    pub reset_daily: bool,
    /// Date (`YYYY-MM-DD`) the counter was last used, for the daily reset.
    pub last_date: String,
}

impl Default for FileNameSettings {
    fn default() -> Self {
        Self { template: "SnapForge_$Y-$M-$D_$H$N$S".into(), counter: 1, reset_daily: true, last_date: String::new() }
    }
}

/// Hotkey combos as text, e.g. `"Ctrl+Shift+PrtSc"`. Empty = unassigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    pub active_window: String,
    pub window_object: String,
    pub region: String,
    pub freehand: String,
    pub full_screen: String,
    pub scrolling: String,
    pub fixed_region: String,
    pub repeat_last: String,
    /// Take over PrtSc even when Windows maps it to the Snipping Tool (low-level hook).
    pub prtsc_override: bool,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            active_window: "Alt+PrtSc".into(),
            window_object: "Shift+PrtSc".into(),
            region: "Ctrl+PrtSc".into(),
            freehand: "Ctrl+Shift+PrtSc".into(),
            full_screen: "PrtSc".into(),
            scrolling: "Ctrl+Alt+PrtSc".into(),
            fixed_region: "Ctrl+Shift+Alt+PrtSc".into(),
            repeat_last: String::new(),
            prtsc_override: true,
        }
    }
}

impl HotkeySettings {
    pub fn for_mode(&self, m: CaptureMode) -> &str {
        match m {
            CaptureMode::ActiveWindow => &self.active_window,
            CaptureMode::WindowObject => &self.window_object,
            CaptureMode::Region => &self.region,
            CaptureMode::Freehand => &self.freehand,
            CaptureMode::FullScreen => &self.full_screen,
            CaptureMode::Scrolling => &self.scrolling,
            CaptureMode::FixedRegion => &self.fixed_region,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolbarSettings {
    pub visible: bool,
    /// Physical screen position; `None` = default (top-centre of the primary monitor).
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub big_buttons: bool,
    pub locked: bool,
    pub auto_hide: bool,
    /// Visible button ids, in order: capture mode ids plus "output", "delay", "menu".
    pub buttons: Vec<String>,
}

impl Default for ToolbarSettings {
    fn default() -> Self {
        let mut buttons: Vec<String> = CaptureMode::ALL.iter().map(|m| m.id().to_string()).collect();
        buttons.extend(["output", "delay", "menu"].map(String::from));
        Self { visible: true, x: None, y: None, big_buttons: false, locked: false, auto_hide: true, buttons }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalEditor {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub last_folder: String,
    pub recent_files: Vec<String>,
    pub external_editors: Vec<ExternalEditor>,
    /// Last window placement [x, y, w, h] in physical pixels.
    pub window: Option<[i32; 4]>,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            last_folder: String::new(),
            recent_files: Vec::new(),
            external_editors: vec![ExternalEditor { name: "MS Paint".into(), path: "mspaint.exe".into() }],
            window: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub capture: CaptureSettings,
    pub output: OutputSettings,
    pub filename: FileNameSettings,
    pub hotkeys: HotkeySettings,
    pub toolbar: ToolbarSettings,
    pub editor: EditorSettings,
    pub editor_ext: editor_ext::EditorExtSettings,
    pub draw: draw::DrawSettings,
    pub recorder: recorder::RecorderSettings,
    pub pin: pin::PinSettings,
    pub ocr: ocr::OcrSettings,
    pub tools: tools::ToolsSettings,
    pub print: print::PrintSettings,
}

impl Settings {
    /// Output folder, falling back to `Pictures\SnapForge`.
    pub fn output_folder(&self) -> PathBuf {
        if self.output.folder.trim().is_empty() {
            win::pictures_dir().join("SnapForge")
        } else {
            PathBuf::from(filename::expand_env_tokens(&self.output.folder))
        }
    }

    pub fn auto_capture_folder(&self) -> PathBuf {
        if self.capture.auto_capture_folder.trim().is_empty() {
            self.output_folder()
        } else {
            PathBuf::from(filename::expand_env_tokens(&self.capture.auto_capture_folder))
        }
    }
}

// ---------------------------------------------------------------- storage

/// `portable.flag` next to the EXE → settings live beside the EXE.
pub fn is_portable() -> bool {
    win::exe_dir().join("portable.flag").exists()
}

static DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Override the settings directory (tests, `--settings-dir`). Call before [`init`].
pub fn set_dir_override(p: PathBuf) {
    let _ = DIR_OVERRIDE.set(p);
}

pub fn dir() -> PathBuf {
    if let Some(p) = DIR_OVERRIDE.get() {
        return p.clone();
    }
    if is_portable() {
        return win::exe_dir();
    }
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(win::exe_dir).join("SnapForge")
}

pub fn path() -> PathBuf {
    dir().join("settings.toml")
}

pub fn load_from(p: &std::path::Path) -> anyhow::Result<Settings> {
    let text = std::fs::read_to_string(p)?;
    Ok(toml::from_str(&text)?)
}

pub fn save_to(s: &Settings, p: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(s)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();

/// Load settings from disk (defaults if missing or invalid). Idempotent.
pub fn init() {
    SETTINGS.get_or_init(|| {
        let p = path();
        let s = match load_from(&p) {
            Ok(s) => s,
            Err(e) => {
                if p.exists() {
                    log::warn!("settings: could not parse {}: {e}; using defaults", p.display());
                    let _ = std::fs::copy(&p, p.with_extension("toml.bad"));
                }
                Settings::default()
            }
        };
        RwLock::new(s)
    });
}

fn cell() -> &'static RwLock<Settings> {
    init();
    SETTINGS.get().expect("settings initialised")
}

/// Read access to the live settings.
pub fn get() -> RwLockReadGuard<'static, Settings> {
    cell().read().unwrap_or_else(|e| e.into_inner())
}

/// A cloned snapshot (for handing to worker threads).
pub fn snapshot() -> Settings {
    get().clone()
}

/// Mutate settings and persist them asynchronously.
pub fn update<R>(f: impl FnOnce(&mut Settings) -> R) -> R {
    let (r, copy) = {
        let mut g = cell().write().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut g);
        (r, g.clone())
    };
    persist_async(copy);
    r
}

/// Replace all settings (import / reset) and persist.
pub fn replace(s: Settings) {
    update(|cur| *cur = s);
}

fn persist_async(s: Settings) {
    let p = path();
    std::thread::Builder::new()
        .name("settings-save".into())
        .spawn(move || {
            if let Err(e) = save_to(&s, &p) {
                log::error!("settings: save failed: {e:#}");
            }
        })
        .ok();
}

/// Write the settings synchronously (used on exit).
pub fn flush() {
    let s = snapshot();
    if let Err(e) = save_to(&s, &path()) {
        log::error!("settings: save failed: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip_defaults() {
        let s = Settings::default();
        let text = toml::to_string_pretty(&s).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let s: Settings = toml::from_str("[output]\ndestination = \"clipboard\"\n").expect("parse");
        assert_eq!(s.output.destination, Destination::Clipboard);
        assert_eq!(s.hotkeys.region, "Ctrl+PrtSc");
    }

    #[test]
    fn ids_roundtrip() {
        for m in CaptureMode::ALL {
            assert_eq!(CaptureMode::from_id(m.id()), Some(m));
        }
        for d in Destination::ALL {
            assert_eq!(Destination::from_id(d.id()), Some(d));
        }
        for f in ImageFormat::ALL {
            assert_eq!(ImageFormat::from_extension(f.extension()), Some(f));
        }
    }
}
