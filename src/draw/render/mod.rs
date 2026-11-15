//! Rendering. Two passes:
//!
//! 1. **Raster pass** ([`super::raster`]): blur/pixelate, eraser, fill and pencil objects are
//!    applied to the base image in list order, producing the *effective base*. The editor keeps
//!    this in a [`super::RasterCache`] so it is recomputed only when those objects change.
//! 2. **Vector pass** (this module): every other object is drawn with tiny-skia, either onto the
//!    effective base ([`render`], for export) or onto a transparent viewport-sized layer at a
//!    zoom ([`render_overlay`], for the live view). Objects that need the pixels below them
//!    (multiply highlighters, blur/dim outside) composite the effective base into the layer under
//!    themselves, so `base + overlay` always equals the exported result.

mod art;
mod line;
mod shape;
pub(crate) mod text;

use super::fx;
use super::model::*;
use super::raster;
use crate::common::Image;
use tiny_skia::{FillRule, IntRect, Mask, Paint, Path, Pixmap, PixmapPaint, Shader, Stroke, StrokeDash, Transform};

/// Which objects to draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderOptions {
    /// Video time; objects whose [`Timing`] excludes it are skipped. `None` draws everything.
    pub time_ms: Option<u64>,
    /// Objects to skip (e.g. the text object being edited in place).
    pub hidden: Vec<ObjectId>,
}

impl RenderOptions {
    pub fn shows(&self, o: &Object) -> bool {
        o.visible_at(self.time_ms) && !self.hidden.contains(&o.id)
    }
}

/// A viewport onto the image: image point (`x`, `y`) maps to the layer's top-left pixel, and one
/// image pixel covers `zoom` layer pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub width: u32,
    pub height: u32,
}

impl View {
    /// The whole image at 100%.
    #[cfg(test)]
    pub fn identity(width: u32, height: u32) -> View {
        View { x: 0.0, y: 0.0, zoom: 1.0, width, height }
    }
    pub fn transform(&self) -> Transform {
        let z = if self.zoom.is_finite() && self.zoom > 0.0 { self.zoom } else { 1.0 };
        Transform::from_scale(z, z).pre_translate(-self.x, -self.y)
    }
    /// Layer pixel → image point.
    #[cfg(test)]
    pub fn to_image(self, px: f32, py: f32) -> Pt {
        let z = if self.zoom > 0.0 { self.zoom } else { 1.0 };
        Pt::new(self.x + px / z, self.y + py / z)
    }
    /// Image point → layer pixel.
    #[cfg(test)]
    pub fn to_view(self, p: Pt) -> (f32, f32) {
        ((p.x - self.x) * self.zoom, (p.y - self.y) * self.zoom)
    }
}

/// Flatten for export: raster objects applied to `base`, then all vector objects on top.
pub fn render(base: &Image, objects: &[Object], opts: &RenderOptions) -> Image {
    let effective = raster::apply(base, objects, opts);
    render_vectors_onto(&effective, objects, opts)
}

/// Draw the vector objects onto an already-effective base (see [`super::RasterCache`]).
pub fn render_vectors_onto(effective: &Image, objects: &[Object], opts: &RenderOptions) -> Image {
    let Some(mut pm) = image_to_pixmap(effective) else { return effective.clone() };
    let ctx = Ctx { base: effective, ts: Transform::identity(), zoom: 1.0, base_in_target: true };
    for obj in objects.iter().filter(|o| !o.is_raster() && opts.shows(o)) {
        draw_object(&mut pm, obj, &ctx);
    }
    pixmap_to_image(&pm)
}

/// Live view: vector objects on a transparent `view.width`×`view.height` layer. `effective` is
/// the base with raster objects applied (from [`super::RasterCache`]); the layer is meant to be
/// composited source-over on top of that same image drawn at the same view.
pub fn render_overlay(effective: &Image, objects: &[Object], view: &View, opts: &RenderOptions) -> Image {
    let (w, h) = (view.width.max(1), view.height.max(1));
    let Some(mut pm) = Pixmap::new(w, h) else { return Image::new(view.width, view.height) };
    let zoom = if view.zoom.is_finite() && view.zoom > 0.0 { view.zoom } else { 1.0 };
    let ctx = Ctx { base: effective, ts: view.transform(), zoom, base_in_target: false };
    let visible = RectF::new(view.x, view.y, w as f32 / zoom, h as f32 / zoom);
    for obj in objects.iter().filter(|o| !o.is_raster() && opts.shows(o)) {
        if needs_full_backdrop(obj) || super::bounds(obj).inflate(2.0).intersects(&visible) {
            draw_object(&mut pm, obj, &ctx);
        }
    }
    pixmap_to_image(&pm)
}

/// Render one object on a transparent canvas of its own bounds (for thumbnails, drag previews).
/// Returns the image and its top-left in image coordinates.
pub fn render_object(effective: &Image, obj: &Object, scale: f32) -> Option<(Image, Pt)> {
    let b = super::bounds(obj);
    let b = match obj.style.shadow {
        Some(s) => b.union(&b.translate(Pt::new(s.dx, s.dy)).inflate(s.blur * 3.0)),
        None => b,
    }
    .inflate(2.0);
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let view = View {
        x: b.x,
        y: b.y,
        zoom: scale,
        width: (b.w * scale).ceil().clamp(1.0, 16384.0) as u32,
        height: (b.h * scale).ceil().clamp(1.0, 16384.0) as u32,
    };
    let img = render_overlay(effective, std::slice::from_ref(obj), &view, &RenderOptions::default());
    Some((img, b.tl()))
}

/// Largest dimension of an arrowhead for a line width (bounds and hit-testing).
pub(crate) fn line_head_extent(head: ArrowHead, w: f32, size: f32) -> f32 {
    let (l, wd, _) = line::head_dims(head, w, size);
    l.max(wd)
}

pub(crate) fn shape_outline(s: &Shape, closed: bool) -> Option<Path> {
    shape::shape_path(s, closed)
}

pub(crate) fn highlight_outline(h: &Highlight) -> Option<Path> {
    shape::highlight_path(h, 0.0, 0.5)
}

pub(crate) fn step_outline(s: &Step) -> Option<Path> {
    shape::step_path(s)
}

pub use art::{cursor_size, sticker_size};

fn needs_full_backdrop(obj: &Object) -> bool {
    matches!(&obj.kind, Kind::Highlight(h) if h.blur_outside > 0.0 || h.dim_outside > 0.0)
}

// ---------------------------------------------------------------------------------------------
// Context and dispatch
// ---------------------------------------------------------------------------------------------

pub(crate) struct Ctx<'a> {
    /// Effective base in image coordinates.
    pub base: &'a Image,
    /// Image → target pixel transform.
    pub ts: Transform,
    pub zoom: f32,
    /// True when the target already contains the base (export); false for overlay layers.
    pub base_in_target: bool,
}

impl Ctx<'_> {
    fn shifted(&self, dx: f32, dy: f32) -> Ctx<'_> {
        Ctx {
            base: self.base,
            ts: self.ts.post_translate(dx, dy),
            zoom: self.zoom,
            base_in_target: self.base_in_target,
        }
    }
    /// Flattening tolerance in image pixels for the current zoom.
    pub fn tol(&self) -> f32 {
        (0.2 / self.zoom).clamp(0.01, 2.0)
    }
}

fn draw_object(pm: &mut Pixmap, obj: &Object, ctx: &Ctx) {
    let st = &obj.style;
    let opacity = if st.opacity.is_finite() { st.opacity.clamp(0.0, 1.0) } else { 1.0 };
    if opacity <= 0.0 {
        return;
    }
    if let Kind::Highlight(h) = &obj.kind {
        shape::draw_highlight(pm, h, st, opacity, ctx);
        return;
    }
    let shadow = st.shadow.filter(|s| s.color.is_visible());
    if opacity >= 0.999 && shadow.is_none() {
        draw_content(pm, obj, ctx);
        return;
    }
    // Draw into a layer, then composite with the shadow and group opacity.
    let mut b = super::bounds(obj).inflate(2.0);
    let margin = shadow.map(|s| s.blur.max(0.0) * 3.0 + 2.0).unwrap_or(0.0);
    b = b.inflate(margin);
    let Some(area) = device_rect(&ctx.ts, b, pm.width(), pm.height()) else { return };
    let Some(mut layer) = Pixmap::new(area.width(), area.height()) else { return };
    let lctx = ctx.shifted(-(area.x() as f32), -(area.y() as f32));
    draw_content(&mut layer, obj, &lctx);
    let paint = PixmapPaint { opacity, ..Default::default() };
    if let Some(s) = shadow {
        let sh = shadow_of(&layer, s.color, s.blur * ctx.zoom);
        let (dx, dy) = ((s.dx * ctx.zoom).round() as i32, (s.dy * ctx.zoom).round() as i32);
        pm.draw_pixmap(area.x() + dx, area.y() + dy, sh.as_ref(), &paint, Transform::identity(), None);
    }
    pm.draw_pixmap(area.x(), area.y(), layer.as_ref(), &paint, Transform::identity(), None);
}

fn draw_content(pm: &mut Pixmap, obj: &Object, ctx: &Ctx) {
    let st = &obj.style;
    match &obj.kind {
        Kind::Text(t) => text::draw_text(pm, t, st, ctx),
        Kind::Line(l) => line::draw_line(pm, l, st, obj.id, ctx),
        Kind::Shape(s) => shape::draw_shape(pm, s, st, ctx),
        Kind::Magnifier(m) => shape::draw_magnifier(pm, m, st, ctx),
        Kind::Step(s) => shape::draw_step(pm, s, st, ctx),
        Kind::Emoji(e) => text::draw_emoji(pm, e, st, ctx),
        Kind::Sticker(s) => art::draw_sticker(pm, s, st, ctx),
        Kind::Image(p) => art::draw_picture(pm, p, ctx),
        Kind::Cursor(c) => art::draw_cursor(pm, c, st, ctx),
        Kind::Highlight(_) | Kind::Blur(_) | Kind::Eraser(_) | Kind::Fill(_) | Kind::Pencil(_) => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Device-pixel rectangle covering `r` (image coords) under `ts`, clipped to the target.
pub(crate) fn device_rect(ts: &Transform, r: RectF, w: u32, h: u32) -> Option<IntRect> {
    if !r.is_finite() {
        return None;
    }
    let mut pts = r.corners().map(super::path::sk);
    ts.map_points(&mut pts);
    let (mut l, mut t, mut rr, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        l = l.min(p.x);
        t = t.min(p.y);
        rr = rr.max(p.x);
        b = b.max(p.y);
    }
    let l = (l.floor() as i64).clamp(0, w as i64) as i32;
    let t = (t.floor() as i64).clamp(0, h as i64) as i32;
    let rr = (rr.ceil() as i64).clamp(0, w as i64) as i32;
    let b = (b.ceil() as i64).clamp(0, h as i64) as i32;
    IntRect::from_ltrb(l, t, rr, b)
}

pub(crate) fn paint_color(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c.to_skia());
    p.anti_alias = true;
    p
}

pub(crate) fn paint_shader(shader: Shader<'_>) -> Paint<'_> {
    Paint { shader, anti_alias: true, ..Default::default() }
}

pub(crate) fn dash_of(dash: Dash, w: f32) -> Option<StrokeDash> {
    let w = w.max(0.5);
    let pattern = match dash {
        Dash::Solid => return None,
        Dash::Dash => vec![2.0 * w, 2.5 * w],
        Dash::Dot => vec![0.01, 2.0 * w],
        Dash::DashDot => vec![3.0 * w, 2.0 * w, 0.01, 2.0 * w],
        Dash::LongDash => vec![6.0 * w, 2.5 * w],
    };
    StrokeDash::new(pattern, 0.0)
}

pub(crate) fn stroke_of(width: f32, dash: Dash) -> Stroke {
    Stroke {
        width,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        dash: dash_of(dash, width),
        ..Default::default()
    }
}

/// Fill several paths as one union, with a border around the union's outline only (no seams
/// where they overlap, e.g. a callout box and its tail). All paths must be wound the same way.
pub(crate) fn fill_and_border_union(
    pm: &mut Pixmap,
    paths: &[Path],
    fill: Color,
    border: Option<(Color, f32, Dash)>,
    ts: Transform,
) {
    let mut pb = tiny_skia::PathBuilder::new();
    for p in paths {
        pb.push_path(p);
    }
    let Some(union) = pb.finish() else { return };
    if fill.is_visible() {
        pm.fill_path(&union, &paint_color(fill), FillRule::Winding, ts, None);
    }
    let Some((color, width, dash)) = border.filter(|b| b.0.is_visible() && b.1 > 0.0) else { return };
    if paths.len() == 1 {
        pm.stroke_path(&union, &paint_color(color), &stroke_of(width, dash), ts, None);
        return;
    }
    // Stroke at double width, clipped to the outside of the union: only the outer half remains.
    let Some(mut mask) = Mask::new(pm.width(), pm.height()) else { return };
    mask.fill_path(&union, FillRule::Winding, true, ts);
    invert_mask(&mut mask);
    let stroke = stroke_of(width * 2.0, dash);
    for p in paths {
        pm.stroke_path(p, &paint_color(color), &stroke, ts, Some(&mask));
    }
}

pub(crate) fn invert_mask(mask: &mut Mask) {
    for v in mask.data_mut() {
        *v = 255 - *v;
    }
}

/// Tinted, blurred copy of a layer's alpha.
fn shadow_of(layer: &Pixmap, color: Color, blur: f32) -> Pixmap {
    let mut sh = layer.clone();
    let c = color.to_skia().premultiply().to_color_u8();
    for px in sh.data_mut().chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((c.red() as u32 * a + 127) / 255) as u8;
        px[1] = ((c.green() as u32 * a + 127) / 255) as u8;
        px[2] = ((c.blue() as u32 * a + 127) / 255) as u8;
        px[3] = ((c.alpha() as u32 * a + 127) / 255) as u8;
    }
    if blur >= 0.5 {
        let (w, h) = (sh.width() as usize, sh.height() as usize);
        fx::blur(sh.data_mut(), w, h, blur);
    }
    sh
}

/// Overlay mode: put the effective base *under* the existing layer pixels in `area`, so blend
/// modes and blurs that read the backdrop see the same pixels as in the export.
pub(crate) fn composite_backdrop(pm: &mut Pixmap, ctx: &Ctx, area: IntRect) {
    if ctx.base_in_target || ctx.base.is_empty() {
        return;
    }
    let Some(inv) = ctx.ts.invert() else { return };
    let w = pm.width() as usize;
    let base = ctx.base;
    let data = pm.data_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let mut p = [tiny_skia::Point::from_xy(x as f32 + 0.5, y as f32 + 0.5)];
            inv.map_points(&mut p);
            let s = sample_bilinear(base, p[0].x - 0.5, p[0].y - 0.5);
            let i = (y as usize * w + x as usize) * 4;
            let ia = 255 - data[i + 3] as u32;
            if ia == 0 {
                continue;
            }
            for c in 0..4 {
                data[i + c] = (data[i + c] as u32 + (s[c] as u32 * ia + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// Bilinear sample of a BGRA image at continuous pixel coordinates; returns premultiplied RGBA.
fn sample_bilinear(img: &Image, x: f32, y: f32) -> [u8; 4] {
    let (w, h) = (img.width as i64, img.height as i64);
    let x0f = x.floor();
    let y0f = y.floor();
    let (fx, fy) = (x - x0f, y - y0f);
    let px = |xx: i64, yy: i64| img.pixel(xx.clamp(0, w - 1) as u32, yy.clamp(0, h - 1) as u32);
    let (x0, y0) = (x0f as i64, y0f as i64);
    let (a, b, c, d) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut out = [0u8; 4];
    // BGRA → RGBA
    for (oc, ic) in [(0usize, 2usize), (1, 1), (2, 0), (3, 3)] {
        let top = a[ic] as f32 * (1.0 - fx) + b[ic] as f32 * fx;
        let bot = c[ic] as f32 * (1.0 - fx) + d[ic] as f32 * fx;
        out[oc] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// BGRA image → RGBA pixmap (both premultiplied).
pub(crate) fn image_to_pixmap(img: &Image) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width, img.height)?;
    for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(img.data.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    Some(pm)
}

pub(crate) fn pixmap_to_image(pm: &Pixmap) -> Image {
    let mut data = Vec::with_capacity(pm.data().len());
    for s in pm.data().chunks_exact(4) {
        data.extend_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    Image::from_bgra(pm.width(), pm.height(), data)
}

/// Transform mapping a `cw`×`ch` content box into a frame (with rotation and flips).
pub(crate) fn frame_transform(f: &Frame, cw: f32, ch: f32) -> Transform {
    let c = f.rect.center();
    let sx = if cw.abs() > 1e-6 { f.rect.w / cw } else { 1.0 } * if f.flip_h { -1.0 } else { 1.0 };
    let sy = if ch.abs() > 1e-6 { f.rect.h / ch } else { 1.0 } * if f.flip_v { -1.0 } else { 1.0 };
    Transform::from_translate(c.x, c.y)
        .pre_concat(Transform::from_rotate(f.rotation))
        .pre_concat(Transform::from_scale(sx, sy))
        .pre_concat(Transform::from_translate(-cw / 2.0, -ch / 2.0))
}
