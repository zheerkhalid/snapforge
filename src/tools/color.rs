//! Color math and text formats for the color picker (pure functions).

use crate::settings::tools::ColorFormat;

/// An 8-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// From 0xRRGGBB.
    pub const fn from_u32(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    /// As 0xRRGGBB.
    pub const fn to_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// From a BGRA pixel (screen pixels are opaque, so premultiplication does not matter).
    pub const fn from_bgra(p: [u8; 4]) -> Self {
        Self { r: p[2], g: p[1], b: p[0] }
    }

    /// Perceived luminance 0..1 (Rec. 709 weights on gamma-encoded values; good enough to
    /// choose a readable text color on top of a swatch).
    pub fn luma(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
}

/// Hue in degrees [0, 360), saturation and value in [0, 1].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

pub fn rgb_to_hsv(c: Rgb) -> Hsv {
    let r = c.r as f32 / 255.0;
    let g = c.g as f32 / 255.0;
    let b = c.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    Hsv { h: h.rem_euclid(360.0), s, v: max }
}

pub fn hsv_to_rgb(c: Hsv) -> Rgb {
    let h = c.h.rem_euclid(360.0);
    let s = c.s.clamp(0.0, 1.0);
    let v = c.v.clamp(0.0, 1.0);
    let ch = v * s;
    let x = ch * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - ch;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (ch, x, 0.0),
        1 => (x, ch, 0.0),
        2 => (0.0, ch, x),
        3 => (0.0, x, ch),
        4 => (x, 0.0, ch),
        _ => (ch, 0.0, x),
    };
    let q = |f: f32| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb::new(q(r), q(g), q(b))
}

/// Integer HSV as shown in the fine-tune panel: hue 0..=359, saturation and value 0..=100.
pub fn hsv_ints(c: Rgb) -> (i32, i32, i32) {
    let h = rgb_to_hsv(c);
    ((h.h.round() as i32) % 360, (h.s * 100.0).round() as i32, (h.v * 100.0).round() as i32)
}

pub fn from_hsv_ints(h: i32, s: i32, v: i32) -> Rgb {
    hsv_to_rgb(Hsv { h: h as f32, s: s.clamp(0, 100) as f32 / 100.0, v: v.clamp(0, 100) as f32 / 100.0 })
}

/// The bare value in a format (no prefix/suffix).
pub fn format_value(c: Rgb, fmt: ColorFormat, lowercase: bool) -> String {
    let s = match fmt {
        ColorFormat::Rgb => return format!("{}, {}, {}", c.r, c.g, c.b),
        ColorFormat::Decimal => return c.to_u32().to_string(),
        ColorFormat::Hex => format!("{:02X}{:02X}{:02X}", c.r, c.g, c.b),
        ColorFormat::Html => format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b),
        ColorFormat::Delphi => format!("$00{:02X}{:02X}{:02X}", c.b, c.g, c.r),
    };
    if lowercase {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

/// Clipboard text: `prefix + value + suffix`.
pub fn format(c: Rgb, fmt: ColorFormat, lowercase: bool, prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}{suffix}", format_value(c, fmt, lowercase))
}

/// Format with the current settings.
pub fn format_with_settings(c: Rgb) -> String {
    let s = crate::settings::get();
    let t = &s.tools;
    format(c, t.color_format, t.color_hex_lowercase, &t.color_prefix, &t.color_suffix)
}

/// Parse `#RGB`, `#RRGGBB`, `RRGGBB` or `r, g, b` (used by the fine-tune hex box).
pub fn parse(text: &str) -> Option<Rgb> {
    let t = text.trim();
    if t.contains(',') {
        let parts: Vec<u8> = t.split(',').map(|p| p.trim().parse::<u8>()).collect::<Result<_, _>>().ok()?;
        return match parts[..] {
            [r, g, b] => Some(Rgb::new(r, g, b)),
            _ => None,
        };
    }
    let h = t.trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok().map(Rgb::from_u32),
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let d = |n: u32| ((n & 0xF) * 17) as u8;
            Some(Rgb::new(d(v >> 8), d(v >> 4), d(v)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORANGE: Rgb = Rgb::new(255, 128, 0);

    #[test]
    fn every_format() {
        assert_eq!(format_value(ORANGE, ColorFormat::Rgb, false), "255, 128, 0");
        assert_eq!(format_value(ORANGE, ColorFormat::Hex, false), "FF8000");
        assert_eq!(format_value(ORANGE, ColorFormat::Decimal, false), "16744448");
        assert_eq!(format_value(ORANGE, ColorFormat::Html, false), "#FF8000");
        assert_eq!(format_value(ORANGE, ColorFormat::Delphi, false), "$000080FF");
        assert_eq!(format_value(ORANGE, ColorFormat::Html, true), "#ff8000");
        assert_eq!(format_value(ORANGE, ColorFormat::Delphi, true), "$000080ff");
        // Lower case never touches non-hex formats.
        assert_eq!(format_value(Rgb::new(1, 2, 3), ColorFormat::Rgb, true), "1, 2, 3");
        assert_eq!(format_value(Rgb::new(0, 0, 1), ColorFormat::Decimal, true), "1");
    }

    #[test]
    fn every_format_with_prefix_and_suffix() {
        let cases = [
            (ColorFormat::Rgb, "rgb(255, 128, 0);"),
            (ColorFormat::Hex, "rgb(FF8000);"),
            (ColorFormat::Decimal, "rgb(16744448);"),
            (ColorFormat::Html, "rgb(#FF8000);"),
            (ColorFormat::Delphi, "rgb($000080FF);"),
        ];
        for (f, want) in cases {
            assert_eq!(format(ORANGE, f, false, "rgb(", ");"), want);
        }
        assert_eq!(format(ORANGE, ColorFormat::Hex, false, "0x", ""), "0xFF8000");
        assert_eq!(format(ORANGE, ColorFormat::Hex, false, "", ""), "FF8000");
        assert_eq!(
            format(Rgb::new(0, 0, 0), ColorFormat::Html, false, "color: ", " !important"),
            "color: #000000 !important"
        );
    }

    #[test]
    fn u32_roundtrip() {
        assert_eq!(Rgb::from_u32(0x123456), Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(Rgb::new(0x12, 0x34, 0x56).to_u32(), 0x123456);
        assert_eq!(Rgb::from_bgra([1, 2, 3, 255]), Rgb::new(3, 2, 1));
    }

    #[test]
    fn hsv_known_values() {
        let h = rgb_to_hsv(Rgb::new(255, 0, 0));
        assert_eq!((h.h, h.s, h.v), (0.0, 1.0, 1.0));
        let h = rgb_to_hsv(Rgb::new(0, 255, 0));
        assert!((h.h - 120.0).abs() < 1e-3);
        let h = rgb_to_hsv(Rgb::new(0, 0, 255));
        assert!((h.h - 240.0).abs() < 1e-3);
        let h = rgb_to_hsv(Rgb::new(255, 0, 255));
        assert!((h.h - 300.0).abs() < 1e-3);
        let g = rgb_to_hsv(Rgb::new(128, 128, 128));
        assert_eq!((g.h, g.s), (0.0, 0.0));
        assert!((g.v - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(rgb_to_hsv(Rgb::new(0, 0, 0)), Hsv { h: 0.0, s: 0.0, v: 0.0 });
        assert_eq!(hsv_ints(ORANGE), (30, 100, 100));
        assert_eq!(hsv_to_rgb(Hsv { h: 360.0, s: 1.0, v: 1.0 }), Rgb::new(255, 0, 0));
        assert_eq!(hsv_to_rgb(Hsv { h: -60.0, s: 1.0, v: 1.0 }), Rgb::new(255, 0, 255));
    }

    #[test]
    fn rgb_hsv_roundtrip_is_exact_for_all_sampled_colors() {
        for r in (0..=255).step_by(5) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(17) {
                    let c = Rgb::new(r as u8, g as u8, b as u8);
                    assert_eq!(hsv_to_rgb(rgb_to_hsv(c)), c, "{c:?}");
                }
            }
        }
    }

    #[test]
    fn integer_hsv_roundtrip_is_close() {
        for v in [0x000000, 0xFFFFFF, 0xFF8000, 0x336699, 0x7F7F7F, 0x0A0B0C] {
            let c = Rgb::from_u32(v);
            let (h, s, vv) = hsv_ints(c);
            let back = from_hsv_ints(h, s, vv);
            for (a, b) in [(c.r, back.r), (c.g, back.g), (c.b, back.b)] {
                assert!(a.abs_diff(b) <= 3, "{c:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn parsing() {
        assert_eq!(parse("#FF8000"), Some(ORANGE));
        assert_eq!(parse("ff8000"), Some(ORANGE));
        assert_eq!(parse(" #f80 "), Some(Rgb::new(255, 136, 0)));
        assert_eq!(parse("255, 128,0"), Some(ORANGE));
        assert_eq!(parse("256, 0, 0"), None);
        assert_eq!(parse("#GG0000"), None);
        assert_eq!(parse("1,2"), None);
        assert_eq!(parse(""), None);
    }
}
