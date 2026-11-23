//! Capture Text result window (native Win32, UI thread): editable text, language
//! dropdown (re-runs OCR), Copy All / Copy Selected / Close and a "Keep line breaks" toggle.
//!
//! Recognition runs on a worker thread. It stores its outcome in a shared slot and posts
//! `WM_OCR_DONE`; a stale outcome (older generation, or a closed window) is dropped.

use super::{OcrError, OcrResult};
use crate::common::win::{self, hwnd_from_isize, hwnd_to_isize, pcwstr, wide};
use crate::common::{Image, Rect};
use crate::i18n::{t, tf};
use crate::output::clipboard;
use crate::settings;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetSysColorBrush, SetBkMode, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW,
    DEFAULT_CHARSET, FW_NORMAL, HDC, HFONT, OUT_TT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{SetWindowTheme, BST_CHECKED, BST_UNCHECKED, EM_GETSEL, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, IsIconic, IsZoomed, LoadCursorW, LoadIconW, PostMessageW, RegisterClassW,
    SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, BM_GETCHECK,
    BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CBN_SELCHANGE, CBS_DROPDOWNLIST,
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, ES_AUTOVSCROLL, ES_MULTILINE, ES_NOHIDESEL,
    ES_WANTRETURN, GWLP_USERDATA, HMENU, IDC_ARROW, MINMAXINFO, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DPICHANGED,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_NCDESTROY, WM_SETFONT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

const CLASS: &str = "SnapForge.OcrResult";
const WM_OCR_DONE: u32 = WM_APP + 40;

const ID_COMBO: u16 = 101;
const ID_EDIT: u16 = 102;
const ID_KEEP: u16 = 103;
const ID_COPY_ALL: u16 = 104;
const ID_COPY_SEL: u16 = 105;
const ID_CLOSE: u16 = 106;

/// A finished recognition handed from the worker to the window.
struct Done {
    generation: u64,
    result: Result<OcrResult, String>,
    no_language: bool,
    langs: Option<Vec<(String, String)>>,
    /// QR codes in the capture (decoded next to the text recognition).
    codes: Vec<crate::qr::Code>,
}

type Slot = Arc<Mutex<Option<Done>>>;

thread_local! {
    /// Open result windows (HWNDs as isize). UI thread only.
    static OPEN: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

// Static and edit control styles/messages (not exported by the `windows` crate's
// WindowsAndMessaging module in 0.58).
const SS_LEFT: u32 = 0x0000;
const SS_RIGHT: u32 = 0x0002;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;
const EM_SETLIMITTEXT: u32 = 0x00C5;

struct OcrWin {
    label: HWND,
    combo: HWND,
    status: HWND,
    edit: HWND,
    keep: HWND,
    copy_all: HWND,
    copy_sel: HWND,
    close: HWND,
    font: HFONT,
    edit_font: HFONT,
    dpi: u32,
    img: Arc<Image>,
    result: Option<OcrResult>,
    codes: Vec<crate::qr::Code>,
    langs: Vec<(String, String)>,
    slot: Slot,
    generation: u64,
    first_result: bool,
}

impl Drop for OcrWin {
    fn drop(&mut self) {
        // SAFETY: fonts we created; controls are already destroyed with the window.
        unsafe {
            let _ = DeleteObject(self.font);
            let _ = DeleteObject(self.edit_font);
        }
    }
}

fn state(hwnd: HWND) -> Option<&'static RefCell<OcrWin>> {
    // SAFETY: GWLP_USERDATA holds a `Box<RefCell<OcrWin>>` pointer from `open`, cleared in
    // WM_NCDESTROY before it is freed; UI thread only.
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<OcrWin>).as_ref() }
}

fn with<R>(hwnd: HWND, f: impl FnOnce(&mut OcrWin) -> R) -> Option<R> {
    let mut g = state(hwnd)?.try_borrow_mut().ok()?;
    Some(f(&mut g))
}

fn s(v: i32, dpi: u32) -> i32 {
    win::scale(v, dpi)
}

fn make_font(pt10: i32, dpi: u32) -> HFONT {
    let face = wide("Segoe UI");
    // SAFETY: valid NUL-terminated face name; the caller deletes the font.
    unsafe {
        CreateFontW(
            -(pt10 * dpi as i32 / 720),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            0,
            pcwstr(&face),
        )
    }
}

fn set_text(h: HWND, text: &str) {
    let w = wide(text);
    // SAFETY: valid NUL-terminated text for a control we own.
    unsafe {
        let _ = SetWindowTextW(h, pcwstr(&w));
    }
}

fn get_text(h: HWND) -> Vec<u16> {
    // SAFETY: buffer sized from GetWindowTextLengthW (+1 for the terminator).
    unsafe {
        let n = GetWindowTextLengthW(h).max(0) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf).max(0) as usize;
        buf.truncate(got);
        buf
    }
}

fn child(parent: HWND, class: &str, text: &str, style: u32, ex: u32, id: u16) -> HWND {
    let c = wide(class);
    let tx = wide(text);
    // SAFETY: creating a child control of our own window.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex),
            pcwstr(&c),
            pcwstr(&tx),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
            0,
            0,
            10,
            10,
            parent,
            HMENU(id as usize as *mut _),
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        )
        .unwrap_or_default()
    }
}

fn register() {
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let class = wide(CLASS);
    // SAFETY: registering our window class; the name outlives the call.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(hinst, PCWSTR(1 as *const u16)).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

fn initial_rect() -> Rect {
    let saved = settings::get().ocr.window;
    if let Some([x, y, w, h]) = saved {
        let r = Rect::new(x, y, w.max(200), h.max(150));
        if win::monitors()
            .iter()
            .any(|m| !m.work.intersect(&r).is_empty() && m.work.intersect(&r).area() > r.area() / 4)
        {
            return r;
        }
    }
    let c = win::cursor_pos();
    let mon = win::monitor_at(c);
    let dpi = mon.as_ref().map(|m| m.dpi).unwrap_or(96);
    let work = mon.map(|m| m.work).unwrap_or_else(win::virtual_screen);
    let (w, h) = (s(600, dpi), s(460, dpi));
    Rect::new(work.x + (work.w - w) / 2, work.y + (work.h - h) / 2, w, h).clamp_inside(&work)
}

/// Open a result window for `img` and start recognition. UI thread.
pub fn open(img: Image) {
    register();
    let r = initial_rect();
    let class = wide(CLASS);
    let title = wide(&t("ocr-title"));
    // SAFETY: creating our own top-level window; state attached right after.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            pcwstr(&class),
            pcwstr(&title),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            r.x,
            r.y,
            r.w,
            r.h,
            None,
            None,
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            log::error!("ocr: result window failed: {e}");
            crate::app::notify(t("app-name"), tf("notify-error", &[("error", &e.to_string())]), true);
            return;
        }
    };
    let dpi = win::dpi_for_window(hwnd);
    let keep_breaks = settings::get().ocr.keep_line_breaks;
    let label = child(hwnd, "STATIC", &t("ocr-language"), SS_LEFT | SS_CENTERIMAGE, 0, 0);
    let combo = child(hwnd, "COMBOBOX", "", CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_VSCROLL.0, 0, ID_COMBO);
    let status = child(hwnd, "STATIC", &t("ocr-status-working"), SS_RIGHT | SS_CENTERIMAGE | SS_ENDELLIPSIS, 0, 0);
    let edit_style =
        (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | ES_NOHIDESEL) as u32 | WS_VSCROLL.0 | WS_TABSTOP.0;
    let edit = child(hwnd, "EDIT", "", edit_style, WS_EX_CLIENTEDGE.0, ID_EDIT);
    let keep = child(hwnd, "BUTTON", &t("ocr-keep-breaks"), BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0, 0, ID_KEEP);
    let copy_all = child(hwnd, "BUTTON", &t("ocr-copy-all"), BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0, 0, ID_COPY_ALL);
    let copy_sel = child(hwnd, "BUTTON", &t("ocr-copy-selected"), BS_PUSHBUTTON as u32 | WS_TABSTOP.0, 0, ID_COPY_SEL);
    let close = child(hwnd, "BUTTON", &t("ocr-close"), BS_PUSHBUTTON as u32 | WS_TABSTOP.0, 0, ID_CLOSE);
    let explorer = wide("Explorer");
    // SAFETY: plain control messages and theming on our own children.
    unsafe {
        let _ = SetWindowTheme(edit, pcwstr(&explorer), PCWSTR::null());
        SendMessageW(edit, EM_SETLIMITTEXT, WPARAM(0), LPARAM(0));
        let chk = if keep_breaks { BST_CHECKED } else { BST_UNCHECKED };
        SendMessageW(keep, BM_SETCHECK, WPARAM(chk.0 as usize), LPARAM(0));
        let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
    }
    let slot: Slot = Arc::new(Mutex::new(None));
    let st = OcrWin {
        label,
        combo,
        status,
        edit,
        keep,
        copy_all,
        copy_sel,
        close,
        font: make_font(90, dpi),
        edit_font: make_font(105, dpi),
        dpi,
        img: Arc::new(img),
        result: None,
        codes: Vec::new(),
        langs: Vec::new(),
        slot,
        generation: 1,
        first_result: true,
    };
    let cell = Box::new(RefCell::new(st));
    // SAFETY: reclaimed exactly once in WM_NCDESTROY.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(cell) as isize);
    }
    OPEN.with(|o| o.borrow_mut().push(hwnd_to_isize(hwnd)));
    apply_fonts(hwnd);
    layout(hwnd);
    // SAFETY: showing our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    let lang = settings::get().ocr.language.clone();
    start(hwnd, lang, true);
}

fn apply_fonts(hwnd: HWND) {
    with(hwnd, |w| {
        for c in [w.label, w.combo, w.status, w.keep, w.copy_all, w.copy_sel, w.close] {
            // SAFETY: WM_SETFONT on our own controls with a font that outlives them.
            unsafe {
                SendMessageW(c, WM_SETFONT, WPARAM(w.font.0 as usize), LPARAM(1));
            }
        }
        // SAFETY: as above.
        unsafe {
            SendMessageW(w.edit, WM_SETFONT, WPARAM(w.edit_font.0 as usize), LPARAM(1));
        }
    });
}

/// Control rectangles for a client area of `cw`×`ch` at `dpi`:
/// [label, combo, status, edit, keep, copy_all, copy_sel, close].
fn compute_layout(cw: i32, ch: i32, dpi: u32) -> [Rect; 8] {
    let m = s(12, dpi);
    let row = s(28, dpi);
    let gap = s(8, dpi);
    let label_w = s(70, dpi);
    let combo_w = s(240, dpi).min((cw - 2 * m - label_w - gap).max(s(80, dpi)));
    let label = Rect::new(m, m, label_w, row);
    let combo = Rect::new(m + label_w, m, combo_w, row);
    let status_x = combo.right() + gap;
    let status = Rect::new(status_x, m, (cw - m - status_x).max(0), row);
    let btn_h = s(30, dpi);
    let by = ch - m - btn_h;
    let close_w = s(88, dpi);
    let sel_w = s(124, dpi);
    let all_w = s(100, dpi);
    let close = Rect::new(cw - m - close_w, by, close_w, btn_h);
    let copy_sel = Rect::new(close.x - gap - sel_w, by, sel_w, btn_h);
    let copy_all = Rect::new(copy_sel.x - gap - all_w, by, all_w, btn_h);
    let keep = Rect::new(m, by, (copy_all.x - gap - m).clamp(0, s(200, dpi)), btn_h);
    let ey = m + row + gap;
    let edit = Rect::new(m, ey, (cw - 2 * m).max(0), (by - gap - ey).max(0));
    [label, combo, status, edit, keep, copy_all, copy_sel, close]
}

fn layout(hwnd: HWND) {
    let mut rc = RECT::default();
    // SAFETY: valid out pointer.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    with(hwnd, |w| {
        let rects = compute_layout(rc.right, rc.bottom, w.dpi);
        let ctrls = [w.label, w.combo, w.status, w.edit, w.keep, w.copy_all, w.copy_sel, w.close];
        for (c, r) in ctrls.iter().zip(rects) {
            // The combo's height includes its drop-down list.
            let h = if *c == w.combo { s(300, w.dpi) } else { r.h };
            // SAFETY: positioning our own child control.
            unsafe {
                let _ = SetWindowPos(*c, None, r.x, r.y, r.w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    });
}

/// Start (or restart) recognition with `lang` ("" = profile languages).
fn start(hwnd: HWND, lang: String, want_langs: bool) {
    let Some((img, slot, generation, status)) = with(hwnd, |w| {
        w.generation += 1;
        (w.img.clone(), w.slot.clone(), w.generation, w.status)
    }) else {
        return;
    };
    set_text(status, &t("ocr-status-working"));
    let target = hwnd_to_isize(hwnd);
    let spawned = std::thread::Builder::new().name("ocr".into()).spawn(move || {
        super::engine::ensure_com();
        let langs = if want_langs { Some(super::languages()) } else { None };
        let res = super::recognize(&img, Some(&lang));
        let codes = crate::qr::decode(&img);
        let no_language = matches!(&res, Err(e) if matches!(e.downcast_ref::<OcrError>(), Some(OcrError::NoLanguage)));
        let done = Done { generation, result: res.map_err(|e| format!("{e:#}")), no_language, langs, codes };
        if let Ok(mut g) = slot.lock() {
            *g = Some(done);
        }
        // SAFETY: posting to a window that may already be gone fails harmlessly.
        unsafe {
            let _ = PostMessageW(hwnd_from_isize(target), WM_OCR_DONE, WPARAM(0), LPARAM(0));
        }
    });
    if let Err(e) = spawned {
        log::error!("ocr: could not start the worker: {e}");
        set_text(status, &tf("notify-error", &[("error", &e.to_string())]));
    }
}

fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn keep_checked(w: &OcrWin) -> bool {
    // SAFETY: plain control query.
    unsafe { SendMessageW(w.keep, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 as u32 == BST_CHECKED.0 }
}

fn on_done(hwnd: HWND) {
    let Some(done) = with(hwnd, |w| w.slot.lock().ok().and_then(|mut g| g.take())).flatten() else { return };
    let auto_copy = settings::get().ocr.auto_copy;
    let copy_text = with(hwnd, |w| {
        if done.generation != w.generation {
            return None;
        }
        w.codes = done.codes;
        if let Some(langs) = done.langs {
            w.langs = langs;
            // SAFETY: filling our own combo box with NUL-terminated strings.
            unsafe {
                SendMessageW(w.combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
                for (_, name) in &w.langs {
                    let n = wide(name);
                    SendMessageW(w.combo, CB_ADDSTRING, WPARAM(0), LPARAM(n.as_ptr() as isize));
                }
            }
        }
        match done.result {
            Ok(r) => {
                let sel = w.langs.iter().position(|(tag, _)| tag.eq_ignore_ascii_case(&r.language));
                // SAFETY: selecting an item in our own combo box (-1 clears).
                unsafe {
                    SendMessageW(
                        w.combo,
                        CB_SETCURSEL,
                        WPARAM(sel.map(|i| i as isize).unwrap_or(-1) as usize),
                        LPARAM(0),
                    );
                }
                let text = super::text_with_codes(&r, &w.codes, keep_checked(w));
                set_text(w.edit, &to_crlf(&text));
                let r = super::without_codes(&r, &w.codes);
                let qr = tf("ocr-status-qr", &[("n", &w.codes.len().to_string())]);
                let status = match (r.word_count(), w.codes.len()) {
                    (0, 0) => t("ocr-status-none"),
                    (0, _) => qr,
                    (words, codes) => {
                        let done = tf(
                            "ocr-status-done",
                            &[("lines", &r.lines.len().to_string()), ("words", &words.to_string())],
                        );
                        if codes == 0 {
                            done
                        } else {
                            format!("{done} · {qr}")
                        }
                    }
                };
                set_text(w.status, &status);
                w.result = Some(r);
                let first = std::mem::replace(&mut w.first_result, false);
                // SAFETY: focusing our own edit control.
                unsafe {
                    let _ = SetFocus(w.edit);
                }
                (first && auto_copy && !text.is_empty()).then_some((text, w.status))
            }
            Err(e) if !w.codes.is_empty() => {
                // No recognizer language (or OCR failed), but a QR code needs none.
                log::warn!("ocr: {e}");
                let text = crate::qr::texts(&w.codes).join("\n");
                set_text(w.edit, &to_crlf(&text));
                set_text(w.status, &tf("ocr-status-qr", &[("n", &w.codes.len().to_string())]));
                let first = std::mem::replace(&mut w.first_result, false);
                // SAFETY: focusing our own edit control (so Esc closes the window).
                unsafe {
                    let _ = SetFocus(w.edit);
                }
                (first && auto_copy).then_some((text, w.status))
            }
            Err(e) => {
                let msg = if done.no_language { t("ocr-no-language") } else { tf("notify-error", &[("error", &e)]) };
                log::warn!("ocr: {e}");
                set_text(w.status, &t("ocr-status-failed"));
                set_text(w.edit, &msg);
                // SAFETY: focusing our own edit control (so Esc closes the window).
                unsafe {
                    let _ = SetFocus(w.edit);
                }
                None
            }
        }
    })
    .flatten();
    if let Some((text, status)) = copy_text {
        match clipboard::copy_text(&to_crlf(&text)) {
            Ok(()) => set_text(status, &t("ocr-status-copied")),
            Err(e) => log::warn!("ocr: auto-copy failed: {e:#}"),
        }
    }
}

fn copy(hwnd: HWND, selected_only: bool) {
    let Some((edit, status)) = with(hwnd, |w| (w.edit, w.status)) else { return };
    let text = get_text(edit);
    let part = if selected_only {
        let (mut a, mut b) = (0u32, 0u32);
        // SAFETY: EM_GETSEL writes two DWORDs through the given pointers.
        unsafe {
            SendMessageW(edit, EM_GETSEL, WPARAM(&mut a as *mut u32 as usize), LPARAM(&mut b as *mut u32 as isize));
        }
        let (a, b) = (a.min(b) as usize, a.max(b) as usize);
        text.get(a..b.min(text.len())).unwrap_or_default().to_vec()
    } else {
        text
    };
    if part.is_empty() {
        set_text(status, &t(if selected_only { "ocr-status-no-selection" } else { "ocr-status-none" }));
        return;
    }
    match clipboard::copy_text(&String::from_utf16_lossy(&part)) {
        Ok(()) => set_text(status, &t("ocr-status-copied")),
        Err(e) => {
            log::warn!("ocr: copy failed: {e:#}");
            set_text(status, &tf("notify-error", &[("error", &format!("{e:#}"))]));
        }
    }
}

fn on_command(hwnd: HWND, id: u16, code: u32) {
    match (id, code) {
        (ID_COMBO, c) if c == CBN_SELCHANGE => {
            let tag = with(hwnd, |w| {
                // SAFETY: plain control query.
                let i = unsafe { SendMessageW(w.combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 };
                usize::try_from(i).ok().and_then(|i| w.langs.get(i)).map(|(t, _)| t.clone())
            })
            .flatten();
            if let Some(tag) = tag {
                let tg = tag.clone();
                settings::update(|s| s.ocr.language = tg);
                start(hwnd, tag, false);
            }
        }
        (ID_KEEP, c) if c == BN_CLICKED => {
            let keep = with(hwnd, |w| {
                let keep = keep_checked(w);
                if let Some(r) = &w.result {
                    set_text(w.edit, &to_crlf(&super::text_with_codes(r, &w.codes, keep)));
                }
                keep
            });
            if let Some(keep) = keep {
                settings::update(|s| s.ocr.keep_line_breaks = keep);
            }
        }
        (ID_COPY_ALL, c) if c == BN_CLICKED => copy(hwnd, false),
        (ID_COPY_SEL, c) if c == BN_CLICKED => copy(hwnd, true),
        (ID_CLOSE, c) if c == BN_CLICKED => {
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        _ => {}
    }
    // Esc and Ctrl+A are handled by the text box: after a click on a button or the
    // check box, give it the keyboard back (otherwise Esc would do nothing).
    if matches!(id, ID_KEEP | ID_COPY_ALL | ID_COPY_SEL) && code == BN_CLICKED {
        focus_text(hwnd);
    }
}

fn focus_text(hwnd: HWND) {
    if let Some(edit) = with(hwnd, |w| w.edit) {
        // SAFETY: focusing our own edit control.
        unsafe {
            let _ = SetFocus(edit);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            layout(hwnd);
            return LRESULT(0);
        }
        WM_GETMINMAXINFO => {
            let dpi = win::dpi_for_window(hwnd);
            // SAFETY: lParam points to a MINMAXINFO for this message.
            if let Some(mmi) = (lp.0 as *mut MINMAXINFO).as_mut() {
                mmi.ptMinTrackSize.x = s(460, dpi);
                mmi.ptMinTrackSize.y = s(240, dpi);
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            on_command(hwnd, (wp.0 & 0xFFFF) as u16, ((wp.0 >> 16) & 0xFFFF) as u32);
            return LRESULT(0);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            SetBkMode(HDC(wp.0 as *mut _), TRANSPARENT);
            return LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize);
        }
        WM_OCR_DONE => {
            on_done(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as u32;
            with(hwnd, |w| {
                w.dpi = dpi.max(96);
                let (f, ef) = (w.font, w.edit_font);
                w.font = make_font(90, w.dpi);
                w.edit_font = make_font(105, w.dpi);
                let _ = DeleteObject(f);
                let _ = DeleteObject(ef);
            });
            apply_fonts(hwnd);
            // SAFETY: lParam points to the suggested RECT for this message.
            if let Some(r) = (lp.0 as *const RECT).as_ref() {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            layout(hwnd);
            return LRESULT(0);
        }
        WM_CLOSE => {
            // A maximized or minimized window has no size worth remembering.
            if !IsZoomed(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
                let mut r = RECT::default();
                let _ = GetWindowRect(hwnd, &mut r);
                let rr = win::rect_from_win(&r);
                settings::update(|s| s.ocr.window = Some([rr.x, rr.y, rr.w, rr.h]));
            }
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let me = hwnd_to_isize(hwnd);
            OPEN.with(|o| o.borrow_mut().retain(|&h| h != me));
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut RefCell<OcrWin>;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !p.is_null() {
                // SAFETY: from Box::into_raw in `open`, cleared above: freed exactly once.
                drop(Box::from_raw(p));
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Edit control subclass: Ctrl+A selects all, Esc closes the window.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN if wp.0 == 0x41 && GetKeyState(VK_CONTROL.0 as i32) < 0 => {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
        WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
            let _ = PostMessageW(GetParent(hwnd).unwrap_or_default(), WM_CLOSE, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        // Swallow the Ctrl+A control character (it would beep).
        WM_CHAR if wp.0 == 1 => return LRESULT(0),
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

/// Open result windows and their screen rectangles (tests and previews).
#[cfg(test)]
pub fn open_windows() -> Vec<(HWND, Rect)> {
    OPEN.with(|o| {
        o.borrow()
            .iter()
            .map(|&h| {
                let hwnd = hwnd_from_isize(h);
                let mut r = RECT::default();
                // SAFETY: valid out pointer.
                unsafe {
                    let _ = GetWindowRect(hwnd, &mut r);
                }
                (hwnd, win::rect_from_win(&r))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_sane() {
        for dpi in [96, 144, 192] {
            let (cw, ch) = (s(600, dpi), s(420, dpi));
            let r = compute_layout(cw, ch, dpi);
            let edit = r[3];
            assert!(edit.w > 0 && edit.h > s(200, dpi), "{edit:?}");
            // Buttons don't overlap, stay inside, and sit below the edit.
            for b in &r[4..] {
                assert!(b.x >= 0 && b.right() <= cw && b.bottom() <= ch);
                assert!(b.y >= edit.bottom());
            }
            assert!(r[5].right() <= r[6].x && r[6].right() <= r[7].x);
            assert!(r[4].right() <= r[5].x);
        }
    }

    #[test]
    fn crlf() {
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
    }
}
