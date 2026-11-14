//! The reusable overlay window and its nested modal loop.

use super::logic::{self, ChainFn, Config, Grab, Handle, Input, Key, Mods, Monitor, Phase, State};
use super::paint::Canvas;
use super::scene::{self, FpKey, Scene};
use super::{Frozen, OverlayMode, Selection, BENCH_AUTOCANCEL, LAST_FIRST_PAINT_QPC};
use crate::common::win::{hwnd_from_isize, hwnd_to_isize, pcwstr, rect_from_win, rect_to_win, wide};
use crate::common::{Point, Rect};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, DeleteObject, EndPaint, GdiFlush, GetRegionData, GetUpdateRgn,
    InvalidateRect, InvalidateRgn, ScreenToClient, UpdateWindow, HRGN, PAINTSTRUCT, RGNDATA, RGNDATAHEADER, RGN_OR,
    RGN_XOR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F4, VK_LEFT,
    VK_MENU, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChildWindowFromPointEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowRect, IsWindow,
    LoadCursorW, PostQuitMessage, RegisterClassExW, SetCursor, SetCursorPos, SetForegroundWindow, SetWindowPos,
    ShowWindow, TranslateMessage, CS_DBLCLKS, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT, HTCLIENT, HWND_TOPMOST,
    IDC_CROSS, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MA_ACTIVATE, MSG, SWP_SHOWWINDOW,
    SW_HIDE, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS: &str = "SnapForge.Overlay";

struct Session {
    hwnd: HWND,
    state: State,
    canvas: Canvas,
    scene: Scene,
    footprint: HashSet<FpKey>,
    done: Option<Option<Selection>>,
    first_paint_done: bool,
    was_active: bool,
    wheel_accum: i32,
    start_qpc: i64,
}

thread_local! {
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
    static CANVAS_CACHE: RefCell<Option<Canvas>> = const { RefCell::new(None) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// The overlay window handle (0 before first use), so captures can exclude it.
pub fn overlay_hwnd() -> isize {
    HWND_CELL.with(|c| c.get())
}

/// Drop the cached screen-sized bitmaps (call when the app goes idle).
pub fn release_idle_resources() {
    let busy = SESSION.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true));
    if !busy {
        CANVAS_CACHE.with(|c| c.borrow_mut().take());
    }
}

fn ensure_window() -> Option<HWND> {
    let existing = hwnd_from_isize(overlay_hwnd());
    // SAFETY: IsWindow accepts any value.
    if !existing.is_invalid() && unsafe { IsWindow(existing) }.as_bool() {
        return Some(existing);
    }
    let class = wide(CLASS);
    // SAFETY: standard class registration (a repeat registration fails harmlessly) and
    // window creation with a static wndproc.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let title = wide("SnapForge Overlay");
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            pcwstr(&class),
            pcwstr(&title),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinst,
            None,
        )
    };
    match hwnd {
        Ok(h) => {
            HWND_CELL.with(|c| c.set(hwnd_to_isize(h)));
            Some(h)
        }
        Err(e) => {
            log::error!("overlay: CreateWindowEx failed: {e}");
            None
        }
    }
}

fn set_cloak(hwnd: HWND, cloak: bool) {
    let v: BOOL = cloak.into();
    // SAFETY: valid pointer and size for a BOOL attribute.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const BOOL as *const c_void, 4);
    }
}

/// Child-window chain under `p`: the window itself, then nested child controls.
/// Consecutive entries with the same rect collapse to the innermost window.
pub fn child_chain(top: isize, rect: Rect, p: Point) -> Vec<(isize, Rect)> {
    let mut v = vec![(top, rect)];
    let mut cur = hwnd_from_isize(top);
    for _ in 0..32 {
        let mut pt = POINT { x: p.x, y: p.y };
        // SAFETY: plain queries on window handles (stale handles just fail).
        let child = unsafe {
            if !ScreenToClient(cur, &mut pt).as_bool() {
                break;
            }
            ChildWindowFromPointEx(cur, pt, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT)
        };
        if child.is_invalid() || child == cur {
            break;
        }
        let mut r = RECT::default();
        // SAFETY: valid out pointer.
        if unsafe { GetWindowRect(child, &mut r) }.is_err() {
            break;
        }
        let r = rect_from_win(&r);
        match v.last_mut() {
            Some(last) if last.1 == r => last.0 = hwnd_to_isize(child),
            _ => v.push((hwnd_to_isize(child), r)),
        }
        cur = child;
    }
    v
}

fn chain_provider() -> ChainFn {
    Box::new(|mode, top, rect, p| {
        if mode == OverlayMode::WindowObject {
            let elements = crate::capture::element_chain_at(top, p);
            if !elements.is_empty() {
                return elements.into_iter().map(|r| (top, r)).collect();
            }
        }
        child_chain(top, rect, p)
    })
}

fn current_mods() -> Mods {
    // SAFETY: plain key state queries.
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

fn map_key(vk: u16) -> Key {
    match VIRTUAL_KEY(vk) {
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_TAB => Key::Tab,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_SPACE => Key::Space,
        _ if vk == b'Z' as u16 => Key::Z,
        _ => Key::Other,
    }
}

pub fn select(frozen: &Frozen, mode: OverlayMode) -> Option<Selection> {
    let t0 = crate::common::time::qpc_now();
    let busy = SESSION.with(|s| s.try_borrow().map(|g| g.is_some()).unwrap_or(true));
    if busy {
        log::warn!("overlay: select() re-entered while an overlay is open; ignoring");
        return None;
    }
    if frozen.image.is_empty() || frozen.origin.is_empty() {
        log::warn!("overlay: empty frozen image");
        return None;
    }
    let hwnd = ensure_window()?;
    let cfg = {
        let s = crate::settings::get();
        let c = &s.capture;
        Config {
            magnifier: c.show_magnifier,
            crosshair: c.show_crosshair,
            capture_on_release: c.capture_on_release,
            presets: c.fixed_presets.iter().map(|p| (p.w as i32, p.h as i32)).collect(),
            fixed_index: c.fixed_index,
        }
    };
    let initial_fixed = cfg.fixed_index;
    // Canvas origin follows the image so painting never reads outside the bitmaps.
    let origin = Rect::new(frozen.origin.x, frozen.origin.y, frozen.image.width as i32, frozen.image.height as i32);
    let cache = CANVAS_CACHE.with(|c| c.borrow_mut().take());
    let Some(mut canvas) = Canvas::load(cache, &frozen.image, origin) else {
        log::error!("overlay: could not allocate bitmaps for {}x{}", origin.w, origin.h);
        return None;
    };
    let t_loaded = crate::common::time::qpc_now();
    let monitors: Vec<Monitor> =
        crate::common::win::monitors().into_iter().map(|m| Monitor { rect: m.rect, dpi: m.dpi }).collect();
    let own = hwnd_to_isize(hwnd);
    let windows = frozen.windows.iter().filter(|w| w.hwnd != own).map(|w| (w.hwnd, w.rect)).collect();
    let cursor = crate::common::win::cursor_pos();
    let state = State::new(mode, origin, windows, monitors, cfg, cursor, chain_provider());
    let scene = scene::build(&state, &mut canvas);
    let footprint = scene.footprint();
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            hwnd,
            state,
            canvas,
            scene,
            footprint,
            done: None,
            first_paint_done: false,
            was_active: false,
            wheel_accum: 0,
            start_qpc: t0,
        })
    });

    // Show cloaked, paint synchronously, then uncloak: no stale or blank frame is ever
    // composed, and the first paint happens before the nested loop starts.
    set_cloak(hwnd, true);
    // SAFETY: our own window; no RefCell borrow is held while these send messages.
    unsafe {
        if let Err(e) = SetWindowPos(hwnd, HWND_TOPMOST, origin.x, origin.y, origin.w, origin.h, SWP_SHOWWINDOW) {
            log::warn!("overlay: SetWindowPos failed: {e}");
        }
        let _ = InvalidateRect(hwnd, None, false);
        let _ = UpdateWindow(hwnd);
    }
    set_cloak(hwnd, false);
    // SAFETY: as above. The app owns the hotkey that started this, so it may take focus.
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
    log::debug!(
        "overlay: {:?} {}x{} load {:.1} ms, shown {:.1} ms",
        mode,
        origin.w,
        origin.h,
        crate::common::time::qpc_ms(t0, t_loaded),
        crate::common::time::qpc_ms(t0, crate::common::time::qpc_now())
    );

    run_loop();

    // SAFETY: our own window; no borrow held.
    unsafe {
        let _ = ReleaseCapture();
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    let sess = SESSION.with(|s| s.borrow_mut().take());
    let sess = sess?;
    if sess.state.fixed_index != initial_fixed && !sess.state.cfg.presets.is_empty() {
        let i = sess.state.fixed_index;
        crate::settings::update(|s| s.capture.fixed_index = i);
    }
    CANVAS_CACHE.with(|c| *c.borrow_mut() = Some(sess.canvas));
    sess.done.flatten()
}

fn is_done() -> bool {
    SESSION.with(|s| s.try_borrow().map(|g| g.as_ref().map_or(true, |x| x.done.is_some())).unwrap_or(false))
}

fn set_done(sel: Option<Selection>) {
    SESSION.with(|s| {
        if let Ok(mut g) = s.try_borrow_mut() {
            if let Some(x) = g.as_mut() {
                x.done.get_or_insert(sel);
            }
        }
    });
}

fn run_loop() {
    let mut msg = MSG::default();
    while !is_done() {
        // SAFETY: standard message loop on the UI thread.
        let r = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) };
        if r.0 == 0 {
            // WM_QUIT: hand it back to the outer loop.
            // SAFETY: plain call.
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            set_done(None);
            break;
        }
        if r.0 == -1 {
            set_done(None);
            break;
        }
        // SAFETY: msg was filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

impl Session {
    fn handle(&mut self, input: Input) -> logic::Effects {
        let mut e = self.state.handle(input, current_mods());
        if let Some(f) = e.finish.take() {
            self.done.get_or_insert(f);
        }
        self.refresh();
        e
    }

    /// Rebuild the scene and invalidate only what changed.
    fn refresh(&mut self) {
        let scene = scene::build(&self.state, &mut self.canvas);
        let fp = scene.footprint();
        let o = self.canvas.origin;
        // SAFETY: region handles are created and deleted here; hwnd is our window.
        unsafe {
            if scene.bright != self.scene.bright {
                let a = region(&self.scene.bright, o);
                let b = region(&scene.bright, o);
                CombineRgn(a, a, b, RGN_XOR);
                let _ = InvalidateRgn(self.hwnd, a, false);
                let _ = DeleteObject(a);
                let _ = DeleteObject(b);
            }
            for k in fp.symmetric_difference(&self.footprint) {
                let r = Rect::new(k.0 - o.x, k.1 - o.y, k.2, k.3);
                let _ = InvalidateRect(self.hwnd, Some(&rect_to_win(&r)), false);
            }
        }
        self.scene = scene;
        self.footprint = fp;
    }

    fn paint(&mut self) {
        let hwnd = self.hwnd;
        let o = self.canvas.origin;
        // SAFETY: standard BeginPaint/EndPaint; region handle created and deleted here.
        unsafe {
            let rgn = CreateRectRgn(0, 0, 0, 0);
            GetUpdateRgn(hwnd, rgn, false);
            let rects = region_rects(rgn);
            let _ = DeleteObject(rgn);
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !self.first_paint_done {
                // First paint covers everything: draw straight to the window, no backbuffer.
                self.canvas.draw(hdc, Point::new(o.x, o.y), o, &self.scene);
                let _ = GdiFlush();
                let now = crate::common::time::qpc_now();
                LAST_FIRST_PAINT_QPC.store(now, Ordering::Release);
                self.first_paint_done = true;
                log::debug!("overlay: first paint after {:.1} ms", crate::common::time::qpc_ms(self.start_qpc, now));
                if BENCH_AUTOCANCEL.load(Ordering::Acquire) {
                    self.done.get_or_insert(None);
                }
            } else {
                let rects = if rects.is_empty() { vec![rect_from_win(&ps.rcPaint)] } else { rects };
                for r in rects {
                    self.canvas.paint_buffered(hdc, r.offset(o.x, o.y), &self.scene);
                }
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn cursor_shape(&self) -> windows::core::PCWSTR {
        if let Phase::Adjust { rect, drag } = &self.state.phase {
            let grab = match drag {
                Some(d) => Some(d.grab),
                None => {
                    let tol = crate::common::win::scale(6, self.state.monitor_at(self.state.cursor).dpi).max(3);
                    logic::hit_grab(rect, self.state.cursor, tol)
                }
            };
            return match grab {
                Some(Grab::Move) => IDC_SIZEALL,
                Some(Grab::Handle(Handle::TopLeft | Handle::BottomRight)) => IDC_SIZENWSE,
                Some(Grab::Handle(Handle::TopRight | Handle::BottomLeft)) => IDC_SIZENESW,
                Some(Grab::Handle(Handle::Top | Handle::Bottom)) => IDC_SIZENS,
                Some(Grab::Handle(Handle::Left | Handle::Right)) => IDC_SIZEWE,
                None => IDC_CROSS,
            };
        }
        IDC_CROSS
    }
}

/// GDI region (client coords) covering `rects` (virtual coords).
///
/// # Safety
/// The caller must delete the returned region.
unsafe fn region(rects: &[Rect], o: Rect) -> HRGN {
    let acc = CreateRectRgn(0, 0, 0, 0);
    for r in rects {
        let one = CreateRectRgn(r.x - o.x, r.y - o.y, r.right() - o.x, r.bottom() - o.y);
        CombineRgn(acc, acc, one, RGN_OR);
        let _ = DeleteObject(one);
    }
    acc
}

/// Rectangles making up a region (bounding box only when there are very many).
///
/// # Safety
/// `rgn` must be a valid region handle.
unsafe fn region_rects(rgn: HRGN) -> Vec<Rect> {
    let size = GetRegionData(rgn, 0, None) as usize;
    if size < std::mem::size_of::<RGNDATAHEADER>() {
        return Vec::new();
    }
    // u64 storage keeps the header and RECTs suitably aligned.
    let mut buf = vec![0u64; size.div_ceil(8)];
    let data = buf.as_mut_ptr() as *mut RGNDATA;
    if GetRegionData(rgn, size as u32, Some(data)) == 0 {
        return Vec::new();
    }
    let hdr = &(*data).rdh;
    if hdr.nCount > 256 {
        return vec![rect_from_win(&hdr.rcBound)];
    }
    let first = (buf.as_ptr() as *const u8).add(hdr.dwSize as usize) as *const RECT;
    (0..hdr.nCount as usize).map(|i| rect_from_win(&*first.add(i))).filter(|r| !r.is_empty()).collect()
}

fn point_from_lparam(lp: LPARAM) -> Point {
    Point::new((lp.0 & 0xFFFF) as u16 as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
}

/// Run `f` on the session if it is not already borrowed (re-entrant messages are ignored).
fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| {
        let mut g = s.try_borrow_mut().ok()?;
        g.as_mut().map(f)
    })
}

fn feed(input: Input) {
    let Some(e) = with_session(|s| s.handle(input)) else {
        return;
    };
    if let Some(p) = e.cursor {
        // SAFETY: plain call (physical coordinates; the process is per-monitor aware).
        unsafe {
            let _ = SetCursorPos(p.x, p.y);
        }
    }
}

fn client_to_virtual(lp: LPARAM) -> Point {
    let p = point_from_lparam(lp);
    let o = with_session(|s| s.canvas.origin).unwrap_or_default();
    Point::new(p.x + o.x, p.y + o.y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            if with_session(|s| s.paint()).is_none() {
                // No session (hidden window) or re-entrant: just validate.
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let shape = with_session(|s| s.cursor_shape()).unwrap_or(IDC_CROSS);
            if let Ok(c) = LoadCursorW(None, shape) {
                SetCursor(c);
            }
            LRESULT(1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        WM_MOUSEMOVE => {
            feed(Input::Move(client_to_virtual(lp)));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            SetCapture(hwnd);
            let p = client_to_virtual(lp);
            feed(if msg == WM_LBUTTONDOWN { Input::LDown(p) } else { Input::LDouble(p) });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let p = client_to_virtual(lp);
            let _ = ReleaseCapture();
            feed(Input::LUp(p));
            LRESULT(0)
        }
        WM_RBUTTONDOWN => LRESULT(0),
        WM_RBUTTONUP => {
            let _ = ReleaseCapture();
            feed(Input::RUp(client_to_virtual(lp)));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            let notches = with_session(|s| {
                s.wheel_accum += delta;
                let n = s.wheel_accum / 120;
                s.wheel_accum -= n * 120;
                n
            })
            .unwrap_or(0);
            if notches != 0 {
                feed(Input::Wheel(notches));
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            if msg == WM_SYSKEYDOWN && VIRTUAL_KEY(vk) == VK_F4 {
                set_done(None);
            } else {
                feed(Input::Key(map_key(vk)));
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let inactive = (wp.0 & 0xFFFF) as u32 == WA_INACTIVE;
            let lost = with_session(|s| {
                if !inactive {
                    s.was_active = true;
                    false
                } else {
                    s.was_active && s.done.is_none()
                }
            })
            .unwrap_or(false);
            if lost {
                // Another window took the foreground (Alt+Tab, Win key): cancel.
                set_done(None);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            set_done(None);
            LRESULT(0)
        }
        // Never resize for DPI changes: the overlay always covers the virtual screen.
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Test/bench helpers: build a session-less scene render of `mode` after `inputs`.
#[cfg(test)]
pub(crate) fn render_offscreen(
    frozen: &Frozen,
    mode: OverlayMode,
    cfg: Config,
    monitors: Vec<Monitor>,
    cursor: Point,
    inputs: &[(Input, Mods)],
) -> Option<crate::common::Image> {
    let mut canvas = Canvas::load(None, &frozen.image, frozen.origin)?;
    let windows = frozen.windows.iter().map(|w| (w.hwnd, w.rect)).collect();
    let chain: ChainFn = Box::new(|_, h, r, _| vec![(h, r), (h, Rect::new(r.x + 20, r.y + 60, r.w - 40, r.h / 2))]);
    let mut st = State::new(mode, frozen.origin, windows, monitors, cfg, cursor, chain);
    for (i, m) in inputs {
        st.handle(*i, *m);
    }
    let sc = scene::build(&st, &mut canvas);
    canvas.render_image(&sc)
}
