//! Background work (decode, encode + save, clipboard, effects, statistics, scanning,
//! external editors) so egui frames never wait on disk I/O or encoding. Results come
//! back through a channel drained each frame.

use super::document::DocId;
use super::effects::{ApplyFn, EffectCtx, ParamValues};
use super::fileops::{self, FileProps};
use super::scan::{self, ScanError};
use super::scan_post::{self, PostOptions};
use crate::common::{Image, Rect};
use crate::output;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// Where an effect result goes.
#[derive(Clone)]
pub enum EffectTarget {
    Whole,
    /// The selection: its bounds and optional freehand mask.
    Region(Rect, Option<Arc<Vec<u8>>>),
}

pub enum JobResult {
    Loaded {
        path: PathBuf,
        /// Workspace the tab goes to (`None` = the active one).
        workspace: Option<u64>,
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
    /// Clipboard image read for Paste (`None`: no image on the clipboard).
    Pasted {
        image: Option<Image>,
        new_tab: bool,
    },
    Deleted {
        doc: DocId,
        path: PathBuf,
        result: Result<(), String>,
    },
    Props(DocId, FileProps),
    /// Unique colours of (doc, key) where key identifies the image / selection state.
    Colors {
        doc: DocId,
        key: (u64, u64),
        count: u64,
    },
    EffectApplied {
        doc: DocId,
        name: String,
        target: EffectTarget,
        result: Image,
    },
    /// A temp copy for "Edit With" was written (or the file is used directly).
    EditReady {
        doc: DocId,
        editor: usize,
        path: PathBuf,
        result: Result<(), String>,
    },
    ExternalChanged {
        doc: DocId,
        path: PathBuf,
    },
    Reloaded {
        doc: DocId,
        path: PathBuf,
        result: Result<Image, String>,
    },
    Scanned(Result<(Image, String), ScanError>),
    ScanFinished,
    Printed(Result<bool, String>),
}

#[derive(Clone)]
pub struct Jobs {
    tx: Sender<JobResult>,
    ctx: egui::Context,
}

pub struct JobQueue {
    pub jobs: Jobs,
    rx: Receiver<JobResult>,
}

impl JobQueue {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { jobs: Jobs { tx, ctx }, rx }
    }

    pub fn poll(&self) -> Vec<JobResult> {
        self.rx.try_iter().collect()
    }
}

impl Jobs {
    /// Send a result from any thread (and wake the editor).
    pub fn send(&self, r: JobResult) {
        let _ = self.tx.send(r);
        self.ctx.request_repaint();
    }

    pub fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let ctx = self.ctx.clone();
        Arc::new(move || ctx.request_repaint())
    }

    fn spawn(&self, name: &str, f: impl FnOnce(&Jobs) + Send + 'static) {
        let me = self.clone();
        let res = std::thread::Builder::new().name(name.into()).spawn(move || f(&me));
        if let Err(e) = res {
            log::error!("editor: cannot start {name} thread: {e}");
        }
    }

    /// Decode files one after another on one thread; each result arrives as it is ready.
    pub fn load(&self, paths: Vec<PathBuf>, workspace: Option<u64>) {
        if paths.is_empty() {
            return;
        }
        self.spawn("editor-load", move |j| {
            for path in paths {
                let t0 = std::time::Instant::now();
                let result = output::save::load_image(&path).map_err(|e| format!("{e:#}"));
                log::debug!("editor: decoded {} in {:?}", path.display(), t0.elapsed());
                j.send(JobResult::Loaded { path, workspace, result });
            }
        });
    }

    pub fn save(&self, doc: DocId, image: Arc<Image>, path: PathBuf, revision: u64, close_after: bool) {
        self.spawn("editor-save", move |j| {
            let t0 = std::time::Instant::now();
            let result = output::save::save_image(&image, &path).map_err(|e| format!("{e:#}"));
            log::debug!("editor: saved {} in {:?}", path.display(), t0.elapsed());
            j.send(JobResult::Saved { doc, path, revision, close_after, result });
        });
    }

    pub fn copy(&self, image: Arc<Image>) {
        self.spawn("editor-copy", move |j| {
            let result = output::clipboard::copy_image(&image).map_err(|e| format!("{e:#}"));
            j.send(JobResult::Copied(result));
        });
    }

    pub fn paste(&self, new_tab: bool) {
        self.spawn("editor-paste", move |j| {
            let image = output::clipboard::paste_image();
            j.send(JobResult::Pasted { image, new_tab });
        });
    }

    pub fn recycle(&self, doc: DocId, path: PathBuf) {
        self.spawn("editor-delete", move |j| {
            let result = fileops::recycle(&path);
            j.send(JobResult::Deleted { doc, path, result });
        });
    }

    pub fn props(&self, doc: DocId, path: Option<PathBuf>, image: Arc<Image>) {
        self.spawn("editor-props", move |j| {
            let mut p = fileops::file_props(path.as_deref(), image.width, image.height, image.has_transparency());
            j.send(JobResult::Props(doc, p.clone()));
            p.colors = Some(super::selection::unique_colors(&image, image.bounds(), |_, _| true));
            j.send(JobResult::Props(doc, p));
        });
    }

    /// Count unique colours of the selection (or the whole image).
    pub fn colors(&self, doc: DocId, key: (u64, u64), image: Arc<Image>, sel: Option<super::selection::Selection>) {
        self.spawn("editor-colors", move |j| {
            let count = match &sel {
                Some(s) => super::selection::unique_colors(&image, s.bounds, |x, y| s.contains(x, y)),
                None => super::selection::unique_colors(&image, image.bounds(), |_, _| true),
            };
            j.send(JobResult::Colors { doc, key, count });
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn effect(
        &self,
        doc: DocId,
        name: String,
        source: Arc<Image>,
        target: EffectTarget,
        apply: ApplyFn,
        values: ParamValues,
        background: [u8; 4],
    ) {
        self.spawn("editor-effect", move |j| {
            let t0 = std::time::Instant::now();
            let ctx = EffectCtx { scale: 1.0, preview: false, background };
            let result = apply(&source, &values, &ctx);
            log::debug!("editor: effect {name} took {:?}", t0.elapsed());
            j.send(JobResult::EffectApplied { doc, name, target, result });
        });
    }

    /// Write a temp copy for an external editor (`image: None` = use `path` as is).
    pub fn prepare_edit(&self, doc: DocId, editor: usize, image: Option<Arc<Image>>, path: PathBuf) {
        self.spawn("editor-edit-with", move |j| {
            let result = match image {
                Some(img) => output::save::save_image(&img, &path).map_err(|e| format!("{e:#}")),
                None => Ok(()),
            };
            j.send(JobResult::EditReady { doc, editor, path, result });
        });
    }

    pub fn reload(&self, doc: DocId, path: PathBuf) {
        self.spawn("editor-reload", move |j| {
            let result = output::save::load_image(&path).map_err(|e| format!("{e:#}"));
            j.send(JobResult::Reloaded { doc, path, result });
        });
    }

    /// Scan pages until the dialog is cancelled (`batch`) or once, post-processing
    /// each page. Runs the WIA dialog on its own STA thread.
    pub fn scan(&self, owner: isize, opts: PostOptions, batch: bool) {
        self.spawn("editor-scan", move |j| {
            let folder = fileops::temp_dir("scan");
            let mut page = 0u32;
            loop {
                let stem = format!("Scan_{}_{}", std::process::id(), page);
                match scan::scan_once(owner, &folder, &stem) {
                    Ok(paths) => {
                        for p in paths {
                            page += 1;
                            let res = output::save::load_image(&p)
                                .map(|img| (scan_post::post_process(img, &opts), format!("Scan {page}")))
                                .map_err(|e| ScanError::Failed(format!("{e:#}")));
                            let _ = std::fs::remove_file(&p);
                            j.send(JobResult::Scanned(res));
                        }
                        if !batch {
                            break;
                        }
                    }
                    Err(ScanError::Cancelled) => break,
                    Err(e) => {
                        j.send(JobResult::Scanned(Err(e)));
                        break;
                    }
                }
            }
            j.send(JobResult::ScanFinished);
        });
    }

    pub fn print(&self, image: Arc<Image>, owner: isize, title: String) {
        self.spawn("editor-print", move |j| {
            let r = output::print::print_image(&image, owner, &title).map_err(|e| format!("{e:#}"));
            j.send(JobResult::Printed(r));
        });
    }
}
