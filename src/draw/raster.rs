//! Raster tools: blur/pixelate, eraser, flood fill and pencil. They modify the base image but
//! are stored as objects, so they stay editable and undoable. They are applied in list order,
//! before (below) all vector objects.

use super::fx;
use super::model::*;
use super::render::RenderOptions;
use crate::common::{Image, Rect};
use std::hash::{Hash, Hasher};
use tiny_skia::{FillRule, Mask, PathBuilder, Transform};

/// The base image with every visible raster object applied, in order.
pub fn apply(base: &Image, objects: &[Object], opts: &RenderOptions) -> Image {
    let mut img = base.clone();
    for o in objects.iter().filter(|o| o.is_raster() && opts.shows(o)) {
        apply_one(&mut img, o);
    }
    img
}

/// Apply one raster object to `img` in place (vector objects are ignored).
pub fn apply_one(img: &mut Image, o: &Object) {
    if img.is_empty() {
        return;
    }
    let opacity = if o.style.opacity.is_finite() { o.style.opacity.clamp(0.0, 1.0) } else { 1.0 };
    match &o.kind {
        Kind::Blur(b) => blur_region(img, b, opacity),
        Kind::Eraser(e) => erase(img, e, opacity),
        Kind::Fill(f) => flood_fill(img, f, o.style.fill.fade(opacity)),
        Kind::Pencil(p) => pencil(img, &p.points, o.style.width, o.style.color.fade(opacity)),
        _ => {}
    }
}

fn int_bounds(r: RectF) -> Rect {
    Rect::from_ltrb(r.x.floor() as i32, r.y.floor() as i32, r.right().ceil() as i32, r.bottom().ceil() as i32)
}

/// Coverage mask over `area` (image px) for a path given in image coordinates.
fn mask_for(area: Rect, path: &tiny_skia::Path, aa: bool) -> Option<Mask> {
    let mut m = Mask::new(area.w as u32, area.h as u32)?;
    m.fill_path(path, FillRule::Winding, aa, Transform::from_translate(-area.x as f32, -area.y as f32));
    Some(m)
}

/// Mask for a round-capped stroke through `points` (a dot for a single point).
fn stroke_mask(img: &Image, points: &[Pt], width: f32, aa: bool) -> Option<(Rect, Mask)> {
    let width = if width.is_finite() { width.clamp(0.5, 2000.0) } else { 1.0 };
    let area = int_bounds(RectF::bounding(points).inflate(width / 2.0 + 2.0)).intersect(&img.bounds());
    if area.is_empty() || points.is_empty() {
        return None;
    }
    let distinct = points.windows(2).any(|w| w[0].dist(w[1]) > 1e-3);
    let path = if distinct {
        let line = super::path::polyline_path(points, false)?;
        let stroke = tiny_skia::Stroke {
            width,
            line_cap: tiny_skia::LineCap::Round,
            line_join: tiny_skia::LineJoin::Round,
            ..Default::default()
        };
        line.stroke(&stroke, 1.0)?
    } else {
        PathBuilder::from_circle(points[0].x, points[0].y, width / 2.0)?
    };
    Some((area, mask_for(area, &path, aa)?))
}

/// Source-over `color` (straight alpha) through a mask covering `area`.
fn paint_masked(img: &mut Image, area: Rect, mask: &Mask, color: Color) {
    let a = color.a as u32;
    let pm = [color.b as u32 * a / 255, color.g as u32 * a / 255, color.r as u32 * a / 255, a];
    let w = img.width as usize;
    let md = mask.data();
    for y in 0..area.h as usize {
        for x in 0..area.w as usize {
            let m = md[y * area.w as usize + x] as u32;
            if m == 0 {
                continue;
            }
            let i = ((area.y as usize + y) * w + area.x as usize + x) * 4;
            let inv = 255 * 255 - a * m;
            for c in 0..4 {
                let v = (pm[c] * m + img.data[i + c] as u32 * inv / 255 + 127) / 255;
                img.data[i + c] = v.min(255) as u8;
            }
        }
    }
}

fn blur_region(img: &mut Image, b: &Blur, opacity: f32) {
    let area = int_bounds(b.rect).intersect(&img.bounds());
    if area.is_empty() {
        return;
    }
    let strength = if b.strength.is_finite() { b.strength.clamp(1.0, 500.0) } else { 8.0 };
    // Work on a margin around the area so the blur near the edges sees real neighbours.
    let margin = if b.mode == BlurMode::Blur { (strength * 3.0).ceil() as i32 } else { 0 };
    let work_r = area.inflate(margin).intersect(&img.bounds());
    let mut work = img.crop(work_r);
    let (ww, wh) = (work.width as usize, work.height as usize);
    match b.mode {
        BlurMode::Blur => fx::blur(&mut work.data, ww, wh, strength),
        BlurMode::Pixelate => fx::pixelate(
            &mut work.data,
            ww,
            wh,
            strength.round() as usize,
            (b.rect.x.floor() as i64 - work_r.x as i64, b.rect.y.floor() as i64 - work_r.y as i64),
        ),
    }
    let shape = if b.ellipse { super::path::ellipse_path(b.rect) } else { super::path::round_rect_path(b.rect, 0.0) };
    let Some(mask) = shape.and_then(|p| mask_for(area, &p, true)) else { return };
    let k = (opacity * 255.0).round() as u32;
    let w = img.width as usize;
    let md = mask.data();
    for y in 0..area.h as usize {
        for x in 0..area.w as usize {
            let m = md[y * area.w as usize + x] as u32 * k / 255;
            if m == 0 {
                continue;
            }
            let i = ((area.y as usize + y) * w + area.x as usize + x) * 4;
            let j = (((area.y - work_r.y) as usize + y) * ww + (area.x - work_r.x) as usize + x) * 4;
            for c in 0..4 {
                let v = (work.data[j + c] as u32 * m + img.data[i + c] as u32 * (255 - m) + 127) / 255;
                img.data[i + c] = v as u8;
            }
        }
    }
}

fn erase(img: &mut Image, e: &Eraser, opacity: f32) {
    let Some((area, mask)) = stroke_mask(img, &e.points, e.radius * 2.0, true) else { return };
    match e.background {
        Some(bg) => {
            let mut scaled = mask;
            let k = (opacity * 255.0).round() as u32;
            for v in scaled.data_mut() {
                *v = (*v as u32 * k / 255) as u8;
            }
            // Painting an opaque background replaces; a translucent one blends.
            paint_masked(img, area, &scaled, bg);
        }
        None => {
            let k = (opacity * 255.0).round() as u32;
            let w = img.width as usize;
            let md = mask.data();
            for y in 0..area.h as usize {
                for x in 0..area.w as usize {
                    let m = md[y * area.w as usize + x] as u32 * k / 255;
                    if m == 0 {
                        continue;
                    }
                    let i = ((area.y as usize + y) * w + area.x as usize + x) * 4;
                    for c in 0..4 {
                        img.data[i + c] = ((img.data[i + c] as u32 * (255 - m) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

fn pencil(img: &mut Image, points: &[Pt], width: f32, color: Color) {
    if let Some((area, mask)) = stroke_mask(img, points, width.max(1.0), false) {
        paint_masked(img, area, &mask, color);
    }
}

/// 4-connected scanline flood fill from `f.at`, matching pixels within `f.tolerance` of the
/// seed (per channel, premultiplied). Fills with `color` or clears to transparent.
fn flood_fill(img: &mut Image, f: &Fill, color: Color) {
    let (sx, sy) = (f.at.x.floor(), f.at.y.floor());
    if !(sx >= 0.0 && sy >= 0.0 && sx < img.width as f32 && sy < img.height as f32) {
        return;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (sx, sy) = (sx as usize, sy as usize);
    let seed = img.pixel(sx as u32, sy as u32);
    let tol = f.tolerance as i32;
    let fill_px: [u8; 4] = if f.transparent {
        [0, 0, 0, 0]
    } else {
        let a = color.a as u32;
        let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        [pm(color.b), pm(color.g), pm(color.r), color.a]
    };
    let matches = |d: &[u8], i: usize| -> bool { (0..4).all(|c| (d[i + c] as i32 - seed[c] as i32).abs() <= tol) };
    let mut visited = vec![false; w * h];
    let mut stack = vec![(sx, sy)];
    let data = &mut img.data;
    let mut filled: Vec<usize> = Vec::new();
    while let Some((x, y)) = stack.pop() {
        if visited[y * w + x] || !matches(data, (y * w + x) * 4) {
            continue;
        }
        // Extend left and right along the row.
        let mut l = x;
        while l > 0 && !visited[y * w + l - 1] && matches(data, (y * w + l - 1) * 4) {
            l -= 1;
        }
        let mut r = x;
        while r + 1 < w && !visited[y * w + r + 1] && matches(data, (y * w + r + 1) * 4) {
            r += 1;
        }
        for xx in l..=r {
            visited[y * w + xx] = true;
            filled.push(y * w + xx);
            for ny in [y.wrapping_sub(1), y + 1] {
                if ny < h && !visited[ny * w + xx] {
                    stack.push((xx, ny));
                }
            }
        }
    }
    // Write after the search so matching always compares against the original pixels.
    let a = fill_px[3] as u32;
    for idx in filled {
        let i = idx * 4;
        if f.transparent || a == 255 {
            data[i..i + 4].copy_from_slice(&fill_px);
        } else {
            for c in 0..4 {
                data[i + c] = (fill_px[c] as u32 + data[i + c] as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

/// Keeps the effective base (base + raster objects) between frames. Re-applies only what
/// changed: appending raster objects, or editing the most recent one (e.g. a pencil stroke being
/// drawn), reuses the previous result.
#[derive(Default)]
pub struct RasterCache {
    base_key: Option<(u64, u32, u32)>,
    keys: Vec<u64>,
    image: Option<Image>,
    /// State before the last applied object.
    prefix: Option<(Vec<u64>, Image)>,
}

impl RasterCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The effective base. `base_rev` must change whenever the base pixels change
    /// (see [`super::Document::base_rev`]).
    pub fn get<'a>(
        &'a mut self,
        base: &'a Image,
        base_rev: u64,
        objects: &[Object],
        opts: &RenderOptions,
    ) -> &'a Image {
        let raster: Vec<&Object> = objects.iter().filter(|o| o.is_raster() && opts.shows(o)).collect();
        if raster.is_empty() {
            return base;
        }
        let base_key = (base_rev, base.width, base.height);
        if self.base_key != Some(base_key) {
            *self = Self { base_key: Some(base_key), ..Self::default() };
        }
        let keys: Vec<u64> = raster.iter().map(|o| object_hash(o)).collect();
        if self.image.is_some() && self.keys == keys {
            return self.image.as_ref().map_or(base, |i| i);
        }
        // Pick the longest cached state that is a prefix of the new list.
        let (start_keys, start_img) = if let (Some(img), true) = (self.image.take(), keys.starts_with(&self.keys)) {
            (std::mem::take(&mut self.keys), img)
        } else if let Some((pk, pimg)) = self.prefix.take().filter(|(pk, _)| keys.starts_with(pk)) {
            (pk, pimg)
        } else {
            (Vec::new(), base.clone())
        };
        let mut img = start_img;
        let n = keys.len();
        for (i, o) in raster.iter().enumerate().skip(start_keys.len()) {
            if i + 1 == n {
                self.prefix = Some((keys[..i].to_vec(), img.clone()));
            }
            apply_one(&mut img, o);
        }
        self.keys = keys;
        self.image = Some(img);
        self.image.as_ref().map_or(base, |i| i)
    }
}

fn object_hash(o: &Object) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match serde_json::to_vec(o) {
        Ok(bytes) => bytes.hash(&mut h),
        Err(_) => o.id.hash(&mut h),
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(id: u64, kind: Kind) -> Object {
        Object::new(id, kind, Style::default())
    }

    #[test]
    fn fill_respects_tolerance_and_borders() {
        let mut img = Image::filled(10, 10, [255, 255, 255, 255]);
        for y in 0..10 {
            img.set_pixel(5, y, [0, 0, 0, 255]);
        }
        let mut o = obj(1, Kind::Fill(Fill { at: Pt::new(1.0, 1.0), tolerance: 10, transparent: false }));
        o.style.fill = Color::rgb(255, 0, 0);
        apply_one(&mut img, &o);
        assert_eq!(img.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(img.pixel(4, 9), [0, 0, 255, 255]);
        assert_eq!(img.pixel(5, 5), [0, 0, 0, 255]);
        assert_eq!(img.pixel(7, 5), [255, 255, 255, 255]);
        let t = obj(2, Kind::Fill(Fill { at: Pt::new(8.0, 1.0), tolerance: 0, transparent: true }));
        apply_one(&mut img, &t);
        assert_eq!(img.pixel(9, 9), [0, 0, 0, 0]);
    }

    #[test]
    fn eraser_and_pencil() {
        let mut img = Image::filled(20, 20, [10, 20, 30, 255]);
        let e = obj(1, Kind::Eraser(Eraser { points: vec![Pt::new(10.0, 10.0)], radius: 4.0, background: None }));
        apply_one(&mut img, &e);
        assert_eq!(img.pixel(10, 10)[3], 0);
        assert_eq!(img.pixel(0, 0)[3], 255);
        let mut p = obj(2, Kind::Pencil(Pencil { points: vec![Pt::new(0.0, 2.5), Pt::new(19.0, 2.5)] }));
        p.style.color = Color::rgb(0, 255, 0);
        p.style.width = 1.0;
        apply_one(&mut img, &p);
        assert_eq!(img.pixel(7, 2), [0, 255, 0, 255], "hard pixels, no anti-aliasing");
    }

    #[test]
    fn cache_reuses_prefix() {
        let base = Image::filled(16, 16, [200, 200, 200, 255]);
        let mut objs = vec![obj(1, Kind::Pencil(Pencil { points: vec![Pt::new(1.0, 1.0), Pt::new(10.0, 1.0)] }))];
        let opts = RenderOptions::default();
        let mut cache = RasterCache::new();
        let a = cache.get(&base, 1, &objs, &opts).clone();
        assert_eq!(a, apply(&base, &objs, &opts));
        objs.push(obj(2, Kind::Pencil(Pencil { points: vec![Pt::new(1.0, 8.0), Pt::new(10.0, 8.0)] })));
        let b = cache.get(&base, 1, &objs, &opts).clone();
        assert_eq!(b, apply(&base, &objs, &opts));
        if let Kind::Pencil(p) = &mut objs[1].kind {
            p.points.push(Pt::new(12.0, 12.0));
        }
        let c = cache.get(&base, 1, &objs, &opts).clone();
        assert_eq!(c, apply(&base, &objs, &opts));
        objs.remove(0);
        let d = cache.get(&base, 1, &objs, &opts).clone();
        assert_eq!(d, apply(&base, &objs, &opts));
        assert!(std::ptr::eq(cache.get(&base, 1, &[], &opts), &base));
    }
}
