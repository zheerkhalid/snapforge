//! The image canvas: zoom/pan input and painting (checkerboard, tiles, drop shadow).

use super::document::Document;
use super::texture::ImageTextures;
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, PointerButton, Rect, Sense, TextureHandle};

/// Physical px per checkerboard cell.
const CHECKER_CELL: f32 = 8.0;

pub struct CanvasOutput {
    /// The document's pixels were painted this frame (bench hook).
    pub painted: bool,
}

/// Pan and zoom with the mouse, then paint. `space_down`: Space is held (pan mode).
pub fn show(ui: &mut egui::Ui, doc: &mut Document, checker: &TextureHandle, space_down: bool) -> CanvasOutput {
    let ctx = ui.ctx().clone();
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let ppp = ctx.pixels_per_point();
    let view_px = [rect.width() * ppp, rect.height() * ppp];
    let img_px = doc.size();
    doc.view.layout(img_px, view_px);

    // ---- input
    let panning = resp.dragged_by(PointerButton::Middle) || (space_down && resp.dragged_by(PointerButton::Primary));
    if panning {
        let d = resp.drag_delta() * ppp;
        doc.view.pan([d.x, d.y], img_px, view_px);
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if space_down && resp.hovered() {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }
    if resp.hovered() {
        let (zoom, scroll, pointer) = ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom != 1.0 {
            let anchor = pointer.map(|p| (p - rect.min) * ppp).unwrap_or(vec2(view_px[0], view_px[1]) / 2.0);
            let z = doc.view.zoom * zoom;
            doc.view.zoom_around(z, [anchor.x, anchor.y], img_px, view_px);
        } else if scroll != egui::Vec2::ZERO {
            let d = scroll * ppp;
            doc.view.pan([d.x, d.y], img_px, view_px);
        }
    }

    // ---- textures (first display, or after an edit)
    if doc.textures.is_none() {
        let t0 = std::time::Instant::now();
        doc.textures = Some(ImageTextures::upload(&ctx, &doc.image, &format!("doc{}", doc.id)));
        log::debug!("editor: prepared {}x{} textures in {:?}", doc.image.width, doc.image.height, t0.elapsed());
    }

    // ---- paint
    let painter = ui.painter_at(rect);
    // Snap the image origin to whole physical pixels so 100% is pixel-exact.
    let origin_px = (rect.min.to_vec2() * ppp).round() + vec2(doc.view.offset[0], doc.view.offset[1]).round();
    let scaled = doc.view.scaled_size(img_px);
    let img_rect = Rect::from_min_size(pos2(origin_px.x, origin_px.y) / ppp, vec2(scaled[0], scaled[1]) / ppp);

    let shadow = egui::Shadow { offset: [0, 2], blur: 12, spread: 0, color: Color32::from_black_alpha(50) };
    painter.add(shadow.as_shape(img_rect, 0));

    let visible = img_rect.intersect(rect);
    if doc.has_alpha && visible.is_positive() {
        let cells = visible.size() * ppp / (CHECKER_CELL * 2.0);
        let uv = Rect::from_min_size(pos2(0.0, 0.0), cells);
        painter.image(checker.id(), visible, uv, Color32::WHITE);
    }

    let mut painted = false;
    if let Some(tex) = &doc.textures {
        let k = doc.view.zoom / ppp;
        for tile in &tex.tiles {
            let r = Rect::from_min_size(
                img_rect.min + vec2(tile.rect.x as f32, tile.rect.y as f32) * k,
                vec2(tile.rect.w as f32, tile.rect.h as f32) * k,
            );
            if r.intersects(rect) {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(tile.tex.id(), r, uv, Color32::WHITE);
                painted = true;
            }
        }
    }
    CanvasOutput { painted }
}
