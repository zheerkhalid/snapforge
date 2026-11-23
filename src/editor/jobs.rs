//! Background work (decode, encode + save, clipboard, effects, statistics, scanning,
//! external editors) so egui frames never wait on disk I/O or encoding. Results come
//! back through a channel drained each frame.

use super::document::{DocId, ExportJob};
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
    /// A `.sfcap` project was read.
    ProjectLoaded {
        path: PathBuf,
        result: Result<(Image, Vec<crate::draw::Object>, crate::draw::Meta), String>,
    },
    ProjectSaved {
        doc: DocId,
        path: PathBuf,
        revision: u64,
        result: Result<(), String>,
    },
    /// An image file for the draw Image tool, encoded as PNG.
    Picture {
        doc: DocId,
        at: crate::draw::Pt,
        result: Result<(Vec<u8>, (u32, u32)), String>,
    },
    /// Show a message (e.g. a colour count).
    Toast(String),
    /// Private details located by text recognition in a picture of `size`.
    PrivateFound {
        doc: DocId,
        size: (u32, u32),
        result: Result<Vec<crate::redact::Hit>, String>,
    },
    /// Texts of the QR codes found in the picture (or the selection).
    QrRead(Vec<String>),
}

/// What an annotated project needs on disk (auto-save next to a saved image).
#[derive(Clone)]
pub struct ProjectData {
    pub base: Arc<Image>,
    pub objects: Vec<crate::draw::Object>,
    pub meta: crate::draw::Meta,
}

/// Write `data` next to `image_path` (same stem, `.sfcap`); if that folder is not
/// writable, into `<settings>/autosave`. Returns where it went.
pub fn autosave_project(image_path: &std::path::Path, data: &ProjectData) -> Result<PathBuf, String> {
    let next_to = image_path.with_extension(crate::draw::sfcap::EXTENSION);
    match crate::draw::save_sfcap(&next_to, &data.base, &data.objects, &data.meta) {
        Ok(()) => Ok(next_to),
        Err(e) => {
            log::debug!("editor: auto-save next to the image failed ({e:#}); using the settings folder");
            let dir = crate::settings::dir().join("autosave");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let name = next_to.file_name().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("autosave.sfcap"));
            let p = dir.join(name);
            crate::draw::save_sfcap(&p, &data.base, &data.objects, &data.meta).map_err(|e| format!("{e:#}"))?;
            Ok(p)
        }
    }
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

    /// Flatten, encode and write; with `project`, also auto-save the annotations.
    pub fn save(
        &self,
        doc: DocId,
        image: ExportJob,
        path: PathBuf,
        revision: u64,
        close_after: bool,
        project: Option<ProjectData>,
    ) {
        self.spawn("editor-save", move |j| {
            let t0 = std::time::Instant::now();
            let img = image.run();
            let result = output::save::save_image(&img, &path).map_err(|e| format!("{e:#}"));
            log::debug!("editor: saved {} in {:?}", path.display(), t0.elapsed());
            if let (Ok(()), Some(p)) = (&result, project) {
                match autosave_project(&path, &p) {
                    Ok(at) => log::info!("editor: annotations auto-saved to {}", at.display()),
                    Err(e) => log::warn!("editor: annotation auto-save failed: {e}"),
                }
            }
            j.send(JobResult::Saved { doc, path, revision, close_after, result });
        });
    }

    pub fn copy(&self, image: ExportJob) {
        self.spawn("editor-copy", move |j| {
            let img = image.run();
            let result = output::clipboard::copy_image(&img).map_err(|e| format!("{e:#}"));
            j.send(JobResult::Copied(result));
        });
    }

    /// Recognize the text of the flattened picture and locate private details in it
    /// (inside `within` when there is a selection).
    pub fn find_private(
        &self,
        doc: DocId,
        image: ExportJob,
        kinds: crate::redact::Kinds,
        within: Option<Rect>,
        lang: String,
    ) {
        self.spawn("editor-redact", move |j| {
            let img = image.run();
            let size = (img.width, img.height);
            let result = crate::ocr::recognize(&img, Some(&lang))
                .map(|ocr| super::privacy::inside(crate::redact::find(&ocr, kinds), within))
                .map_err(|e| format!("{e:#}"));
            j.send(JobResult::PrivateFound { doc, size, result });
        });
    }

    /// Decode the QR codes in the flattened picture (or selection).
    pub fn read_qr(&self, image: ExportJob) {
        self.spawn("editor-qr", move |j| {
            let img = image.run();
            j.send(JobResult::QrRead(crate::qr::texts(&crate::qr::decode(&img))));
        });
    }

    /// Flatten on a worker, then hand the pixels to `f` (deliver, pin…).
    pub fn with_export(&self, image: ExportJob, f: impl FnOnce(Image) + Send + 'static) {
        self.spawn("editor-export", move |_| {
            let img = image.run();
            f(Arc::try_unwrap(img).unwrap_or_else(|a| (*a).clone()));
        });
    }

    pub fn load_project(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.spawn("editor-load-project", move |j| {
            for path in paths {
                let result = crate::draw::load_sfcap(&path).map_err(|e| format!("{e:#}"));
                j.send(JobResult::ProjectLoaded { path, result });
            }
        });
    }

    pub fn save_project(&self, doc: DocId, data: ProjectData, path: PathBuf, revision: u64) {
        self.spawn("editor-save-project", move |j| {
            let result =
                crate::draw::save_sfcap(&path, &data.base, &data.objects, &data.meta).map_err(|e| format!("{e:#}"));
            j.send(JobResult::ProjectSaved { doc, path, revision, result });
        });
    }

    /// Decode an image file and encode it as PNG for an image object.
    pub fn load_picture(&self, doc: DocId, path: PathBuf, at: crate::draw::Pt) {
        self.spawn("editor-picture", move |j| {
            let result = output::save::load_image(&path).map_err(|e| format!("{e:#}")).and_then(|img| encode_png(&img));
            j.send(JobResult::Picture { doc, at, result });
        });
    }

    /// Encode an in-memory image (pasted) as an image object.
    pub fn picture_from_image(&self, doc: DocId, img: Image, at: crate::draw::Pt) {
        self.spawn("editor-picture", move |j| {
            let result = encode_png(&img);
            j.send(JobResult::Picture { doc, at, result });
        });
    }

    pub fn count_colors(&self, image: Arc<Image>) {
        self.spawn("editor-count-colors", move |j| {
            let n = crate::effects::count_colors(&image);
            j.send(JobResult::Toast(crate::i18n::tf("fx-count-colors-result", &[("count", &n.to_string())])));
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
            let mut result = apply(&source, &values, &ctx);
            // A freehand selection: blend through its mask (soft-edged masks blend partially).
            if let EffectTarget::Region(r, Some(mask)) = &target {
                if (result.width, result.height) == (source.width, source.height)
                    && mask.len() == (r.w.max(0) * r.h.max(0)) as usize
                    && (source.width as i32, source.height as i32) == (r.w, r.h)
                {
                    let m = crate::effects::Mask { width: source.width, height: source.height, data: (**mask).clone() };
                    result = crate::effects::apply_masked(&source, &result, &m);
                }
            }
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

    pub fn print(&self, image: ExportJob, owner: isize, title: String) {
        self.spawn("editor-print", move |j| {
            let image = image.run();
            let r = output::print::print_image(&image, owner, &title).map_err(|e| format!("{e:#}"));
            j.send(JobResult::Printed(r));
        });
    }
}

/// PNG bytes and size of an image (for draw image objects).
pub fn encode_png(img: &Image) -> Result<(Vec<u8>, (u32, u32)), String> {
    use image::ImageEncoder;
    let rgba = img.to_rgba_straight();
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, img.width, img.height, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok((out, (img.width, img.height)))
}
