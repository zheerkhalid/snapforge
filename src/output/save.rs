//! Encoding, atomic saving and loading of image files.

use super::pcx;
use crate::common::Image;
use crate::settings::{self, ImageFormat};
use anyhow::{anyhow, bail, ensure, Context, Result};
use image::codecs::{bmp::BmpEncoder, gif::GifEncoder, jpeg::JpegEncoder, png::PngEncoder};
use image::codecs::{tga::TgaEncoder, tiff::TiffEncoder};
use image::{ExtendedColorType, ImageEncoder};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

/// WebP's hard limit on either dimension.
const WEBP_MAX_DIM: u32 = 16383;

/// Encode to bytes. `quality` (1–100) applies to JPEG and WebP (100 = lossless WebP).
/// Formats without alpha are composited over white.
pub fn encode(img: &Image, format: ImageFormat, quality: u8) -> Result<Vec<u8>> {
    ensure!(!img.is_empty(), "cannot encode an empty image");
    let (w, h) = (img.width, img.height);
    let quality = quality.clamp(1, 100);
    let alpha = format.supports_alpha() && img.has_transparency();
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => {
            let enc = PngEncoder::new(&mut out);
            if alpha {
                enc.write_image(&img.to_rgba_straight(), w, h, ExtendedColorType::Rgba8)
            } else {
                enc.write_image(&img.to_rgb_over_white(), w, h, ExtendedColorType::Rgb8)
            }
            .context("PNG encoding failed")?;
        }
        ImageFormat::Jpeg => {
            JpegEncoder::new_with_quality(&mut out, quality)
                .write_image(&img.to_rgb_over_white(), w, h, ExtendedColorType::Rgb8)
                .context("JPEG encoding failed")?;
        }
        ImageFormat::Bmp => {
            BmpEncoder::new(&mut out)
                .write_image(&img.to_rgb_over_white(), w, h, ExtendedColorType::Rgb8)
                .context("BMP encoding failed")?;
        }
        ImageFormat::Gif => {
            ensure!(w <= 65535 && h <= 65535, "GIF: image too large ({w}x{h}, max 65535)");
            let rgba = gif_pixels(img);
            let mut enc = GifEncoder::new_with_speed(&mut out, 10);
            enc.encode(&rgba, w, h, ExtendedColorType::Rgba8).context("GIF encoding failed")?;
        }
        ImageFormat::Tiff => {
            let mut cur = Cursor::new(&mut out);
            let enc = TiffEncoder::new(&mut cur);
            if alpha {
                enc.write_image(&img.to_rgba_straight(), w, h, ExtendedColorType::Rgba8)
            } else {
                enc.write_image(&img.to_rgb_over_white(), w, h, ExtendedColorType::Rgb8)
            }
            .context("TIFF encoding failed")?;
        }
        ImageFormat::Tga => {
            let enc = TgaEncoder::new(&mut out);
            if alpha {
                enc.write_image(&img.to_rgba_straight(), w, h, ExtendedColorType::Rgba8)
            } else {
                enc.write_image(&img.to_rgb_over_white(), w, h, ExtendedColorType::Rgb8)
            }
            .context("TGA encoding failed")?;
        }
        ImageFormat::Pcx => {
            out = pcx::encode_rgb(w, h, &img.to_rgb_over_white())?;
        }
        ImageFormat::Webp => {
            ensure!(
                w <= WEBP_MAX_DIM && h <= WEBP_MAX_DIM,
                "WebP supports at most {WEBP_MAX_DIM}x{WEBP_MAX_DIM} pixels (image is {w}x{h}); choose PNG instead"
            );
            let lossless = quality >= 100;
            let rgba;
            let rgb;
            let enc = if alpha {
                rgba = img.to_rgba_straight();
                webp::Encoder::from_rgba(&rgba, w, h)
            } else {
                rgb = img.to_rgb_over_white();
                webp::Encoder::from_rgb(&rgb, w, h)
            };
            let mem = enc
                .encode_simple(lossless, if lossless { 75.0 } else { quality as f32 })
                .map_err(|e| anyhow!("WebP encoding failed: {e:?}"))?;
            out = mem.to_vec();
        }
    }
    Ok(out)
}

/// GIF only has 1-bit transparency: alpha < 128 becomes fully transparent, the rest opaque.
fn gif_pixels(img: &Image) -> Vec<u8> {
    let mut rgba = img.to_rgba_straight();
    for px in rgba.chunks_exact_mut(4) {
        if px[3] < 128 {
            px.copy_from_slice(&[0, 0, 0, 0]);
        } else {
            px[3] = 255;
        }
    }
    rgba
}

/// Format for a path's extension.
pub fn format_for_path(path: &Path) -> Result<ImageFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    ImageFormat::from_extension(ext)
        .ok_or_else(|| anyhow!("unsupported image file type \".{ext}\" ({})", path.display()))
}

/// Save to `path`; format from the extension, quality from settings. Creates folders.
/// Writes to a temp file then renames (no half-written files).
pub fn save_image(img: &Image, path: &Path) -> Result<()> {
    let format = format_for_path(path)?;
    let quality = {
        let s = settings::get();
        match format {
            ImageFormat::Jpeg => s.output.jpeg_quality,
            ImageFormat::Webp => s.output.webp_quality,
            _ => 100,
        }
    };
    save_image_as(img, path, format, quality)
}

/// [`save_image`] with an explicit format and quality.
pub fn save_image_as(img: &Image, path: &Path, format: ImageFormat, quality: u8) -> Result<()> {
    let bytes = encode(img, format, quality).with_context(|| format!("could not encode {}", path.display()))?;
    write_atomic(path, &bytes)?;
    log::info!("saved {} ({}x{}, {} bytes)", path.display(), img.width, img.height, bytes.len());
    Ok(())
}

/// Write `bytes` to a temp file next to `path`, flush it to disk, then rename over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("could not create folder {}", parent.display()))?;
    }
    let tmp = temp_path_for(path);
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("could not create {}", tmp.display()))?;
        f.write_all(bytes).with_context(|| format!("could not write {}", tmp.display()))?;
        f.sync_all().with_context(|| format!("could not flush {}", tmp.display()))?;
        drop(f);
        std::fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn temp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    path.with_file_name(format!(".{name}.{}-{nanos}.sftmp", std::process::id()))
}

/// Decode any supported file (png, jpg, bmp, gif, tif, tga, pcx, webp, ico) to an Image.
pub fn load_image(path: &Path) -> Result<Image> {
    let data = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    decode(&data, &ext).with_context(|| format!("could not open {}", path.display()))
}

/// Decode bytes; the format is detected from the content, falling back to `ext_hint`.
pub fn decode(data: &[u8], ext_hint: &str) -> Result<Image> {
    if is_webp(data) {
        return decode_webp(data);
    }
    if let Ok(fmt) = image::guess_format(data) {
        return decode_with(data, fmt);
    }
    if pcx::is_pcx(data) || ext_hint == "pcx" {
        let (w, h, rgba) = pcx::decode(data)?;
        return Ok(Image::from_rgba_straight(w, h, &rgba));
    }
    match image::ImageFormat::from_extension(ext_hint) {
        Some(fmt) => decode_with(data, fmt),
        None => bail!("unrecognised image format"),
    }
}

fn decode_with(data: &[u8], fmt: image::ImageFormat) -> Result<Image> {
    let dynimg = image::load_from_memory_with_format(data, fmt).with_context(|| format!("{fmt:?} decoding failed"))?;
    let rgba = dynimg.to_rgba8();
    Ok(Image::from_rgba_straight(rgba.width(), rgba.height(), rgba.as_raw()))
}

fn is_webp(data: &[u8]) -> bool {
    data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP"
}

fn decode_webp(data: &[u8]) -> Result<Image> {
    let img =
        webp::Decoder::new(data).decode().ok_or_else(|| anyhow!("WebP decoding failed (animated or corrupt?)"))?;
    let (w, h) = (img.width(), img.height());
    if img.is_alpha() {
        Ok(Image::from_rgba_straight(w, h, &img))
    } else {
        let rgba: Vec<u8> = img.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        Ok(Image::from_rgba_straight(w, h, &rgba))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gradient with an opaque left half and a semi/fully transparent right half.
    fn sample(w: u32, h: u32, transparent: bool) -> Image {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = if !transparent || x < w / 2 {
                    255
                } else if y < h / 2 {
                    0
                } else {
                    128
                };
                rgba.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 90, a]);
            }
        }
        Image::from_rgba_straight(w, h, &rgba)
    }

    fn max_diff(a: &Image, b: &Image) -> u8 {
        a.data.iter().zip(&b.data).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
    }

    fn mean_diff(a: &Image, b: &Image) -> f64 {
        let s: u64 = a.data.iter().zip(&b.data).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        s as f64 / a.data.len() as f64
    }

    fn over_white(img: &Image) -> Image {
        let rgb = img.to_rgb_over_white();
        let rgba: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        Image::from_rgba_straight(img.width, img.height, &rgba)
    }

    #[test]
    fn roundtrip_every_format() {
        let src = sample(37, 23, true);
        for fmt in ImageFormat::ALL {
            let bytes = encode(&src, fmt, 100).unwrap_or_else(|e| panic!("{fmt:?}: {e:#}"));
            let back = decode(&bytes, fmt.extension()).unwrap_or_else(|e| panic!("{fmt:?}: {e:#}"));
            assert_eq!((back.width, back.height), (37, 23), "{fmt:?}");
            match fmt {
                // Lossless with alpha.
                ImageFormat::Png | ImageFormat::Tiff | ImageFormat::Tga | ImageFormat::Webp => {
                    assert!(max_diff(&src, &back) <= 1, "{fmt:?} max diff {}", max_diff(&src, &back));
                }
                // Lossless, composited over white.
                ImageFormat::Bmp | ImageFormat::Pcx => {
                    assert!(!back.has_transparency(), "{fmt:?}");
                    assert!(max_diff(&over_white(&src), &back) <= 1, "{fmt:?}");
                }
                ImageFormat::Jpeg => {
                    assert!(!back.has_transparency());
                    assert!(mean_diff(&over_white(&src), &back) < 4.0, "{}", mean_diff(&over_white(&src), &back));
                }
                ImageFormat::Gif => {
                    // 1-bit transparency survives; colours are quantised.
                    assert_eq!(back.pixel(36, 0)[3], 0);
                    assert_eq!(back.pixel(36, 22)[3], 255);
                    assert_eq!(back.pixel(0, 0)[3], 255);
                    let opaque_src = sample(37, 23, false);
                    let mut b2 = back.clone();
                    b2.set_opaque();
                    // Compare the opaque left half only.
                    let l = |i: &Image| i.crop(crate::common::Rect::new(0, 0, 18, 23));
                    assert!(mean_diff(&l(&opaque_src), &l(&b2)) < 12.0);
                }
            }
        }
    }

    #[test]
    fn lossy_webp_and_jpeg_quality() {
        let src = sample(64, 48, false);
        let hi = encode(&src, ImageFormat::Jpeg, 95).unwrap();
        let lo = encode(&src, ImageFormat::Jpeg, 10).unwrap();
        assert!(lo.len() < hi.len());
        let w = encode(&src, ImageFormat::Webp, 80).unwrap();
        let back = decode(&w, "webp").unwrap();
        assert_eq!((back.width, back.height), (64, 48));
        assert!(mean_diff(&src, &back) < 6.0);
        // Lossy WebP with alpha keeps the alpha channel.
        let t = sample(64, 48, true);
        let back = decode(&encode(&t, ImageFormat::Webp, 80).unwrap(), "webp").unwrap();
        assert_eq!(back.pixel(63, 0)[3], 0);
        assert_eq!(back.pixel(0, 0)[3], 255);
    }

    #[test]
    fn opaque_png_is_rgb() {
        let src = sample(10, 10, false);
        let bytes = encode(&src, ImageFormat::Png, 100).unwrap();
        // IHDR colour type at byte 25: 2 = RGB.
        assert_eq!(bytes[25], 2);
        assert_eq!(decode(&bytes, "").unwrap(), src);
    }

    #[test]
    fn save_and_load_files_atomically() {
        let dir = std::env::temp_dir().join(format!("snapforge-save-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = sample(20, 12, true);
        for fmt in ImageFormat::ALL {
            let p = dir.join("nested").join(format!("shot.{}", fmt.extension()));
            save_image_as(&src, &p, fmt, 90).unwrap();
            // Overwrite works too.
            save_image_as(&src, &p, fmt, 90).unwrap();
            let back = load_image(&p).unwrap();
            assert_eq!((back.width, back.height), (20, 12), "{fmt:?}");
        }
        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".sftmp"))
            .collect();
        assert!(leftovers.is_empty());
        assert!(format_for_path(Path::new("x.xyz")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_ico_and_detects_by_content() {
        // Build a 16x16 ICO containing a PNG image.
        let src = sample(16, 16, true);
        let png = encode(&src, ImageFormat::Png, 100).unwrap();
        let mut ico = vec![0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0];
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(&png);
        let back = decode(&ico, "ico").unwrap();
        assert_eq!((back.width, back.height), (16, 16));
        assert!(max_diff(&src, &back) <= 1);
        // A PNG with the wrong extension hint is still detected by content.
        assert!(decode(&png, "jpg").is_ok());
        assert!(decode(b"definitely not an image", "").is_err());
    }
}
