//! Manual harness (ignored tests):
//! `cargo test tools::preview -- --ignored --test-threads=1` renders every tool's paint
//! routine offscreen over a fake desktop to PNG files in `$SNAPFORGE_SNAP_DIR`
//! (default `target/tools-previews`). The two native dialogs are created for real
//! (briefly on screen) and captured with PrintWindow.

use super::color::Rgb;
use super::gfx::{self, col, Align, Dib, Face, Text};
use super::lens::{Grid, LensOptions};
use crate::common::{Image, Point, Rect};
use crate::settings::tools::RulerUnit;

fn out_dir() -> String {
    let d = std::env::var("SNAPFORGE_SNAP_DIR").unwrap_or_else(|_| "target/tools-previews".into());
    std::fs::create_dir_all(&d).expect("preview dir");
    d
}

fn save(img: &Image, name: &str) {
    let path = format!("{}/{name}.png", out_dir());
    image::save_buffer(&path, &img.to_rgba_straight(), img.width, img.height, image::ExtendedColorType::Rgba8)
        .expect("save png");
    println!("wrote {path}");
}

/// A fake desktop: gradient wallpaper, a light document window with text and color
/// blocks, and a dark window.
fn desktop(w: i32, h: i32) -> Image {
    let mut dib = Dib::new(w, h).expect("dib");
    {
        let data = dib.data_mut();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let r = (40 + x * 70 / w) as u8;
                let g = (90 + y * 80 / h) as u8;
                data[i..i + 4].copy_from_slice(&[170, g, r, 255]);
            }
        }
    }
    let mut text = Text::default();
    {
        let mut pm = dib.pixmap().expect("pm");
        gfx::shadow(&mut pm, 120.0, 90.0, 760.0, 520.0, 10.0, 14.0);
        gfx::fill(&mut pm, gfx::rrect(120.0, 90.0, 760.0, 520.0, 10.0), col(250, 250, 252, 255));
        gfx::fill(&mut pm, gfx::rrect(120.0, 90.0, 760.0, 40.0, 10.0), col(232, 234, 240, 255));
        let colors = [(239, 68, 68), (245, 158, 11), (16, 185, 129), (59, 130, 246), (139, 92, 246), (236, 72, 153)];
        for (i, (r, g, b)) in colors.iter().enumerate() {
            gfx::fill(&mut pm, gfx::rrect(160.0 + i as f32 * 110.0, 420.0, 90.0, 90.0, 12.0), col(*r, *g, *b, 255));
        }
        gfx::fill(&mut pm, gfx::rrect(940.0, 160.0, 520.0, 420.0, 10.0), col(32, 34, 42, 255));
        for i in 0..8 {
            let y = 200.0 + i as f32 * 40.0;
            gfx::fill(&mut pm, gfx::rrect(970.0, y, 120.0 + (i * 37 % 200) as f32, 14.0, 4.0), col(90, 160, 250, 200));
        }
    }
    text.draw(&mut dib, "Quarterly report", 160, 150, 34, Face::UiSemibold, [20, 22, 28, 255], Align::Left);
    for (i, l) in [
        "Revenue grew 18% year over year, driven by the new",
        "subscription tier and lower churn in the EU region.",
        "The magnifier should render this text crisply.",
    ]
    .iter()
    .enumerate()
    {
        text.draw(&mut dib, l, 160, 210 + i as i32 * 30, 18, Face::Ui, [60, 64, 76, 255], Align::Left);
    }
    text.draw(&mut dib, "SnapForge", 140, 100, 16, Face::UiSemibold, [80, 84, 96, 255], Align::Left);
    let mut img = dib.to_image();
    img.set_opaque();
    img
}

fn over(bg: &Image, fg: &Dib, at: Point) -> Image {
    let mut out = bg.clone();
    out.draw_over(&fg.to_image(), at.x, at.y);
    out
}

#[test]
#[ignore]
fn picker_card() {
    let desk = desktop(1600, 900);
    for (dpi, frozen, zoom) in [(96u32, false, 10u32), (144, true, 8)] {
        let cursor = Point::new(162, 424);
        let grid = Grid::fit(super::card::sci(212.0 - 24.0, dpi), zoom, dpi);
        let src_rect = grid.source_rect(cursor);
        let src = desk.crop(src_rect);
        let color = Rgb::from_bgra(desk.pixel(cursor.x as u32, cursor.y as u32));
        let mut text = Text::default();
        let view = super::picker::CardView { src: &src, src_rect, center: cursor, grid, color, frozen, zoom, dpi };
        let dib = super::picker::render_card(None, &mut text, &view).expect("card");
        let bg = desk.crop(Rect::new(120, 360, dib.w + 80, dib.h + 120));
        save(&over(&bg, &dib, Point::new(60, 90)), &format!("picker_{dpi}"));
    }
}

#[test]
#[ignore]
fn magnifier_views() {
    let desk = desktop(1600, 900);
    for (dpi, zoom, smooth, negative) in [(96u32, 4u32, false, false), (144, 3, true, false), (96, 6, false, true)] {
        let center = Point::new(300, 250);
        let size = 280u32;
        let d = super::card::sci(size as f32, dpi);
        let src_rect = super::lens::grab_rect(center, (d, d), zoom as f64);
        let src = desk.crop(src_rect);
        let mut text = Text::default();
        let view = super::magnifier::View {
            src: &src,
            src_rect,
            center,
            zoom,
            opts: LensOptions { smooth, negative },
            dpi,
            hud: Some(format!("{zoom}\u{00D7}{}", if smooth { " \u{00B7} Smooth" } else { "" })),
        };
        let dib = super::magnifier::render_lens(None, &mut text, &view, size).expect("lens");
        let at = Point::new(center.x - dib.w / 2, center.y - dib.h / 2);
        let bg = desk.crop(Rect::new(0, 0, 700, 600));
        save(
            &over(&bg, &dib, at),
            &format!(
                "magnifier_lens_{dpi}_{zoom}x{}{}",
                if smooth { "_smooth" } else { "" },
                if negative { "_neg" } else { "" }
            ),
        );
    }
    let center = Point::new(170, 166);
    let (w, h) = (480, 320);
    let src_rect = super::lens::grab_rect(center, (w, h), 4.0);
    let src = desk.crop(src_rect);
    let mut text = Text::default();
    let view = super::magnifier::View {
        src: &src,
        src_rect,
        center,
        zoom: 4,
        opts: LensOptions { smooth: false, negative: false },
        dpi: 96,
        hud: Some("4\u{00D7}".into()),
    };
    let dib = super::magnifier::render_pane(None, &mut text, &view, w, h).expect("pane");
    save(&dib.to_image(), "magnifier_pane_96");
}

#[test]
#[ignore]
fn crosshair_view() {
    let desk = desktop(1600, 900);
    let mut img = desk.clone();
    let p = Point::new(620, 455);
    let vs = Rect::new(0, 0, 1600, 900);
    let segs = super::crosshair::segments(p, vs, 12);
    // Paint the four segments as the layered windows would show them.
    for (i, r) in segs.iter().enumerate() {
        let mut d = Dib::new(r.w.max(1), r.h.max(1)).expect("dib");
        for k in 0..3 {
            let px = if k == 1 { gfx::premul(255, 59, 107, 255) } else { gfx::premul(0, 0, 0, 96) };
            let rr = if i < 2 { Rect::new(0, k, r.w, 1) } else { Rect::new(k, 0, 1, r.h) };
            gfx::fill_rect_raw(&mut d, rr, px);
        }
        img.draw_over(&d.to_image(), r.x, r.y);
    }
    for dpi in [96u32, 144] {
        let grid = Grid::fit(super::card::sci(168.0 - 20.0, dpi), 8, dpi);
        let src_rect = grid.source_rect(p);
        let src = desk.crop(src_rect);
        let color = Rgb::from_bgra(desk.pixel(p.x as u32, p.y as u32));
        let mut text = Text::default();
        let dib = super::crosshair::render_loupe(None, &mut text, &src, src_rect, p, grid, color, dpi).expect("loupe");
        let r = crate::overlay::loupe::place(p, (dib.w, dib.h), 10, &vs);
        let out = over(&img, &dib, Point::new(r.x, r.y));
        save(&out.crop(Rect::new(300, 200, 800, 560)), &format!("crosshair_{dpi}"));
    }
}

#[test]
#[ignore]
fn focus_views() {
    use super::focus::{paint_area, Spot};
    let desk = desktop(1600, 900);
    let alpha = (65u32 * 255 / 100) as u8;
    for (name, spot) in [
        ("focus_circle", Spot::circle(Point::new(330, 300), 160.0, 3.0)),
        ("focus_rect", Spot::rect(Rect::new(140, 400, 700, 130), 8.0, 3.0)),
        ("focus_roundrect", Spot::rect(Rect::new(1000 - 256, 380 - 160, 512, 320), 18.0, 3.0)),
    ] {
        let mut d = Dib::new(1600, 900).expect("dib");
        paint_area(&mut d, Point::new(0, 0), Rect::new(0, 0, 1600, 900), Some(&spot), alpha);
        save(&over(&desk, &d, Point::new(0, 0)), name);
    }
}

#[test]
#[ignore]
fn ruler_views() {
    let desk = desktop(1600, 900);
    for (dpi, unit, measure, cursor) in [
        (96u32, RulerUnit::Px, Some((120.0, 384.0)), Some(520.5)),
        (96, RulerUnit::Cm, None, Some(300.5)),
        (144, RulerUnit::Inch, Some((40.0, 330.0)), None),
        (96, RulerUnit::Px, None, None),
    ] {
        let len = super::card::sci(760.0, dpi);
        let v = super::ruler::View { len, dpi, unit, ppi: 108.8, opacity: 88, cursor_u: cursor, measure, hover: None };
        let mut text = Text::default();
        let base = super::ruler::render_base(&v, &mut text).expect("base");
        let dib = super::ruler::render_ruler(&base, &mut text, &v).expect("ruler");
        let bg = desk.crop(Rect::new(100, 120, len + 80, dib.h + 80));
        let suffix = format!(
            "{}{}",
            if measure.is_some() { "_measure" } else { "" },
            if cursor.is_some() { "_cursor" } else { "" }
        );
        save(&over(&bg, &dib, Point::new(40, 40)), &format!("ruler_{}_{dpi}{suffix}", unit.suffix()));
        if dpi == 96 && unit == RulerUnit::Cm {
            let rot = super::ruler::rotate_cw(&dib).expect("rotate");
            let bg = desk.crop(Rect::new(880, 60, rot.w + 80, 860));
            save(&over(&bg, &rot, Point::new(40, 40)), "ruler_vertical_96");
        }
    }
    for (dpi, arms) in [(96u32, [20.0, 75.0]), (144, [300.0, 200.0])] {
        let v = super::ruler::ProtView { size: super::card::sci(320.0, dpi), dpi, opacity: 88, arms, hover: None };
        let mut text = Text::default();
        let dib = super::ruler::render_protractor(None, &mut text, &v).expect("protractor");
        let bg = desk.crop(Rect::new(100, 60, dib.w + 80, dib.h + 80));
        save(&over(&bg, &dib, Point::new(40, 40)), &format!("protractor_{dpi}"));
    }
}

/// Create the real native dialogs, pump messages briefly and capture them.
#[test]
#[ignore]
fn native_dialogs() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetWindowRect, PeekMessageW, SetWindowDisplayAffinity, TranslateMessage, MSG, PM_REMOVE,
        WDA_NONE,
    };
    // SAFETY: plain call; harmless if already set.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    crate::settings::set_dir_override(std::env::temp_dir().join("snapforge-tools-preview"));
    crate::settings::update(|s| {
        for c in [0xEF4444, 0xF59E0B, 0x10B981, 0x3B82F6, 0x8B5CF6, 0xEC4899, 0x111827, 0xF3F4F6, 0x0EA5E9] {
            s.tools.push_recent_color(c);
        }
        s.tools.color_prefix = "color: ".into();
        s.tools.color_suffix = ";".into();
    });
    let pump = || {
        let start = std::time::Instant::now();
        let mut msg = MSG::default();
        while start.elapsed() < std::time::Duration::from_millis(400) {
            // SAFETY: standard message pumping on this thread.
            unsafe {
                while start.elapsed() < std::time::Duration::from_millis(400)
                    && PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool()
                {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let grab = |hwnd: isize, name: &str| {
        let h = crate::common::win::hwnd_from_isize(hwnd);
        let mut rc = windows::Win32::Foundation::RECT::default();
        // SAFETY: test-only capture of our own window into a DIB.
        unsafe {
            let _ = SetWindowDisplayAffinity(h, WDA_NONE);
            let _ = GetWindowRect(h, &mut rc);
            let mut d = Dib::new(rc.right - rc.left, rc.bottom - rc.top).expect("dib");
            let _ = PrintWindow(h, d.dc, PRINT_WINDOW_FLAGS(2));
            for px in d.data_mut().chunks_exact_mut(4) {
                px[3] = 255;
            }
            save(&d.to_image(), name);
        }
    };
    super::finetune::open(Rgb::new(59, 130, 246), Point::new(400, 300));
    pump();
    super::finetune::set_adjusted(Rgb::new(80, 150, 230));
    pump();
    if let Some(&h) = super::finetune::hwnds().first() {
        grab(h, "color_panel");
    }
    super::finetune::close();
    if let Some(m) = crate::common::win::monitor_at(Point::new(400, 300)) {
        super::calibrate::open(m);
        pump();
        if let Some(&h) = super::calibrate::hwnds().first() {
            grab(h, "calibrate_dialog");
        }
        super::calibrate::close();
    }
}

/// GDI BitBlt from the screen must not see windows excluded from capture: the
/// magnifier and loupes rely on it.
#[test]
#[ignore]
fn bitblt_skips_excluded_windows() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyWindow, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    // SAFETY: plain call.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    assert!(super::wnd::register(
        "SnapForge.TestExcl",
        Some(def_proc),
        windows::Win32::UI::WindowsAndMessaging::IDC_ARROW
    ));
    let r = Rect::new(100, 100, 64, 64);
    let h = super::wnd::create(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        "SnapForge.TestExcl",
        "",
        WS_POPUP,
        r,
        windows::Win32::Foundation::HWND::default(),
    )
    .expect("window");
    let mut d = Dib::new(64, 64).expect("dib");
    gfx::fill_rect_raw(&mut d, Rect::new(0, 0, 64, 64), [0, 0, 255, 255]);
    gfx::present(h, &d, Point::new(r.x, r.y));
    // SAFETY: our own window.
    unsafe {
        let _ =
            windows::Win32::UI::WindowsAndMessaging::ShowWindow(h, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNA);
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    let mut g = gfx::Grabber::default();
    let visible = g.grab(Rect::new(132, 132, 1, 1)).expect("grab").pixel(0, 0);
    crate::common::win::exclude_from_capture(h);
    std::thread::sleep(std::time::Duration::from_millis(150));
    let hidden = g.grab(Rect::new(132, 132, 1, 1)).expect("grab").pixel(0, 0);
    // SAFETY: our own window.
    unsafe {
        let _ = DestroyWindow(h);
    }
    println!("visible {visible:?} excluded {hidden:?}");
    assert_eq!(visible, [0, 0, 255, 255], "the red test window is visible to BitBlt before exclusion");
    assert_ne!(hidden, [0, 0, 255, 255], "excluded window must not be captured");
}

unsafe extern "system" fn def_proc(
    h: windows::Win32::Foundation::HWND,
    m: u32,
    w: windows::Win32::Foundation::WPARAM,
    l: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l)
}

/// Open every tool for real through the public API, drive a few interactions, pump
/// messages, then close everything (re-entrancy, hook lifetime, cleanup).
#[test]
#[ignore]
fn live_smoke() {
    use crate::app::ScreenTool;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
    // SAFETY: plain call.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    crate::settings::set_dir_override(std::env::temp_dir().join("snapforge-tools-smoke"));
    let pump = |ms: u64| {
        let start = std::time::Instant::now();
        let mut msg = MSG::default();
        while start.elapsed() < std::time::Duration::from_millis(ms) {
            // SAFETY: standard message pumping on this thread.
            unsafe {
                while start.elapsed() < std::time::Duration::from_millis(ms)
                    && PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool()
                {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    let all =
        [ScreenTool::Crosshair, ScreenTool::Focus, ScreenTool::Ruler, ScreenTool::Magnifier, ScreenTool::ColorPicker];
    for t in all {
        super::open(t);
        pump(250);
        assert!(super::is_open(t), "{t:?} should be open");
    }
    assert!(super::hook::is_running(), "hook runs while crosshair/focus are open");
    super::focus::on_wheel(2);
    super::focus::on_drag(true, Point::new(100, 100));
    super::focus::on_drag(false, Point::new(500, 400));
    super::crosshair::on_move(Point::new(300, 300));
    pump(150);
    super::close(ScreenTool::ColorPicker);
    super::finetune::open(Rgb::new(10, 20, 30), Point::new(300, 300));
    super::finetune::set_adjusted(Rgb::new(200, 100, 50));
    pump(150);
    println!("tool windows: {}", super::tool_hwnds().len());
    super::close(ScreenTool::Crosshair);
    super::close(ScreenTool::Focus);
    pump(100);
    assert!(!super::hook::is_running(), "hook stops with the last click-through tool");
    // Magnifier: close, then reopen as a pane.
    super::toggle(ScreenTool::Magnifier);
    crate::settings::update(|s| s.tools.magnifier_mode = crate::settings::tools::MagnifierMode::Pane);
    super::open(ScreenTool::Magnifier);
    pump(250);
    super::close_all();
    pump(50);
    for t in all {
        assert!(!super::is_open(t), "{t:?} should be closed");
    }
    assert!(super::tool_hwnds().is_empty());
}
