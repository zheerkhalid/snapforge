//! UI-thread controller: startup, message loop, and execution of [`AppCommand`]s.

use crate::app::{self, AppCommand, WM_APP_COMMAND};
use crate::common::win::{hwnd_to_isize, pcwstr, wide};
use crate::{editor, hotkeys, i18n, logging, settings, toolbar, tray};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage, RegisterClassW, TranslateMessage,
    HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
};

pub const APP_CLASS: &str = "SnapForge.App";

pub fn main_entry(_args: Vec<String>) -> i32 {
    settings::init();
    let (lang, level) = {
        let s = settings::get();
        (s.general.language.clone(), s.general.log_level.clone())
    };
    logging::init(&level);
    i18n::init(&lang);
    log::info!("SnapForge {} starting", env!("CARGO_PKG_VERSION"));
    match run() {
        Ok(()) => 0,
        Err(e) => {
            log::error!("fatal: {e:#}");
            1
        }
    }
}

fn run() -> anyhow::Result<()> {
    let hwnd = create_message_window()?;
    app::set_app_hwnd(hwnd_to_isize(hwnd));
    tray::create()?;
    toolbar::create()?;
    if settings::get().toolbar.visible {
        toolbar::show(true);
    }
    for problem in hotkeys::start() {
        log::warn!("hotkeys: {problem}");
    }
    message_loop();
    hotkeys::stop();
    tray::remove();
    editor::shutdown();
    settings::flush();
    Ok(())
}

fn create_message_window() -> anyhow::Result<HWND> {
    let class = wide(APP_CLASS);
    // SAFETY: standard window class registration with a static wndproc.
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: pcwstr(&class),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            pcwstr(&class),
            pcwstr(&class),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            hinst,
            None,
        )?;
        Ok(hwnd)
    }
}

fn message_loop() {
    let mut msg = MSG::default();
    // SAFETY: standard Win32 message loop.
    unsafe {
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_APP_COMMAND {
        for cmd in app::take_commands() {
            execute(cmd);
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn execute(cmd: AppCommand) {
    match cmd {
        AppCommand::Exit => {
            // SAFETY: ends the UI thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        AppCommand::Notify { title, text, is_error } => tray::notify(&title, &text, is_error),
        _ => log::debug!("command not handled yet"),
    }
}
