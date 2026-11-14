//! Renders toolbar and icon previews to PNG for visual review.
//!
//! `cargo test render_previews -- --ignored` writes into `%SNAPFORGE_PREVIEW_DIR%`
//! (default: `%TEMP%\snapforge-preview`).

use super::icons::{self, Icon, Palette};
use super::layout::{self, Theme};
use super::paint::{self, Dib, IconCache, View};
use crate::common::win::{pcwstr, wide};
use crate::settings::{Destination, ToolbarSettings};
use image::{imageops, Rgba, RgbaImage};
use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_LIBRARY_AS_IMAGE_RESOURCE};
use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR};

fn app_icon(size: i32) -> HICON {
    let own = super::load_app_icon(size);
    if !own.is_invalid() {
        return own;
    }
    // Test binaries carry no resources: borrow the icon from the built SnapForge.exe.
    let Ok(exe) = std::env::current_exe() else { return HICON::default() };
    let Some(dir) = exe.parent().and_then(|p| p.parent()) else { return HICON::default() };
    let path = wide(&dir.join("SnapForge.exe").display().to_string());
    // SAFETY: loads the EXE as a resource-only image; leaked for the test's lifetime.
    unsafe {
        let Ok(m) = LoadLibraryExW(pcwstr(&path), None, LOAD_LIBRARY_AS_IMAGE_RESOURCE) else {
            return HICON::default();
        };
        LoadImageW(m, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default()
    }
}

fn dib_to_image(d: &mut Dib) -> RgbaImage {
    let (w, h) = (d.w as u32, d.h as u32);
    let mut px = d.bytes().to_vec();
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
        p[3] = 255;
    }
    RgbaImage::from_raw(w, h, px).unwrap_or_else(|| RgbaImage::new(1, 1))
}

struct Variant {
    dark: bool,
    big: bool,
    dpi: u32,
    hover: Option<usize>,
    pressed: Option<usize>,
    dest: Destination,
    delay: u32,
}

fn render_toolbar(v: &Variant, cache: &mut IconCache) -> RgbaImage {
    let buttons = layout::buttons_from_ids(&ToolbarSettings::default().buttons);
    let l = layout::compute(&buttons, v.dpi, v.big);
    let icon = app_icon(l.app_icon.w);
    let view = View {
        layout: &l,
        theme: Theme::new(v.dark),
        hover: v.hover,
        pressed: v.pressed,
        dest: v.dest,
        delay: v.delay,
        app_icon: icon,
    };
    let Some(mut dib) = Dib::new(l.w, l.h) else { return RgbaImage::new(1, 1) };
    paint::render(&mut dib, &view, cache);
    dib_to_image(&mut dib)
}

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("SNAPFORGE_PREVIEW_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("snapforge-preview"));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn stack(images: &[(RgbaImage, Rgba<u8>)], margin: u32) -> RgbaImage {
    let w = images.iter().map(|(i, _)| i.width()).max().unwrap_or(1) + 2 * margin;
    let h: u32 = images.iter().map(|(i, _)| i.height() + 2 * margin).sum();
    let mut out = RgbaImage::new(w, h);
    let mut y = 0;
    for (img, bg) in images {
        let band = img.height() + 2 * margin;
        for yy in y..y + band {
            for x in 0..w {
                out.put_pixel(x, yy, *bg);
            }
        }
        imageops::overlay(&mut out, img, margin as i64, (y + margin) as i64);
        y += band;
    }
    out
}

#[test]
#[ignore = "writes preview PNGs; run explicitly"]
fn render_previews() {
    let dir = out_dir();
    let mut cache = IconCache::default();
    let desk_light = Rgba([0xC9, 0xD3, 0xE6, 255]);
    let desk_dark = Rgba([0x0E, 0x10, 0x16, 255]);
    let mut rows = Vec::new();
    for big in [false, true] {
        for dark in [false, true] {
            let bg = if dark { desk_dark } else { desk_light };
            let plain = Variant { dark, big, dpi: 96, hover: None, pressed: None, dest: Destination::Editor, delay: 0 };
            rows.push((render_toolbar(&plain, &mut cache), bg));
            let states = Variant {
                hover: Some(2),
                pressed: Some(4),
                dest: Destination::Clipboard,
                delay: if big { 10 } else { 3 },
                ..plain
            };
            rows.push((render_toolbar(&states, &mut cache), bg));
        }
    }
    let toolbar = stack(&rows, 12);
    toolbar.save(dir.join("toolbar.png")).expect("save toolbar.png");
    let zoom = imageops::resize(&toolbar, toolbar.width() * 2, toolbar.height() * 2, imageops::FilterType::Nearest);
    zoom.save(dir.join("toolbar@2x-zoom.png")).expect("save zoom");

    let hi = [
        (
            render_toolbar(
                &Variant {
                    dark: false,
                    big: false,
                    dpi: 144,
                    hover: Some(8),
                    pressed: None,
                    dest: Destination::Word,
                    delay: 5,
                },
                &mut cache,
            ),
            desk_light,
        ),
        (
            render_toolbar(
                &Variant {
                    dark: true,
                    big: false,
                    dpi: 192,
                    hover: Some(1),
                    pressed: None,
                    dest: Destination::Upload,
                    delay: 2,
                },
                &mut cache,
            ),
            desk_dark,
        ),
    ];
    stack(&hi, 12).save(dir.join("toolbar-hidpi.png")).expect("save hidpi");

    // Icon sheet: every icon at 20/28 px on both themes, plus a 4x zoom.
    let all = Icon::all();
    let cell = 48u32;
    let mut sheet = RgbaImage::new(cell * all.len() as u32, cell * 4);
    for (row, (dark, size)) in [(false, 20u32), (false, 28), (true, 20), (true, 28)].into_iter().enumerate() {
        let th = Theme::new(dark);
        let bg = Rgba(th.bg);
        for (col, icon) in all.iter().enumerate() {
            let x0 = col as u32 * cell;
            let y0 = row as u32 * cell;
            for y in y0..y0 + cell {
                for x in x0..x0 + cell {
                    sheet.put_pixel(x, y, bg);
                }
            }
            let pm = icons::render(*icon, size, Palette::for_theme(dark)).expect("icon");
            // Un-premultiply into an RGBA image for compositing.
            let mut img = RgbaImage::new(size, size);
            for (i, p) in pm.pixels().iter().enumerate() {
                let c = p.demultiply();
                img.put_pixel(i as u32 % size, i as u32 / size, Rgba([c.red(), c.green(), c.blue(), c.alpha()]));
            }
            let off = (cell - size) / 2;
            imageops::overlay(&mut sheet, &img, (x0 + off) as i64, (y0 + off) as i64);
        }
    }
    sheet.save(dir.join("icons.png")).expect("save icons");
    let half = all.len() as u32 / 2 * cell;
    for (name, x) in [("icons-zoom-a.png", 0), ("icons-zoom-b.png", half)] {
        let part = imageops::crop_imm(&sheet, x, 0, (sheet.width() - x).min(half + cell), sheet.height()).to_image();
        let z = imageops::resize(&part, part.width() * 3, part.height() * 3, imageops::FilterType::Nearest);
        z.save(dir.join(name)).expect("save zoom");
    }
    eprintln!("previews written to {}", dir.display());
}
