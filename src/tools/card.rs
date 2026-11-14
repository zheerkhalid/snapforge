//! The shared dark, translucent "card" look of the tool popups (loupes, readouts).

use super::gfx::{self, col, Dib};
use tiny_skia::{FilterQuality, IntSize, Pattern, Pixmap, SpreadMode, Transform};

pub const BG: [u8; 4] = [24, 26, 33, 238];
pub const TEXT: [u8; 4] = [240, 242, 247, 255];
pub const TEXT_DIM: [u8; 4] = [150, 157, 172, 255];
pub const ACCENT: [u8; 4] = [79, 107, 255, 255];

/// Logical → physical pixels.
pub fn sc(v: f32, dpi: u32) -> f32 {
    v * dpi as f32 / 96.0
}

pub fn sci(v: f32, dpi: u32) -> i32 {
    sc(v, dpi).round() as i32
}

/// Card background with a soft shadow, inside `dib` at (x, y, w, h).
pub fn draw_card(dib: &mut Dib, x: f32, y: f32, w: f32, h: f32, dpi: u32) {
    let r = sc(12.0, dpi);
    let Some(mut pm) = dib.pixmap() else {
        return;
    };
    gfx::shadow(&mut pm, x, y, w, h, r, sc(10.0, dpi));
    gfx::fill(&mut pm, gfx::rrect(x, y, w, h, r), col(BG[0], BG[1], BG[2], BG[3]));
    let b = 0.5;
    gfx::stroke(&mut pm, gfx::rrect(x + b, y + b, w - 2.0 * b, h - 2.0 * b, r), col(255, 255, 255, 30), 1.0);
}

/// Draw an opaque BGRA block (`side_w` x `side_h`) clipped to a rounded rect, with a
/// thin outline.
pub fn draw_rounded_pixels(dib: &mut Dib, px: &[u8], side_w: i32, side_h: i32, x: f32, y: f32, radius: f32) {
    let Some(size) = IntSize::from_wh(side_w.max(1) as u32, side_h.max(1) as u32) else {
        return;
    };
    let need = (side_w * side_h * 4) as usize;
    if px.len() < need {
        return;
    }
    let Some(src) = Pixmap::from_vec(px[..need].to_vec(), size) else {
        return;
    };
    let Some(mut pm) = dib.pixmap() else {
        return;
    };
    let pat = Pattern::new(src.as_ref(), SpreadMode::Pad, FilterQuality::Nearest, 1.0, Transform::from_translate(x, y));
    let mut p = tiny_skia::Paint { anti_alias: true, ..Default::default() };
    p.shader = pat;
    if let Some(path) = gfx::rrect(x, y, side_w as f32, side_h as f32, radius) {
        pm.fill_path(&path, &p, tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
    gfx::stroke(
        &mut pm,
        gfx::rrect(x - 0.5, y - 0.5, side_w as f32 + 1.0, side_h as f32 + 1.0, radius + 0.5),
        col(255, 255, 255, 40),
        1.0,
    );
}

/// A small pill label (e.g. "FROZEN") with its left edge at `x`.
pub fn pill(dib: &mut Dib, text: &mut gfx::Text, s: &str, x: i32, y: i32, dpi: u32, bg: [u8; 4]) {
    let fpx = sci(10.5, dpi);
    let (tw, th) = text.measure(s, fpx, gfx::Face::UiSemibold);
    let pad = sci(7.0, dpi);
    let h = th + sci(2.0, dpi);
    if let Some(mut pm) = dib.pixmap() {
        gfx::fill(
            &mut pm,
            gfx::rrect(x as f32, y as f32, (tw + 2 * pad) as f32, h as f32, h as f32 / 2.0),
            col(bg[0], bg[1], bg[2], bg[3]),
        );
    }
    text.draw(dib, s, x + pad, y + sci(1.0, dpi), fpx, gfx::Face::UiSemibold, [255, 255, 255, 255], gfx::Align::Left);
}
