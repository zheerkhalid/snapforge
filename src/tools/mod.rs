//! Phase 5 screen tools: color picker, magnifier, crosshair, focus spotlight, ruler.
//!
//! CONTRACT:
//! * Everything runs on the UI thread. [`open`] creates the tool's windows and returns
//!   immediately (no nested message loop); the app's normal message loop drives them.
//! * Tool windows are excluded from capture, DPI-aware (PerMonitorV2, physical pixels
//!   throughout) and closed with Esc. Timers and hooks exist only while a tool is open.
//! * Settings live in `settings.tools` ([`crate::settings::tools::ToolsSettings`]).

pub mod color;
pub mod lens;
pub mod measure;

mod calibrate;
mod card;
mod crosshair;
mod finetune;
mod focus;
mod gfx;
mod hook;
mod magnifier;
mod picker;
mod ruler;
mod wnd;

#[cfg(test)]
mod preview;

use crate::app::ScreenTool;
use crate::common::win::{hwnd_from_isize, hwnd_to_isize};
use std::cell::{Cell, RefCell};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, HWND_MESSAGE, IDC_ARROW, WINDOW_EX_STYLE, WINDOW_STYLE,
};

/// Open a tool. If it is already open it is brought to the front (the color picker
/// restarts picking).
pub fn open(tool: ScreenTool) {
    log::info!("tools: open {tool:?}");
    let ok = match tool {
        ScreenTool::ColorPicker => picker::open(),
        ScreenTool::Magnifier => magnifier::open(),
        ScreenTool::Crosshair => crosshair::open(),
        ScreenTool::Focus => focus::open(),
        ScreenTool::Ruler => ruler::open(),
    };
    if ok {
        ORDER.with(|o| {
            let mut o = o.borrow_mut();
            o.retain(|t| *t != tool);
            o.push(tool);
        });
    }
    update_hook();
}

/// Close the tool if it is open, else open it (for hotkeys and toolbar buttons).
pub fn toggle(tool: ScreenTool) {
    if is_open(tool) {
        close(tool);
    } else {
        open(tool);
    }
}

pub fn close(tool: ScreenTool) {
    match tool {
        ScreenTool::ColorPicker => {
            picker::close();
            finetune::close();
        }
        ScreenTool::Magnifier => magnifier::close(),
        ScreenTool::Crosshair => crosshair::close(),
        ScreenTool::Focus => focus::close(),
        ScreenTool::Ruler => ruler::close(),
    }
    closed(tool);
}

pub fn close_all() {
    for t in ALL {
        close(t);
    }
    if let Some(h) = MSG_HWND.with(|m| m.take()) {
        // SAFETY: our own message-only window.
        unsafe {
            let _ = DestroyWindow(hwnd_from_isize(h));
        }
    }
}

pub fn is_open(tool: ScreenTool) -> bool {
    match tool {
        ScreenTool::ColorPicker => picker::is_open() || finetune::is_open(),
        ScreenTool::Magnifier => magnifier::is_open(),
        ScreenTool::Crosshair => crosshair::is_open(),
        ScreenTool::Focus => focus::is_open(),
        ScreenTool::Ruler => ruler::is_open(),
    }
}

/// Our tool windows (for `capture::snapshot_windows` exclusion lists).
pub fn tool_hwnds() -> Vec<isize> {
    let mut v = Vec::new();
    v.extend(picker::hwnds());
    v.extend(finetune::hwnds());
    v.extend(magnifier::hwnds());
    v.extend(crosshair::hwnds());
    v.extend(focus::hwnds());
    v.extend(ruler::hwnds());
    v
}

const ALL: [ScreenTool; 5] =
    [ScreenTool::ColorPicker, ScreenTool::Magnifier, ScreenTool::Crosshair, ScreenTool::Focus, ScreenTool::Ruler];

thread_local! {
    /// Open tools, least recent first (Esc through the hook closes the most recent).
    static ORDER: RefCell<Vec<ScreenTool>> = const { RefCell::new(Vec::new()) };
    static MSG_HWND: Cell<Option<isize>> = const { Cell::new(None) };
}

/// A tool closed itself (Esc, click, window closed): forget it and update the hook.
pub(crate) fn closed(tool: ScreenTool) {
    if !is_open(tool) {
        ORDER.with(|o| o.borrow_mut().retain(|t| *t != tool));
    }
    update_hook();
}

/// Run the low-level hook only while a click-through tool is open.
fn update_hook() {
    let cross = crosshair::is_open();
    let foc = focus::is_open();
    if !(cross || foc) {
        hook::stop();
        return;
    }
    use std::sync::atomic::Ordering::Release;
    hook::WANT_ESC.store(true, Release);
    hook::WANT_RCLICK.store(cross, Release);
    hook::WANT_WHEEL.store(foc, Release);
    hook::WANT_CTRL_SHIFT_DRAG.store(foc, Release);
    if let Some(h) = msg_window() {
        hook::start(h);
    }
}

fn msg_window() -> Option<HWND> {
    if let Some(h) = MSG_HWND.with(|m| m.get()) {
        return Some(hwnd_from_isize(h));
    }
    const CLASS: &str = "SnapForge.ToolsMsg";
    if !wnd::register(CLASS, Some(msg_proc), IDC_ARROW) {
        return None;
    }
    let h = wnd::create(WINDOW_EX_STYLE(0), CLASS, "", WINDOW_STYLE(0), crate::common::Rect::default(), HWND_MESSAGE)?;
    MSG_HWND.with(|m| m.set(Some(hwnd_to_isize(h))));
    Some(h)
}

unsafe extern "system" fn msg_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        hook::WM_HOOK_MOVE => {
            hook::move_handled();
            let p = crate::common::win::cursor_pos();
            crosshair::on_move(p);
            focus::on_move(p);
            LRESULT(0)
        }
        hook::WM_HOOK_ESC => {
            if let Some(t) = ORDER.with(|o| o.borrow().last().copied()) {
                close(t);
            }
            LRESULT(0)
        }
        hook::WM_HOOK_RCLICK => {
            if crosshair::is_open() {
                close(ScreenTool::Crosshair);
            }
            LRESULT(0)
        }
        hook::WM_HOOK_WHEEL => {
            focus::on_wheel(wnd::notches(wp.0 as u16 as i16 as i32));
            LRESULT(0)
        }
        hook::WM_HOOK_DRAG => {
            focus::on_drag(wp.0 == 0, hook::unpack_point(lp));
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
