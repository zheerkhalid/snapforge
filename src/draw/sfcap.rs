//! `.sfcap` project files: a zip with
//! - `original.png`: the base image (lossless, alpha kept),
//! - `annotations.json`: `{"version":1,"objects":[...]}` (every annotation, still editable),
//! - `metadata.json`: size, creation time, app version, source and title.
//!
//! Writes are atomic (temp file + rename). Readers ignore unknown JSON fields, and refuse
//! annotation versions newer than [`ANNOTATIONS_VERSION`].

use super::model::Object;
use crate::common::Image;
use crate::output::save;
use crate::settings::ImageFormat;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use std::path::Path;

pub const ANNOTATIONS_VERSION: u32 = 1;
pub const EXTENSION: &str = "sfcap";

const ORIGINAL: &str = "original.png";
const ANNOTATIONS: &str = "annotations.json";
const METADATA: &str = "metadata.json";
/// Refuse absurd entries (zip bombs, corrupt headers).
const MAX_ENTRY: u64 = 1 << 30;

/// Project metadata (`metadata.json`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub width: u32,
    pub height: u32,
    /// Local time, `YYYY-MM-DD HH:MM:SS`.
    pub created: String,
    pub app_version: String,
    /// Where the capture came from (e.g. "region", "window: Notepad", a file path).
    pub source: String,
    pub title: String,
}

impl Meta {
    /// Metadata for a new project now.
    pub fn new(source: &str, title: &str) -> Self {
        Self {
            width: 0,
            height: 0,
            created: crate::common::time::DateTime::now_local().display(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            source: source.to_string(),
            title: title.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Annotations {
    version: u32,
    #[serde(default)]
    objects: Vec<Object>,
}

/// Annotation list as versioned JSON.
pub fn annotations_to_json(objects: &[Object]) -> Result<String> {
    #[derive(Serialize)]
    struct Out<'a> {
        version: u32,
        objects: &'a [Object],
    }
    serde_json::to_string_pretty(&Out { version: ANNOTATIONS_VERSION, objects })
        .context("could not serialize annotations")
}

/// Parse versioned annotation JSON.
pub fn annotations_from_json(json: &str) -> Result<Vec<Object>> {
    let a: Annotations = serde_json::from_str(json).context("annotations.json is not valid")?;
    if a.version > ANNOTATIONS_VERSION {
        bail!("this project was saved by a newer SnapForge (annotation format {})", a.version);
    }
    Ok(a.objects)
}

/// Encode a project to bytes.
pub fn to_bytes(base: &Image, objects: &[Object], meta: &Meta) -> Result<Vec<u8>> {
    ensure!(!base.is_empty(), "cannot save an empty image");
    let png = save::encode(base, ImageFormat::Png, 100)?;
    let mut meta = meta.clone();
    meta.width = base.width;
    meta.height = base.height;
    if meta.app_version.is_empty() {
        meta.app_version = env!("CARGO_PKG_VERSION").to_string();
    }
    let ann = annotations_to_json(objects)?;
    let meta_json = serde_json::to_string_pretty(&meta).context("could not serialize metadata")?;

    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        // PNG is already compressed: store it.
        zip.start_file(ORIGINAL, stored)?;
        zip.write_all(&png)?;
        zip.start_file(ANNOTATIONS, deflated)?;
        zip.write_all(ann.as_bytes())?;
        zip.start_file(METADATA, deflated)?;
        zip.write_all(meta_json.as_bytes())?;
        zip.finish()?;
    }
    Ok(buf.into_inner())
}

/// Decode a project from bytes.
pub fn from_bytes(bytes: &[u8]) -> Result<(Image, Vec<Object>, Meta)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("not a SnapForge project (.sfcap) file")?;
    let png = read_entry(&mut zip, ORIGINAL)?.with_context(|| format!("project is missing {ORIGINAL}"))?;
    let base = save::decode(&png, "png").context("project image is damaged")?;
    let objects = match read_entry(&mut zip, ANNOTATIONS)? {
        Some(b) => annotations_from_json(&String::from_utf8_lossy(&b))?,
        None => Vec::new(),
    };
    let mut meta = match read_entry(&mut zip, METADATA)? {
        Some(b) => serde_json::from_slice::<Meta>(&b).unwrap_or_else(|e| {
            log::warn!("sfcap: metadata.json ignored: {e}");
            Meta::default()
        }),
        None => Meta::default(),
    };
    meta.width = base.width;
    meta.height = base.height;
    Ok((base, objects, meta))
}

fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Option<Vec<u8>>> {
    let mut f = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not read {name}")),
    };
    ensure!(f.size() <= MAX_ENTRY, "{name} is too large");
    let mut out = Vec::with_capacity(f.size() as usize);
    f.by_ref().take(MAX_ENTRY + 1).read_to_end(&mut out).with_context(|| format!("could not read {name}"))?;
    ensure!(out.len() as u64 <= MAX_ENTRY, "{name} is too large");
    Ok(Some(out))
}

/// Save a project atomically.
pub fn save_sfcap(path: &Path, base: &Image, objects: &[Object], meta: &Meta) -> Result<()> {
    let bytes = to_bytes(base, objects, meta).with_context(|| format!("could not save {}", path.display()))?;
    save::write_atomic(path, &bytes)?;
    log::info!("saved project {} ({} objects)", path.display(), objects.len());
    Ok(())
}

/// Load a project.
pub fn load_sfcap(path: &Path) -> Result<(Image, Vec<Object>, Meta)> {
    let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    from_bytes(&bytes).with_context(|| format!("could not open {}", path.display()))
}

/// True if the path has the `.sfcap` extension.
pub fn is_sfcap(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}
