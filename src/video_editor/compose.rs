//! CPU frame composition shared by export and preview: title cards, zoom & pan crop/scale and
//! timed annotations.
//!
//! A video frame is composed as: raster annotations (blur, …) applied to the source frame →
//! the zoom rectangle scaled to the output size → vector annotations rendered at the output
//! resolution on top (`draw::render_overlay` with a view onto the zoom rectangle), so zoomed-in
//! annotations stay sharp. The preview uses the same view mapping.

use super::model::{Background, EditState, TitleCard};
use super::zoom;
use crate::common::Image;
use crate::draw::{self, Color, Kind, Object, RectF, RenderOptions, Style, TextAlign, View};

/// Opaque BGRA pixel from a draw colour (alpha ignored).
fn bgra(c: Color) -> [u8; 4] {
    [c.b, c.g, c.r, 255]
}

/// Fill a background (solid or linear gradient).
pub fn fill_background(bg: &Background, w: u32, h: u32) -> Image {
    match *bg {
        Background::Solid { color } => Image::filled(w, h, bgra(color)),
        Background::Gradient { from, to, angle } => {
            let mut img = Image::new(w, h);
            let a = if angle.is_finite() { angle.to_radians() } else { 0.0 };
            let (dx, dy) = (a.cos(), a.sin());
            // Project the corners to find the gradient's extent along the direction.
            let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
            let half = (cx * dx.abs() + cy * dy.abs()).max(1.0);
            let (f, t) = (bgra(from), bgra(to));
            let row_bytes = w as usize * 4;
            for y in 0..h as usize {
                let row = &mut img.data[y * row_bytes..(y + 1) * row_bytes];
                let py = (y as f32 + 0.5 - cy) * dy;
                for (x, px) in row.chunks_exact_mut(4).enumerate() {
                    let k = (((x as f32 + 0.5 - cx) * dx + py) / half * 0.5 + 0.5).clamp(0.0, 1.0);
                    for c in 0..3 {
                        px[c] = (f[c] as f32 + (t[c] as f32 - f[c] as f32) * k).round() as u8;
                    }
                    px[3] = 255;
                }
            }
            img
        }
    }
}

fn text_object(id: u64, text: &str, rect: RectF, size_pt: f32, color: Color, bold: bool) -> Object {
    let mut style = Style { width: 0.0, ..Style::default() };
    style.font.size = size_pt.clamp(1.0, draw::Font::MAX_SIZE);
    style.font.color = color;
    style.font.bold = bold;
    style.font.align = TextAlign::Center;
    style.shadow = Some(draw::Shadow { dx: 0.0, dy: 2.0, blur: 6.0, color: Color::rgba(0, 0, 0, 90) });
    let t = draw::Text { rect, text: text.to_string(), wrap: true, padding: 0.0, ..draw::Text::default() };
    Object::new(id, Kind::Text(t), style)
}

/// Render a title card at `w`×`h`.
pub fn render_title(card: &TitleCard, w: u32, h: u32) -> Image {
    let (w, h) = (w.max(2), h.max(2));
    let bg = fill_background(&card.background, w, h);
    let k = h as f32 / 1080.0;
    let size = if card.font_size.is_finite() { card.font_size.clamp(6.0, 200.0) } else { 54.0 };
    let title_pt = (size * k).max(4.0);
    let sub_pt = (title_pt * 0.45).max(4.0);
    let px = |pt: f32| pt * 96.0 / 72.0;
    let lines = |s: &str| if s.trim().is_empty() { 0 } else { s.lines().count().max(1) };
    let (tl, sl) = (lines(&card.title), lines(&card.subtitle));
    let title_h = tl as f32 * px(title_pt) * 1.35;
    let sub_h = sl as f32 * px(sub_pt) * 1.35;
    let gap = if tl > 0 && sl > 0 { px(title_pt) * 0.45 } else { 0.0 };
    let y0 = (h as f32 - title_h - gap - sub_h) / 2.0;
    let x = w as f32 * 0.08;
    let tw = w as f32 * 0.84;
    let mut objs = Vec::new();
    if tl > 0 {
        objs.push(text_object(1, &card.title, RectF::new(x, y0, tw, title_h), title_pt, card.text_color, true));
    }
    if sl > 0 {
        let c = card.text_color.with_alpha(220);
        objs.push(text_object(2, &card.subtitle, RectF::new(x, y0 + title_h + gap, tw, sub_h), sub_pt, c, false));
    }
    if objs.is_empty() {
        return bg;
    }
    draw::render(&bg, &objs, &RenderOptions::default())
}

/// Bilinear crop-and-scale of `r` (source pixels) to `ow`×`oh`. Fixed-point, opaque or
/// premultiplied input.
pub fn scale_region(src: &Image, r: RectF, ow: u32, oh: u32) -> Image {
    let (ow, oh) = (ow.max(1), oh.max(1));
    if src.is_empty() {
        return Image::new(ow, oh);
    }
    let full = RectF::new(0.0, 0.0, src.width as f32, src.height as f32);
    let r = if r.is_finite() && !r.is_empty() { r } else { full };
    if (ow, oh) == (src.width, src.height) && r == full {
        return src.clone();
    }
    // Per-axis sample positions: index of the left/top sample and an 8-bit weight.
    let axis = |start: f32, len: f32, n: u32, max: u32| -> Vec<(usize, usize, u32)> {
        let step = len / n as f32;
        (0..n)
            .map(|i| {
                let s = (start + (i as f32 + 0.5) * step - 0.5).clamp(0.0, (max - 1) as f32);
                let i0 = s.floor() as usize;
                let i1 = (i0 + 1).min(max as usize - 1);
                let f = ((s - i0 as f32) * 256.0).round() as u32;
                (i0, i1, f.min(256))
            })
            .collect()
    };
    let xs = axis(r.x, r.w, ow, src.width);
    let ys = axis(r.y, r.h, oh, src.height);
    let mut out = Image::new(ow, oh);
    let sw = src.width as usize * 4;
    let orow = ow as usize * 4;
    for (oy, &(y0, y1, fy)) in ys.iter().enumerate() {
        let r0 = &src.data[y0 * sw..(y0 + 1) * sw];
        let r1 = &src.data[y1 * sw..(y1 + 1) * sw];
        let dst = &mut out.data[oy * orow..(oy + 1) * orow];
        for (px, &(x0, x1, fx)) in dst.chunks_exact_mut(4).zip(&xs) {
            let (a0, a1) = (x0 * 4, x1 * 4);
            for c in 0..4 {
                let top = r0[a0 + c] as u32 * (256 - fx) + r0[a1 + c] as u32 * fx;
                let bot = r1[a0 + c] as u32 * (256 - fx) + r1[a1 + c] as u32 * fx;
                px[c] = ((top * (256 - fy) + bot * fy + 32768) >> 16) as u8;
            }
        }
    }
    out
}

/// Premultiplied source-over of a same-size layer onto `dst`.
pub fn over(dst: &mut Image, layer: &Image) {
    if dst.width != layer.width || dst.height != layer.height {
        return;
    }
    for (d, s) in dst.data.chunks_exact_mut(4).zip(layer.data.chunks_exact(4)) {
        let a = s[3] as u32;
        if a == 0 {
            continue;
        }
        if a == 255 {
            d.copy_from_slice(s);
            continue;
        }
        let ia = 255 - a;
        for c in 0..4 {
            d[c] = (s[c] as u32 + (d[c] as u32 * ia + 127) / 255).min(255) as u8;
        }
    }
}

/// The view (for `draw::render_overlay`) that maps source rectangle `r` onto an `ow`×`oh` layer.
pub fn view_for(r: RectF, ow: u32, oh: u32) -> View {
    View { x: r.x, y: r.y, zoom: ow as f32 / r.w.max(1e-3), width: ow, height: oh }
}

/// Compose one output frame from source frame `frame` shown at source time `src_ms`.
pub fn compose_video_frame(frame: &Image, src_ms: u64, edit: &EditState, ow: u32, oh: u32) -> Image {
    let opts = RenderOptions { time_ms: Some(src_ms), hidden: Vec::new() };
    let visible: Vec<&Object> = edit.annotations.iter().filter(|o| opts.shows(o)).collect();
    let any_raster = visible.iter().any(|o| o.is_raster());
    let effective;
    let base = if any_raster {
        effective = draw::raster::apply(frame, &edit.annotations, &opts);
        &effective
    } else {
        frame
    };
    let r = zoom::view_at(&edit.zoom, src_ms).rect(frame.width as f32, frame.height as f32);
    let mut out = scale_region(base, r, ow, oh);
    if visible.iter().any(|o| !o.is_raster()) {
        let layer = draw::render_overlay(base, &edit.annotations, &view_for(r, ow, oh), &opts);
        over(&mut out, &layer);
    }
    out
}

/// Output size for a source size and a target width (keeps the aspect ratio, even numbers when
/// `even`).
pub fn fit_width(src_w: u32, src_h: u32, width: u32, even: bool) -> (u32, u32) {
    let w = width.clamp(16, src_w.max(16));
    let h = ((src_h as f64 * w as f64 / src_w.max(1) as f64).round() as u32).max(2);
    if even {
        (w & !1, h & !1)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_identity_and_halves() {
        let mut img = Image::filled(4, 2, [10, 20, 30, 255]);
        img.set_pixel(0, 0, [200, 200, 200, 255]);
        assert_eq!(scale_region(&img, RectF::new(0.0, 0.0, 4.0, 2.0), 4, 2), img);
        let half = scale_region(&img, RectF::new(0.0, 0.0, 4.0, 2.0), 2, 1);
        assert_eq!((half.width, half.height), (2, 1));
        // Crop the right half 1:1.
        let crop = scale_region(&img, RectF::new(2.0, 0.0, 2.0, 2.0), 2, 2);
        assert_eq!(crop.pixel(0, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn gradient_runs_from_to() {
        let bg = Background::Gradient { from: Color::rgb(0, 0, 0), to: Color::rgb(255, 255, 255), angle: 0.0 };
        let img = fill_background(&bg, 100, 10);
        assert!(img.pixel(0, 5)[0] < 10);
        assert!(img.pixel(99, 5)[0] > 245);
        let solid = fill_background(&Background::Solid { color: Color::rgb(1, 2, 3) }, 3, 3);
        assert_eq!(solid.pixel(1, 1), [3, 2, 1, 255]);
    }

    #[test]
    fn zoom_crops_the_frame() {
        let mut frame = Image::filled(100, 100, [0, 0, 0, 255]);
        for y in 0..50 {
            for x in 0..50 {
                frame.set_pixel(x, y, [255, 255, 255, 255]);
            }
        }
        let mut edit = EditState::new(1000);
        edit.zoom.push(crate::video_editor::model::ZoomKey {
            time_ms: 0,
            cx: 0.25,
            cy: 0.25,
            scale: 2.0,
            transition_ms: 0,
        });
        let out = compose_video_frame(&frame, 500, &edit, 50, 50);
        // Zoomed into the white top-left quarter.
        assert_eq!(out.pixel(25, 25), [255, 255, 255, 255]);
        assert_eq!(out.pixel(2, 47), [255, 255, 255, 255]);
    }

    #[test]
    fn fits_width_even() {
        assert_eq!(fit_width(1920, 1080, 640, true), (640, 360));
        assert_eq!(fit_width(1001, 501, 5000, true), (1000, 500));
    }

    #[test]
    fn title_card_renders_text() {
        let card = TitleCard {
            title: "Hello".into(),
            subtitle: "World".into(),
            background: Background::Solid { color: Color::rgb(0, 0, 0) },
            text_color: Color::WHITE,
            ..TitleCard::default()
        };
        let img = render_title(&card, 320, 180);
        let bright = img.data.chunks_exact(4).filter(|p| p[0] > 128).count();
        assert!(bright > 50, "no text pixels ({bright})");
    }
}
