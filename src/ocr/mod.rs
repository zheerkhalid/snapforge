//! Capture Text (OCR) with Windows.Media.Ocr.
//!
//! CONTRACT:
//! * [`recognize`] and [`languages`] block (WinRT async `.get()`): call them from a worker
//!   thread, never the UI thread. They initialise COM (MTA) on the calling thread.
//! * [`capture_text_flow`] runs on the UI thread: it opens the result window at once
//!   (showing progress) and recognizes on a worker thread, which posts the result back to
//!   the window.

mod engine;
mod tile;
mod window;

use crate::common::Image;
use anyhow::Result;

/// A float rectangle in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoxF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BoxF {
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn union(&self, o: &BoxF) -> BoxF {
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        BoxF { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    fn scaled(&self, k: f32) -> BoxF {
        BoxF { x: self.x * k, y: self.y * k, w: self.w * k, h: self.h * k }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub rect: BoxF,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OcrLine {
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    pub fn bounds(&self) -> BoxF {
        let mut it = self.words.iter();
        let Some(first) = it.next() else { return BoxF::default() };
        it.fold(first.rect, |acc, w| acc.union(&w.rect))
    }

    /// Words joined with spaces, except between CJK characters (or for zh/ja recognizers).
    pub fn text(&self, lang: &str) -> String {
        let no_space_lang = lang.starts_with("zh") || lang.starts_with("ja");
        let mut s = String::new();
        for w in &self.words {
            if let (Some(prev), Some(next)) = (s.chars().last(), w.text.chars().next()) {
                if !(no_space_lang || (is_cjk(prev) && is_cjk(next))) {
                    s.push(' ');
                }
            }
            s.push_str(&w.text);
        }
        s
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Ext A
        | 0x4E00..=0x9FFF // CJK Unified
        | 0xF900..=0xFAFF // Compatibility
        | 0xFF66..=0xFF9F // Half-width Katakana
        | 0x3000..=0x303F) // CJK punctuation
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OcrResult {
    pub lines: Vec<OcrLine>,
    /// BCP-47 tag of the recognizer that produced the result.
    pub language: String,
    /// Text angle in degrees, when the engine detected one.
    pub angle: Option<f64>,
}

impl OcrResult {
    /// The text, lines separated by '\n' (`keep_line_breaks`) or by spaces.
    pub fn text(&self, keep_line_breaks: bool) -> String {
        let sep = if keep_line_breaks {
            "\n"
        } else if self.language.starts_with("zh") || self.language.starts_with("ja") {
            ""
        } else {
            " "
        };
        self.lines.iter().map(|l| l.text(&self.language)).collect::<Vec<_>>().join(sep)
    }

    pub fn word_count(&self) -> usize {
        self.lines.iter().map(|l| l.words.len()).sum()
    }

    pub fn words(&self) -> impl Iterator<Item = &OcrWord> {
        self.lines.iter().flat_map(|l| l.words.iter())
    }

    /// Median word height (0 when empty).
    fn median_word_height(&self) -> f32 {
        let mut h: Vec<f32> = self.words().map(|w| w.rect.h).collect();
        if h.is_empty() {
            return 0.0;
        }
        h.sort_by(|a, b| a.total_cmp(b));
        h[h.len() / 2]
    }

    fn scale_coords(&mut self, k: f32) {
        for l in &mut self.lines {
            for w in &mut l.words {
                w.rect = w.rect.scaled(k);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("no OCR language is installed")]
    NoLanguage,
    #[error("the image is empty")]
    EmptyImage,
}

/// Text height (pixels) below which a second pass on an upscaled image is tried.
const SMALL_TEXT_PX: f32 = 20.0;
/// Images with a side shorter than this are upscaled before the first pass.
const TINY_SIDE: u32 = 40;
/// Upscaling is skipped when the result would exceed this many pixels.
const MAX_UPSCALED_PX: u64 = 64_000_000;

/// Recognize the text in `img`. `lang` is a BCP-47 tag; `None` or "" uses the user's
/// profile languages (falling back to the first installed recognizer). Blocking.
pub fn recognize(img: &Image, lang: Option<&str>) -> Result<OcrResult> {
    if img.is_empty() {
        return Err(OcrError::EmptyImage.into());
    }
    engine::ensure_com();
    let eng = engine::Engine::new(lang)?;
    let flat = flatten_over_white(img);
    let max = eng.max_dimension();
    let area = img.len_px() as u64;

    let first_scale = if img.width.min(img.height) < TINY_SIDE { upscale_factor_for(img, 0.0) } else { 1 };
    let mut best = recognize_scaled(&eng, &flat, first_scale, max)?;
    if first_scale == 1 {
        let mh = best.median_word_height();
        let k = upscale_factor_for(img, mh);
        if k > 1 && area * (k * k) as u64 <= MAX_UPSCALED_PX && (best.word_count() == 0 || mh < SMALL_TEXT_PX) {
            match recognize_scaled(&eng, &flat, k, max) {
                Ok(r) if r.word_count() >= best.word_count() => {
                    log::debug!("ocr: {k}x upscale pass kept ({} words, median {mh:.1} px)", r.word_count());
                    best = r;
                }
                Ok(_) => {}
                Err(e) => log::warn!("ocr: upscale pass failed: {e:#}"),
            }
        }
    }
    log::info!(
        "ocr: {}x{} → {} lines, {} words ({})",
        img.width,
        img.height,
        best.lines.len(),
        best.word_count(),
        best.language
    );
    Ok(best)
}

/// Upscale factor for text of median height `mh` (0 = unknown): 3× for very small text,
/// 2× otherwise, 1 when the image would get too large.
fn upscale_factor_for(img: &Image, mh: f32) -> u32 {
    let k = if mh > 0.0 && mh < 10.0 { 3 } else { 2 };
    if img.len_px() as u64 * (k * k) as u64 > MAX_UPSCALED_PX {
        1
    } else {
        k
    }
}

fn recognize_scaled(eng: &engine::Engine, flat: &Image, k: u32, max: u32) -> Result<OcrResult> {
    let scaled;
    let src = if k > 1 {
        scaled = upscale(flat, k);
        &scaled
    } else {
        flat
    };
    let mut res = if src.width <= max && src.height <= max {
        eng.recognize(src)?
    } else {
        let tiles = tile::tiles(src.width, src.height, max, tile::OVERLAP);
        log::info!("ocr: {}x{} split into {} tiles", src.width, src.height, tiles.len());
        let mut results = Vec::with_capacity(tiles.len());
        let mut language = String::new();
        for t in tiles {
            let part = src.crop(t);
            let r = eng.recognize(&part)?;
            language = r.language.clone();
            results.push(tile::TileResult { tile: t, lines: r.lines });
        }
        let lines = tile::merge(results, src.width, src.height, max, tile::OVERLAP);
        OcrResult { lines, language, angle: None }
    };
    if k > 1 {
        res.scale_coords(1.0 / k as f32);
    }
    Ok(res)
}

/// Opaque copy composited over white (transparent pixels would otherwise read as black).
fn flatten_over_white(img: &Image) -> Image {
    if !img.has_transparency() {
        return img.clone();
    }
    let mut out = img.clone();
    for px in out.data.chunks_exact_mut(4) {
        let ia = 255 - px[3] as u32;
        for c in px.iter_mut().take(3) {
            *c = (*c as u32 + ia).min(255) as u8;
        }
        px[3] = 255;
    }
    out
}

/// Bilinear upscale by an integer factor.
pub fn upscale(img: &Image, k: u32) -> Image {
    let k = k.max(1);
    let (w, h) = (img.width * k, img.height * k);
    let mut out = Image::new(w, h);
    let (sw, sh) = (img.width as f32, img.height as f32);
    for y in 0..h {
        let fy = ((y as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, sh - 1.0);
        let y0 = fy.floor() as u32;
        let y1 = (y0 + 1).min(img.height - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            let fx = ((x as f32 + 0.5) / k as f32 - 0.5).clamp(0.0, sw - 1.0);
            let x0 = fx.floor() as u32;
            let x1 = (x0 + 1).min(img.width - 1);
            let tx = fx - x0 as f32;
            let (a, b, c, d) = (img.pixel(x0, y0), img.pixel(x1, y0), img.pixel(x0, y1), img.pixel(x1, y1));
            let mut o = [0u8; 4];
            for i in 0..4 {
                let top = a[i] as f32 * (1.0 - tx) + b[i] as f32 * tx;
                let bot = c[i] as f32 * (1.0 - tx) + d[i] as f32 * tx;
                o[i] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
            out.set_pixel(x, y, o);
        }
    }
    out
}

/// Installed recognizer languages as (BCP-47 tag, display name). Blocking-free but uses
/// WinRT; call from a worker thread.
pub fn languages() -> Vec<(String, String)> {
    engine::ensure_com();
    engine::languages()
}

/// Open the result window for `img` and recognize it in the background. UI thread.
pub fn capture_text_flow(img: Image) {
    window::open(img);
}

#[cfg(test)]
pub fn open_windows_for_test() -> Vec<(windows::Win32::Foundation::HWND, crate::common::Rect)> {
    window::open_windows()
}

#[cfg(test)]
pub fn close_for_test(h: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_CLOSE};
    // SAFETY: closing our own window synchronously.
    unsafe {
        SendMessageW(h, WM_CLOSE, None, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> bool {
        let langs = std::thread::spawn(languages).join().unwrap_or_default();
        if langs.is_empty() {
            eprintln!("ocr: no recognizer language installed; skipping");
        }
        !langs.is_empty()
    }

    fn render(text: &str, px: i32) -> Image {
        crate::pin::gdi::text_image(text, px, [0, 0, 0], [255, 255, 255], 24).expect("render text")
    }

    fn words_of(r: &OcrResult) -> Vec<String> {
        r.words().map(|w| w.text.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).collect()
    }

    #[test]
    fn recognizes_gdi_text() {
        if !installed() {
            return;
        }
        let img = render("The quick brown fox\njumps over the lazy dog", 36);
        let r = std::thread::spawn(move || recognize(&img, None)).join().expect("thread").expect("ocr");
        let words = words_of(&r);
        for expect in ["quick", "brown", "fox", "jumps", "lazy", "dog"] {
            assert!(words.iter().any(|w| w == expect), "missing {expect:?} in {words:?}");
        }
        assert_eq!(r.lines.len(), 2, "{:?}", r.text(true));
        assert!(r.text(false).contains("fox jumps"));
    }

    #[test]
    fn recognizes_small_text_via_upscale() {
        if !installed() {
            return;
        }
        let img = render("Invoice number 4821", 11);
        let r = std::thread::spawn(move || recognize(&img, None)).join().expect("thread").expect("ocr");
        let words = words_of(&r);
        assert!(words.iter().any(|w| w == "invoice"), "{words:?}");
        assert!(words.iter().any(|w| w == "4821"), "{words:?}");
    }

    #[test]
    fn tall_image_is_tiled_and_merged() {
        if !installed() {
            return;
        }
        // Taller than the engine's max dimension: text at the top and bottom, plus a line
        // inside the tile overlap that must appear exactly once.
        let max = std::thread::spawn(|| {
            engine::ensure_com();
            engine::Engine::new(None).map(|e| e.max_dimension()).unwrap_or(10000)
        })
        .join()
        .unwrap_or(10000);
        let h = max + 3000;
        let mut img = Image::filled(900, h, [255, 255, 255, 255]);
        let top = render("Alpha harbor", 40);
        let mid = render("Bravo meadow", 40);
        let bottom = render("Charlie summit", 40);
        img.blit(&top, 20, 40);
        let ys = tile::axis_tiles(h, max, tile::OVERLAP);
        let ov_mid = ((ys[1].0 + ys[0].0 + ys[0].1) / 2) as i32;
        img.blit(&mid, 20, ov_mid - 30);
        img.blit(&bottom, 20, h as i32 - 150);
        let r = std::thread::spawn(move || recognize(&img, Some("en-US"))).join().expect("thread").expect("ocr");
        let words = words_of(&r);
        for expect in ["alpha", "harbor", "bravo", "meadow", "charlie", "summit"] {
            assert_eq!(words.iter().filter(|w| *w == expect).count(), 1, "{expect:?} in {words:?}");
        }
        assert_eq!(r.lines.len(), 3, "{:?}", r.text(true));
    }

    #[test]
    fn line_text_joins_words() {
        let l = OcrLine {
            words: vec![
                OcrWord { text: "Hello".into(), rect: BoxF::default() },
                OcrWord { text: "world".into(), rect: BoxF::default() },
            ],
        };
        assert_eq!(l.text("en-US"), "Hello world");
        let cjk = OcrLine {
            words: vec![
                OcrWord { text: "日本".into(), rect: BoxF::default() },
                OcrWord { text: "語".into(), rect: BoxF::default() },
            ],
        };
        assert_eq!(cjk.text("en-US"), "日本語");
        let r = OcrResult { lines: vec![l.clone(), l], language: "en-US".into(), angle: None };
        assert_eq!(r.text(true), "Hello world\nHello world");
        assert_eq!(r.text(false), "Hello world Hello world");
    }

    #[test]
    fn upscale_doubles() {
        let mut img = Image::filled(2, 1, [0, 0, 0, 255]);
        img.set_pixel(1, 0, [255, 255, 255, 255]);
        let up = upscale(&img, 2);
        assert_eq!((up.width, up.height), (4, 2));
        assert_eq!(up.pixel(0, 0), [0, 0, 0, 255]);
        assert_eq!(up.pixel(3, 1), [255, 255, 255, 255]);
        let mid = up.pixel(1, 0)[0];
        assert!(mid > 30 && mid < 128, "{mid}");
    }

    #[test]
    fn flatten_makes_opaque() {
        let img = Image::new(2, 2);
        let f = flatten_over_white(&img);
        assert_eq!(f.pixel(0, 0), [255, 255, 255, 255]);
    }
}
