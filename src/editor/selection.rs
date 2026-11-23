//! Selections (rectangle / freehand), screen ↔ image mapping, floating pixels and the
//! image-edge resize math. Pure: no egui state, so everything here is unit-tested.

use crate::common::{Image, Rect};
use std::sync::Arc;

// ---------------------------------------------------------------- mapping

/// Maps canvas positions (egui points) to image pixels and back.
///
/// `origin` is the image's top-left corner in points (already snapped to a whole physical
/// pixel by the canvas) and `scale` is points per image pixel (`zoom / pixels_per_point`).
/// Image pixel `(x, y)` covers `[x, x+1) × [y, y+1)` in image space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasMap {
    pub origin: [f32; 2],
    pub scale: f32,
}

impl CanvasMap {
    /// Continuous image coordinates (f64 so huge images at 5000% stay exact).
    pub fn to_image(self, p: [f32; 2]) -> [f64; 2] {
        let s = self.scale.max(1e-6) as f64;
        [(p[0] as f64 - self.origin[0] as f64) / s, (p[1] as f64 - self.origin[1] as f64) / s]
    }

    /// The pixel under a point (may be outside the image).
    pub fn pixel_at(self, p: [f32; 2]) -> (i32, i32) {
        let q = self.to_image(p);
        (q[0].floor() as i32, q[1].floor() as i32)
    }

    /// Nearest pixel *edge* (grid line) to a point: used for rectangle corners so a drag
    /// from one grid line to another selects exactly the pixels in between.
    pub fn grid_at(self, p: [f32; 2]) -> (i32, i32) {
        let q = self.to_image(p);
        (q[0].round() as i32, q[1].round() as i32)
    }

    pub fn to_screen(self, p: [f64; 2]) -> [f32; 2] {
        [
            (self.origin[0] as f64 + p[0] * self.scale as f64) as f32,
            (self.origin[1] as f64 + p[1] * self.scale as f64) as f32,
        ]
    }
}

/// Rectangle selection between two drag points, in pixels, clipped to `bounds`.
///
/// When zoomed in far enough that a pixel is at least 4 points wide, the selection snaps
/// to pixel *cells* (both end pixels included, like a paint program); zoomed out it snaps
/// to grid lines, which is what the eye expects when pixels are smaller than the cursor.
pub fn drag_rect(map: &CanvasMap, a: [f32; 2], b: [f32; 2], bounds: Rect) -> Rect {
    let r = if map.scale >= 4.0 {
        let (ax, ay) = map.pixel_at(a);
        let (bx, by) = map.pixel_at(b);
        Rect::from_ltrb(ax.min(bx), ay.min(by), ax.max(bx) + 1, ay.max(by) + 1)
    } else {
        let (ax, ay) = map.grid_at(a);
        let (bx, by) = map.grid_at(b);
        Rect::from_ltrb(ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
    };
    r.intersect(&bounds)
}

// ---------------------------------------------------------------- selection

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Bounding box in image pixels, inside the image.
    pub bounds: Rect,
    /// Freehand coverage (0 / 255), `bounds.w * bounds.h`, row-major. `None` = full rect.
    pub mask: Option<Arc<Vec<u8>>>,
    /// Outline in image coordinates (closed polygon) for the marching ants.
    pub outline: Vec<[f32; 2]>,
}

impl Selection {
    pub fn rect(r: Rect) -> Option<Self> {
        if r.is_empty() {
            return None;
        }
        let (l, t, rr, b) = (r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32);
        Some(Self { bounds: r, mask: None, outline: vec![[l, t], [rr, t], [rr, b], [l, b]] })
    }

    /// A freehand selection from a polygon in image coordinates, clipped to `image`.
    pub fn freehand(points: &[[f32; 2]], image: Rect) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in points {
            l = l.min(p[0]);
            t = t.min(p[1]);
            r = r.max(p[0]);
            b = b.max(p[1]);
        }
        let bounds =
            Rect::from_ltrb(l.floor() as i32, t.floor() as i32, r.ceil() as i32, b.ceil() as i32).intersect(&image);
        if bounds.is_empty() {
            return None;
        }
        let mask = rasterize_polygon(points, bounds);
        let count = mask.iter().filter(|&&m| m != 0).count();
        if count == 0 {
            return None;
        }
        Some(Self { bounds, mask: Some(Arc::new(mask)), outline: points.to_vec() })
    }

    pub fn is_rect(&self) -> bool {
        self.mask.is_none()
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        if !self.bounds.contains(crate::common::Point::new(x, y)) {
            return false;
        }
        match &self.mask {
            None => true,
            Some(m) => m[((y - self.bounds.y) * self.bounds.w + (x - self.bounds.x)) as usize] != 0,
        }
    }

    pub fn pixel_count(&self) -> u64 {
        match &self.mask {
            None => self.bounds.area() as u64,
            Some(m) => m.iter().filter(|&&v| v != 0).count() as u64,
        }
    }

    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            bounds: self.bounds.offset(dx, dy),
            mask: self.mask.clone(),
            outline: self.outline.iter().map(|p| [p[0] + dx as f32, p[1] + dy as f32]).collect(),
        }
    }

    /// The part of the selection inside `area` (`None` when nothing is left). Pasted or
    /// moved pixels may hang over the edge of the picture; once they are merged, the
    /// selection must lie inside it again, or effects and Delete would work on a
    /// rectangle that does not match the pixels they are given.
    pub fn clipped(&self, area: Rect) -> Option<Selection> {
        let r = self.bounds.intersect(&area);
        if r.is_empty() {
            return None;
        }
        if r == self.bounds {
            return Some(self.clone());
        }
        let Some(mask) = &self.mask else { return Selection::rect(r) };
        let (bw, bh) = (self.bounds.w.max(0) as usize, self.bounds.h.max(0) as usize);
        if mask.len() != bw * bh {
            return Selection::rect(r);
        }
        let (ox, oy) = ((r.x - self.bounds.x) as usize, (r.y - self.bounds.y) as usize);
        let (w, h) = (r.w as usize, r.h as usize);
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = (oy + y) * bw + ox;
            out.extend_from_slice(&mask[row..row + w]);
        }
        Some(Selection { bounds: r, mask: Some(Arc::new(out)), outline: self.outline.clone() })
    }

    /// The selected pixels (outside a freehand mask transparent).
    pub fn extract(&self, img: &Image) -> Image {
        let mut out = img.crop(self.bounds);
        if let Some(m) = &self.mask {
            if (out.width as i32, out.height as i32) == (self.bounds.w, self.bounds.h) {
                out.apply_mask(m);
            }
        }
        out
    }

    /// Fill the selected pixels of `img` with `color` (premultiplied BGRA).
    pub fn fill(&self, img: &mut Image, color: [u8; 4]) {
        let r = self.bounds.intersect(&img.bounds());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                if self.contains(x, y) {
                    img.set_pixel(x as u32, y as u32, color);
                }
            }
        }
    }
}

/// Even-odd polygon fill sampled at pixel centres, for the pixels of `bounds`.
pub fn rasterize_polygon(points: &[[f32; 2]], bounds: Rect) -> Vec<u8> {
    let (w, h) = (bounds.w.max(0) as usize, bounds.h.max(0) as usize);
    let mut mask = vec![0u8; w * h];
    if points.len() < 3 || w == 0 {
        return mask;
    }
    let mut xs: Vec<f32> = Vec::new();
    for row in 0..h {
        let cy = bounds.y as f32 + row as f32 + 0.5;
        xs.clear();
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            if (a[1] <= cy && b[1] > cy) || (b[1] <= cy && a[1] > cy) {
                let t = (cy - a[1]) / (b[1] - a[1]);
                xs.push(a[0] + t * (b[0] - a[0]));
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks_exact(2) {
            // Pixel centres x + 0.5 inside [pair0, pair1).
            let x0 = ((pair[0] - 0.5).ceil() as i32 - bounds.x).clamp(0, w as i32);
            let x1 = ((pair[1] - 0.5).ceil() as i32 - bounds.x).clamp(0, w as i32);
            for v in &mut mask[row * w + x0 as usize..row * w + x1 as usize] {
                *v = 255;
            }
        }
    }
    mask
}

/// Drop freehand points closer than `min_dist` (image px) to the previous kept point.
pub fn push_point(points: &mut Vec<[f32; 2]>, p: [f32; 2], min_dist: f32) {
    if let Some(last) = points.last() {
        let (dx, dy) = (p[0] - last[0], p[1] - last[1]);
        if dx * dx + dy * dy < min_dist * min_dist {
            return;
        }
    }
    points.push(p);
}

// ---------------------------------------------------------------- floating pixels

/// Pixels lifted off the image (move / Ctrl+drag copy) or pasted, not yet merged.
#[derive(Clone)]
pub struct Floating {
    pub image: Arc<Image>,
    /// Top-left in image coordinates (may be outside the image).
    pub x: i32,
    pub y: i32,
    /// Freehand mask of the lifted pixels (same size as `image`), for the outline.
    pub selection: Selection,
    /// Where the pixels came from and what was there before lifting (a move cleared
    /// it). `None` for a copy or a paste.
    pub lifted: Option<(Rect, Image)>,
    pub kind: FloatKind,
    /// Top-left where the pixels started.
    pub origin: (i32, i32),
    /// Bumped when `image` changes (texture refresh).
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatKind {
    Move,
    Copy,
    Paste,
}

impl Floating {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.image.width as i32, self.image.height as i32)
    }

    /// Lift the selected pixels. A move clears them to `bg` in `img`.
    pub fn lift(img: &mut Image, sel: &Selection, copy: bool, bg: [u8; 4]) -> Option<Floating> {
        let sel_bounds = sel.bounds.intersect(&img.bounds());
        if sel_bounds.is_empty() {
            return None;
        }
        let pixels = sel.extract(img);
        let lifted = if copy {
            None
        } else {
            let before = img.crop(sel_bounds);
            sel.fill(img, bg);
            Some((sel_bounds, before))
        };
        Some(Floating {
            image: Arc::new(pixels),
            x: sel.bounds.x,
            y: sel.bounds.y,
            selection: sel.clone(),
            lifted,
            kind: if copy { FloatKind::Copy } else { FloatKind::Move },
            origin: (sel.bounds.x, sel.bounds.y),
            version: 0,
        })
    }

    pub fn pasted(img: Image, x: i32, y: i32) -> Floating {
        let r = Rect::new(x, y, img.width as i32, img.height as i32);
        let selection = Selection::rect(r).unwrap_or(Selection { bounds: r, mask: None, outline: Vec::new() });
        Floating {
            image: Arc::new(img),
            x,
            y,
            selection,
            lifted: None,
            kind: FloatKind::Paste,
            origin: (x, y),
            version: 0,
        }
    }

    /// The selection as it was before the pixels moved.
    pub fn selection_at_source(&self) -> Selection {
        self.selection.translated(self.origin.0 - self.x, self.origin.1 - self.y)
    }

    pub fn move_by(&mut self, dx: i32, dy: i32) {
        self.x += dx;
        self.y += dy;
        self.selection = self.selection.translated(dx, dy);
    }

    /// The region of the image a commit changes, and its pixels *before the lift*
    /// (reconstructed from the current image plus the saved source pixels).
    pub fn commit_region(&self, img: &Image) -> (Rect, Image) {
        let dest = self.rect().intersect(&img.bounds());
        let region = match &self.lifted {
            Some((src, _)) => dest.union(src),
            None => dest,
        };
        let mut before = img.crop(region);
        if let Some((src, pixels)) = &self.lifted {
            before.blit(pixels, src.x - region.x, src.y - region.y);
        }
        (region, before)
    }

    /// Composite the floating pixels into `img` (source-over, so freehand edges and
    /// transparent pastes blend).
    pub fn stamp(&self, img: &mut Image) {
        img.draw_over(&self.image, self.x, self.y);
    }

    /// Undo the lift: put the source pixels back.
    pub fn restore_source(&self, img: &mut Image) {
        if let Some((src, pixels)) = &self.lifted {
            img.blit(pixels, src.x, src.y);
        }
    }
}

// ---------------------------------------------------------------- image edge handles

/// A resize handle on the image border (canvas size by dragging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    pub const ALL: [Edge; 8] = [
        Edge::Left,
        Edge::Right,
        Edge::Top,
        Edge::Bottom,
        Edge::TopLeft,
        Edge::TopRight,
        Edge::BottomLeft,
        Edge::BottomRight,
    ];

    /// (moves left, moves top, moves right, moves bottom)
    fn sides(self) -> (bool, bool, bool, bool) {
        match self {
            Edge::Left => (true, false, false, false),
            Edge::Right => (false, false, true, false),
            Edge::Top => (false, true, false, false),
            Edge::Bottom => (false, false, false, true),
            Edge::TopLeft => (true, true, false, false),
            Edge::TopRight => (false, true, true, false),
            Edge::BottomLeft => (true, false, false, true),
            Edge::BottomRight => (false, false, true, true),
        }
    }

    /// Handle anchor as fractions of the image size.
    pub fn anchor(self) -> [f32; 2] {
        match self {
            Edge::Left => [0.0, 0.5],
            Edge::Right => [1.0, 0.5],
            Edge::Top => [0.5, 0.0],
            Edge::Bottom => [0.5, 1.0],
            Edge::TopLeft => [0.0, 0.0],
            Edge::TopRight => [1.0, 0.0],
            Edge::BottomLeft => [0.0, 1.0],
            Edge::BottomRight => [1.0, 1.0],
        }
    }
}

/// New canvas rectangle (in the old image's coordinates) after dragging `edge` by
/// `(dx, dy)` image pixels. Outward drags add space, inward drags trim; at least 1×1.
pub fn edge_drag_bounds(w: u32, h: u32, edge: Edge, dx: i32, dy: i32) -> Rect {
    let (ml, mt, mr, mb) = edge.sides();
    let (w, h) = (w as i32, h as i32);
    let mut l = if ml { dx } else { 0 };
    let mut t = if mt { dy } else { 0 };
    let mut r = if mr { w + dx } else { w };
    let mut b = if mb { h + dy } else { h };
    if ml {
        l = l.min(r - 1);
    } else if mr {
        r = r.max(l + 1);
    }
    if mt {
        t = t.min(b - 1);
    } else if mb {
        b = b.max(t + 1);
    }
    Rect::from_ltrb(l, t, r, b)
}

/// A new image covering `r` (old-image coordinates): old pixels where they overlap,
/// `fill` elsewhere.
pub fn resize_canvas(img: &Image, r: Rect, fill: [u8; 4]) -> Image {
    let mut out = Image::filled(r.w.max(1) as u32, r.h.max(1) as u32, fill);
    out.blit(img, -r.x, -r.y);
    out
}

// ---------------------------------------------------------------- statistics

/// Number of distinct colours (premultiplied BGRA) among the pixels `pick` accepts.
/// Opaque colours use a 2 MB bitset (fast on 4K images); others a hash set.
pub fn unique_colors(img: &Image, rect: Rect, pick: impl Fn(i32, i32) -> bool) -> u64 {
    let r = rect.intersect(&img.bounds());
    let mut bits = vec![0u64; (1 << 24) / 64];
    let mut other = std::collections::HashSet::new();
    let mut count = 0u64;
    for y in r.y..r.bottom() {
        let row = img.row(y as u32);
        for x in r.x..r.right() {
            if !pick(x, y) {
                continue;
            }
            let p = &row[x as usize * 4..x as usize * 4 + 4];
            if p[3] == 255 {
                let k = (p[0] as usize) | (p[1] as usize) << 8 | (p[2] as usize) << 16;
                let (word, bit) = (k / 64, 1u64 << (k % 64));
                if bits[word] & bit == 0 {
                    bits[word] |= bit;
                    count += 1;
                }
            } else if other.insert(u32::from_le_bytes([p[0], p[1], p[2], p[3]])) {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(zoom: f32, ppp: f32, origin: [f32; 2]) -> CanvasMap {
        CanvasMap { origin, scale: zoom / ppp }
    }

    #[test]
    fn mapping_roundtrip_at_many_zooms() {
        for &zoom in &[0.01f32, 0.0833, 0.3333, 0.5, 1.0, 1.5, 3.7, 16.0, 50.0] {
            for &ppp in &[1.0f32, 1.25, 1.5, 2.0] {
                let m = map(zoom, ppp, [13.2, 47.6]);
                for &(x, y) in &[(0i32, 0i32), (1, 2), (999, 77), (19_999, 8_999)] {
                    // The centre of a pixel maps back into that pixel.
                    let s = m.to_screen([x as f64 + 0.5, y as f64 + 0.5]);
                    assert_eq!(m.pixel_at(s), (x, y), "zoom {zoom} ppp {ppp} px {x},{y}");
                }
            }
        }
    }

    #[test]
    fn pixel_hit_is_exact_at_high_zoom() {
        // 5000% at 150% DPI: each pixel spans 33.3 points.
        let m = map(50.0, 1.5, [0.0, 0.0]);
        let w = 50.0 / 1.5;
        assert_eq!(m.pixel_at([w - 0.01, 0.0]), (0, 0));
        assert_eq!(m.pixel_at([w + 0.01, 0.0]), (1, 0));
        assert_eq!(m.pixel_at([-0.01, -0.01]), (-1, -1));
        // Fractional zoom 33.33%: 3 image px per point.
        let m = map(1.0 / 3.0, 1.0, [10.0, 10.0]);
        assert_eq!(m.pixel_at([10.0, 10.0]), (0, 0));
        assert_eq!(m.pixel_at([11.05, 10.0]), (3, 0));
        assert_eq!(m.pixel_at([10.95, 10.0]), (2, 0));
    }

    #[test]
    fn drag_rect_snaps_cells_or_grid() {
        let bounds = Rect::new(0, 0, 100, 100);
        // Zoomed in (8 points per pixel): cell snapping includes both end pixels.
        let m = map(8.0, 1.0, [0.0, 0.0]);
        assert_eq!(drag_rect(&m, [4.0, 4.0], [4.0, 4.0], bounds), Rect::new(0, 0, 1, 1));
        assert_eq!(drag_rect(&m, [20.0, 4.0], [4.0, 30.0], bounds), Rect::new(0, 0, 3, 4));
        // 100%: grid lines.
        let m = map(1.0, 1.0, [0.0, 0.0]);
        assert_eq!(drag_rect(&m, [10.4, 10.6], [20.2, 30.0], bounds), Rect::new(10, 11, 10, 19));
        // Clipped to the image.
        assert_eq!(drag_rect(&m, [-50.0, -50.0], [500.0, 20.0], bounds), Rect::new(0, 0, 100, 20));
        // 25% at 200% DPI: each point is 8 image px.
        let m = map(0.25, 2.0, [0.0, 0.0]);
        assert_eq!(drag_rect(&m, [1.0, 1.0], [3.0, 2.0], bounds), Rect::new(8, 8, 16, 8));
    }

    #[test]
    fn freehand_mask_and_hit() {
        // A right triangle (0,0)-(10,0)-(0,10).
        let sel = Selection::freehand(&[[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]], Rect::new(0, 0, 50, 50)).expect("sel");
        assert_eq!(sel.bounds, Rect::new(0, 0, 10, 10));
        assert!(sel.contains(0, 0) && sel.contains(4, 4) && sel.contains(8, 0));
        assert!(!sel.contains(5, 5) && !sel.contains(9, 9) && !sel.contains(11, 0));
        // Pixel centres strictly inside x + y < 10 → 45 pixels.
        assert_eq!(sel.pixel_count(), 45);
        // Degenerate polygons are rejected.
        assert!(Selection::freehand(&[[0.0, 0.0], [5.0, 5.0]], Rect::new(0, 0, 9, 9)).is_none());
        assert!(Selection::freehand(&[[0.0, 0.0], [5.0, 0.0], [10.0, 0.0]], Rect::new(0, 0, 9, 9)).is_none());
    }

    #[test]
    fn clipped_keeps_the_part_inside_the_picture() {
        let area = Rect::new(0, 0, 10, 10);
        // Inside: unchanged. Overhanging: cut. Outside: gone.
        let inside = Selection::rect(Rect::new(2, 2, 3, 3)).expect("sel");
        assert_eq!(inside.clipped(area), Some(inside.clone()));
        let over = Selection::rect(Rect::new(-4, 6, 8, 8)).expect("sel");
        let c = over.clipped(area).expect("clipped");
        assert_eq!(c.bounds, Rect::new(0, 6, 4, 4));
        assert!(c.mask.is_none());
        assert!(c.contains(0, 6) && c.contains(3, 9) && !c.contains(4, 6));
        assert!(Selection::rect(Rect::new(20, 20, 5, 5)).expect("sel").clipped(area).is_none());
        // A mask keeps exactly the rows and columns that are left.
        let mask: Vec<u8> = (0..16u8).collect();
        let free = Selection { bounds: Rect::new(8, -1, 4, 4), mask: Some(Arc::new(mask)), outline: Vec::new() };
        let c = free.clipped(area).expect("clipped");
        assert_eq!(c.bounds, Rect::new(8, 0, 2, 3));
        assert_eq!(c.mask.as_deref().map(|m| m.as_slice()), Some(&[4u8, 5, 8, 9, 12, 13][..]));
        // What it extracts now has the size of its bounds.
        let img = Image::filled(10, 10, [1, 2, 3, 255]);
        let out = c.extract(&img);
        assert_eq!((out.width, out.height), (2, 3));
    }

    #[test]
    fn freehand_is_clipped_to_image() {
        let sel =
            Selection::freehand(&[[-10.0, -10.0], [5.0, -10.0], [5.0, 5.0], [-10.0, 5.0]], Rect::new(0, 0, 20, 20))
                .expect("sel");
        assert_eq!(sel.bounds, Rect::new(0, 0, 5, 5));
        assert_eq!(sel.pixel_count(), 25);
    }

    fn numbered(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, [x as u8, y as u8, 1, 255]);
            }
        }
        img
    }

    #[test]
    fn floating_move_and_commit_region() {
        let orig = numbered(10, 10);
        let mut img = orig.clone();
        let sel = Selection::rect(Rect::new(2, 2, 3, 3)).expect("sel");
        let mut f = Floating::lift(&mut img, &sel, false, [255, 255, 255, 255]).expect("lift");
        // Source cleared to the background.
        assert_eq!(img.pixel(3, 3), [255, 255, 255, 255]);
        assert_eq!(f.image.pixel(0, 0), orig.pixel(2, 2));
        f.move_by(4, 1);
        assert_eq!(f.rect(), Rect::new(6, 3, 3, 3));
        let (region, before) = f.commit_region(&img);
        assert_eq!(region, Rect::new(2, 2, 7, 4));
        // "before" is exactly the pre-lift image in that region.
        assert_eq!(before, orig.crop(region));
        f.stamp(&mut img);
        assert_eq!(img.pixel(6, 3), orig.pixel(2, 2));
        assert_eq!(img.pixel(8, 5), orig.pixel(4, 4));
        assert_eq!(img.pixel(2, 2), [255, 255, 255, 255]);
    }

    #[test]
    fn floating_copy_keeps_source_and_cancel_restores() {
        let orig = numbered(8, 8);
        let mut img = orig.clone();
        let sel = Selection::rect(Rect::new(0, 0, 2, 2)).expect("sel");
        let mut f = Floating::lift(&mut img, &sel, true, [0, 0, 0, 0]).expect("lift");
        assert_eq!(img, orig);
        assert_eq!(f.kind, FloatKind::Copy);
        f.move_by(5, 5);
        f.stamp(&mut img);
        assert_eq!(img.pixel(6, 6), orig.pixel(1, 1));
        assert_eq!(img.pixel(1, 1), orig.pixel(1, 1));
        // Cancel a move: source pixels come back.
        let mut img2 = orig.clone();
        let f = Floating::lift(&mut img2, &sel, false, [0, 0, 0, 0]).expect("lift");
        assert_ne!(img2, orig);
        f.restore_source(&mut img2);
        assert_eq!(img2, orig);
    }

    #[test]
    fn floating_freehand_and_off_image() {
        let orig = numbered(10, 10);
        let mut img = orig.clone();
        let sel = Selection::freehand(&[[0.0, 0.0], [6.0, 0.0], [0.0, 6.0]], img.bounds()).expect("sel");
        let mut f = Floating::lift(&mut img, &sel, false, [0, 0, 0, 0]).expect("lift");
        // Outside the triangle the floating pixels are transparent and the source kept.
        assert_eq!(f.image.pixel(5, 5)[3], 0);
        assert_eq!(img.pixel(5, 5), orig.pixel(5, 5));
        assert_eq!(img.pixel(1, 1), [0, 0, 0, 0]);
        // Dragged partly off the image: the commit region is clipped.
        f.move_by(-3, 7);
        let (region, before) = f.commit_region(&img);
        assert_eq!(region, Rect::new(0, 0, 6, 10));
        assert_eq!(before, orig.crop(region));
        // Paste of a 1×1 transparent image.
        let p = Floating::pasted(Image::new(1, 1), 3, 3);
        assert_eq!(p.rect(), Rect::new(3, 3, 1, 1));
        assert_eq!(p.selection.pixel_count(), 1);
    }

    #[test]
    fn edge_drag_trim_and_expand() {
        // Expand right by 20, trim bottom by 10.
        assert_eq!(edge_drag_bounds(100, 50, Edge::Right, 20, 99), Rect::new(0, 0, 120, 50));
        assert_eq!(edge_drag_bounds(100, 50, Edge::Bottom, 99, -10), Rect::new(0, 0, 100, 40));
        // Left edge outward (negative dx) adds space on the left.
        assert_eq!(edge_drag_bounds(100, 50, Edge::Left, -30, 0), Rect::new(-30, 0, 130, 50));
        assert_eq!(edge_drag_bounds(100, 50, Edge::TopLeft, 10, 5), Rect::new(10, 5, 90, 45));
        assert_eq!(edge_drag_bounds(100, 50, Edge::BottomRight, -10, 10), Rect::new(0, 0, 90, 60));
        // Never smaller than 1×1, even when dragged past the opposite edge.
        assert_eq!(edge_drag_bounds(100, 50, Edge::Left, 500, 0), Rect::new(99, 0, 1, 50));
        assert_eq!(edge_drag_bounds(100, 50, Edge::Top, 0, 500), Rect::new(0, 49, 100, 1));
        assert_eq!(edge_drag_bounds(100, 50, Edge::Right, -500, 0), Rect::new(0, 0, 1, 50));
    }

    #[test]
    fn resize_canvas_trims_and_pads() {
        let img = numbered(4, 3);
        let out = resize_canvas(&img, Rect::new(-2, 1, 7, 2), [9, 9, 9, 255]);
        assert_eq!((out.width, out.height), (7, 2));
        assert_eq!(out.pixel(0, 0), [9, 9, 9, 255]);
        assert_eq!(out.pixel(2, 0), img.pixel(0, 1));
        assert_eq!(out.pixel(5, 1), img.pixel(3, 2));
        assert_eq!(out.pixel(6, 1), [9, 9, 9, 255]);
    }

    #[test]
    fn unique_colour_count() {
        let mut img = Image::filled(10, 10, [1, 2, 3, 255]);
        img.set_pixel(0, 0, [9, 9, 9, 255]);
        img.set_pixel(1, 0, [0, 0, 0, 0]);
        img.set_pixel(2, 0, [0, 0, 0, 0]);
        assert_eq!(unique_colors(&img, img.bounds(), |_, _| true), 3);
        assert_eq!(unique_colors(&img, Rect::new(5, 5, 2, 2), |_, _| true), 1);
        assert_eq!(unique_colors(&img, img.bounds(), |x, _| x > 2), 1);
    }
}
