//! `.sfvid` project files: JSON with the source path and the whole edit state.
//!
//! ```json
//! { "app": "SnapForge", "version": 1, "source": "C:\\clips\\demo.mp4",
//!   "source_info": { "duration_ms": 12000, "width": 1920, "height": 1080, "fps": 30.0 },
//!   "segments": [...], "titles": [...], "zoom": [...], "annotations": [draw::Object...] }
//! ```
//! Readers ignore unknown fields and refuse newer versions. Written atomically.

use super::model::EditState;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
pub const EXTENSION: &str = "sfvid";

/// Facts about the source recorded when the project was saved (informational; the video is
/// probed again on open).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    #[serde(default)]
    pub app: String,
    pub version: u32,
    pub source: PathBuf,
    #[serde(default)]
    pub source_info: SourceInfo,
    #[serde(flatten)]
    pub edit: EditState,
}

impl ProjectFile {
    pub fn new(source: PathBuf, source_info: SourceInfo, edit: EditState) -> Self {
        Self { app: "SnapForge".into(), version: VERSION, source, source_info, edit }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("could not serialize the project")
    }

    pub fn from_json(s: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Probe {
            version: u32,
        }
        let probe: Probe = serde_json::from_str(s).context("not a SnapForge video project")?;
        if probe.version > VERSION {
            bail!("this project was saved by a newer version of SnapForge (format {})", probe.version);
        }
        serde_json::from_str(s).context("the project file is damaged")
    }

    /// The source video: the stored path, or a file with the same name next to the project
    /// (projects moved together with their video).
    pub fn resolve_source(&self, project_path: &Path) -> PathBuf {
        if self.source.exists() {
            return self.source.clone();
        }
        if let (Some(dir), Some(name)) = (project_path.parent(), self.source.file_name()) {
            let p = dir.join(name);
            if p.exists() {
                return p;
            }
        }
        self.source.clone()
    }
}

pub fn save(path: &Path, project: &ProjectFile) -> Result<()> {
    let json = project.to_json()?;
    crate::output::save::write_atomic(path, json.as_bytes())
}

pub fn load(path: &Path) -> Result<ProjectFile> {
    let s = std::fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    ProjectFile::from_json(&s)
}

#[cfg(test)]
mod tests {
    use super::super::model::*;
    use super::*;
    use crate::draw::{self, Pt, Timing, Tool, ToolOptions};

    fn sample() -> ProjectFile {
        let mut edit = EditState::new(10_000);
        crate::video_editor::timeline::cut(&mut edit.segments, &mut edit.titles, 2_000, 3_000);
        edit.titles.push(TitleCard {
            id: 1,
            anchor: Anchor::Source(5_000),
            title: "Hello \u{1F600}".into(),
            subtitle: "sub".into(),
            background: Background::Solid { color: draw::Color::rgb(1, 2, 3) },
            ..TitleCard::default()
        });
        edit.titles.push(TitleCard { id: 2, anchor: Anchor::End, ..TitleCard::default() });
        edit.zoom.push(ZoomKey { time_ms: 4_000, cx: 0.3, cy: 0.7, scale: 2.5, transition_ms: 400 });
        for (i, tool) in
            [Tool::Arrow, Tool::Rect, Tool::Text, Tool::Step, Tool::Blur, Tool::Highlighter].into_iter().enumerate()
        {
            let style = draw::default_style(tool);
            let opts = ToolOptions::for_tool(tool);
            let mut o = draw::create(tool, i as u64 + 1, &style, &opts, Pt::new(10.0, 10.0), Pt::new(200.0, 90.0), &[])
                .expect("object");
            o.timing = Some(Timing { start_ms: i as u64 * 1000, end_ms: i as u64 * 1000 + 2500 });
            edit.annotations.push(o);
        }
        ProjectFile::new(
            PathBuf::from(r"C:\videos\demo clip.mp4"),
            SourceInfo { duration_ms: 10_000, width: 1280, height: 720, fps: 30.0 },
            edit,
        )
    }

    #[test]
    fn json_round_trip() {
        let p = sample();
        let json = p.to_json().expect("json");
        assert!(json.contains("\"version\": 1"));
        let back = ProjectFile::from_json(&json).expect("parse");
        assert_eq!(back, p);
    }

    #[test]
    fn file_round_trip_and_source_next_to_project() {
        let dir = std::env::temp_dir().join(format!("sfvid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("p.sfvid");
        let mut p = sample();
        p.source = PathBuf::from(r"Z:\nowhere\clip.mp4");
        save(&path, &p).expect("save");
        let back = load(&path).expect("load");
        assert_eq!(back, p);
        assert_eq!(back.resolve_source(&path), p.source);
        std::fs::write(dir.join("clip.mp4"), b"x").expect("write");
        assert_eq!(back.resolve_source(&path), dir.join("clip.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_newer_and_garbage_and_ignores_unknown_fields() {
        assert!(ProjectFile::from_json("{\"version\": 99, \"source\": \"a\"}").is_err());
        assert!(ProjectFile::from_json("not json").is_err());
        let p = ProjectFile::from_json("{\"version\": 1, \"source\": \"a.mp4\", \"future\": [1,2]}").expect("ok");
        assert!(p.edit.segments.is_empty());
    }
}
