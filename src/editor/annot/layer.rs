//! [`DrawLayer`]: the editor's [`DocOverlay`] backed by `draw::Document` + `draw::History`.
//!
//! The base pixels stay in the editor document (pixel edits, effects and their undo live
//! there); the layer only owns the annotation objects. Every object edit is recorded in
//! the layer's own `draw::History` *and* as a `StepKind::Overlay` marker in the editor's
//! history, so Ctrl+Z walks one timeline across pixel and object edits.
//!
//! Live view: the visible part of the image is rendered at the canvas zoom (physical
//! pixels) into a transparent layer with `draw::render_overlay` and cached as a texture
//! until the objects, the base or the view change. When raster objects (blur, eraser,
//! fill, pencil) exist, the layer also carries the effective base for the visible area.

use super::super::ext::{DocOverlay, Flattener};
use super::super::selection::CanvasMap;
use super::super::texture::{image_options, tile_to_color_image, TileRect};
use crate::common::{Image, Rect};
use crate::draw::{self, Object, ObjectId, RasterCache, RenderOptions};
use eframe::egui::{self, pos2, Color32, TextureHandle};
use std::any::Any;
use std::sync::Arc;

/// Largest side of the live layer texture (the zoom is reduced beyond that).
const MAX_LAYER_SIDE: f32 = 4096.0;

#[derive(Clone, PartialEq)]
struct CacheKey {
    rev: u64,
    base: usize,
    rect: Rect,
    size: (u32, u32),
    hidden: Vec<ObjectId>,
}

pub struct DrawLayer {
    pub doc: draw::Document,
    pub history: draw::History,
    /// Bumped on every object change (live drags included): texture cache key.
    rev: u64,
    /// Objects not drawn (the text being edited in place).
    pub hidden: Vec<ObjectId>,
    raster: RasterCache,
    base_ptr: usize,
    tex: Option<(CacheKey, TextureHandle)>,
}

impl DrawLayer {
    pub fn new(base: Arc<Image>) -> Self {
        let mut doc = draw::Document::new(Image::new(1, 1));
        doc.set_base(base.clone());
        Self {
            doc,
            history: draw::History::new(),
            rev: 1,
            hidden: Vec::new(),
            raster: RasterCache::new(),
            base_ptr: Arc::as_ptr(&base) as usize,
            tex: None,
        }
    }

    pub fn with_objects(base: Arc<Image>, objects: Vec<Object>) -> Self {
        let mut l = Self::new(base);
        l.doc.objects = objects;
        l
    }

    pub fn objects(&self) -> &[Object] {
        &self.doc.objects
    }

    /// Mutable objects (invalidates the live texture).
    pub fn objects_mut(&mut self) -> &mut Vec<Object> {
        self.rev = self.rev.wrapping_add(1);
        &mut self.doc.objects
    }

    /// Record `before → current` in the layer history; true if anything changed (the
    /// caller then records the editor's overlay marker).
    pub fn record(&mut self, before: &[Object]) -> bool {
        self.rev = self.rev.wrapping_add(1);
        self.history.record_objects(before, &self.doc.objects)
    }

    fn sync_base(&mut self, base: &Arc<Image>) {
        let p = Arc::as_ptr(base) as usize;
        if p != self.base_ptr {
            self.base_ptr = p;
            self.doc.set_base(base.clone());
        }
    }

    /// Base with the raster objects applied.
    pub fn effective(&mut self, base: &Arc<Image>) -> Image {
        self.sync_base(base);
        let opts = RenderOptions::default();
        self.raster.get(&self.doc.base, self.doc.base_rev, &self.doc.objects, &opts).clone()
    }

    fn render_view(&mut self, rect: Rect, size: (u32, u32), zoom: f32) -> Image {
        let opts = RenderOptions { time_ms: None, hidden: self.hidden.clone() };
        let has_raster = self.doc.objects.iter().any(|o| o.is_raster());
        let eff: &Image = self.raster.get(&self.doc.base, self.doc.base_rev, &self.doc.objects, &opts);
        let view = draw::View { x: rect.x as f32, y: rect.y as f32, zoom, width: size.0, height: size.1 };
        let overlay = draw::render_overlay(eff, &self.doc.objects, &view, &opts);
        if !has_raster {
            return overlay;
        }
        // Raster objects changed the pixels under the vectors: carry them in the layer.
        let crop = eff.crop(rect);
        let filter = if zoom >= 1.0 {
            crate::effects::ResampleFilter::Nearest
        } else {
            crate::effects::ResampleFilter::Bilinear
        };
        let mut img = crate::effects::resize_to(&crop, size.0, size.1, filter);
        img.draw_over(&overlay, 0, 0);
        img
    }
}

impl DocOverlay for DrawLayer {
    fn paint(&mut self, ctx: &egui::Context, painter: &egui::Painter, map: &CanvasMap, base: &Arc<Image>) {
        self.sync_base(base);
        if self.doc.objects.is_empty() {
            self.tex = None;
            return;
        }
        let ppp = ctx.pixels_per_point();
        let clip = painter.clip_rect();
        let a = map.to_image([clip.min.x, clip.min.y]);
        let b = map.to_image([clip.max.x, clip.max.y]);
        let (w, h) = (base.width as i32, base.height as i32);
        let l = (a[0].floor() as i32).clamp(0, w);
        let t = (a[1].floor() as i32).clamp(0, h);
        let r = (b[0].ceil() as i32).clamp(0, w);
        let bo = (b[1].ceil() as i32).clamp(0, h);
        let rect = Rect::from_ltrb(l, t, r, bo);
        if rect.is_empty() {
            return;
        }
        let mut zoom = map.scale * ppp;
        let longest = rect.w.max(rect.h) as f32 * zoom;
        if longest > MAX_LAYER_SIDE {
            zoom *= MAX_LAYER_SIDE / longest;
        }
        let size = (((rect.w as f32 * zoom).ceil() as u32).max(1), ((rect.h as f32 * zoom).ceil() as u32).max(1));
        let key = CacheKey { rev: self.rev, base: self.base_ptr, rect, size, hidden: self.hidden.clone() };
        let stale = !matches!(&self.tex, Some((k, _)) if *k == key);
        if stale {
            let img = self.render_view(rect, size, zoom);
            let ci = tile_to_color_image(&img, TileRect { x: 0, y: 0, w: img.width, h: img.height });
            match &mut self.tex {
                Some((k, tex)) if k.size == size => {
                    tex.set(ci, image_options());
                    *k = key;
                }
                _ => {
                    let tex = ctx.load_texture("draw-layer", ci, image_options());
                    self.tex = Some((key, tex));
                }
            }
        }
        if let Some((_, tex)) = &self.tex {
            let p0 = map.to_screen([rect.x as f64, rect.y as f64]);
            let p1 = map.to_screen([rect.right() as f64, rect.bottom() as f64]);
            let screen = egui::Rect::from_min_max(pos2(p0[0], p0[1]), pos2(p1[0], p1[1]));
            let uv = egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(tex.id(), screen, uv, Color32::WHITE);
        }
    }

    fn flatten(&self, base: &Image) -> Image {
        if self.doc.objects.is_empty() {
            return base.clone();
        }
        draw::render(base, &self.doc.objects, &RenderOptions::default())
    }

    fn flattener(&self) -> Option<Flattener> {
        if self.doc.objects.is_empty() {
            return None;
        }
        let objects = self.doc.objects.clone();
        Some(Arc::new(move |base: &Image| draw::render(base, &objects, &RenderOptions::default())))
    }

    fn undo(&mut self) {
        self.history.undo(&mut self.doc);
        self.rev = self.rev.wrapping_add(1);
    }

    fn redo(&mut self) {
        self.history.redo(&mut self.doc);
        self.rev = self.rev.wrapping_add(1);
    }

    fn is_empty(&self) -> bool {
        self.doc.objects.is_empty()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The document's draw layer, if it has one.
pub fn layer(doc: &super::super::document::Document) -> Option<&DrawLayer> {
    doc.overlay.as_ref().and_then(|o| o.as_any().downcast_ref::<DrawLayer>())
}

/// The document's draw layer, created on first use.
pub fn layer_mut(doc: &mut super::super::document::Document) -> Option<&mut DrawLayer> {
    if doc.overlay.is_none() {
        doc.overlay = Some(Box::new(DrawLayer::new(doc.image.clone())));
    }
    doc.overlay.as_mut().and_then(|o| o.as_any_mut().downcast_mut::<DrawLayer>())
}

/// Apply `f` to the objects and record one undo step called `name` in both histories.
/// Returns true if anything changed.
pub fn edit(doc: &mut super::super::document::Document, name: &str, f: impl FnOnce(&mut Vec<Object>) -> bool) -> bool {
    let Some(l) = layer_mut(doc) else { return false };
    let before = l.doc.objects.clone();
    if !f(l.objects_mut()) {
        return false;
    }
    let changed = l.record(&before);
    if changed {
        doc.record_overlay_step(name);
    }
    changed
}

/// Record a change already applied live (drags): `before` is the state at drag start.
pub fn commit(doc: &mut super::super::document::Document, name: &str, before: &[Object]) -> bool {
    let Some(l) = layer_mut(doc) else { return false };
    let changed = l.record(before);
    if changed {
        doc.record_overlay_step(name);
    }
    changed
}
