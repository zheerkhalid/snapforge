//! Manual harness (ignored tests):
//! * `cargo test overlay::harness::snapshots -- --ignored` renders every mode offscreen to
//!   PNG files in `$SNAPFORGE_SNAP_DIR` (default `target/overlay-snapshots`).
//! * `cargo test --release overlay::harness::bench_first_paint -- --ignored --nocapture`
//!   shows the real overlay with `BENCH_AUTOCANCEL` and prints select() → first paint.

use super::logic::{Config, Input, Key, Mods, Monitor};
use super::window::render_offscreen;
use super::*;
use crate::common::{Image, Point, Rect};
use std::sync::atomic::Ordering;

/// A fake desktop: gradient wallpaper plus a few "windows" with title bars.
fn desktop(w: u32, h: u32) -> (Image, Vec<WindowEntry>) {
    let mut img = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let r = (40 + x * 80 / w) as u8;
            let g = (70 + y * 90 / h) as u8;
            img.set_pixel(x, y, [150, g, r, 255]);
        }
    }
    let wins = [Rect::new(200, 150, 900, 600), Rect::new(700, 400, 800, 500), Rect::new(1300, 120, 500, 380)];
    let mut entries = Vec::new();
    for (i, r) in wins.iter().enumerate().rev() {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let title = y < r.y + 32;
                let stripe = !title && ((y - r.y) / 24) % 2 == 0 && x > r.x + 20 && x < r.right() - 20;
                let c = if title {
                    [70, 60, 50, 255]
                } else if stripe {
                    [228, 232, 236, 255]
                } else {
                    [250, 250, 250, 255]
                };
                img.set_pixel(x as u32, y as u32, c);
            }
        }
        // A red marker pixel block for the loupe.
        for y in 0..5 {
            for x in 0..5 {
                img.set_pixel((r.x + 40 + x) as u32, (r.y + 60 + y) as u32, [40, 40, 220, 255]);
            }
        }
        entries.push(WindowEntry { hwnd: 100 + i as isize, rect: *r, title: format!("w{i}"), class: "test".into() });
    }
    entries.reverse();
    (img, entries)
}

#[test]
#[ignore]
fn snapshots() {
    let (img, windows) = desktop(1920, 1080);
    let frozen = Frozen { image: img, origin: Rect::new(0, 0, 1920, 1080), windows };
    let dir = std::env::var("SNAPFORGE_SNAP_DIR").unwrap_or_else(|_| "target/overlay-snapshots".into());
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    let none = Mods::default();
    let ctrl = Mods { ctrl: true, ..Mods::default() };
    let p = Point::new;
    type Case = (&'static str, OverlayMode, Config, Point, Vec<(Input, Mods)>);
    let cases: Vec<Case> = vec![
        ("region_hover", OverlayMode::Region, Config::default(), p(560, 420), vec![]),
        (
            "region_drag",
            OverlayMode::Region,
            Config::default(),
            p(300, 250),
            vec![(Input::LDown(p(300, 250)), none), (Input::Move(p(842, 562)), none)],
        ),
        (
            "region_adjust",
            OverlayMode::Region,
            Config { capture_on_release: false, ..Config::default() },
            p(300, 250),
            vec![(Input::LDown(p(300, 250)), none), (Input::Move(p(842, 562)), none), (Input::LUp(p(842, 562)), none)],
        ),
        (
            "freehand",
            OverlayMode::Freehand,
            Config::default(),
            p(400, 400),
            std::iter::once((Input::LDown(p(400, 400)), none))
                .chain((0..120).map(|i| {
                    let a = i as f32 / 20.0;
                    (Input::Move(p(600 + (a.cos() * 200.0) as i32, 400 + (a.sin() * 150.0) as i32)), none)
                }))
                .collect(),
        ),
        (
            "window_object",
            OverlayMode::WindowObject,
            Config::default(),
            p(1000, 600),
            vec![
                (Input::Move(p(1500, 300)), none),
                (Input::Wheel(1), none),
                (Input::LDown(p(1500, 300)), ctrl),
                (Input::LUp(p(1500, 300)), ctrl),
                (Input::Move(p(1000, 600)), none),
            ],
        ),
        (
            "fixed",
            OverlayMode::Fixed,
            Config { presets: vec![(640, 480), (800, 600)], ..Config::default() },
            p(900, 500),
            vec![(Input::Key(Key::Right), Mods { shift: true, ..Mods::default() })],
        ),
        ("scroll_pick", OverlayMode::ScrollPick, Config::default(), p(1000, 600), vec![]),
    ];
    for dpi in [96u32, 144] {
        let monitors = vec![Monitor { rect: frozen.origin, dpi }];
        for (name, mode, cfg, cursor, inputs) in &cases {
            let out = render_offscreen(&frozen, *mode, cfg.clone(), monitors.clone(), *cursor, inputs).expect("render");
            let path = format!("{dir}/{name}_{dpi}.png");
            image::save_buffer(&path, &out.to_rgba_straight(), out.width, out.height, image::ExtendedColorType::Rgba8)
                .expect("save png");
        }
    }
    // Countdown frame.
    let mut painter = super::countdown::Painter::new(240, 144).expect("painter");
    let pm = painter.frame(3, 0.35).expect("frame");
    let img = Image::from_bgra(240, 240, pm.data().chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect());
    // Composite over a mid-grey so the translucency is visible.
    let mut bg = Image::filled(260, 260, [120, 120, 120, 255]);
    bg.draw_over(&img, 10, 10);
    let path = format!("{dir}/countdown.png");
    image::save_buffer(&path, &bg.to_rgba_straight(), bg.width, bg.height, image::ExtendedColorType::Rgba8)
        .expect("save png");
}

#[test]
#[ignore]
fn bench_first_paint() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    // SAFETY: plain call; fails harmlessly if awareness was already set.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let vs = crate::common::win::virtual_screen();
    BENCH_AUTOCANCEL.store(true, Ordering::Release);
    for (label, w, h) in [("screen", vs.w as u32, vs.h as u32), ("4K", 3840, 2160)] {
        let (img, windows) = desktop(w, h);
        let frozen = Frozen { image: img, origin: Rect::new(vs.x, vs.y, w as i32, h as i32), windows };
        let mut times = Vec::new();
        for _ in 0..6 {
            let t0 = crate::common::time::qpc_now();
            let r = select(&frozen, OverlayMode::Region);
            let t1 = LAST_FIRST_PAINT_QPC.load(Ordering::Acquire);
            assert!(r.is_none());
            times.push(crate::common::time::qpc_ms(t0, t1));
        }
        println!("{label} {w}x{h}: select() -> first paint ms: {times:.1?}");
        release_idle_resources();
    }
    BENCH_AUTOCANCEL.store(false, Ordering::Release);
}
