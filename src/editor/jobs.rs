//! Background work (decode, encode + save, clipboard) so egui frames never wait on disk
//! I/O or encoding. Results come back through a channel drained each frame.

use super::document::DocId;
use crate::common::Image;
use crate::output;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub enum JobResult {
    Loaded {
        path: PathBuf,
        result: Result<Image, String>,
    },
    Saved {
        doc: DocId,
        path: PathBuf,
        revision: u64,
        /// Close the tab once saved (Ctrl+W → Save).
        close_after: bool,
        result: Result<(), String>,
    },
    Copied(Result<(), String>),
}

pub struct Jobs {
    tx: Sender<JobResult>,
    rx: Receiver<JobResult>,
    ctx: egui::Context,
}

impl Jobs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, ctx }
    }

    fn spawn(&self, name: &str, f: impl FnOnce(&dyn Fn(JobResult)) + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let res = std::thread::Builder::new().name(name.into()).spawn(move || {
            let send = |r: JobResult| {
                let _ = tx.send(r);
                ctx.request_repaint();
            };
            f(&send);
        });
        if let Err(e) = res {
            log::error!("editor: cannot start {name} thread: {e}");
        }
    }

    /// Decode files one after another on one thread; each result arrives as it is ready.
    pub fn load(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.spawn("editor-load", move |send| {
            for path in paths {
                let t0 = std::time::Instant::now();
                let result = output::save::load_image(&path).map_err(|e| format!("{e:#}"));
                log::debug!("editor: decoded {} in {:?}", path.display(), t0.elapsed());
                send(JobResult::Loaded { path, result });
            }
        });
    }

    pub fn save(&self, doc: DocId, image: Arc<Image>, path: PathBuf, revision: u64, close_after: bool) {
        self.spawn("editor-save", move |send| {
            let t0 = std::time::Instant::now();
            let result = output::save::save_image(&image, &path).map_err(|e| format!("{e:#}"));
            log::debug!("editor: saved {} in {:?}", path.display(), t0.elapsed());
            send(JobResult::Saved { doc, path, revision, close_after, result });
        });
    }

    pub fn copy(&self, image: Arc<Image>) {
        self.spawn("editor-copy", move |send| {
            let result = output::clipboard::copy_image(&image).map_err(|e| format!("{e:#}"));
            send(JobResult::Copied(result));
        });
    }

    pub fn poll(&self) -> Vec<JobResult> {
        self.rx.try_iter().collect()
    }
}
