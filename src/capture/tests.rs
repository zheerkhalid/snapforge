//! Capture tests. Display-dependent tests return early (pass) when no monitor is present,
//! e.g. on a headless CI agent or in a non-interactive session.

use super::*;
use crate::common::win::{hwnd_to_isize, monitors, wide};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW, RegisterClassW,
    SetForegroundWindow, TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

/// Make the test process per-monitor aware (like the app) and report whether a display exists.
fn display_available() -> bool {
    // SAFETY: plain call; fails harmlessly if already set.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let ok = !monitors().is_empty() && !win::virtual_screen().is_empty();
    if !ok {
        eprintln!("no display: skipping display-dependent capture test");
    }
    ok
}

fn primary() -> Rect {
    monitors().iter().find(|m| m.primary).map(|m| m.rect).unwrap_or_else(win::virtual_screen)
}

fn opts(engine: CaptureEngine) -> GrabOptions {
    GrabOptions { engine, include_cursor: false }
}

unsafe extern "system" fn test_wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

const RED: u32 = 0x0000_00FF; // COLORREF is 0x00BBGGRR
const BLUE: u32 = 0x00FF_0000;

/// A solid-colour top-level window, destroyed on drop. Pumps messages while alive.
struct TestWindow {
    hwnd: HWND,
    brush: HBRUSH,
}

impl TestWindow {
    fn new(x: i32, y: i32, w: i32, h: i32) -> Option<Self> {
        Self::colored(x, y, w, h, RED)
    }

    fn colored(x: i32, y: i32, w: i32, h: i32, color: u32) -> Option<Self> {
        let class = wide(if color == RED { "SnapForgeCaptureTest" } else { "SnapForgeCaptureTest2" });
        let title = wide("SnapForge capture test");
        // SAFETY: standard window creation with a static wndproc; resources freed in Drop.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color));
            let inst = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(test_wndproc),
                hInstance: inst.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                hbrBackground: brush,
                ..Default::default()
            };
            RegisterClassW(&wc); // may already exist from another test
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                x,
                y,
                w,
                h,
                None,
                None,
                inst,
                None,
            )
            .ok()?;
            let _ = SetForegroundWindow(hwnd);
            let tw = Self { hwnd, brush };
            tw.pump(Duration::from_millis(300));
            Some(tw)
        }
    }

    fn pump(&self, d: Duration) {
        let end = Instant::now() + d;
        let mut msg = MSG::default();
        while Instant::now() < end {
            // SAFETY: standard message pump for windows owned by this thread.
            unsafe {
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: destroying our own window and brush.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.brush);
        }
    }
}

#[test]
fn window_crop_math() {
    let wr = Rect::new(100, 100, 816, 639);
    let b = Rect::new(107, 100, 802, 632);
    assert_eq!(window_crop(816, 639, wr, b), Rect::new(7, 0, 802, 632));
    assert_eq!(window_crop(802, 632, wr, b), Rect::new(0, 0, 802, 632));
}

#[test]
fn gdi_small_rect_is_opaque_and_sized() {
    if !display_available() {
        return;
    }
    let p = primary();
    let r = Rect::new(p.x + 10, p.y + 10, 37, 23);
    let img = grab_rect(r, &opts(CaptureEngine::Gdi)).expect("gdi grab");
    assert_eq!((img.width, img.height), (37, 23));
    assert!(!img.has_transparency());
    let with_cursor = grab_rect(r, &GrabOptions { engine: CaptureEngine::Gdi, include_cursor: true }).expect("gdi");
    assert!(!with_cursor.has_transparency());
}

#[test]
fn auto_grabs_a_monitor() {
    if !display_available() {
        return;
    }
    let p = primary();
    let img = grab_rect(p, &opts(CaptureEngine::Auto)).expect("auto grab");
    assert_eq!((img.width, img.height), (p.w as u32, p.h as u32));
    assert!(!img.has_transparency());
    // The desktop is never a single flat colour in a real session.
    let first = img.pixel(0, 0);
    assert!(img.data.chunks_exact(4).any(|px| px != first), "monitor capture is uniform");
}

#[test]
fn backends_agree_on_a_static_area() {
    if !display_available() {
        return;
    }
    let _w = TestWindow::new(200, 200, 400, 300);
    let p = primary();
    let r = Rect::new(p.x, p.y, p.w.min(640), p.h.min(480));
    let gdi = grab_with(Backend::Gdi, r, false).expect("gdi");
    for b in [Backend::Dxgi, Backend::Wgc] {
        match grab_with(b, r, false) {
            Ok(img) => {
                assert_eq!((img.width, img.height), (gdi.width, gdi.height), "{}", b.name());
                assert!(!img.has_transparency(), "{}", b.name());
            }
            Err(e) => eprintln!("{} unavailable here: {e}", b.name()),
        }
    }
}

#[test]
fn cross_monitor_rect_composes() {
    if !display_available() {
        return;
    }
    let vs = win::virtual_screen();
    let img = grab_rect(vs, &opts(CaptureEngine::Auto)).expect("virtual screen");
    assert_eq!((img.width, img.height), (vs.w as u32, vs.h as u32));
    // A rect that sticks out of the virtual screen is still the requested size.
    let r = Rect::new(vs.x - 10, vs.y - 10, 50, 50);
    for b in [Backend::Gdi, Backend::Dxgi] {
        if let Ok(img) = grab_with(b, r, false) {
            assert_eq!((img.width, img.height), (50, 50));
        }
    }
}

#[test]
fn snapshot_excludes_given_windows() {
    if !display_available() {
        return;
    }
    let Some(tw) = TestWindow::new(150, 150, 300, 200) else { return };
    let h = hwnd_to_isize(tw.hwnd);
    let all = snapshot_windows(&[]);
    let mine = all.iter().find(|e| e.hwnd == h).expect("test window in snapshot");
    assert_eq!(mine.class, "SnapForgeCaptureTest");
    assert_eq!(mine.title, "SnapForge capture test");
    assert!(!mine.rect.is_empty());
    assert!(all.iter().all(|e| e.class != "Progman" && e.class != "WorkerW"));
    let without = snapshot_windows(&[h]);
    assert!(without.iter().all(|e| e.hwnd != h));
}

#[test]
fn window_grab_matches_bounds_even_when_covered() {
    if !display_available() {
        return;
    }
    let Some(tw) = TestWindow::new(120, 120, 420, 320) else { return };
    let h = hwnd_to_isize(tw.hwnd);
    // Cover it completely with a blue window.
    let _cover = TestWindow::colored(80, 80, 520, 420, BLUE);
    tw.pump(Duration::from_millis(100));
    let (img, bounds) = grab_window(h, &opts(CaptureEngine::Auto)).expect("window grab");
    assert_eq!((img.width as i32, img.height as i32), (bounds.w, bounds.h));
    assert!(!img.has_transparency());
    // The client area is red only if WGC captured the window itself, not the screen.
    let px = img.pixel(img.width / 2, img.height / 2);
    assert!(px[2] > 200 && px[1] < 60 && px[0] < 60, "expected red client area, got {px:?}");
    let chain = element_chain_at(h, bounds.center());
    assert_eq!(chain.first(), Some(&bounds));
    drop(_cover);
    drop(tw);
    assert!(matches!(grab_window(h, &opts(CaptureEngine::Auto)), Err(CaptureError::WindowGone)));
}

#[test]
fn empty_rect_is_an_error() {
    assert!(matches!(grab_rect(Rect::new(0, 0, 0, 10), &opts(CaptureEngine::Gdi)), Err(CaptureError::Empty)));
}

/// Manual check: `cargo test --release capture::tests::inspect -- --ignored --nocapture`.
/// Writes PNGs of every backend to %TEMP%\snapforge-capture and prints latencies.
#[test]
#[ignore]
fn inspect() {
    if !display_available() {
        return;
    }
    let dir = std::env::temp_dir().join("snapforge-capture");
    let _ = std::fs::create_dir_all(&dir);
    let save = |name: &str, img: &Image| {
        let path = dir.join(format!("{name}.png"));
        let rgba = img.to_rgba_straight();
        if let Err(e) = image::save_buffer(&path, &rgba, img.width, img.height, image::ExtendedColorType::Rgba8) {
            eprintln!("save {name}: {e}");
        }
    };
    for o in d3d::outputs() {
        eprintln!("output {:?} hdr={} rotated={} dev={}", o.rect, o.hdr, o.rotated, o.device);
    }
    let p = primary();
    eprintln!("primary {p:?} virtual {:?}", win::virtual_screen());
    let ws = || {
        use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows::Win32::System::Threading::GetCurrentProcess;
        let mut c = PROCESS_MEMORY_COUNTERS::default();
        // SAFETY: valid out pointer of the stated size.
        unsafe {
            let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c, std::mem::size_of_val(&c) as u32);
        }
        format!(
            "ws {:.1} MB, private {:.1} MB",
            c.WorkingSetSize as f64 / 1_048_576.0,
            c.PagefileUsage as f64 / 1_048_576.0
        )
    };
    eprintln!("memory at start: {}", ws());
    let _ = grab_with(Backend::Gdi, p, false);
    eprintln!("memory after one GDI grab: {}", ws());
    for o in d3d::outputs() {
        for b in bench_engines(o.rect) {
            eprintln!("bench {:?} {:<10} {:?}", o.rect, b.0, b.1);
        }
    }
    let vs = win::virtual_screen();
    for b in bench_engines(vs) {
        eprintln!("bench virtual {:<10} {:?}", b.0, b.1);
    }
    // Auto policy as the overlay sees it: cold (GDI + background DXGI warm-up), then warm.
    release_idle_resources();
    std::thread::sleep(Duration::from_millis(300));
    eprintln!("working set after release: {}", ws());
    for i in 0..4 {
        let t = Instant::now();
        let r = grab_rect(p, &opts(CaptureEngine::Auto));
        eprintln!("auto #{i}: {:?} ok={} warm={}", t.elapsed(), r.is_ok(), worker::dxgi_is_warm());
        std::thread::sleep(Duration::from_millis(250));
    }
    let t = Instant::now();
    let _ = grab_rect(vs, &opts(CaptureEngine::Auto));
    eprintln!("auto virtual warm: {:?}", t.elapsed());
    eprintln!("working set warm: {}", ws());
    release_idle_resources();
    std::thread::sleep(Duration::from_millis(300));
    eprintln!("working set after release: {}", ws());
    let t = Instant::now();
    let outs = d3d::outputs();
    eprintln!("outputs() cached: {:?} ({} outputs)", t.elapsed(), outs.len());
    d3d::invalidate_outputs();
    let t = Instant::now();
    let _ = d3d::outputs();
    eprintln!("outputs() cold: {:?}", t.elapsed());
    for b in [Backend::Gdi, Backend::Dxgi, Backend::Wgc] {
        match grab_with(b, p, true) {
            Ok(img) => save(b.name(), &img),
            Err(e) => eprintln!("{}: {e}", b.name()),
        }
    }
    let tw = TestWindow::new(120, 120, 420, 320);
    let cover = TestWindow::colored(80, 80, 520, 420, BLUE);
    if let Some(tw) = &tw {
        tw.pump(Duration::from_millis(200));
        let h = hwnd_to_isize(tw.hwnd);
        let t = Instant::now();
        match grab_window(h, &opts(CaptureEngine::Auto)) {
            Ok((img, b)) => {
                eprintln!("window {b:?} {}x{} in {:?}", img.width, img.height, t.elapsed());
                save("window", &img);
            }
            Err(e) => eprintln!("window: {e}"),
        }
    }
    drop(cover);
    drop(tw);
    let snap = snapshot_windows(&[]);
    let t = Instant::now();
    let snap2 = snapshot_windows(&[]);
    eprintln!("snapshot: {} windows in {:?}", snap2.len(), t.elapsed());
    for e in snap.iter().take(25) {
        eprintln!("  {:>10x} {:?} [{}] {}", e.hwnd, e.rect, e.class, e.title);
    }
    // Element chain on the first normal app window, at its centre, a few times.
    if let Some(w) = snap.iter().find(|e| e.class != "Shell_TrayWnd" && !e.title.is_empty() && e.rect.w > 300) {
        let c = w.rect.center();
        for i in 0..4 {
            let t = Instant::now();
            let chain = element_chain_at(w.hwnd, c);
            eprintln!("chain #{i} for [{}] {}: {:?} -> {chain:?}", w.class, w.title, t.elapsed());
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}
