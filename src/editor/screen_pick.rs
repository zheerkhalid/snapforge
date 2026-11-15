//! Pick a colour anywhere on screen: the virtual screen is frozen (GDI grab) and shown in a
//! topmost popup covering every monitor, with a crosshair cursor and a magnified loupe that
//! shows the hex value. Left click picks, Esc or right click cancels.
//!
//! [`pick_screen_color`] runs a modal message loop on the calling thread (like the file
//! dialogs), so the editor simply does not repaint while picking.

use crate::common::win::{hwnd_from_isize, pcwstr, wide};
use crate::common::{Image, Point, Rect};
use crate::draw::Color;
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, FrameRect, InvalidateRect, SelectObject, SetBkMode, SetStretchBltMode,
    SetTextColor, StretchBlt, TextOutW, BITMAPINFO, BITMAPINFOHEADER, COLORONCOLOR, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, IsWindow, LoadCursorW,
    RegisterClassW, SetForegroundWindow, ShowWindow, TranslateMessage, HCURSOR, IDC_CROSS, MSG, SW_SHOW, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

const CLASS: &str = "SnapForge.ScreenColorPick";
/// Source pixels shown in the loupe (odd, so the picked pixel is in the centre).
const LOUPE_SRC: i32 = 15;
/// Magnification of the loupe.
const LOUPE_ZOOM: i32 = 8;
const LABEL_H: i32 = 22;

struct State {
    img: Image,
    /// Memory DC holding a DIB section with the frozen screen (top-left origin).
    src: Option<SourceDc>,
    cursor: Point,
    done: bool,
    picked: Option<Color>,
}

struct SourceDc {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
}

impl SourceDc {
    fn new(img: &Image) -> Option<Self> {
        let bmi = bitmap_info(img.width as i32, img.height as i32);
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: the DIB section is sized for the image; we copy exactly its bytes into it.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let bmp = match CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                _ => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            std::ptr::copy_nonoverlapping(img.data.as_ptr(), bits as *mut u8, img.data.len());
            let old = SelectObject(dc, bmp);
            Some(Self { dc, bmp, old })
        }
    }
}

impl Drop for SourceDc {
    fn drop(&mut self) {
        // SAFETY: objects created in `new`, released once.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static REGISTERED: RefCell<bool> = const { RefCell::new(false) };
}

/// Straight RGBA of a premultiplied BGRA pixel.
fn straight(p: [u8; 4]) -> Color {
    let a = p[3] as u32;
    if a == 0 {
        return Color::rgba(0, 0, 0, 0);
    }
    let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
    Color::rgba(un(p[2]), un(p[1]), un(p[0]), p[3])
}

/// Where the loupe (plus label) goes for a cursor at `c` in a `w`×`h` client area: below
/// right of the cursor, flipped to the other side near the edges.
pub fn loupe_rect(c: Point, w: i32, h: i32) -> Rect {
    let side = LOUPE_SRC * LOUPE_ZOOM;
    let (lw, lh) = (side, side + LABEL_H);
    let gap = 24;
    let x = if c.x + gap + lw <= w { c.x + gap } else { c.x - gap - lw };
    let y = if c.y + gap + lh <= h { c.y + gap } else { c.y - gap - lh };
    Rect::new(x.max(0), y.max(0), lw, lh)
}

/// Freeze the screen and let the user click a pixel. `None` when cancelled or when the
/// screen cannot be captured.
pub fn pick_screen_color(owner: isize) -> Option<Color> {
    let vs = crate::common::win::virtual_screen();
    let img = match crate::capture::grab_with(crate::capture::Backend::Gdi, vs, false) {
        Ok(i) if !i.is_empty() => i,
        Ok(_) => return None,
        Err(e) => {
            log::warn!("screen pick: capture failed: {e:?}");
            return None;
        }
    };
    if !register_class() {
        return None;
    }
    let cursor = crate::common::win::cursor_pos();
    let cursor = Point::new(cursor.x - vs.x, cursor.y - vs.y);
    let Some(src) = SourceDc::new(&img) else {
        log::warn!("screen pick: cannot create the source bitmap");
        return None;
    };
    STATE.with(|s| *s.borrow_mut() = Some(State { img, src: Some(src), cursor, done: false, picked: None }));
    let class = wide(CLASS);
    let title = wide("SnapForge color picker");
    let owner_h = hwnd_from_isize(owner);
    // SAFETY: the class is registered on this thread; strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            pcwstr(&class),
            pcwstr(&title),
            WS_POPUP,
            vs.x,
            vs.y,
            vs.w,
            vs.h,
            owner_h,
            None,
            hinstance(),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            log::warn!("screen pick: cannot create the window: {e}");
            STATE.with(|s| *s.borrow_mut() = None);
            return None;
        }
    };
    // SAFETY: plain calls on our own windows; the loop ends when `done` is set or the window
    // disappears.
    unsafe {
        if owner != 0 {
            let _ = EnableWindow(owner_h, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let mut msg = MSG::default();
        loop {
            let done = STATE.with(|s| s.borrow().as_ref().map_or(true, |st| st.done));
            if done || !IsWindow(hwnd).as_bool() {
                break;
            }
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 == 0 {
                // WM_QUIT: put it back for the outer loop.
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if r.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if owner != 0 {
            let _ = EnableWindow(owner_h, true);
        }
        let _ = DestroyWindow(hwnd);
        if owner != 0 {
            let _ = SetForegroundWindow(owner_h);
        }
    }
    STATE.with(|s| s.borrow_mut().take()).and_then(|s| s.picked)
}

fn hinstance() -> HINSTANCE {
    // SAFETY: plain query for our own module.
    unsafe { GetModuleHandleW(None).map(|m| HINSTANCE(m.0)).unwrap_or_default() }
}

fn register_class() -> bool {
    if REGISTERED.with(|r| *r.borrow()) {
        return true;
    }
    let name = wide(CLASS);
    // SAFETY: valid class name kept alive during the call (the system copies it).
    let ok = unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance(),
            lpszClassName: pcwstr(&name),
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or(HCURSOR::default()),
            ..Default::default()
        };
        RegisterClassW(&wc) != 0
    };
    if ok {
        REGISTERED.with(|r| *r.borrow_mut() = true);
    } else {
        log::warn!("screen pick: RegisterClassW failed");
    }
    ok
}

fn lparam_point(lp: LPARAM) -> Point {
    let v = lp.0 as u32;
    Point::new((v & 0xFFFF) as i16 as i32, ((v >> 16) & 0xFFFF) as i16 as i32)
}

fn win_rect(r: Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn finish(picked: Option<Color>) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.done = true;
            st.picked = picked;
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let p = lparam_point(lp);
            let dirty = STATE.with(|s| {
                let mut b = s.borrow_mut();
                let st = b.as_mut()?;
                let (w, h) = (st.img.width as i32, st.img.height as i32);
                let old = loupe_rect(st.cursor, w, h);
                st.cursor = p;
                Some((old, loupe_rect(p, w, h)))
            });
            if let Some((a, b)) = dirty {
                let _ = InvalidateRect(hwnd, Some(&win_rect(a)), false);
                let _ = InvalidateRect(hwnd, Some(&win_rect(b)), false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let p = lparam_point(lp);
            let c = STATE.with(|s| {
                let b = s.borrow();
                let st = b.as_ref()?;
                let inside = p.x >= 0 && p.y >= 0 && (p.x as u32) < st.img.width && (p.y as u32) < st.img.height;
                inside.then(|| straight(st.img.pixel(p.x as u32, p.y as u32)))
            });
            finish(c);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            finish(None);
            LRESULT(0)
        }
        WM_KEYDOWN if wp.0 as u16 == VK_ESCAPE.0 => {
            finish(None);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn bitmap_info(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard BeginPaint/EndPaint pair; GDI objects are created and released here.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                paint_area(hdc, st, ps.rcPaint);
            }
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Paint `rc` (client coordinates) through a memory DC so the loupe never flickers.
unsafe fn paint_area(hdc: HDC, st: &State, rc: RECT) {
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, w, h);
    let old = SelectObject(mem, bmp);
    let (iw, ih) = (st.img.width as i32, st.img.height as i32);
    let Some(src) = st.src.as_ref() else { return };
    // Frozen screen for the dirty area.
    let _ = BitBlt(mem, 0, 0, w, h, src.dc, rc.left, rc.top, SRCCOPY);
    // Loupe.
    let lr = loupe_rect(st.cursor, iw, ih);
    let local = |r: Rect| Rect::new(r.x - rc.left, r.y - rc.top, r.w, r.h);
    let side = LOUPE_SRC * LOUPE_ZOOM;
    let zoom_r = local(Rect::new(lr.x, lr.y, side, side));
    let half = LOUPE_SRC / 2;
    SetStretchBltMode(mem, COLORONCOLOR);
    let (sx, sy) = (st.cursor.x - half, st.cursor.y - half);
    let _ = StretchBlt(mem, zoom_r.x, zoom_r.y, side, side, src.dc, sx, sy, LOUPE_SRC, LOUPE_SRC, SRCCOPY);
    let frame = |r: Rect, c: (u8, u8, u8)| {
        let br = CreateSolidBrush(COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16));
        FrameRect(mem, &win_rect(r), br);
        let _ = DeleteObject(br);
    };
    // Centre pixel marker.
    let cell = Rect::new(zoom_r.x + half * LOUPE_ZOOM, zoom_r.y + half * LOUPE_ZOOM, LOUPE_ZOOM, LOUPE_ZOOM);
    frame(cell.inflate(1), (255, 255, 255));
    frame(cell, (0, 0, 0));
    frame(zoom_r, (0x4F, 0x6B, 0xFF));
    // Label with the hex value.
    let label = local(Rect::new(lr.x, lr.y + side, side, LABEL_H));
    let bg = CreateSolidBrush(COLORREF(0x30_2826));
    FillRect(mem, &win_rect(label), bg);
    let _ = DeleteObject(bg);
    let inside = st.cursor.x >= 0 && st.cursor.y >= 0 && st.cursor.x < iw && st.cursor.y < ih;
    if inside {
        let c = straight(st.img.pixel(st.cursor.x as u32, st.cursor.y as u32));
        let sw = Rect::new(label.x + 5, label.y + 5, LABEL_H - 10, LABEL_H - 10);
        let swb = CreateSolidBrush(COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16));
        FillRect(mem, &win_rect(sw), swb);
        let _ = DeleteObject(swb);
        let text = wide(&format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b));
        SetBkMode(mem, TRANSPARENT);
        SetTextColor(mem, COLORREF(0x00FF_FFFF));
        let n = text.len().saturating_sub(1);
        let _ = TextOutW(mem, sw.right() + 6, label.y + 3, &text[..n]);
    }
    let _ = BitBlt(hdc, rc.left, rc.top, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loupe_stays_on_screen() {
        let (w, h) = (1920, 1080);
        let r = loupe_rect(Point::new(10, 10), w, h);
        assert!(r.x > 10 && r.y > 10);
        let r = loupe_rect(Point::new(1910, 1070), w, h);
        assert!(r.right() < 1910 && r.bottom() < 1070);
        assert!(r.x >= 0 && r.y >= 0);
    }

    #[test]
    fn straight_alpha_conversion() {
        assert_eq!(straight([10, 20, 30, 255]), Color::rgba(30, 20, 10, 255));
        assert_eq!(straight([0, 0, 0, 0]), Color::rgba(0, 0, 0, 0));
        assert_eq!(straight([0, 0, 64, 128]), Color::rgba(128, 0, 0, 128));
    }
}
