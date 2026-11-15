//! Text engine on DirectWrite. Text is laid out by DirectWrite (wrapping, alignment, font
//! fallback, bidi, underline) and every glyph is converted to a vector outline with
//! `GetGlyphRunOutline`, so tiny-skia renders it like any other path: anti-aliased, with
//! outlines, shadows, and glyphs placed along arcs or paths. Color fonts (Segoe UI Emoji) are
//! split into colored layers with `TranslateColorGlyphRun`. See DECISIONS.md.

use super::model::{Color, TextAlign};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use tiny_skia::{Path, PathBuilder};
use windows::core::{implement, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::{BOOL, FALSE, TRUE};
use windows::Win32::Graphics::Direct2D::Common::{
    ID2D1SimplifiedGeometrySink, ID2D1SimplifiedGeometrySink_Impl, D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN,
    D2D1_FIGURE_END, D2D1_FILL_MODE, D2D1_PATH_SEGMENT, D2D_POINT_2F,
};
use windows::Win32::Graphics::DirectWrite::*;

/// One glyph (or one color layer of a glyph) as an outline.
#[derive(Clone, Debug)]
pub struct Glyph {
    /// Outline relative to the glyph's baseline origin.
    pub path: Path,
    /// Baseline origin in layout coordinates (top-left of the layout box = 0,0).
    pub x: f32,
    pub y: f32,
    /// Horizontal advance (used to centre glyphs on curved baselines).
    pub advance: f32,
    /// Layer color from a color font; `None` = use the text color.
    pub color: Option<Color>,
}

/// An underline (or other decoration) rectangle in layout coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deco {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TextLayout {
    pub glyphs: Vec<Glyph>,
    pub decos: Vec<Deco>,
    /// Size of the laid-out text (the layout box width when wrapping).
    pub width: f32,
    pub height: f32,
    /// Ink width of the longest line.
    pub content_width: f32,
    /// Number of laid-out lines (checked by the tests).
    #[cfg(test)]
    pub line_count: u32,
}

/// What to lay out.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec<'a> {
    pub text: &'a str,
    pub family: &'a str,
    /// Em size in pixels.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub align: TextAlign,
    /// Wrap width; `None` = no wrapping (each paragraph is one line).
    pub max_width: Option<f32>,
}

thread_local! {
    static ENGINE: Option<Engine> = Engine::new();
    static CACHE: RefCell<HashMap<String, Arc<TextLayout>>> = RefCell::new(HashMap::new());
}

const CACHE_CAP: usize = 512;

struct Engine {
    factory: IDWriteFactory,
    factory2: Option<IDWriteFactory2>,
}

impl Engine {
    fn new() -> Option<Engine> {
        // SAFETY: plain factory creation; DirectWrite needs no COM apartment.
        let factory: IDWriteFactory = match unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) } {
            Ok(f) => f,
            Err(e) => {
                log::error!("draw: DirectWrite is unavailable: {e}");
                return None;
            }
        };
        let factory2 = factory.cast::<IDWriteFactory2>().ok();
        Some(Engine { factory, factory2 })
    }
}

/// Lay out text (cached per thread). Returns an empty layout if DirectWrite fails.
pub fn layout(spec: &TextSpec) -> Arc<TextLayout> {
    let key = format!(
        "{}\u{1}{}\u{1}{}\u{1}{}{}{}\u{1}{:?}\u{1}{:?}",
        spec.text,
        spec.family,
        spec.size.to_bits(),
        spec.bold as u8,
        spec.italic as u8,
        spec.underline as u8,
        spec.align,
        spec.max_width.map(f32::to_bits)
    );
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let result = ENGINE.with(|e| match e {
        Some(engine) => build(engine, spec).unwrap_or_else(|err| {
            log::warn!("draw: text layout failed: {err}");
            TextLayout::default()
        }),
        None => TextLayout::default(),
    });
    let result = Arc::new(result);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key, result.clone());
    });
    result
}

/// Installed font family names (sorted, localized to en-us where available).
pub fn font_families() -> Vec<String> {
    ENGINE.with(|e| {
        let Some(engine) = e else { return Vec::new() };
        let mut out = families(engine).unwrap_or_default();
        out.sort_by_key(|s| s.to_lowercase());
        out.dedup();
        out
    })
}

fn families(engine: &Engine) -> windows::core::Result<Vec<String>> {
    let mut coll: Option<IDWriteFontCollection> = None;
    // SAFETY: valid out pointer; the collection is reference counted.
    unsafe { engine.factory.GetSystemFontCollection(&mut coll, FALSE)? };
    let Some(coll) = coll else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    // SAFETY: indices are within the reported count; buffers sized from GetStringLength.
    unsafe {
        for i in 0..coll.GetFontFamilyCount() {
            let fam = coll.GetFontFamily(i)?;
            let names = fam.GetFamilyNames()?;
            let mut idx = 0u32;
            let mut exists = FALSE;
            names.FindLocaleName(windows::core::w!("en-us"), &mut idx, &mut exists)?;
            if !exists.as_bool() {
                idx = 0;
            }
            let len = names.GetStringLength(idx)?;
            let mut buf = vec![0u16; len as usize + 1];
            names.GetString(idx, &mut buf)?;
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
    }
    Ok(out)
}

fn build(engine: &Engine, spec: &TextSpec) -> windows::core::Result<TextLayout> {
    let size = spec.size.clamp(0.5, 2000.0);
    let family = HSTRING::from(if spec.family.trim().is_empty() { "Segoe UI" } else { spec.family });
    let weight = if spec.bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
    let style = if spec.italic { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
    let wide: Vec<u16> = spec.text.encode_utf16().collect();
    // SAFETY: all pointers passed below reference live locals for the duration of each call.
    unsafe {
        let format = engine.factory.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            windows::core::w!("en-us"),
        )?;
        let wrap = spec.max_width.filter(|w| w.is_finite() && *w > 0.0);
        format.SetWordWrapping(if wrap.is_some() {
            DWRITE_WORD_WRAPPING_WRAP
        } else {
            DWRITE_WORD_WRAPPING_NO_WRAP
        })?;
        format.SetTextAlignment(match spec.align {
            TextAlign::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            TextAlign::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        })?;
        let layout = engine.factory.CreateTextLayout(&wide, &format, wrap.unwrap_or(1.0e6), 1.0e6)?;
        if spec.underline && !wide.is_empty() {
            layout.SetUnderline(TRUE, DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 })?;
        }
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        let width = match wrap {
            Some(w) => w,
            None => {
                // Align lines within the widest line.
                let w = m.widthIncludingTrailingWhitespace.max(m.width).max(1.0);
                layout.SetMaxWidth(w)?;
                layout.GetMetrics(&mut m)?;
                w
            }
        };
        let collector = Rc::new(RefCell::new(Collected::default()));
        let renderer: IDWriteTextRenderer =
            Collector { out: collector.clone(), factory2: engine.factory2.clone() }.into();
        layout.Draw(None, &renderer, 0.0, 0.0)?;
        let c = std::mem::take(&mut *collector.borrow_mut());
        Ok(TextLayout {
            glyphs: c.glyphs,
            decos: c.decos,
            width,
            height: m.height,
            content_width: m.width,
            #[cfg(test)]
            line_count: m.lineCount.max(1),
        })
    }
}

#[derive(Default)]
struct Collected {
    glyphs: Vec<Glyph>,
    decos: Vec<Deco>,
}

#[implement(IDWriteTextRenderer)]
struct Collector {
    out: Rc<RefCell<Collected>>,
    factory2: Option<IDWriteFactory2>,
}

impl Collector {
    /// SAFETY: `run` must be a valid glyph run from DirectWrite for the duration of the call.
    unsafe fn add_run(&self, run: &DWRITE_GLYPH_RUN, x: f32, y: f32, color: Option<Color>) {
        let Some(face) = run.fontFace.as_ref() else { return };
        let n = run.glyphCount as usize;
        if n == 0 || run.glyphIndices.is_null() {
            return;
        }
        let indices = std::slice::from_raw_parts(run.glyphIndices, n);
        let advances = (!run.glyphAdvances.is_null()).then(|| std::slice::from_raw_parts(run.glyphAdvances, n));
        let offsets = (!run.glyphOffsets.is_null()).then(|| std::slice::from_raw_parts(run.glyphOffsets, n));
        let rtl = run.bidiLevel & 1 == 1;
        let mut pen = 0.0f32;
        let mut out = self.out.borrow_mut();
        for i in 0..n {
            let adv = advances.map(|a| a[i]).unwrap_or(0.0);
            let (ox, oy) = offsets.map(|o| (o[i].advanceOffset, o[i].ascenderOffset)).unwrap_or((0.0, 0.0));
            let gx = if rtl { x - pen - adv - ox } else { x + pen + ox };
            pen += adv;
            let Some(path) = glyph_outline(face, run.fontEmSize, indices[i]) else { continue };
            out.glyphs.push(Glyph { path, x: gx, y: y - oy, advance: adv, color });
        }
    }
}

/// Outline of one glyph, relative to its baseline origin.
fn glyph_outline(face: &IDWriteFontFace, em: f32, index: u16) -> Option<Path> {
    let shared = Rc::new(RefCell::new(Some(PathBuilder::new())));
    let sink: ID2D1SimplifiedGeometrySink = OutlineSink { pb: shared.clone() }.into();
    // SAFETY: one glyph index on the stack; the sink is a live COM object.
    let ok = unsafe { face.GetGlyphRunOutline(em, &index, None, None, 1, FALSE, FALSE, &sink) };
    if ok.is_err() {
        return None;
    }
    drop(sink);
    let pb = shared.borrow_mut().take()?;
    pb.finish()
}

impl IDWritePixelSnapping_Impl for Collector_Impl {
    fn IsPixelSnappingDisabled(&self, _ctx: *const core::ffi::c_void) -> windows::core::Result<BOOL> {
        Ok(TRUE)
    }
    fn GetCurrentTransform(&self, _ctx: *const core::ffi::c_void, t: *mut DWRITE_MATRIX) -> windows::core::Result<()> {
        if !t.is_null() {
            // SAFETY: DirectWrite passes a valid out pointer.
            unsafe { *t = DWRITE_MATRIX { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 } };
        }
        Ok(())
    }
    fn GetPixelsPerDip(&self, _ctx: *const core::ffi::c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}

impl IDWriteTextRenderer_Impl for Collector_Impl {
    fn DrawGlyphRun(
        &self,
        _ctx: *const core::ffi::c_void,
        x: f32,
        y: f32,
        mode: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _effect: Option<&windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        if run.is_null() {
            return Ok(());
        }
        // SAFETY: DirectWrite guarantees `run` (and `desc` when non-null) are valid during the call.
        unsafe {
            if let Some(f2) = &self.factory2 {
                let d = (!desc.is_null()).then_some(desc);
                if let Ok(layers) = f2.TranslateColorGlyphRun(x, y, run, d, mode, None, 0) {
                    while layers.MoveNext()?.as_bool() {
                        let cr = layers.GetCurrentRun()?;
                        if cr.is_null() {
                            continue;
                        }
                        let cr = &*cr;
                        let color = (cr.paletteIndex != 0xFFFF).then(|| {
                            let c = cr.runColor;
                            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                            Color::rgba(q(c.r), q(c.g), q(c.b), q(c.a))
                        });
                        self.add_run(&cr.glyphRun, cr.baselineOriginX, cr.baselineOriginY, color);
                    }
                    return Ok(());
                }
            }
            self.add_run(&*run, x, y, None);
        }
        Ok(())
    }

    fn DrawUnderline(
        &self,
        _ctx: *const core::ffi::c_void,
        x: f32,
        y: f32,
        underline: *const DWRITE_UNDERLINE,
        _effect: Option<&windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        if underline.is_null() {
            return Ok(());
        }
        // SAFETY: valid for the duration of the call.
        let u = unsafe { &*underline };
        let left = if u.readingDirection == DWRITE_READING_DIRECTION_RIGHT_TO_LEFT { x - u.width } else { x };
        self.out.borrow_mut().decos.push(Deco { x: left, y: y + u.offset, w: u.width, h: u.thickness.max(1.0) });
        Ok(())
    }

    fn DrawStrikethrough(
        &self,
        _ctx: *const core::ffi::c_void,
        _x: f32,
        _y: f32,
        _s: *const DWRITE_STRIKETHROUGH,
        _effect: Option<&windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn DrawInlineObject(
        &self,
        _ctx: *const core::ffi::c_void,
        _x: f32,
        _y: f32,
        _obj: Option<&IDWriteInlineObject>,
        _sideways: BOOL,
        _rtl: BOOL,
        _effect: Option<&windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

#[implement(ID2D1SimplifiedGeometrySink)]
struct OutlineSink {
    pb: Rc<RefCell<Option<PathBuilder>>>,
}

impl ID2D1SimplifiedGeometrySink_Impl for OutlineSink_Impl {
    fn SetFillMode(&self, _mode: D2D1_FILL_MODE) {}
    fn SetSegmentFlags(&self, _flags: D2D1_PATH_SEGMENT) {}
    fn BeginFigure(&self, p: &D2D_POINT_2F, _begin: D2D1_FIGURE_BEGIN) {
        if let Some(pb) = self.pb.borrow_mut().as_mut() {
            pb.move_to(p.x, p.y);
        }
    }
    fn AddLines(&self, points: *const D2D_POINT_2F, count: u32) {
        if points.is_null() {
            return;
        }
        // SAFETY: Direct2D passes `count` valid points.
        let pts = unsafe { std::slice::from_raw_parts(points, count as usize) };
        if let Some(pb) = self.pb.borrow_mut().as_mut() {
            for p in pts {
                pb.line_to(p.x, p.y);
            }
        }
    }
    fn AddBeziers(&self, beziers: *const D2D1_BEZIER_SEGMENT, count: u32) {
        if beziers.is_null() {
            return;
        }
        // SAFETY: Direct2D passes `count` valid segments.
        let segs = unsafe { std::slice::from_raw_parts(beziers, count as usize) };
        if let Some(pb) = self.pb.borrow_mut().as_mut() {
            for s in segs {
                pb.cubic_to(s.point1.x, s.point1.y, s.point2.x, s.point2.y, s.point3.x, s.point3.y);
            }
        }
    }
    fn EndFigure(&self, _end: D2D1_FIGURE_END) {
        if let Some(pb) = self.pb.borrow_mut().as_mut() {
            pb.close();
        }
    }
    fn Close(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str, max_width: Option<f32>) -> TextSpec<'_> {
        TextSpec {
            text,
            family: "Segoe UI",
            size: 20.0,
            bold: false,
            italic: false,
            underline: true,
            align: TextAlign::Left,
            max_width,
        }
    }

    #[test]
    fn lays_out_and_wraps() {
        let one = layout(&spec("Hello world, this wraps", None));
        assert!(!one.glyphs.is_empty());
        assert_eq!(one.line_count, 1);
        assert!(!one.decos.is_empty(), "underline collected");
        let wrapped = layout(&spec("Hello world, this wraps", Some(80.0)));
        assert!(wrapped.line_count >= 2);
        assert!(wrapped.height > one.height);
        let empty = layout(&spec("", None));
        assert!(empty.glyphs.is_empty() && empty.height > 0.0);
    }

    #[test]
    fn emoji_has_color_layers() {
        let s = TextSpec { family: "Segoe UI Emoji", underline: false, ..spec("\u{1F600}", None) };
        let l = layout(&s);
        assert!(l.glyphs.iter().any(|g| g.color.is_some()), "expected COLR layers");
    }

    #[test]
    fn families_listed() {
        let f = font_families();
        assert!(f.iter().any(|n| n == "Segoe UI"));
    }
}
