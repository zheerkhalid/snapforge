//! GPU textures for documents: BGRA → RGBA swizzle and tiling of images larger than the
//! GL max texture size.

use crate::common::Image;
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

/// Upper bound for one texture side. GL guarantees much less, but every GPU SnapForge
/// targets supports at least 8192; we also respect the driver's reported limit.
pub const MAX_TILE_SIDE: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Split a `w`×`h` image into tiles no larger than `max`×`max`, row-major.
pub fn split_tiles(w: u32, h: u32, max: u32) -> Vec<TileRect> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut y = 0;
    while y < h {
        let th = (h - y).min(max);
        let mut x = 0;
        while x < w {
            let tw = (w - x).min(max);
            out.push(TileRect { x, y, w: tw, h: th });
            x += tw;
        }
        y += th;
    }
    out
}

/// Premultiplied BGRA → premultiplied RGBA (`Color32`), one row.
#[inline]
pub fn swizzle_row(src: &[u8], dst: &mut [Color32]) {
    for (px, out) in src.chunks_exact(4).zip(dst.iter_mut()) {
        *out = Color32::from_rgba_premultiplied(px[2], px[1], px[0], px[3]);
    }
}

/// Swizzle the sub-rectangle `r` of `img` into a `ColorImage`. Large tiles are split
/// across a few threads (a 4K frame converts in a couple of ms).
pub fn tile_to_color_image(img: &Image, r: TileRect) -> ColorImage {
    let (w, h) = (r.w as usize, r.h as usize);
    let mut pixels = vec![Color32::TRANSPARENT; w * h];
    let stride = img.width as usize * 4;
    let convert = |rows: &mut [Color32], first_row: usize| {
        for (i, dst) in rows.chunks_exact_mut(w).enumerate() {
            let sy = r.y as usize + first_row + i;
            let start = sy * stride + r.x as usize * 4;
            swizzle_row(&img.data[start..start + w * 4], dst);
        }
    };
    let threads =
        if w * h >= 1 << 20 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(8) } else { 1 };
    if threads <= 1 || h < threads {
        convert(&mut pixels, 0);
    } else {
        let rows_per = h.div_ceil(threads);
        std::thread::scope(|s| {
            for (n, chunk) in pixels.chunks_mut(rows_per * w).enumerate() {
                let convert = &convert;
                s.spawn(move || convert(chunk, n * rows_per));
            }
        });
    }
    ColorImage { size: [w, h], pixels }
}

/// Magnify with nearest-neighbour (crisp pixels above 100%), minify with linear +
/// mipmaps (smooth below 100%).
pub fn image_options() -> TextureOptions {
    TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        wrap_mode: egui::TextureWrapMode::ClampToEdge,
        mipmap_mode: Some(egui::TextureFilter::Linear),
    }
}

pub struct Tile {
    pub rect: TileRect,
    pub tex: TextureHandle,
}

/// All textures for one image.
pub struct ImageTextures {
    pub tiles: Vec<Tile>,
}

impl ImageTextures {
    pub fn upload(ctx: &egui::Context, img: &Image, name: &str) -> Self {
        let max_side = ctx.input(|i| i.max_texture_side) as u32;
        let max_side = if max_side == 0 { MAX_TILE_SIDE } else { max_side.min(MAX_TILE_SIDE) };
        let tiles = split_tiles(img.width, img.height, max_side)
            .into_iter()
            .enumerate()
            .map(|(i, rect)| {
                let ci = tile_to_color_image(img, rect);
                Tile { rect, tex: ctx.load_texture(format!("{name}#{i}"), ci, image_options()) }
            })
            .collect();
        Self { tiles }
    }
}

/// 2×2 checkerboard, sampled with `Repeat` wrap so one quad covers any area.
pub fn checker_texture(ctx: &egui::Context, dark: bool) -> TextureHandle {
    let (a, b) = if dark {
        (Color32::from_gray(58), Color32::from_gray(74))
    } else {
        (Color32::from_gray(255), Color32::from_gray(222))
    };
    let img = ColorImage { size: [2, 2], pixels: vec![a, b, b, a] };
    let opts = TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Nearest,
        wrap_mode: egui::TextureWrapMode::Repeat,
        mipmap_mode: None,
    };
    ctx.load_texture("editor-checker", img, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_image_exactly() {
        let tiles = split_tiles(20000, 9000, 8192);
        assert_eq!(tiles.len(), 3 * 2);
        assert_eq!(tiles[0], TileRect { x: 0, y: 0, w: 8192, h: 8192 });
        assert_eq!(tiles[2], TileRect { x: 16384, y: 0, w: 3616, h: 8192 });
        assert_eq!(tiles[5], TileRect { x: 16384, y: 8192, w: 3616, h: 808 });
        let area: u64 = tiles.iter().map(|t| t.w as u64 * t.h as u64).sum();
        assert_eq!(area, 20000 * 9000);
    }

    #[test]
    fn small_image_is_one_tile() {
        assert_eq!(split_tiles(100, 50, 8192), vec![TileRect { x: 0, y: 0, w: 100, h: 50 }]);
        assert!(split_tiles(0, 50, 8192).is_empty());
        assert_eq!(split_tiles(8192, 8193, 8192).len(), 2);
    }

    #[test]
    fn swizzle_bgra_to_rgba() {
        let src = [10u8, 20, 30, 255, 1, 2, 3, 4];
        let mut dst = [Color32::TRANSPARENT; 2];
        swizzle_row(&src, &mut dst);
        assert_eq!(dst[0].to_array(), [30, 20, 10, 255]);
        assert_eq!(dst[1].to_array(), [3, 2, 1, 4]);
    }

    #[test]
    fn tile_conversion_matches_pixels() {
        // Big enough to take the multi-threaded path.
        let (w, h) = (1100u32, 1000u32);
        let mut img = Image::new(w, h);
        for y in (0..h).step_by(37) {
            for x in (0..w).step_by(41) {
                img.set_pixel(x, y, [x as u8, y as u8, 7, 255]);
            }
        }
        let r = TileRect { x: 41, y: 37, w: 1000, h: 900 };
        let ci = tile_to_color_image(&img, r);
        assert_eq!(ci.size, [1000, 900]);
        assert_eq!(ci.pixels[0].to_array(), [7, 37, 41, 255]);
        let (x, y) = (41 * 5u32, 37 * 20u32);
        let i = ((y - r.y) * r.w + (x - r.x)) as usize;
        assert_eq!(ci.pixels[i].to_array(), [7, y as u8, x as u8, 255]);
    }
}
