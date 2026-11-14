//! Windows.Media.Ocr wrapper: engine creation by language, Image → SoftwareBitmap and
//! result conversion. Blocking (`.get()` on the WinRT async operation): worker threads only.

use super::{BoxF, OcrError, OcrLine, OcrResult, OcrWord};
use crate::common::Image;
use anyhow::{Context, Result};
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoIncrementMTAUsage, CoInitializeEx, COINIT_MULTITHREADED};

/// Join the multithreaded apartment on this thread, and keep the MTA alive for the rest of
/// the process. The `windows` crate caches WinRT activation factories in statics; if the
/// MTA were torn down when a short-lived worker exits, the next OCR call would use a
/// dangling factory (access violation).
pub fn ensure_com() {
    static MTA: std::sync::Once = std::sync::Once::new();
    MTA.call_once(|| {
        // SAFETY: plain call; the cookie is intentionally leaked (process-lifetime MTA).
        if let Err(e) = unsafe { CoIncrementMTAUsage() } {
            log::warn!("ocr: CoIncrementMTAUsage failed: {e}");
        }
    });
    // SAFETY: plain COM initialisation; S_FALSE / RPC_E_CHANGED_MODE are fine (an STA
    // caller keeps its apartment; the blocking `.get()` contract is documented).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

pub fn languages() -> Vec<(String, String)> {
    let list = match OcrEngine::AvailableRecognizerLanguages() {
        Ok(l) => l,
        Err(e) => {
            log::warn!("ocr: AvailableRecognizerLanguages failed: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for lang in &list {
        let tag = lang.LanguageTag().map(|s| s.to_string()).unwrap_or_default();
        let name = lang.DisplayName().map(|s| s.to_string()).unwrap_or_else(|_| tag.clone());
        if !tag.is_empty() {
            out.push((tag, name));
        }
    }
    out
}

pub struct Engine {
    inner: OcrEngine,
    tag: String,
}

impl Engine {
    /// Engine for `lang` (BCP-47) when installed; otherwise the user's profile languages,
    /// then the first installed recognizer.
    pub fn new(lang: Option<&str>) -> Result<Engine> {
        if let Some(tag) = lang.map(str::trim).filter(|t| !t.is_empty()) {
            match Self::for_tag(tag) {
                Ok(Some(e)) => return Ok(e),
                Ok(None) => log::warn!("ocr: language {tag} is not installed; using the profile languages"),
                Err(e) => log::warn!("ocr: language {tag}: {e:#}"),
            }
        }
        if let Ok(inner) = OcrEngine::TryCreateFromUserProfileLanguages() {
            let tag =
                inner.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|s| s.to_string()).unwrap_or_default();
            return Ok(Engine { inner, tag });
        }
        for (tag, _) in languages() {
            if let Ok(Some(e)) = Self::for_tag(&tag) {
                return Ok(e);
            }
        }
        Err(OcrError::NoLanguage.into())
    }

    fn for_tag(tag: &str) -> Result<Option<Engine>> {
        let lang = Language::CreateLanguage(&HSTRING::from(tag)).context("invalid language tag")?;
        if !OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
            return Ok(None);
        }
        let inner = OcrEngine::TryCreateFromLanguage(&lang).context("OcrEngine::TryCreateFromLanguage")?;
        let tag = lang.LanguageTag().map(|s| s.to_string()).unwrap_or_else(|_| tag.to_string());
        Ok(Some(Engine { inner, tag }))
    }

    /// The largest width/height the engine accepts (about 10000 on current Windows).
    pub fn max_dimension(&self) -> u32 {
        OcrEngine::MaxImageDimension().unwrap_or(10000).max(1000)
    }

    /// Recognize one image no larger than [`Self::max_dimension`].
    pub fn recognize(&self, img: &Image) -> Result<OcrResult> {
        let bmp = to_software_bitmap(img)?;
        let res = self.inner.RecognizeAsync(&bmp).context("RecognizeAsync")?.get().context("OCR failed")?;
        let mut lines = Vec::new();
        for line in &res.Lines()? {
            let mut words = Vec::new();
            for w in &line.Words()? {
                let r = w.BoundingRect()?;
                words.push(OcrWord {
                    text: w.Text()?.to_string(),
                    rect: BoxF { x: r.X, y: r.Y, w: r.Width, h: r.Height },
                });
            }
            if !words.is_empty() {
                lines.push(OcrLine { words });
            }
        }
        let angle = res.TextAngle().and_then(|a| a.Value()).ok();
        Ok(OcrResult { lines, language: self.tag.clone(), angle })
    }
}

/// Premultiplied BGRA8 SoftwareBitmap copied from the image (via a DataWriter buffer).
fn to_software_bitmap(img: &Image) -> Result<SoftwareBitmap> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&img.data)?;
    let buf = writer.DetachBuffer()?;
    let bmp = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buf,
        BitmapPixelFormat::Bgra8,
        img.width as i32,
        img.height as i32,
        BitmapAlphaMode::Premultiplied,
    )
    .context("SoftwareBitmap::CreateCopyWithAlphaFromBuffer")?;
    Ok(bmp)
}
