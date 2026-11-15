//! Video editor (Phase 4): trim and cut MP4/WMV recordings, add timed annotations, zoom & pan
//! and title cards, export MP4 (H.264/AAC) or animated GIF, save re-editable `.sfvid` projects.
//!
//! Hosting (inside the editor's eframe app, like `settings_ui::SettingsWindow`):
//! ```ignore
//! let ve = video_editor::VideoEditorWindow::new();   // keep it in the eframe app
//! ve.open(path);                                      // on AppCommand::OpenVideoEditor(path)
//! ve.ui(ctx);                                         // every frame of the host app
//! ```
//! It is a *deferred* viewport: once open it repaints on its own (playback, background jobs),
//! even while the host's root window is hidden. The host must keep calling
//! [`VideoEditorWindow::ui`] so egui keeps the viewport alive (and drops it after closing).
//!
//! Layout:
//! * `window.rs`     – session state, loading, undo, playback, actions, hosting glue
//! * `ui_panels.rs`  – toolbar, tool palette, inspector, segment list, export dialog
//! * `ui_preview.rs` – preview (frame + zoom + annotations), annotation and zoom-key editing
//! * `ui_timeline.rs`– transport and timeline tracks
//! * `model.rs`      – segments, title cards, zoom keys, edit state (source-time based)
//! * `timeline.rs`   – output timeline math (cuts, trims, source ↔ output mapping)
//! * `zoom.rs`       – keyframe interpolation · `compose.rs` – CPU frame composition
//! * `mf.rs`         – Media Foundation reader / writer · `decoder.rs` – playback frame cache
//! * `analysis.rs`   – thumbnails and waveform · `audio_out.rs` – WASAPI preview audio
//! * `export.rs`     – MP4 / GIF export and still frames · `gif_export.rs` – GIF encoder
//! * `project.rs`    – `.sfvid` files · `dialogs.rs` – file dialogs

mod analysis;
mod audio_out;
pub mod compose;
mod decoder;
mod dialogs;
pub mod export;
pub mod gif_export;
pub mod mf;
pub mod model;
pub mod project;
pub mod timeline;
mod ui_panels;
mod ui_preview;
mod ui_timeline;
mod window;
pub mod zoom;

use eframe::egui::{self, ViewportCommand, ViewportId};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use window::{Inner, Session};

/// Handle to the video editor window. Cheap to clone; all clones share one window.
#[derive(Clone, Default)]
pub struct VideoEditorWindow {
    inner: Arc<Mutex<Inner>>,
}

impl VideoEditorWindow {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn viewport_id() -> ViewportId {
        window::viewport_id()
    }

    /// Open the window (or bring it to the front), optionally loading a video (`.mp4`, `.wmv`,
    /// …) or a `.sfvid` project.
    pub fn open(&self, path: Option<PathBuf>) {
        let mut g = self.lock();
        if g.session.is_none() {
            g.session = Some(Session::new());
        }
        if path.is_some() {
            g.pending_open = path;
        }
        g.raise = true;
    }

    pub fn is_open(&self) -> bool {
        self.lock().session.is_some()
    }

    /// Call on every frame of the host app (any viewport's frame is fine).
    pub fn ui(&mut self, ctx: &egui::Context) {
        let (builder, raise) = {
            let mut g = self.lock();
            let raise = std::mem::take(&mut g.raise);
            match g.session.as_ref() {
                Some(s) => (s.builder.clone(), raise),
                None => return,
            }
        };
        let id = Self::viewport_id();
        if raise {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Visible(true));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Focus);
            ctx.request_repaint_of(id);
        }
        let inner = self.inner.clone();
        ctx.show_viewport_deferred(id, builder, move |ctx, class| {
            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
            g.frame(ctx, class);
        });
    }
}
