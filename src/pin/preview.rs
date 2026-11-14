//! Renders pin, palette and OCR window previews to PNG for visual review.
//!
//! `cargo test pin_previews -- --ignored` (offscreen paint routines) and
//! `cargo test live_previews -- --ignored --test-threads=1` (real windows, captured from
//! the screen, then closed) write into `%SNAPFORGE_PREVIEW_DIR%` (default:
//! `%TEMP%\snapforge-preview`).

use super::annot::{self, Style, Tool};
use super::gdi::{self, Dib};
use super::paint::{self, Live, View};
use super::palette::{self, Item};
use crate::common::{Image, Rect};
use crate::output::save;
use std::path::PathBuf;
use tiny_skia::Transform;
use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BitBlt, GetDC, ReleaseDC, SRCCOPY};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindowRect, PeekMessageW, SendMessageW, TranslateMessage, MSG, PM_REMOVE, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
};

fn out_dir() -> PathBuf {
    let d = std::env::var_os("SNAPFORGE_PREVIEW_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("snapforge-preview"));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn write(name: &str, img: &Image) {
    let mut opaque = img.clone();
    opaque.set_opaque();
    let p = out_dir().join(name);
    save::save_image(&opaque, &p).expect("write preview");
    eprintln!("preview: {}", p.display());
}

/// A screenshot-like sample: window chrome, a header and some text.
fn sample(w: u32, h: u32) -> Image {
    let mut img = Image::filled(w, h, [0xFA, 0xF8, 0xF6, 255]);
    img.blit(&Image::filled(w, 36, [0xD4, 0x78, 0x00, 255]), 0, 0);
    if let Some(t) = gdi::text_image("Quarterly report", 18, [255, 255, 255], [0xD4, 0x78, 0x00], 0) {
        img.blit(&t, 14, 7);
    }
    let lines = ["Revenue grew 18% year over year.", "Churn fell to 2.1% in Q3.", "Next: expand the EU team."];
    for (i, l) in lines.iter().enumerate() {
        if let Some(t) = gdi::text_image(l, 15, [0x30, 0x30, 0x30], [0xF6, 0xF8, 0xFA], 0) {
            img.blit(&t, 16, 52 + i as i32 * 26);
        }
    }
    img
}

fn render_view(v: &View, w: i32, h: i32) -> Image {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    paint::render(&mut buf, w, h, v);
    Image::from_bgra(w as u32, h as u32, buf)
}

fn pad(img: &Image, m: u32) -> Image {
    let mut out = Image::filled(img.width + 2 * m, img.height + 2 * m, [0x40, 0x3A, 0x36, 255]);
    out.blit(img, m as i32, m as i32);
    out
}

#[test]
#[ignore]
fn pin_previews() {
    let style = Style { color: 0xE5_39_35, size: 1 };
    let mut img = sample(380, 150);
    // Annotate like a user would: a pen circle, two step stamps.
    {
        let mut pm = annot::pixmap_mut(&mut img).expect("pixmap");
        let pts: Vec<(f32, f32)> = (0..=40)
            .map(|i| {
                let a = i as f32 / 40.0 * std::f32::consts::TAU;
                (300.0 + a.cos() * 60.0, 70.0 + a.sin() * 22.0)
            })
            .collect();
        annot::stroke(&mut pm, &pts, style.color, style.pen_width(), Transform::identity());
        annot::step(&mut pm, 250.0, 120.0, 1, 0x1E_88_E5, style.stamp_radius(), Transform::identity());
        annot::step(&mut pm, 290.0, 120.0, 2, 0x1E_88_E5, style.stamp_radius(), Transform::identity());
        annot::text(&mut pm, 16.0, 124.0, "Check churn", 0x43_A0_47, 18, Transform::identity());
    }

    // 1. Normal pin at 100% with a zoom badge.
    let (w, h) = (img.width as i32 + 2, img.height as i32 + 2);
    let v = View {
        img: &img,
        transparent: false,
        zoom: 1.0,
        border: 1,
        border_rgb: 0x00_78_D4,
        live: Live::None,
        badge: Some("100%"),
        dpi: 96,
    };
    write("pin-normal.png", &pad(&render_view(&v, w, h), 20));

    // 2. Draw Mode, 150%, typing text with caret, step ghost not shown.
    let z = 1.5;
    let (vw, vh) = super::geom::view_size(img.width, img.height, z);
    let v = View {
        img: &img,
        transparent: false,
        zoom: z,
        border: 1,
        border_rgb: 0xF5_9E_0B,
        live: Live::Text { x: 140.0, y: 124.0, text: "and costs", rgb: style.color, px: style.text_px(), caret: true },
        badge: None,
        dpi: 96,
    };
    write("pin-draw-text-150.png", &pad(&render_view(&v, vw + 2, vh + 2), 20));

    // 3. Canvas expanded (transparent fill) at 75% with a live stroke and a step ghost.
    let canvas = super::geom::canvas_after_drag(
        img.width,
        img.height,
        super::geom::Edges { right: true, bottom: true, ..Default::default() },
        60,
        40,
    );
    let big = super::geom::apply_canvas(&img, canvas, [0, 0, 0, 0]);
    let z = 0.75;
    let (vw, vh) = super::geom::view_size(big.width, big.height, z);
    let v = View {
        img: &big,
        transparent: true,
        zoom: z,
        border: 1,
        border_rgb: 0xF5_9E_0B,
        live: Live::Step { x: 400.0, y: 170.0, n: 3, rgb: 0x1E_88_E5, radius: style.stamp_radius() },
        badge: Some("440 × 190"),
        dpi: 96,
    };
    write("pin-canvas-75.png", &pad(&render_view(&v, vw + 2, vh + 2), 20));

    // 4. Palettes at 100% and 150% DPI.
    for (dpi, tool, hover) in [(96, Tool::Pen, Some(Item::Undo)), (144, Tool::Step, Some(Item::Color(4)))] {
        let l = palette::layout(dpi);
        let mut buf = vec![0u8; (l.w * l.h * 4) as usize];
        let pv = palette::View { tool, style, hover, can_undo: dpi == 96, accent: 0x00_78_D4, dpi };
        palette::render(&mut buf, l.w, l.h, &l, &pv);
        write(&format!("pin-palette-{dpi}.png"), &pad(&Image::from_bgra(l.w as u32, l.h as u32, buf), 12));
    }

    // 5. Collapsed tab.
    let (tw, th) = paint::tab_size(144);
    let mut buf = vec![0u8; (tw * th * 4) as usize];
    paint::render_tab(&mut buf, tw, th, &img, 0x00_78_D4, 144);
    write("pin-tab-144.png", &pad(&Image::from_bgra(tw as u32, th as u32, buf), 12));
}

fn pump(ms: u64) {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut msg = MSG::default();
    while std::time::Instant::now() < end {
        // SAFETY: standard message pump on the test thread that owns the windows.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn grab_screen(r: Rect) -> Image {
    let mut dib = Dib::new(r.w, r.h).expect("dib");
    // SAFETY: screen DC is released; BitBlt into our own DIB.
    unsafe {
        let sdc = GetDC(None);
        let _ = BitBlt(dib.dc, 0, 0, r.w, r.h, sdc, r.x, r.y, SRCCOPY);
        ReleaseDC(None, sdc);
    }
    let mut img = Image::from_bgra(r.w as u32, r.h as u32, dib.bytes().to_vec());
    img.set_opaque();
    img
}

#[test]
#[ignore]
fn live_previews() {
    crate::settings::set_dir_override(std::env::temp_dir().join("snapforge-preview-settings"));
    // A real pin, captured from the screen with its DWM shadow.
    let img = sample(380, 150);
    let at = Rect::new(200, 200, img.width as i32, img.height as i32);
    super::pin_image(img, Some(at));
    pump(500);
    write("live-pin.png", &grab_screen(at.inflate(24)));
    // Draw Mode with the palette.
    if let Some(h) = super::window::all().first().copied() {
        super::window::run_cmd_for_test(h, super::keys::Cmd::EnterDraw);
    }
    pump(400);
    write("live-pin-draw.png", &grab_screen(Rect::new(at.x - 24, at.y - 24, at.w + 120, at.h + 110)));
    super::close_all();
    pump(100);
    assert_eq!(super::count(), 0);

    // OCR result window on a rendered text sample.
    let text = gdi::text_image("SnapForge recognizes\nthis sample text", 28, [0, 0, 0], [255, 255, 255], 20)
        .expect("text image");
    crate::ocr::capture_text_flow(text);
    pump(4000);
    let r = crate::ocr::open_windows_for_test();
    if let Some((h, rect)) = r.first().copied() {
        write("live-ocr.png", &grab_screen(rect));
        crate::ocr::close_for_test(h);
    }
    pump(100);
}

fn rect(h: windows::Win32::Foundation::HWND) -> Rect {
    let mut r = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    crate::common::win::rect_from_win(&r)
}

fn send(h: windows::Win32::Foundation::HWND, msg: u32, wp: usize, x: i32, y: i32) {
    let lp = ((y as u16 as isize) << 16) | (x as u16 as isize);
    // SAFETY: synchronous message to our own window.
    unsafe {
        SendMessageW(h, msg, WPARAM(wp), LPARAM(lp));
    }
}

/// Drives a real pin with window messages: draw a stroke, undo, zoom with the wheel,
/// Space → tab → Space, Ctrl+drag-free canvas logic is covered by unit tests.
#[test]
#[ignore]
fn live_interaction() {
    crate::settings::set_dir_override(std::env::temp_dir().join("snapforge-preview-settings"));
    let img = Image::filled(200, 100, [255, 255, 255, 255]);
    let at = Rect::new(300, 300, 200, 100);
    super::pin_image(img, Some(at));
    pump(200);
    let h = *super::window::all().first().expect("pin open");
    assert_eq!(rect(h), at.inflate(1), "pinned 1:1 at its screen position");

    // Draw Mode, pen stroke across the middle (client coords include the 1 px border).
    super::window::run_cmd_for_test(h, super::keys::Cmd::EnterDraw);
    send(h, WM_LBUTTONDOWN, 1, 21, 51);
    for x in (21..180).step_by(10) {
        send(h, WM_MOUSEMOVE, 1, x, 51);
    }
    send(h, WM_LBUTTONUP, 0, 180, 51);
    let drawn = super::pinned_images().pop().expect("image");
    let px = drawn.pixel(100, 50);
    assert!(px[0] < 128 && px[2] > 128, "pen stroke is red: {px:?}");
    // Ctrl+Z undoes it.
    let ctrl_z = super::keys::Cmd::Undo;
    super::window::run_cmd_for_test(h, ctrl_z);
    assert_eq!(super::pinned_images().pop().expect("image").pixel(100, 50), [255, 255, 255, 255]);
    super::window::run_cmd_for_test(h, super::keys::Cmd::ExitDraw);

    // Wheel up two notches at the image centre: 121% (1.1²), centre stays put.
    let c = at.center();
    send(h, WM_MOUSEWHEEL, (240u32 << 16) as usize, c.x, c.y);
    let r = rect(h);
    assert_eq!((r.w - 2, r.h - 2), (242, 121), "{r:?}");
    assert!((r.center().x - c.x).abs() <= 1 && (r.center().y - c.y).abs() <= 1, "{r:?}");

    // Space (default: hide to tab) collapses to a tab at the monitor top; Space restores.
    send(h, WM_KEYDOWN, 0x20, 0, 0);
    let tab = rect(h);
    let mon = crate::common::win::monitor_at(c).expect("monitor");
    assert_eq!(tab.y, mon.rect.y);
    assert!(tab.h < 60, "{tab:?}");
    send(h, WM_KEYDOWN, 0x20, 0, 0);
    assert_eq!(rect(h), r);

    // Esc closes.
    send(h, WM_KEYDOWN, 0x1B, 0, 0);
    pump(50);
    assert_eq!(super::count(), 0);
}
