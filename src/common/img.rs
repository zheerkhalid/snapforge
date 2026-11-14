//! In-memory image: premultiplied BGRA, top-down, tightly packed (stride = width * 4).

use super::geom::Rect;

#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA8, `width * height * 4` bytes.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Image({}x{})", self.width, self.height)
    }
}

impl Image {
    /// A fully transparent image.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    pub fn filled(width: u32, height: u32, bgra: [u8; 4]) -> Self {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
            data.extend_from_slice(&bgra);
        }
        Self { width, height, data }
    }

    pub fn from_bgra(width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), width as usize * height as usize * 4);
        Self { width, height, data }
    }

    /// Build from straight-alpha RGBA (e.g. decoded files).
    pub fn from_rgba_straight(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut data = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            let a = px[3] as u32;
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            data.extend_from_slice(&[pm(px[2]), pm(px[1]), pm(px[0]), px[3]]);
        }
        Self { width, height, data }
    }

    pub fn len_px(&self) -> usize {
        self.width as usize * self.height as usize
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, bgra: [u8; 4]) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.data[i..i + 4].copy_from_slice(&bgra);
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let s = self.width as usize * 4;
        &self.data[y as usize * s..(y as usize + 1) * s]
    }

    /// Copy of the sub-rectangle `r` (clipped to the image).
    pub fn crop(&self, r: Rect) -> Image {
        let r = r.intersect(&self.bounds());
        let mut out = Image::new(r.w.max(0) as u32, r.h.max(0) as u32);
        if r.is_empty() {
            return out;
        }
        let rs = r.w as usize * 4;
        for row in 0..r.h as usize {
            let src = ((r.y as usize + row) * self.width as usize + r.x as usize) * 4;
            out.data[row * rs..(row + 1) * rs].copy_from_slice(&self.data[src..src + rs]);
        }
        out
    }

    /// Copy `src` into this image at (x, y), replacing pixels (no blending). Clipped.
    pub fn blit(&mut self, src: &Image, x: i32, y: i32) {
        let dst = Rect::new(x, y, src.width as i32, src.height as i32).intersect(&self.bounds());
        if dst.is_empty() {
            return;
        }
        let rs = dst.w as usize * 4;
        for row in 0..dst.h {
            let sy = (dst.y - y + row) as usize;
            let sx = (dst.x - x) as usize;
            let s = (sy * src.width as usize + sx) * 4;
            let d = ((dst.y + row) as usize * self.width as usize + dst.x as usize) * 4;
            self.data[d..d + rs].copy_from_slice(&src.data[s..s + rs]);
        }
    }

    /// Source-over composite of premultiplied `src` onto this image at (x, y).
    pub fn draw_over(&mut self, src: &Image, x: i32, y: i32) {
        let dst = Rect::new(x, y, src.width as i32, src.height as i32).intersect(&self.bounds());
        for row in 0..dst.h {
            for col in 0..dst.w {
                let sp = src.pixel((dst.x - x + col) as u32, (dst.y - y + row) as u32);
                let (dx, dy) = ((dst.x + col) as u32, (dst.y + row) as u32);
                let dp = self.pixel(dx, dy);
                let ia = 255 - sp[3] as u32;
                let mut o = [0u8; 4];
                for c in 0..4 {
                    o[c] = (sp[c] as u32 + (dp[c] as u32 * ia + 127) / 255).min(255) as u8;
                }
                self.set_pixel(dx, dy, o);
            }
        }
    }

    /// Multiply every pixel by a coverage mask (0..=255), same size as the image.
    pub fn apply_mask(&mut self, mask: &[u8]) {
        debug_assert_eq!(mask.len(), self.len_px());
        for (px, &m) in self.data.chunks_exact_mut(4).zip(mask) {
            if m == 255 {
                continue;
            }
            let m = m as u32;
            for c in px.iter_mut() {
                *c = ((*c as u32 * m + 127) / 255) as u8;
            }
        }
    }

    /// True if any pixel is not fully opaque.
    pub fn has_transparency(&self) -> bool {
        self.data.chunks_exact(4).any(|p| p[3] != 255)
    }

    /// Force every pixel opaque (used for GDI captures that leave alpha = 0).
    pub fn set_opaque(&mut self) {
        for px in self.data.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    /// Straight-alpha RGBA bytes, for encoders.
    pub fn to_rgba_straight(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len());
        for px in self.data.chunks_exact(4) {
            let a = px[3] as u32;
            if a == 255 {
                out.extend_from_slice(&[px[2], px[1], px[0], 255]);
            } else if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
                out.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), px[3]]);
            }
        }
        out
    }

    /// RGB bytes composited over white (for formats without alpha).
    pub fn to_rgb_over_white(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len_px() * 3);
        for px in self.data.chunks_exact(4) {
            let ia = 255 - px[3] as u32;
            out.push((px[2] as u32 + ia).min(255) as u8);
            out.push((px[1] as u32 + ia).min(255) as u8);
            out.push((px[0] as u32 + ia).min(255) as u8);
        }
        out
    }

    /// Dimmed copy for overlay backgrounds: `c * num / 256`.
    pub fn dimmed(&self, num: u32) -> Image {
        let data = self.data.iter().map(|&c| ((c as u32 * num) >> 8) as u8).collect();
        Image { width: self.width, height: self.height, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_and_blit_roundtrip() {
        let mut img = Image::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                img.set_pixel(x, y, [x as u8, y as u8, 0, 255]);
            }
        }
        let c = img.crop(Rect::new(2, 3, 4, 2));
        assert_eq!(c.width, 4);
        assert_eq!(c.pixel(0, 0), [2, 3, 0, 255]);
        let mut dst = Image::new(8, 8);
        dst.blit(&c, 2, 3);
        assert_eq!(dst.pixel(5, 4), img.pixel(5, 4));
        assert_eq!(dst.pixel(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn crop_clips() {
        let img = Image::filled(4, 4, [1, 2, 3, 255]);
        let c = img.crop(Rect::new(-2, -2, 4, 4));
        assert_eq!((c.width, c.height), (2, 2));
    }

    #[test]
    fn premultiply_roundtrip() {
        let rgba = [200u8, 100, 50, 128];
        let img = Image::from_rgba_straight(1, 1, &rgba);
        let back = img.to_rgba_straight();
        for i in 0..3 {
            assert!((back[i] as i32 - rgba[i] as i32).abs() <= 2);
        }
        assert_eq!(back[3], 128);
    }
}
