//! Turning a [`Selection`] into the final image from the frozen screen.

use super::{Frozen, Selection};
use crate::common::{Image, Point, Rect};

pub fn render_selection(frozen: &Frozen, sel: &Selection) -> Image {
    let o = frozen.origin;
    match sel {
        Selection::Rect(r) | Selection::Window { rect: r, .. } => frozen.image.crop(r.offset(-o.x, -o.y)),
        Selection::Polygon(points) => polygon(&frozen.image, o, points),
        Selection::Multi(rects) => multi(&frozen.image, o, rects),
    }
}

/// Inclusive pixel bounding box of a point list.
pub fn points_bounds(points: &[Point]) -> Rect {
    let Some(first) = points.first() else {
        return Rect::default();
    };
    let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.x, first.y);
    for p in points {
        l = l.min(p.x);
        t = t.min(p.y);
        r = r.max(p.x);
        b = b.max(p.y);
    }
    Rect::from_ltrb(l, t, r + 1, b + 1)
}

fn polygon(img: &Image, origin: Rect, points: &[Point]) -> Image {
    // Bounding box in image coordinates, clipped to the image.
    let bb = points_bounds(points).offset(-origin.x, -origin.y).intersect(&img.bounds());
    let mut out = img.crop(bb);
    if out.is_empty() || points.len() < 3 {
        return Image::new(out.width, out.height);
    }
    let mut pb = tiny_skia::PathBuilder::new();
    // Path through pixel centres, relative to the crop.
    let (dx, dy) = ((origin.x + bb.x) as f32 - 0.5, (origin.y + bb.y) as f32 - 0.5);
    for (i, p) in points.iter().enumerate() {
        let (x, y) = (p.x as f32 - dx, p.y as f32 - dy);
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    pb.close();
    let Some(path) = pb.finish() else {
        return Image::new(out.width, out.height);
    };
    let Some(mut mask) = tiny_skia::Mask::new(out.width, out.height) else {
        return Image::new(out.width, out.height);
    };
    mask.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
    out.apply_mask(mask.data());
    out
}

fn multi(img: &Image, origin: Rect, rects: &[Rect]) -> Image {
    let bb = rects.iter().fold(Rect::default(), |acc, r| acc.union(r));
    if bb.is_empty() {
        return Image::new(0, 0);
    }
    let mut out = Image::new(bb.w as u32, bb.h as u32);
    for r in rects {
        let part = img.crop(r.offset(-origin.x, -origin.y));
        // `crop` clips to the image: recompute where the clipped part starts.
        let clipped = r.offset(-origin.x, -origin.y).intersect(&img.bounds());
        out.blit(&part, clipped.x + origin.x - bb.x, clipped.y + origin.y - bb.y);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frozen() -> Frozen {
        // A virtual screen starting at (-100, -50), with a gradient so crops are checkable.
        let mut img = Image::new(200, 100);
        for y in 0..100 {
            for x in 0..200 {
                img.set_pixel(x, y, [x as u8, y as u8, 7, 255]);
            }
        }
        Frozen { image: img, origin: Rect::new(-100, -50, 200, 100), windows: Vec::new() }
    }

    #[test]
    fn rect_crop_uses_virtual_coordinates() {
        let f = frozen();
        let out = render_selection(&f, &Selection::Rect(Rect::new(-90, -40, 20, 10)));
        assert_eq!((out.width, out.height), (20, 10));
        assert_eq!(out.pixel(0, 0), [10, 10, 7, 255]);
        let w = render_selection(&f, &Selection::Window { hwnd: 5, rect: Rect::new(0, 0, 5, 5) });
        assert_eq!(w.pixel(0, 0), [100, 50, 7, 255]);
    }

    #[test]
    fn polygon_masks_outside() {
        let f = frozen();
        // Right triangle with the right angle at the top-left.
        let pts = vec![Point::new(-80, -40), Point::new(-41, -40), Point::new(-80, -1)];
        let out = render_selection(&f, &Selection::Polygon(pts));
        assert_eq!((out.width, out.height), (40, 40));
        // Well inside: untouched and opaque.
        assert_eq!(out.pixel(5, 5), [25, 15, 7, 255]);
        // Beyond the hypotenuse: fully transparent.
        for (x, y) in [(39, 39), (30, 30), (25, 30), (38, 5)] {
            assert_eq!(out.pixel(x, y), [0, 0, 0, 0], "pixel {x},{y}");
        }
        // Every pixel is valid premultiplied data.
        for px in out.data.chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]);
        }
        assert!(out.data.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255), "edges are anti-aliased");
    }

    #[test]
    fn multi_is_transparent_between_rects() {
        let f = frozen();
        let sel = Selection::Multi(vec![Rect::new(-100, -50, 10, 10), Rect::new(80, 40, 20, 10)]);
        let out = render_selection(&f, &sel);
        assert_eq!((out.width, out.height), (200, 100));
        assert_eq!(out.pixel(0, 0), [0, 0, 7, 255]);
        assert_eq!(out.pixel(185, 95), [185, 95, 7, 255]);
        assert_eq!(out.pixel(100, 50), [0, 0, 0, 0]);
        assert_eq!(out.pixel(10, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn multi_clips_offscreen_parts() {
        let f = frozen();
        let sel = Selection::Multi(vec![Rect::new(-110, -50, 20, 5), Rect::new(0, 0, 4, 4)]);
        let out = render_selection(&f, &sel);
        assert_eq!((out.width, out.height), (114, 54));
        assert_eq!(out.pixel(0, 0), [0, 0, 0, 0], "off-screen part stays transparent");
        assert_eq!(out.pixel(10, 0), [0, 0, 7, 255]);
        assert_eq!(out.pixel(110, 50), [100, 50, 7, 255]);
    }
}
