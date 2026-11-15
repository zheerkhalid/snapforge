//! Settings for the Phase 4 screen recorder (and the video editor later).
//! Every struct uses `#[serde(default)]` so older settings files still load.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VideoFormat {
    /// H.264 + AAC in MP4 (fragmented while recording, so a crash loses at most a fragment).
    #[default]
    Mp4,
    /// WMV9 + WMA in ASF.
    Wmv,
    /// No video: the mixed audio as MP3.
    Mp3AudioOnly,
}

impl VideoFormat {
    pub const ALL: [VideoFormat; 3] = [VideoFormat::Mp4, VideoFormat::Wmv, VideoFormat::Mp3AudioOnly];

    pub fn extension(self) -> &'static str {
        match self {
            VideoFormat::Mp4 => "mp4",
            VideoFormat::Wmv => "wmv",
            VideoFormat::Mp3AudioOnly => "mp3",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            VideoFormat::Mp4 => "rec-format-mp4",
            VideoFormat::Wmv => "rec-format-wmv",
            VideoFormat::Mp3AudioOnly => "rec-format-mp3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VideoQuality {
    Low,
    #[default]
    Medium,
    High,
    /// Use `video_bitrate_kbps`.
    Custom,
}

impl VideoQuality {
    pub const ALL: [VideoQuality; 4] =
        [VideoQuality::Low, VideoQuality::Medium, VideoQuality::High, VideoQuality::Custom];

    /// Bits per pixel per frame used to derive the bitrate from size and frame rate.
    pub fn bits_per_pixel(self) -> f64 {
        match self {
            VideoQuality::Low => 0.05,
            VideoQuality::Medium | VideoQuality::Custom => 0.10,
            VideoQuality::High => 0.18,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EncoderPreference {
    /// Hardware MFT when one is available, otherwise software.
    #[default]
    Auto,
    Hardware,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebcamShape {
    #[default]
    Circle,
    RoundedRect,
    Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleTarget {
    #[default]
    FullScreen,
    /// `ScheduleSettings::region` in virtual-screen pixels.
    Region,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebcamSettings {
    pub enabled: bool,
    /// Symbolic link of the camera (empty = first camera). See `recorder::list_cameras`.
    pub device_id: String,
    pub corner: Corner,
    /// Width of the picture-in-picture as a percentage of the video width.
    pub size_percent: u32,
    pub shape: WebcamShape,
    /// Distance from the video edges, in output pixels.
    pub margin: u32,
    pub mirror: bool,
}

impl Default for WebcamSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_id: String::new(),
            corner: Corner::BottomRight,
            size_percent: 20,
            shape: WebcamShape::Circle,
            margin: 16,
            mirror: true,
        }
    }
}

/// Floating control bar colours, `#RRGGBB` (or `#AARRGGBB`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlBarSettings {
    pub background: String,
    pub foreground: String,
    pub accent: String,
    pub large: bool,
    /// Last position (physical pixels); `None` = top centre of the recorded monitor.
    pub x: Option<i32>,
    pub y: Option<i32>,
}

impl Default for ControlBarSettings {
    fn default() -> Self {
        Self {
            background: "#E61E1F26".into(),
            foreground: "#F2F2F5".into(),
            accent: "#E5484D".into(),
            large: false,
            x: None,
            y: None,
        }
    }
}

/// One scheduled recording. Times are local wall clock, `YYYY-MM-DD HH:MM`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleSettings {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub target: ScheduleTarget,
    /// [x, y, w, h] for `ScheduleTarget::Region`.
    pub region: [i32; 4],
    /// Put the PC to sleep when the scheduled recording finishes.
    pub sleep_after: bool,
}

impl Default for ScheduleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            start: String::new(),
            end: String::new(),
            target: ScheduleTarget::FullScreen,
            region: [0, 0, 1280, 720],
            sleep_after: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderSettings {
    // ---- video
    pub format: VideoFormat,
    /// Frames per second, 1..=60.
    pub fps: u32,
    pub quality: VideoQuality,
    /// Used when `quality` is `Custom`.
    pub video_bitrate_kbps: u32,
    pub encoder: EncoderPreference,
    /// AAC / WMA / MP3 bitrate.
    pub audio_bitrate_kbps: u32,
    /// Experimental: hand GPU textures straight to the encoder (zero copy). Off by
    /// default: it intermittently deadlocked with some drivers; the default reads the
    /// GPU-converted NV12 frame back to memory, which the hardware encoder takes as well.
    pub zero_copy_gpu: bool,
    /// Seconds between key frames (the MP4 fragment length, so also the most a crash can lose).
    pub keyframe_interval_secs: u32,

    // ---- capture
    pub show_cursor: bool,
    /// Fixed-size regions: the region follows the mouse.
    pub follow_mouse: bool,
    pub highlight_clicks: bool,
    /// `#RRGGBB` / `#AARRGGBB`.
    pub highlight_color: String,
    /// Ring diameter in pixels.
    pub highlight_size: u32,
    pub click_sounds: bool,
    /// 0..=100.
    pub click_volume: u32,
    /// Countdown before recording starts (0 = none).
    pub countdown_secs: u32,
    /// Zoom factor for the F5 live zoom.
    pub zoom_factor: f32,

    // ---- audio
    pub record_system_audio: bool,
    pub record_microphone: bool,
    /// Endpoint id (empty = default communications/console capture device).
    pub mic_device_id: String,
    /// Microphone volume 0..=200 %.
    pub mic_volume: u32,
    /// System audio volume 0..=200 %.
    pub system_volume: u32,
    /// Automatic gain control on the microphone.
    pub auto_boost: bool,
    /// Follow the default output device when it changes during a recording.
    pub auto_switch_output: bool,

    pub webcam: WebcamSettings,
    pub control_bar: ControlBarSettings,

    // ---- output
    /// Empty = `Videos\SnapForge`.
    pub folder: String,
    /// Empty = reuse the screenshot file-name template.
    pub filename_template: String,
    /// Open the folder / play the file when a recording finishes.
    pub open_after: bool,

    pub schedule: ScheduleSettings,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            format: VideoFormat::Mp4,
            fps: 30,
            quality: VideoQuality::Medium,
            video_bitrate_kbps: 8000,
            encoder: EncoderPreference::Auto,
            audio_bitrate_kbps: 192,
            zero_copy_gpu: false,
            keyframe_interval_secs: 2,
            show_cursor: true,
            follow_mouse: false,
            highlight_clicks: true,
            highlight_color: "#C0FFC800".into(),
            highlight_size: 48,
            click_sounds: false,
            click_volume: 50,
            countdown_secs: 0,
            zoom_factor: 2.0,
            record_system_audio: true,
            record_microphone: false,
            mic_device_id: String::new(),
            mic_volume: 100,
            system_volume: 100,
            auto_boost: false,
            auto_switch_output: true,
            webcam: WebcamSettings::default(),
            control_bar: ControlBarSettings::default(),
            folder: String::new(),
            filename_template: String::new(),
            open_after: false,
            schedule: ScheduleSettings::default(),
        }
    }
}

/// Frame rates offered in the settings UI.
pub const FPS_CHOICES: [u32; 7] = [10, 15, 20, 24, 25, 30, 60];

/// Parse `#RRGGBB` or `#AARRGGBB` into straight-alpha `[r, g, b, a]`.
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]),
        8 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]),
        _ => None,
    }
}

impl RecorderSettings {
    pub fn fps_clamped(&self) -> u32 {
        self.fps.clamp(1, 60)
    }

    /// Video bitrate in bits per second for an output of `w`x`h`.
    pub fn video_bitrate(&self, w: u32, h: u32) -> u32 {
        if self.quality == VideoQuality::Custom {
            return self.video_bitrate_kbps.clamp(100, 200_000) * 1000;
        }
        let bps = w as f64 * h as f64 * self.fps_clamped() as f64 * self.quality.bits_per_pixel();
        (bps as u32).clamp(500_000, 100_000_000)
    }

    pub fn output_folder(&self) -> std::path::PathBuf {
        if self.folder.trim().is_empty() {
            crate::common::win::known_folder(&windows::Win32::UI::Shell::FOLDERID_Videos)
                .unwrap_or_else(crate::common::win::documents_dir)
                .join("SnapForge")
        } else {
            std::path::PathBuf::from(super::filename::expand_env_tokens(&self.folder))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#FF0000"), Some([255, 0, 0, 255]));
        assert_eq!(parse_color("80102030"), Some([0x10, 0x20, 0x30, 0x80]));
        assert_eq!(parse_color("#12"), None);
    }

    #[test]
    fn toml_roundtrip() {
        let s = RecorderSettings::default();
        let t = toml::to_string_pretty(&s).expect("ser");
        let back: RecorderSettings = toml::from_str(&t).expect("de");
        assert_eq!(s, back);
        let partial: RecorderSettings = toml::from_str("fps = 60\n[webcam]\nenabled = true\n").expect("de");
        assert_eq!(partial.fps, 60);
        assert!(partial.webcam.enabled);
        assert_eq!(partial.webcam.size_percent, 20);
    }

    #[test]
    fn bitrate() {
        let s = RecorderSettings::default();
        let b = s.video_bitrate(1920, 1080);
        assert!(b > 4_000_000 && b < 10_000_000, "{b}");
    }
}
