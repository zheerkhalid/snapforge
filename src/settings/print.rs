//! Settings for Phase 2 printing and batch print. Owned by that module's agent: add fields here
//! (keep `#[serde(default)]` so older settings files still load).

use crate::compose::combine::CombineParams;
use crate::compose::pdf::PdfOptions;
use serde::{Deserialize, Serialize};

/// How each image is scaled onto the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintFit {
    /// Whole image on one page, scaled up or down to fit.
    #[default]
    FitPage,
    /// Fill the page width; tall images continue on further pages.
    FitWidth,
    /// 96 DPI screen pixels at their physical size (shrunk only if wider than the page);
    /// tall images continue on further pages.
    ActualSize,
}

impl PrintFit {
    pub const ALL: [PrintFit; 3] = [PrintFit::FitPage, PrintFit::FitWidth, PrintFit::ActualSize];

    pub fn label_key(self) -> &'static str {
        match self {
            PrintFit::FitPage => "print-fit-page",
            PrintFit::FitWidth => "print-fit-width",
            PrintFit::ActualSize => "print-actual-size",
        }
    }
}

/// Page margins in millimetres, measured from the paper edge.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarginsMm {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Default for MarginsMm {
    fn default() -> Self {
        Self { left: 12.7, top: 12.7, right: 12.7, bottom: 12.7 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintSettings {
    pub margins: MarginsMm,
    /// Header template. Tokens: `{file}`, `{path}`, `{n}`, `{total}`, `{date}`, `{time}`.
    /// Empty = no header.
    pub header: String,
    /// Footer template (same tokens). Empty = no footer.
    pub footer: String,
    /// Header and footer font size in points.
    pub header_footer_pt: f32,
    pub fit: PrintFit,
    /// When an image continues on the next page, cut at a blank row near the page boundary
    /// so no line of text is split in half.
    pub avoid_splitting_lines: bool,
    /// Centre each image horizontally (always; vertically too in `FitPage`).
    pub center: bool,
    /// Last options used in Tools > Images to PDF.
    pub last_pdf: PdfOptions,
    /// Last options used in Tools > Combine Images.
    pub last_combine: CombineParams,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            margins: MarginsMm::default(),
            header: "{file}".into(),
            footer: "Page {n} of {total}".into(),
            header_footer_pt: 9.0,
            fit: PrintFit::FitPage,
            avoid_splitting_lines: true,
            center: true,
            last_pdf: PdfOptions::default(),
            last_combine: CombineParams::default(),
        }
    }
}
