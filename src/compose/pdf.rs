//! Images to a multi-page PDF (pdf-writer). Long images are split across pages at quiet rows.

use super::split::split_image_rows;
use crate::common::{Image, Rect};
use anyhow::{bail, ensure, Context, Result};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref, TextStr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageSize {
    #[default]
    A4,
    Letter,
    Legal,
    A3,
    /// `custom_width_mm` x `custom_height_mm`.
    Custom,
    /// Each page is the size of its image (plus margins).
    FitToImage,
}

impl PageSize {
    pub const ALL: [PageSize; 6] =
        [PageSize::A4, PageSize::Letter, PageSize::Legal, PageSize::A3, PageSize::Custom, PageSize::FitToImage];

    /// Portrait size in points, if fixed.
    pub fn points(self) -> Option<(f32, f32)> {
        match self {
            PageSize::A4 => Some((595.276, 841.89)),
            PageSize::Letter => Some((612.0, 792.0)),
            PageSize::Legal => Some((612.0, 1008.0)),
            PageSize::A3 => Some((841.89, 1190.551)),
            PageSize::Custom | PageSize::FitToImage => None,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            PageSize::A4 => "pdf-page-a4",
            PageSize::Letter => "pdf-page-letter",
            PageSize::Legal => "pdf-page-legal",
            PageSize::A3 => "pdf-page-a3",
            PageSize::Custom => "pdf-page-custom",
            PageSize::FitToImage => "pdf-page-fit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOrientation {
    /// Landscape for wide images, portrait otherwise.
    #[default]
    Auto,
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfQuality {
    /// JPEG (DCTDecode) at `jpeg_quality`: small files.
    #[default]
    Jpeg,
    /// Deflate (FlateDecode): exact pixels, best for text screenshots.
    Lossless,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfOptions {
    pub page_size: PageSize,
    pub custom_width_mm: f32,
    pub custom_height_mm: f32,
    pub orientation: PageOrientation,
    pub margin_mm: f32,
    pub quality: PdfQuality,
    pub jpeg_quality: u8,
    /// Split images taller than a page across pages (otherwise they shrink to fit).
    pub split_long_images: bool,
    /// Document title (metadata).
    pub title: String,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            page_size: PageSize::A4,
            custom_width_mm: 210.0,
            custom_height_mm: 297.0,
            orientation: PageOrientation::Auto,
            margin_mm: 10.0,
            quality: PdfQuality::Jpeg,
            jpeg_quality: 88,
            split_long_images: true,
            title: String::new(),
        }
    }
}

/// An input: a file to load, or an image already in memory.
#[derive(Debug, Clone)]
pub enum PdfSource {
    Path(PathBuf),
    Image(Image),
}

/// Points per pixel at the screen's nominal 96 DPI. Images are never enlarged beyond this.
pub const PT_PER_PX: f32 = 72.0 / 96.0;
/// The largest page PDF readers accept (200 inches).
const MAX_PAGE_PT: f32 = 14400.0;

fn mm_to_pt(mm: f32) -> f32 {
    mm / 25.4 * 72.0
}

/// One output page: the source rows to show and where they go (points, origin bottom-left).
#[derive(Debug, Clone, PartialEq)]
pub struct PdfPagePlan {
    pub page_w: f32,
    pub page_h: f32,
    /// Source rows `y0..y1` of the image.
    pub y0: u32,
    pub y1: u32,
    /// Placement of the image slice in points: x, y (bottom), width, height.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Pure page layout for one `w`x`h` image. `split` returns the chunk end rows for a given
/// maximum chunk height (see [`super::split`]).
pub fn plan_pages(w: u32, h: u32, opts: &PdfOptions, split: impl Fn(u32) -> Vec<u32>) -> Vec<PdfPagePlan> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let m = mm_to_pt(opts.margin_mm.clamp(0.0, 100.0));
    let (wf, hf) = (w as f32, h as f32);
    if opts.page_size == PageSize::FitToImage {
        let s = PT_PER_PX.min((MAX_PAGE_PT - 2.0 * m) / wf).min((MAX_PAGE_PT - 2.0 * m) / hf);
        let (iw, ih) = (wf * s, hf * s);
        return vec![PdfPagePlan {
            page_w: iw + 2.0 * m,
            page_h: ih + 2.0 * m,
            y0: 0,
            y1: h,
            x: m,
            y: m,
            w: iw,
            h: ih,
        }];
    }
    let (pw, ph) = match opts.page_size.points() {
        Some(p) => p,
        None => {
            (mm_to_pt(opts.custom_width_mm.clamp(10.0, 5000.0)), mm_to_pt(opts.custom_height_mm.clamp(10.0, 5000.0)))
        }
    };
    let (short, long) = (pw.min(ph), pw.max(ph));
    let fit_w_portrait = (short - 2.0 * m) / wf;
    let landscape = match opts.orientation {
        PageOrientation::Portrait => false,
        PageOrientation::Landscape => true,
        // Wide images go landscape, unless they will be split across pages anyway.
        PageOrientation::Auto => w > h && !(opts.split_long_images && hf * fit_w_portrait > long - 2.0 * m),
    };
    let (pw, ph) = if landscape { (long, short) } else { (short, long) };
    let (aw, ah) = ((pw - 2.0 * m).max(1.0), (ph - 2.0 * m).max(1.0));
    let sw = PT_PER_PX.min(aw / wf);
    if hf * sw <= ah || !opts.split_long_images {
        // One page, centred.
        let s = sw.min(ah / hf);
        let (iw, ih) = (wf * s, hf * s);
        return vec![PdfPagePlan {
            page_w: pw,
            page_h: ph,
            y0: 0,
            y1: h,
            x: m + (aw - iw) / 2.0,
            y: m + (ah - ih) / 2.0,
            w: iw,
            h: ih,
        }];
    }
    // Split: full width, chunks top-aligned.
    let max_rows = ((ah / sw).floor() as u32).max(1);
    let mut pages = Vec::new();
    let mut y0 = 0;
    for y1 in split(max_rows) {
        let y1 = y1.clamp(y0 + 1, h);
        let ih = (y1 - y0) as f32 * sw;
        let iw = wf * sw;
        pages.push(PdfPagePlan {
            page_w: pw,
            page_h: ph,
            y0,
            y1,
            x: m + (aw - iw) / 2.0,
            y: ph - m - ih,
            w: iw,
            h: ih,
        });
        y0 = y1;
        if y0 >= h {
            break;
        }
    }
    pages
}

/// Write the PDF to `out_path` (atomically). Returns the number of pages.
pub fn images_to_pdf(sources: &[PdfSource], opts: &PdfOptions, out_path: &Path) -> Result<usize> {
    let (bytes, pages) = images_to_pdf_bytes(sources, opts)?;
    crate::output::save::write_atomic(out_path, &bytes)?;
    log::info!("pdf: wrote {} ({pages} pages, {} bytes)", out_path.display(), bytes.len());
    Ok(pages)
}

/// Build the PDF in memory: (bytes, page count).
pub fn images_to_pdf_bytes(sources: &[PdfSource], opts: &PdfOptions) -> Result<(Vec<u8>, usize)> {
    ensure!(!sources.is_empty(), "no images to put in the PDF");
    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let tree_id = Ref::new(2);
    let info_id = Ref::new(3);
    let mut next = 4;
    let mut alloc = || {
        let r = Ref::new(next);
        next += 1;
        r
    };
    let mut page_ids = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        let img = match src {
            PdfSource::Path(p) => crate::output::save::load_image(p)?,
            PdfSource::Image(img) => img.clone(),
        };
        if img.is_empty() {
            log::warn!("pdf: skipping empty image #{i}");
            continue;
        }
        let plans = plan_pages(img.width, img.height, opts, |max| split_image_rows(&img, max, true));
        for plan in plans {
            let (page_id, content_id, image_id) = (alloc(), alloc(), alloc());
            let slice = img.crop(Rect::new(0, plan.y0 as i32, img.width as i32, (plan.y1 - plan.y0) as i32));
            let (filter, data) = encode_slice(&slice, opts)?;
            let mut x = pdf.image_xobject(image_id, &data);
            x.filter(filter);
            x.width(slice.width as i32);
            x.height(slice.height as i32);
            x.color_space().device_rgb();
            x.bits_per_component(8);
            x.finish();

            let name = Name(b"Im0");
            let mut content = Content::new();
            content.save_state();
            content.transform([plan.w, 0.0, 0.0, plan.h, plan.x, plan.y]);
            content.x_object(name);
            content.restore_state();
            pdf.stream(content_id, &content.finish());

            let mut page = pdf.page(page_id);
            page.media_box(PdfRect::new(0.0, 0.0, plan.page_w, plan.page_h));
            page.parent(tree_id);
            page.contents(content_id);
            page.resources().x_objects().pair(name, image_id);
            page.finish();
            page_ids.push(page_id);
        }
    }
    if page_ids.is_empty() {
        bail!("none of the images could be added to the PDF");
    }
    let count = page_ids.len();
    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id).kids(page_ids).count(count as i32);
    let mut info = pdf.document_info(info_id);
    if !opts.title.trim().is_empty() {
        info.title(TextStr(&opts.title));
    }
    info.creator(TextStr("SnapForge"));
    info.producer(TextStr("SnapForge (zheer.dev)"));
    info.finish();
    Ok((pdf.finish(), count))
}

fn encode_slice(img: &Image, opts: &PdfOptions) -> Result<(Filter, Vec<u8>)> {
    let rgb = img.to_rgb_over_white();
    match opts.quality {
        PdfQuality::Jpeg => {
            use image::ImageEncoder;
            let mut out = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, opts.jpeg_quality.clamp(1, 100))
                .write_image(&rgb, img.width, img.height, image::ExtendedColorType::Rgb8)
                .context("JPEG encoding for the PDF failed")?;
            Ok((Filter::DctDecode, out))
        }
        PdfQuality::Lossless => Ok((Filter::FlateDecode, miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6))),
    }
}
