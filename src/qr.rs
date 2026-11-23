//! QR code reading for captured pictures (`rqrr`, pure Rust, no camera or network).
//!
//! Screen captures are easy input (flat, sharp, upright), but the codes are often small
//! or light-on-dark, so [`decode`] retries on an upscaled and on an inverted copy.

use crate::common::Image;

/// One decoded code and where it sits in the picture (image pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct Code {
    pub text: String,
    /// Bounding box: (left, top, right, bottom).
    pub bounds: (i32, i32, i32, i32),
}

impl Code {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (l, t, r, b) = self.bounds;
        x >= l as f32 && x <= r as f32 && y >= t as f32 && y <= b as f32
    }
}

/// True for text a browser can open.
pub fn is_link(text: &str) -> bool {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && t.len() > 8
        && !t.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Pictures above this many pixels are not upscaled for the second pass.
const MAX_UPSCALE_PX: usize = 6_000_000;

/// Every QR code found in `img`, in reading order (top to bottom, left to right).
pub fn decode(img: &Image) -> Vec<Code> {
    if img.width < 21 || img.height < 21 {
        return Vec::new();
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let luma = luma_over_white(img);
    let mut found = pass(&luma, w, h, 1, false);
    if found.is_empty() {
        found = pass(&luma, w, h, 1, true);
    }
    if found.is_empty() && w * h <= MAX_UPSCALE_PX {
        // Codes with modules of one or two pixels need more resolution.
        let k = if w.max(h) < 400 { 4 } else { 2 };
        found = pass(&luma, w, h, k, false);
        if found.is_empty() {
            found = pass(&luma, w, h, k, true);
        }
    }
    found.sort_by_key(|c| (c.bounds.1, c.bounds.0));
    found.dedup_by(|a, b| a.text == b.text && a.bounds == b.bounds);
    found
}

/// The decoded texts, one per code.
pub fn texts(codes: &[Code]) -> Vec<String> {
    codes.iter().map(|c| c.text.clone()).collect()
}

fn luma_over_white(img: &Image) -> Vec<u8> {
    img.data
        .chunks_exact(4)
        .map(|p| {
            // Premultiplied BGRA over white, then Rec. 601 luma.
            let ia = 255 - p[3] as u32;
            let (b, g, r) = (p[0] as u32 + ia, p[1] as u32 + ia, p[2] as u32 + ia);
            ((r * 299 + g * 587 + b * 114) / 1000).min(255) as u8
        })
        .collect()
}

/// One detection pass on the luma plane scaled by the integer factor `k` (nearest
/// neighbour: module edges stay sharp), optionally inverted.
fn pass(luma: &[u8], w: usize, h: usize, k: usize, invert: bool) -> Vec<Code> {
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w * k, h * k, |x, y| {
        let v = luma[(y / k) * w + x / k];
        if invert {
            255 - v
        } else {
            v
        }
    });
    let mut out = Vec::new();
    for grid in prepared.detect_grids() {
        let Ok((_, text)) = grid.decode() else { continue };
        if text.is_empty() {
            continue;
        }
        let xs = grid.bounds.iter().map(|p| p.x / k as i32);
        let ys = grid.bounds.iter().map(|p| p.y / k as i32);
        let bounds = (
            xs.clone().min().unwrap_or(0),
            ys.clone().min().unwrap_or(0),
            xs.max().unwrap_or(0),
            ys.max().unwrap_or(0),
        );
        out.push(Code { text, bounds });
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A QR code for `text` drawn with `module` pixels per module and a 4-module quiet
    /// zone, dark on light (or the reverse).
    pub(crate) fn render(text: &str, module: u32, inverted: bool) -> Image {
        let code = qrcode::QrCode::new(text.as_bytes()).expect("encode");
        let n = code.width() as u32;
        let colors = code.to_colors();
        let (dark, light) = if inverted { ([255u8; 4], [20, 20, 20, 255]) } else { ([0, 0, 0, 255], [255u8; 4]) };
        let side = (n + 8) * module;
        let mut img = Image::filled(side, side, light);
        for y in 0..n {
            for x in 0..n {
                if colors[(y * n + x) as usize] == qrcode::Color::Dark {
                    for dy in 0..module {
                        for dx in 0..module {
                            img.set_pixel((x + 4) * module + dx, (y + 4) * module + dy, dark);
                        }
                    }
                }
            }
        }
        img
    }

    #[test]
    fn reads_a_code_at_several_sizes() {
        for module in [2u32, 3, 6, 11] {
            let img = render("https://example.com/a?b=1", module, false);
            let codes = decode(&img);
            assert_eq!(texts(&codes), ["https://example.com/a?b=1"], "module {module}");
            assert!(is_link(&codes[0].text));
        }
    }

    #[test]
    fn reads_a_one_pixel_module_code_through_the_upscale_pass() {
        let img = render("tiny code", 1, false);
        assert_eq!(texts(&decode(&img)), ["tiny code"]);
    }

    #[test]
    fn reads_light_on_dark_codes() {
        let img = render("inverted colours", 4, true);
        let codes = decode(&img);
        assert_eq!(texts(&codes), ["inverted colours"]);
        assert!(!is_link(&codes[0].text));
    }

    #[test]
    fn finds_two_codes_inside_a_larger_capture_in_reading_order() {
        let a = render("first", 4, false);
        let b = render("second: ünïcode ✓", 4, false);
        let mut page = Image::filled(700, 500, [240, 240, 240, 255]);
        page.blit(&b, 380, 260);
        page.blit(&a, 30, 20);
        let codes = decode(&page);
        assert_eq!(texts(&codes), ["first", "second: ünïcode ✓"]);
        // Bounds land on the code (inside the quiet zone), in page coordinates.
        let (l, t, r, bt) = codes[0].bounds;
        assert!(l >= 30 && t >= 20 && r <= 30 + a.width as i32 && bt <= 20 + a.height as i32, "{:?}", codes[0].bounds);
        assert!(codes[0].contains(100.0, 90.0));
        assert!(!codes[0].contains(500.0, 400.0));
        assert!(codes[1].contains(480.0, 360.0));
    }

    #[test]
    fn transparent_background_reads_as_white() {
        let solid = render("alpha", 5, false);
        let mut img = Image::new(solid.width, solid.height);
        for y in 0..solid.height {
            for x in 0..solid.width {
                if solid.pixel(x, y)[0] == 0 {
                    img.set_pixel(x, y, [0, 0, 0, 255]);
                }
            }
        }
        assert_eq!(texts(&decode(&img)), ["alpha"]);
    }

    #[test]
    fn nothing_in_plain_or_tiny_pictures() {
        assert!(decode(&Image::filled(300, 200, [200, 180, 90, 255])).is_empty());
        assert!(decode(&Image::filled(5, 5, [0, 0, 0, 255])).is_empty());
        assert!(decode(&Image::new(0, 0)).is_empty());
        // Noise must not panic or invent codes.
        let mut noise = Image::new(160, 120);
        let mut s = 12345u32;
        for px in noise.data.chunks_exact_mut(4) {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let v = (s >> 24) as u8;
            px.copy_from_slice(&[v, v, v, 255]);
        }
        assert!(decode(&noise).is_empty());
    }

    #[test]
    fn link_detection() {
        assert!(is_link("https://example.com"));
        assert!(is_link(" HTTP://example.com/x "));
        assert!(!is_link("https://"));
        assert!(!is_link("example.com"));
        assert!(!is_link("https://a b"));
        assert!(!is_link("WIFI:S:net;T:WPA;P:secret;;"));
    }
}
