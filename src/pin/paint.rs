//! Pin rendering into a BGRA back buffer (client size): checkerboard behind transparent
//! pixels, the zoomed image, live Draw Mode previews, a transient zoom/opacity badge and
//! the thin accent border. Also the collapsed "tab" look. Pure CPU, so previews can be
//! rendered offscreen in tests.

use super::annot::{self, sk_color};
use super::gdi;
use crate::common::Image;
use tiny_skia::{FillRule, FilterQuality, Paint, PathBuilder, PixmapMut, Rect as SkRect, Transform};

/// An in-progress annotation drawn over the image (image coordinates).
pub enum Live<'a> {
    None,
    Stroke {
        pts: &'a [(f32, f32)],
        rgb: u32,
        width: f32,
    },
    Text {
        x: f32,
        y: f32,
        text: &'a str,
        rgb: u32,
        px: i32,
        caret: bool,
    },
    /// Step stamp preview under the cursor.
    Step {
        x: f32,
        y: f32,
        n: u32,
        rgb: u32,
        radius: f32,
    },
}

pub struct View<'a> {
    pub img: &'a Image,
    /// `img.has_transparency()`, cached by the caller.
    pub transparent: bool,
    pub zoom: f64,
    /// Border thickness in pixels (the image starts at (border, border)).
    pub border: i32,
    pub border_rgb: u32,
    pub live: Live<'a>,
    pub badge: Option<&'a str>,
    pub dpi: u32,
}

const CHECK_A: [u8; 4] = [255, 255, 255, 255];
const CHECK_B: [u8; 4] = [0xE6, 0xE6, 0xE6, 255];

fn checker(buf: &mut [u8], stride: usize, x0: usize, y0: usize, w: usize, h: usize, cell: usize) {
    for y in 0..h {
        let row = &mut buf[(y0 + y) * stride * 4..];
        for x in 0..w {
            let c = if ((x / cell) + (y / cell)) % 2 == 0 { CHECK_A } else { CHECK_B };
            let i = (x0 + x) * 4;
            row[i..i + 4].copy_from_slice(&c);
        }
    }
}

fn fill_rect(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, rgb: u32, a: u8) {
    if let Some(r) = SkRect::from_xywh(x, y, w, h) {
        let mut p = Paint::default();
        p.set_color(sk_color(rgb, a));
        pm.fill_rect(r, &p, Transform::identity(), None);
    }
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn scale(v: i32, dpi: u32) -> i32 {
    crate::common::win::scale(v, dpi)
}

/// Render the pin into `buf` (`w`×`h` BGRA, top-down).
pub fn render(buf: &mut [u8], w: i32, h: i32, v: &View) {
    if w <= 0 || h <= 0 || buf.len() < w as usize * h as usize * 4 {
        return;
    }
    let b = v.border.max(0);
    let (vw, vh) = ((w - 2 * b).max(0), (h - 2 * b).max(0));
    let stride = w as usize;

    // 1. Background + image.
    if v.transparent {
        checker(buf, stride, b as usize, b as usize, vw as usize, vh as usize, scale(8, v.dpi).max(4) as usize);
    }
    let fast = (v.zoom - 1.0).abs() < 1e-9 && !v.transparent;
    if fast {
        let cw = (v.img.width as usize).min(vw as usize) * 4;
        for y in 0..(v.img.height as usize).min(vh as usize) {
            let d = ((y + b as usize) * stride + b as usize) * 4;
            buf[d..d + cw].copy_from_slice(&v.img.row(y as u32)[..cw]);
        }
    }
    let Some(mut pm) = PixmapMut::from_bytes(buf, w as u32, h as u32) else { return };
    let ts = Transform::from_translate(b as f32, b as f32).pre_scale(v.zoom as f32, v.zoom as f32);
    if !fast {
        if !v.transparent {
            fill_rect(&mut pm, b as f32, b as f32, vw as f32, vh as f32, 0xFF_FF_FF, 255);
        }
        let q = if v.zoom < 1.0 { FilterQuality::Bicubic } else { FilterQuality::Nearest };
        annot::draw_image(&mut pm, v.img, 0.0, 0.0, ts, q);
    }

    // 2. Live annotation.
    match v.live {
        Live::None => {}
        Live::Stroke { pts, rgb, width } => annot::stroke(&mut pm, pts, rgb, width, ts),
        Live::Text { x, y, text, rgb, px, caret } => {
            annot::text(&mut pm, x, y, text, rgb, px, ts);
            if caret {
                let cx = x + annot::text_width(text, px) + 1.0;
                let mut p0 = tiny_skia::Point::from_xy(cx, y + px as f32 * 0.12);
                let mut p1 = tiny_skia::Point::from_xy(cx, y + px as f32 * 1.25);
                ts.map_point(&mut p0);
                ts.map_point(&mut p1);
                let cw = (scale(2, v.dpi) as f32).max(1.0);
                fill_rect(&mut pm, p0.x, p0.y, cw, (p1.y - p0.y).max(2.0), rgb, 255);
            }
        }
        Live::Step { x, y, n, rgb, radius } => {
            // Semi-transparent ghost: render onto a scratch image, then fade.
            let r = radius.ceil() as u32 + 2;
            let mut ghost = Image::new(2 * r, 2 * r);
            if let Some(mut gp) = annot::pixmap_mut(&mut ghost) {
                annot::step(&mut gp, r as f32, r as f32, n, rgb, radius, Transform::identity());
            }
            for c in ghost.data.iter_mut() {
                *c = (*c as u32 * 140 / 255) as u8;
            }
            annot::draw_image(&mut pm, &ghost, x - r as f32, y - r as f32, ts, FilterQuality::Bilinear);
        }
    }

    // 3. Badge (zoom / opacity readout), bottom-right inside the image.
    if let Some(text) = v.badge {
        let px = scale(12, v.dpi);
        if let Some(m) = gdi::text_mask(text, px, true) {
            let pad = scale(6, v.dpi) as f32;
            let (bw, bh) = (m.w as f32 + pad, m.h as f32 + pad * 0.6);
            let bx = (w - b) as f32 - bw - pad;
            let by = (h - b) as f32 - bh - pad;
            if bx > b as f32 && by > b as f32 {
                if let Some(path) = round_rect(bx, by, bw, bh, bh / 2.0) {
                    let mut p = Paint::default();
                    p.set_color(sk_color(0x20_20_20, 210));
                    p.anti_alias = true;
                    pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
                }
                let img = gdi::colorize(&m, [255, 255, 255], 255);
                annot::draw_image(
                    &mut pm,
                    &img,
                    bx + pad / 2.0,
                    by + pad * 0.3,
                    Transform::identity(),
                    FilterQuality::Nearest,
                );
            }
        }
    }

    // 4. Border.
    if b > 0 {
        let (wf, hf, bf) = (w as f32, h as f32, b as f32);
        fill_rect(&mut pm, 0.0, 0.0, wf, bf, v.border_rgb, 255);
        fill_rect(&mut pm, 0.0, hf - bf, wf, bf, v.border_rgb, 255);
        fill_rect(&mut pm, 0.0, bf, bf, hf - 2.0 * bf, v.border_rgb, 255);
        fill_rect(&mut pm, wf - bf, bf, bf, hf - 2.0 * bf, v.border_rgb, 255);
    }
}

/// Size of the collapsed tab at `dpi`.
pub fn tab_size(dpi: u32) -> (i32, i32) {
    (scale(96, dpi), scale(18, dpi))
}

/// The collapsed tab: accent strip with a thumbnail of the pin and a grip.
pub fn render_tab(buf: &mut [u8], w: i32, h: i32, img: &Image, accent: u32, dpi: u32) {
    if w <= 0 || h <= 0 || buf.len() < w as usize * h as usize * 4 {
        return;
    }
    let Some(mut pm) = PixmapMut::from_bytes(buf, w as u32, h as u32) else { return };
    fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, accent, 255);
    // Thumbnail on the left, fitted to the inner height.
    let inset = scale(2, dpi) as f32;
    let th = h as f32 - 2.0 * inset;
    if !img.is_empty() && th > 0.0 {
        let k = (th / img.height as f32).min((w as f32 * 0.6) / img.width as f32);
        let tw = img.width as f32 * k;
        fill_rect(&mut pm, inset, inset, tw, img.height as f32 * k, 0xFF_FF_FF, 255);
        let ts = Transform::from_translate(inset, inset).pre_scale(k, k);
        annot::draw_image(&mut pm, img, 0.0, 0.0, ts, FilterQuality::Bicubic);
        // Grip: three short bars to the right of the thumbnail.
        let gx = inset + tw + (w as f32 - inset - tw) / 2.0 - scale(6, dpi) as f32;
        let bar_h = (h as f32 * 0.45).max(3.0);
        for i in 0..3 {
            fill_rect(
                &mut pm,
                gx + i as f32 * scale(5, dpi) as f32,
                (h as f32 - bar_h) / 2.0,
                scale(2, dpi).max(1) as f32,
                bar_h,
                0xFF_FF_FF,
                220,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(img: &Image, zoom: f64) -> View<'_> {
        View {
            img,
            transparent: img.has_transparency(),
            zoom,
            border: 1,
            border_rgb: 0x00_78_D4,
            live: Live::None,
            badge: None,
            dpi: 96,
        }
    }

    #[test]
    fn border_and_image_pixels() {
        let img = Image::filled(10, 6, [10, 20, 30, 255]);
        let (w, h) = (12, 8);
        let mut buf = vec![0u8; w * h * 4];
        render(&mut buf, w as i32, h as i32, &view(&img, 1.0));
        // Border pixel: accent 0x0078D4 → BGRA [D4, 78, 00, FF].
        assert_eq!(&buf[0..4], &[0xD4, 0x78, 0x00, 0xFF]);
        let i = (w + 1) * 4;
        assert_eq!(&buf[i..i + 4], &[10, 20, 30, 255]);
    }

    #[test]
    fn zoomed_render_fills_view() {
        let img = Image::filled(10, 10, [0, 0, 255, 255]);
        let (w, h) = (22, 22); // 200% + 1px border
        let mut buf = vec![0u8; w * h * 4];
        render(&mut buf, w as i32, h as i32, &view(&img, 2.0));
        let i = (20 * w + 20) * 4;
        assert_eq!(&buf[i..i + 4], &[0, 0, 255, 255]);
    }
}
