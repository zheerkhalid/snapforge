//! HDR (scRGB FP16) → SDR sRGB conversion for captures of HDR monitors.
//!
//! Input is what WGC delivers for `R16G16B16A16Float`: linear scRGB, where 1.0 = 80 nits
//! and SDR content (the desktop, normal apps) is drawn at the user's "SDR content
//! brightness", i.e. SDR white = `sdr_white` (e.g. 2.5 for 200 nits).
//!
//! The curve (documented in DECISIONS.md):
//! 1. Normalize: `c / sdr_white`, so SDR white becomes 1.0 and SDR content maps 1:1.
//! 2. Gamut: negative scRGB components (colors outside sRGB/BT.709) are clipped to 0.
//! 3. Highlights: let `m = max(r, g, b)` and `peak` = the frame's largest `m` (≥ 1).
//!    Below the knee `K = 0.8`, and for every pixel when `peak <= 1` (pure SDR frame),
//!    nothing changes. Above the knee, `m` is compressed with an extended-Reinhard
//!    shoulder that has slope 1 at the knee and reaches exactly 1.0 at `peak`:
//!    `t = (m-K)/(1-K)`, `s = (peak-K)/(1-K)`, `m' = K + (1-K)·t(1+t/s²)/(1+t)`.
//!    The pixel is scaled by `m'/m`, which keeps hue and saturation.
//! 4. Encode with the sRGB transfer function to 8 bits.

use crate::common::Image;
use std::sync::OnceLock;

pub const KNEE: f32 = 0.8;
/// Highlights brighter than this many times SDR white are treated as this bright.
const MAX_PEAK: f32 = 64.0;

/// IEEE 754 binary16 → f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign << 31
        } else {
            // Subnormal: value = man * 2^-24.
            let v = man as f32 * (1.0 / 16_777_216.0);
            return if sign == 1 { -v } else { v };
        }
    } else if exp == 31 {
        (sign << 31) | 0x7f80_0000 | (man << 13)
    } else {
        (sign << 31) | ((exp + 112) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

fn f16_lut() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| (0..=u16::MAX).map(f16_to_f32).collect())
}

const SRGB_LUT_SIZE: usize = 16384;

fn srgb_lut() -> &'static [u8] {
    static LUT: OnceLock<Vec<u8>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..SRGB_LUT_SIZE)
            .map(|i| {
                let l = i as f32 / (SRGB_LUT_SIZE - 1) as f32;
                (linear_to_srgb(l) * 255.0 + 0.5) as u8
            })
            .collect()
    })
}

/// sRGB opto-electronic transfer function, input clamped to 0..=1.
pub fn linear_to_srgb(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[inline]
fn encode(l: f32) -> u8 {
    let i = (l.clamp(0.0, 1.0) * (SRGB_LUT_SIZE - 1) as f32 + 0.5) as usize;
    srgb_lut()[i.min(SRGB_LUT_SIZE - 1)]
}

/// The highlight curve for one frame.
#[derive(Debug, Clone, Copy)]
pub struct Curve {
    /// Frame peak relative to SDR white (≥ 1).
    peak: f32,
    inv_s2: f32,
}

impl Curve {
    pub fn new(peak: f32) -> Self {
        let peak = if peak.is_finite() { peak.clamp(1.0, MAX_PEAK) } else { MAX_PEAK };
        let s = (peak - KNEE) / (1.0 - KNEE);
        Self { peak, inv_s2: 1.0 / (s * s) }
    }

    /// Map the max-channel value `m` (normalized, ≥ 0) into 0..=1.
    pub fn map(&self, m: f32) -> f32 {
        if self.peak <= 1.0 || m <= KNEE {
            return m.min(1.0);
        }
        let t = (m.min(self.peak) - KNEE) / (1.0 - KNEE);
        let g = t * (1.0 + t * self.inv_s2) / (1.0 + t);
        (KNEE + (1.0 - KNEE) * g).min(1.0)
    }
}

/// Tone-map one normalized linear pixel (SDR white = 1.0) to 8-bit sRGB `[r, g, b]`.
pub fn tonemap_pixel(rgb: [f32; 3], curve: &Curve) -> [u8; 3] {
    let r = rgb[0].max(0.0);
    let g = rgb[1].max(0.0);
    let b = rgb[2].max(0.0);
    let m = r.max(g).max(b);
    if !m.is_finite() {
        return [255, 255, 255];
    }
    let k = if m > KNEE { curve.map(m) / m } else { 1.0 };
    [encode(r * k), encode(g * k), encode(b * k)]
}

/// Convert an FP16 RGBA (scRGB) surface to an opaque BGRA [`Image`].
/// `rows` yields each row as `width * 4` half-floats.
pub fn tonemap_rows<'a>(width: u32, height: u32, rows: impl Fn(u32) -> &'a [u16], sdr_white: f32) -> Image {
    let lut = f16_lut();
    let inv_white = 1.0 / sdr_white.max(0.01);
    // Pass 1: frame peak (max channel), in SDR-white units.
    let mut peak = 0.0f32;
    for y in 0..height {
        for px in rows(y).chunks_exact(4) {
            let m = lut[px[0] as usize].max(lut[px[1] as usize]).max(lut[px[2] as usize]);
            if m > peak && m.is_finite() {
                peak = m;
            }
        }
    }
    let curve = Curve::new(peak * inv_white);
    // Pass 2: map.
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for px in rows(y).chunks_exact(4) {
            let rgb =
                [lut[px[0] as usize] * inv_white, lut[px[1] as usize] * inv_white, lut[px[2] as usize] * inv_white];
            let [r, g, b] = tonemap_pixel(rgb, &curve);
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
    Image::from_bgra(width, height, out)
}

/// SDR white level of the display whose GDI device name is `device` (e.g. `\\.\DISPLAY1`),
/// in scRGB units (1.0 = 80 nits). Falls back to 2.5 (200 nits) if it can't be queried.
pub fn sdr_white_level(device: &str) -> f32 {
    query_sdr_white(device).unwrap_or_else(|| {
        log::debug!("SDR white level for {device} unavailable; assuming 200 nits");
        2.5
    })
}

fn query_sdr_white(device: &str) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;

    // SAFETY: buffers are sized by GetDisplayConfigBufferSizes; info packets carry their own
    // size in the header as DisplayConfigGetDeviceInfo requires.
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            != ERROR_SUCCESS
        {
            return None;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            if !crate::common::win::from_wide(&src.viewGdiDeviceName).eq_ignore_ascii_case(device) {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                    size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut white.header) != 0 || white.SDRWhiteLevel == 0 {
                return None;
            }
            // SDRWhiteLevel: 1000 = 80 nits.
            return Some(white.SDRWhiteLevel as f32 / 1000.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_decoding() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert!((f16_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12);
        assert!(f16_to_f32(0x7c00).is_infinite());
    }

    #[test]
    fn sdr_frame_is_identity() {
        let c = Curve::new(1.0);
        assert_eq!(tonemap_pixel([1.0, 1.0, 1.0], &c), [255, 255, 255]);
        assert_eq!(tonemap_pixel([0.0, 0.0, 0.0], &c), [0, 0, 0]);
        // Linear 0.2158605 is sRGB 128.
        assert_eq!(tonemap_pixel([0.215_860_5; 3], &c), [128, 128, 128]);
        // Out-of-gamut negatives clip to 0.
        assert_eq!(tonemap_pixel([-0.5, 1.0, 0.0], &c), [0, 255, 0]);
    }

    #[test]
    fn hdr_highlights_compress_smoothly() {
        let c = Curve::new(4.0);
        // Below the knee nothing changes even in an HDR frame.
        assert_eq!(c.map(0.5), 0.5);
        assert_eq!(c.map(KNEE), KNEE);
        // The peak maps to exactly 1, above it clamps.
        assert!((c.map(4.0) - 1.0).abs() < 1e-5);
        assert!((c.map(100.0) - 1.0).abs() < 1e-5);
        // Monotonic, continuous at the knee and below 1 in between.
        let mut prev = 0.0;
        for i in 0..=400 {
            let m = i as f32 / 100.0;
            let v = c.map(m);
            assert!(v >= prev - 1e-6, "not monotonic at {m}");
            assert!(v <= 1.0);
            prev = v;
        }
        assert!((c.map(KNEE + 1e-4) - (KNEE + 1e-4)).abs() < 1e-4);
        // SDR white stays bright in an HDR frame.
        assert!(c.map(1.0) > 0.85);
    }

    #[test]
    fn hue_is_preserved() {
        let c = Curve::new(10.0);
        let [r, g, b] = tonemap_pixel([6.0, 3.0, 0.0], &c);
        assert_eq!(b, 0);
        assert!(r > g && g > 0);
    }

    #[test]
    fn frame_tonemap_uses_sdr_white() {
        // One white pixel at 2.5 (200 nits = SDR white) and one mid gray.
        // Half-float bit patterns: 2.5, 1.0, 0.5.
        let (w, one, half) = (0x4100u16, 0x3c00u16, 0x3800u16);
        assert_eq!(f16_to_f32(w), 2.5);
        let row: Vec<u16> = vec![w, w, w, one, half, half, half, one];
        let img = tonemap_rows(2, 1, |_| &row[..], 2.5);
        assert_eq!(img.pixel(0, 0), [255, 255, 255, 255]);
        // 0.5 / 2.5 = 0.2 linear → sRGB ≈ 124.
        let p = img.pixel(1, 0);
        assert!((p[0] as i32 - 124).abs() <= 1, "{p:?}");
        assert_eq!(p[3], 255);
    }
}
