//! UI-thread controller: startup, single instance, message loop, and execution of
//! [`AppCommand`]s (capture flows and the output pipeline).

use crate::app::{self, AppCommand, CaptureOutcome, CaptureRequest, CaptureSource, WM_APP_COMMAND};
use crate::capture::{self, scroll::ScrollTarget, CaptureError, GrabOptions};
use crate::cli::{self, Cli};
use crate::common::time::{qpc_now, DateTime};
use crate::common::win::{self, hwnd_from_isize, hwnd_to_isize, pcwstr, wide};
use crate::common::Image;
use crate::output::{self, office::OfficeApp};
use crate::overlay::{self, Frozen, OverlayMode, Selection};
use crate::settings::{self, filename, CaptureMode, Destination};
use crate::{editor, hotkeys, i18n, logging, toolbar, tray};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GetAncestor,
    GetForegroundWindow, GetMessageW, KillTimer, MessageBoxW, PostQuitMessage, RegisterClassW, SendMessageTimeoutW,
    SetTimer, TranslateMessage, WindowFromPoint, ASFW_ANY, GA_ROOT, HWND_MESSAGE, IDYES, MB_ICONINFORMATION, MB_YESNO,
    MSG, SMTO_ABORTIFHUNG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA, WM_TIMER, WNDCLASSW,
};

pub const APP_CLASS: &str = "SnapForge.App";
const COPYDATA_ARGS: usize = 0x5346_4152; // "SFAR"

const TIMER_AUTO_CAPTURE: usize = 1;
const TIMER_IDLE: usize = 2;
const TIMER_SETTINGS_WATCH: usize = 3;
const IDLE_RELEASE_MS: u32 = 10_000;

/// Bench support: QPC timestamp when the most recent capture command started executing.
pub static CAPTURE_START_QPC: AtomicI64 = AtomicI64::new(0);

/// What "repeat last capture" repeats.
#[derive(Clone)]
struct LastCapture {
    mode: CaptureMode,
    source: CaptureSource,
}

#[derive(Default)]
struct State {
    last: Option<LastCapture>,
    /// Exit after the pending output finishes (standalone CLI run).
    exit_after_output: bool,
    settings_mtime: Option<std::time::SystemTime>,
    settings_watch_ticks: u32,
}

type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static IO: RefCell<Option<mpsc::Sender<Job>>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

// ------------------------------------------------------------------ entry

pub fn main_entry(args: Vec<String>) -> i32 {
    let cli = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            cli::attach_console();
            eprintln!("SnapForge: {e}\n\n{}", cli::HELP);
            return 2;
        }
    };
    if cli.help || cli.version {
        cli::attach_console();
        if cli.version {
            println!("SnapForge {}", env!("CARGO_PKG_VERSION"));
        } else {
            println!("{}", cli::HELP);
        }
        return 0;
    }
    if let Some(dir) = &cli.settings_dir {
        settings::set_dir_override(dir.clone());
    }
    if cli.bench {
        cli::attach_console();
        return crate::bench::run_parent();
    }
    if cli.uninstall_cleanup {
        return uninstall_cleanup();
    }

    settings::init();
    let (lang, level) = {
        let s = settings::get();
        (s.general.language.clone(), s.general.log_level.clone())
    };
    logging::init(&level);
    i18n::init(&lang);

    // Single instance: forward the command line to the running instance.
    if !cli.bench_child && !acquire_single_instance() {
        let forwarded = forward_to_running_instance(&args);
        log::info!("another instance is running; forwarded={forwarded}");
        return if forwarded { 0 } else { 1 };
    }

    log::info!("SnapForge {} starting (portable={})", env!("CARGO_PKG_VERSION"), settings::is_portable());
    match run(cli) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("fatal: {e:#}");
            1
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let hwnd = create_message_window()?;
    start_io_worker();
    app::set_app_hwnd(hwnd_to_isize(hwnd));

    // A standalone automation run (`--capture` with no instance running) skips the tray UI.
    let standalone_capture = cli.capture.is_some() && !cli.bench_child;
    if !standalone_capture {
        tray::create()?;
        toolbar::create()?;
        let show_toolbar = cli.show_toolbar || (!cli.tray && settings::get().toolbar.visible);
        if show_toolbar {
            toolbar::show(true);
        }
        for problem in hotkeys::start() {
            log::warn!("hotkeys: {problem}");
            tray::notify(&i18n::t("app-name"), &problem, true);
        }
        apply_auto_capture_timer();
    }
    if cli.bench_child {
        crate::bench::start_child();
    }
    handle_cli(&cli, standalone_capture);

    message_loop();

    hotkeys::stop();
    tray::remove();
    editor::shutdown();
    settings::flush();
    log::info!("SnapForge exiting");
    Ok(())
}

/// Apply the action part of a command line (fresh start or forwarded from another instance).
fn handle_cli(cli: &Cli, exit_after: bool) {
    if !cli.open.is_empty() {
        editor::open_files(cli.open.clone());
    }
    if let Some(mode) = cli.capture {
        app::post(AppCommand::CaptureWith(CaptureRequest {
            mode,
            delay_secs: cli.delay,
            dest: cli.out,
            out_path: cli.file.clone(),
            exit_after,
        }));
    } else if cli.show_toolbar {
        toolbar::show(true);
    }
}

// ------------------------------------------------------------------ single instance

fn acquire_single_instance() -> bool {
    let name = wide("Local\\SnapForge.SingleInstance.v1");
    // SAFETY: named mutex; the handle intentionally lives for the whole process.
    unsafe {
        match CreateMutexW(None, true, pcwstr(&name)) {
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}

fn forward_to_running_instance(args: &[String]) -> bool {
    let class = wide(APP_CLASS);
    // SAFETY: message-only windows are found with HWND_MESSAGE as parent; the payload
    // buffer outlives the synchronous SendMessageTimeout call.
    unsafe {
        let Ok(target) = FindWindowExW(HWND_MESSAGE, HWND::default(), pcwstr(&class), windows::core::PCWSTR::null())
        else {
            return false;
        };
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let payload: Vec<u16> = if args.is_empty() {
            wide("--toolbar")
        } else {
            let mut v: Vec<u16> = Vec::new();
            for a in args {
                v.extend(a.encode_utf16());
                v.push(0);
            }
            v
        };
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_ARGS,
            cbData: (payload.len() * 2) as u32,
            lpData: payload.as_ptr() as *mut _,
        };
        let mut result = 0usize;
        SendMessageTimeoutW(
            target,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            5000,
            Some(&mut result),
        );
        true
    }
}

fn decode_copydata(lp: LPARAM) -> Option<Vec<String>> {
    // SAFETY: WM_COPYDATA guarantees lParam points to a valid COPYDATASTRUCT during the call.
    unsafe {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData != COPYDATA_ARGS || cds.lpData.is_null() {
            return None;
        }
        let words = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
        Some(words.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect())
    }
}

// ------------------------------------------------------------------ window + loop

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
    match msg {
        WM_APP_COMMAND => {
            for cmd in app::take_commands() {
                execute(cmd);
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            if let Some(args) = decode_copydata(lp) {
                app::post(AppCommand::ForwardedArgs(args));
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_AUTO_CAPTURE => auto_capture_tick(),
                TIMER_IDLE => {
                    let _ = KillTimer(hwnd, TIMER_IDLE);
                    go_idle();
                }
                TIMER_SETTINGS_WATCH => settings_watch_tick(),
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn app_hwnd() -> HWND {
    hwnd_from_isize(app::app_hwnd())
}

// ------------------------------------------------------------------ I/O worker

fn start_io_worker() {
    let (tx, rx) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new().name("io".into()).spawn(move || {
        while let Ok(job) = rx.recv() {
            job();
        }
    });
    if spawned.is_ok() {
        IO.with(|io| *io.borrow_mut() = Some(tx));
    } else {
        log::error!("could not start the I/O worker thread");
    }
}

/// Run a job on the I/O worker (falls back to a fresh thread).
fn io(job: impl FnOnce() + Send + 'static) {
    let job: Job = Box::new(job);
    let leftover = IO.with(|io| match io.borrow().as_ref() {
        Some(tx) => tx.send(job).err().map(|e| e.0),
        None => Some(job),
    });
    if let Some(job) = leftover {
        let _ = std::thread::Builder::new().name("io-fallback".into()).spawn(job);
    }
}

// ------------------------------------------------------------------ commands

fn execute(cmd: AppCommand) {
    match cmd {
        AppCommand::Capture(mode) => start_capture(CaptureRequest { mode, ..Default::default() }),
        AppCommand::CaptureWith(req) => start_capture(req),
        AppCommand::RepeatLast => repeat_last(),
        AppCommand::ToggleAutoCapture => {
            settings::update(|s| s.capture.auto_capture_enabled = !s.capture.auto_capture_enabled);
            apply_auto_capture_timer();
            refresh_ui();
        }
        AppCommand::SetDestination(d) => {
            settings::update(|s| s.output.destination = d);
            refresh_ui();
        }
        AppCommand::SetDelay(secs) => {
            settings::update(|s| s.capture.delay_secs = secs);
            refresh_ui();
        }
        AppCommand::ToggleAutoCopy => {
            settings::update(|s| s.output.auto_copy = !s.output.auto_copy);
            refresh_ui();
        }
        AppCommand::TogglePreview => {
            settings::update(|s| s.output.preview_in_editor = !s.output.preview_in_editor);
            refresh_ui();
        }
        AppCommand::ToggleTimestamp => {
            settings::update(|s| s.output.timestamp = !s.output.timestamp);
            refresh_ui();
        }
        AppCommand::ShowToolbar(v) => toolbar::show(v),
        AppCommand::ToggleToolbar => toolbar::toggle(),
        AppCommand::OpenEditor => editor::show(),
        AppCommand::OpenSettings => open_settings_file(),
        AppCommand::OpenOutputFolder => {
            let dir = settings::get().output_folder();
            let _ = std::fs::create_dir_all(&dir);
            win::shell_open(&dir.display().to_string());
        }
        AppCommand::About => show_about(),
        AppCommand::SettingsChanged => {
            for problem in hotkeys::reload() {
                log::warn!("hotkeys: {problem}");
                tray::notify(&i18n::t("app-name"), &problem, true);
            }
            apply_auto_capture_timer();
            refresh_ui();
        }
        AppCommand::Captured(outcome) => {
            BUSY.with(|b| b.set(false));
            deliver(*outcome);
            schedule_idle();
        }
        AppCommand::Deliver { image, dest } => deliver_to(image, dest, None, false),
        AppCommand::Notify { title, text, is_error } => tray::notify(&title, &text, is_error),
        AppCommand::ForwardedArgs(args) => match cli::parse(&args) {
            Ok(c) => handle_cli(&c, false),
            Err(e) => log::warn!("forwarded args rejected: {e}"),
        },
        AppCommand::Exit => {
            // SAFETY: ends the UI thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        // Phase 2-6 commands are dispatched in integration.rs.
        other => crate::integration::execute(other),
    }
}

fn refresh_ui() {
    toolbar::refresh();
    tray::refresh();
}

// ------------------------------------------------------------------ capture flows

fn our_windows() -> Vec<isize> {
    vec![toolbar::hwnd(), app::app_hwnd(), overlay::overlay_hwnd()].into_iter().filter(|&h| h != 0).collect()
}

fn start_capture(req: CaptureRequest) {
    if BUSY.with(|b| b.replace(true)) {
        log::debug!("capture ignored: another capture is in progress");
        return;
    }
    CAPTURE_START_QPC.store(qpc_now(), Ordering::Release);
    if req.exit_after {
        with_state(|s| s.exit_after_output = true);
    }
    let finish = |outcome: Option<CaptureOutcome>| {
        BUSY.with(|b| b.set(false));
        match outcome {
            Some(o) => deliver(o),
            None => {
                if with_state(|s| s.exit_after_output) {
                    app::post(AppCommand::Exit);
                }
            }
        }
        schedule_idle();
    };

    let delay = req.delay_secs.unwrap_or_else(|| settings::get().capture.delay_secs);
    if delay > 0 && !overlay::countdown(delay) {
        finish(None);
        return;
    }

    let opts = GrabOptions::from_settings();
    let result: Result<Option<CaptureOutcome>, CaptureError> = match req.mode {
        CaptureMode::FullScreen => capture_full_screen(&opts).map(Some),
        CaptureMode::ActiveWindow => capture_active_window(&opts).map(Some),
        CaptureMode::Region | CaptureMode::Freehand | CaptureMode::WindowObject | CaptureMode::FixedRegion => {
            capture_interactive(req.mode, &opts)
        }
        CaptureMode::Scrolling => {
            // Runs on its own thread; BUSY is cleared when `Captured` arrives.
            match start_scrolling(&req, &opts) {
                Ok(true) => return,
                Ok(false) => Ok(None),
                Err(e) => Err(e),
            }
        }
    };
    match result {
        Ok(Some(mut o)) => {
            o.dest = req.dest;
            o.out_path = req.out_path;
            finish(Some(o));
        }
        Ok(None) => finish(None),
        Err(e) => {
            log::error!("capture failed: {e}");
            tray::notify(&i18n::t("app-name"), &i18n::tf("notify-error", &[("error", &e.to_string())]), true);
            finish(None);
        }
    }
}

fn capture_full_screen(opts: &GrabOptions) -> Result<CaptureOutcome, CaptureError> {
    let rect = if settings::get().capture.fullscreen_all_monitors {
        win::virtual_screen()
    } else {
        win::monitor_at(win::cursor_pos()).map(|m| m.rect).unwrap_or_else(win::virtual_screen)
    };
    let image = capture::grab_rect(rect, opts)?;
    Ok(outcome(image, CaptureMode::FullScreen, CaptureSource::Screen(rect)))
}

fn capture_active_window(opts: &GrabOptions) -> Result<CaptureOutcome, CaptureError> {
    // SAFETY: plain queries.
    let fg = unsafe { GetForegroundWindow() };
    let ours = our_windows();
    if fg.0.is_null() || ours.contains(&hwnd_to_isize(fg)) {
        return capture_full_screen(opts);
    }
    let (image, _rect) = capture::grab_window(hwnd_to_isize(fg), opts)?;
    Ok(outcome(image, CaptureMode::ActiveWindow, CaptureSource::Window(hwnd_to_isize(fg))))
}

fn freeze(opts: &GrabOptions) -> Result<Frozen, CaptureError> {
    let origin = win::virtual_screen();
    // Frozen backgrounds never include the cursor.
    let o = GrabOptions { include_cursor: false, ..*opts };
    let image = capture::grab_rect(origin, &o)?;
    let windows = capture::snapshot_windows(&our_windows());
    Ok(Frozen { image, origin, windows })
}

fn capture_interactive(mode: CaptureMode, opts: &GrabOptions) -> Result<Option<CaptureOutcome>, CaptureError> {
    let frozen = freeze(opts)?;
    let omode = match mode {
        CaptureMode::Freehand => OverlayMode::Freehand,
        CaptureMode::WindowObject => OverlayMode::WindowObject,
        CaptureMode::FixedRegion => OverlayMode::Fixed,
        _ => OverlayMode::Region,
    };
    let Some(sel) = overlay::select(&frozen, omode) else {
        return Ok(None);
    };
    let mut image = overlay::render_selection(&frozen, &sel);
    if opts.include_cursor {
        log::debug!("cursor overlay is skipped for frozen captures");
    }
    if image.is_empty() {
        return Err(CaptureError::Empty);
    }
    let source = match sel {
        Selection::Rect(r) => CaptureSource::Screen(r),
        Selection::Window { rect, .. } => CaptureSource::Screen(rect),
        Selection::Polygon(p) => CaptureSource::Polygon(p),
        Selection::Multi(r) => CaptureSource::MultiRect(r),
    };
    // Keep the 1×1..5×5 tiny captures exactly as selected.
    if image.width == 0 || image.height == 0 {
        image = Image::new(1, 1);
    }
    Ok(Some(outcome(image, mode, source)))
}

/// Returns Ok(true) when a scrolling thread was started.
fn start_scrolling(req: &CaptureRequest, opts: &GrabOptions) -> Result<bool, CaptureError> {
    let frozen = freeze(opts)?;
    let Some(sel) = overlay::select(&frozen, OverlayMode::ScrollPick) else {
        return Ok(false);
    };
    drop(frozen);
    let target = match sel {
        Selection::Window { hwnd, rect } => ScrollTarget { rect, hwnd: Some(hwnd) },
        Selection::Rect(rect) => {
            let c = rect.center();
            // SAFETY: plain queries.
            let h = unsafe { WindowFromPoint(POINT { x: c.x, y: c.y }) };
            ScrollTarget { rect, hwnd: (!h.0.is_null()).then(|| hwnd_to_isize(h)) }
        }
        Selection::Polygon(_) | Selection::Multi(_) => return Ok(false),
    };
    let scroll_settings = settings::get().capture.scroll.clone();
    let opts = *opts;
    let (dest, out_path) = (req.dest, req.out_path.clone());
    std::thread::Builder::new()
        .name("scroll-capture".into())
        .spawn(move || match capture::scroll::run(target, &scroll_settings, &opts) {
            Ok(image) => app::post(AppCommand::Captured(Box::new(CaptureOutcome {
                image,
                mode: CaptureMode::Scrolling,
                source: CaptureSource::Scrolling,
                dest,
                out_path,
            }))),
            Err(CaptureError::Cancelled) => app::post(AppCommand::Captured(Box::new(CaptureOutcome {
                image: Image::new(0, 0),
                mode: CaptureMode::Scrolling,
                source: CaptureSource::Scrolling,
                dest,
                out_path,
            }))),
            Err(e) => {
                log::error!("scrolling capture failed: {e}");
                app::notify(i18n::t("app-name"), i18n::tf("notify-error", &[("error", &e.to_string())]), true);
                app::post(AppCommand::Captured(Box::new(CaptureOutcome {
                    image: Image::new(0, 0),
                    mode: CaptureMode::Scrolling,
                    source: CaptureSource::Scrolling,
                    dest,
                    out_path,
                })));
            }
        })
        .map_err(|e| CaptureError::Failed(e.to_string()))?;
    Ok(true)
}

fn outcome(image: Image, mode: CaptureMode, source: CaptureSource) -> CaptureOutcome {
    CaptureOutcome { image, mode, source, dest: None, out_path: None }
}

fn repeat_last() {
    let Some(last) = with_state(|s| s.last.clone()) else {
        // Nothing yet: behave like a region capture.
        start_capture(CaptureRequest { mode: CaptureMode::Region, ..Default::default() });
        return;
    };
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    let opts = GrabOptions::from_settings();
    let result = recapture(&last, &opts);
    BUSY.with(|b| b.set(false));
    match result {
        Ok(img) => deliver(outcome(img, last.mode, last.source)),
        Err(e) => {
            log::warn!("repeat capture failed: {e}");
            tray::notify(&i18n::t("app-name"), &i18n::tf("notify-error", &[("error", &e.to_string())]), true);
        }
    }
    schedule_idle();
}

fn recapture(last: &LastCapture, opts: &GrabOptions) -> Result<Image, CaptureError> {
    match &last.source {
        CaptureSource::Screen(r) => capture::grab_rect(*r, opts),
        CaptureSource::Window(h) => capture::grab_window(*h, opts).map(|(i, _)| i),
        CaptureSource::Polygon(_) | CaptureSource::MultiRect(_) => {
            let frozen = freeze(opts)?;
            let sel = match &last.source {
                CaptureSource::Polygon(p) => Selection::Polygon(p.clone()),
                CaptureSource::MultiRect(r) => Selection::Multi(r.clone()),
                _ => return Err(CaptureError::Empty),
            };
            Ok(overlay::render_selection(&frozen, &sel))
        }
        CaptureSource::Scrolling | CaptureSource::Other => {
            Err(CaptureError::Failed("cannot repeat this capture".into()))
        }
    }
}

// ------------------------------------------------------------------ auto capture

fn apply_auto_capture_timer() {
    let (enabled, secs) = {
        let s = settings::get();
        (s.capture.auto_capture_enabled, s.capture.auto_capture_interval_secs.max(1))
    };
    // SAFETY: timers on our own message window.
    unsafe {
        let _ = KillTimer(app_hwnd(), TIMER_AUTO_CAPTURE);
        if enabled {
            SetTimer(app_hwnd(), TIMER_AUTO_CAPTURE, secs.saturating_mul(1000), None);
        }
    }
}

fn auto_capture_tick() {
    if BUSY.with(|b| b.get()) {
        return;
    }
    let s = settings::snapshot();
    let opts = GrabOptions::from_settings();
    let mode = s.capture.auto_capture_mode;
    let result = match mode {
        CaptureMode::ActiveWindow => capture_active_window(&opts),
        // Region-like modes repeat the last selected area; otherwise full screen.
        _ => match with_state(|st| st.last.clone()) {
            Some(last) if matches!(last.source, CaptureSource::Screen(_)) && mode != CaptureMode::FullScreen => {
                recapture(&last, &opts).map(|img| outcome(img, mode, last.source))
            }
            _ => capture_full_screen(&opts),
        },
    };
    match result {
        Ok(o) => {
            let folder = s.auto_capture_folder();
            let path = filename::next_output_path(&folder, s.output.format.extension());
            io(move || match output::save::save_image(&o.image, &path) {
                Ok(()) => log::info!("auto capture saved {}", path.display()),
                Err(e) => log::error!("auto capture save failed: {e:#}"),
            });
        }
        Err(e) => log::warn!("auto capture failed: {e}"),
    }
    schedule_idle();
}

// ------------------------------------------------------------------ output pipeline

fn deliver(o: CaptureOutcome) {
    if o.image.is_empty() {
        if with_state(|s| s.exit_after_output) {
            app::post(AppCommand::Exit);
        }
        return;
    }
    if !matches!(o.source, CaptureSource::Scrolling | CaptureSource::Other) {
        with_state(|s| s.last = Some(LastCapture { mode: o.mode, source: o.source.clone() }));
    }
    let s = settings::snapshot();
    let mut image = o.image;
    if s.output.timestamp {
        output::stamp::stamp(&mut image, &filename::timestamp_text(&s.output.timestamp_format));
    }
    let explicit = o.dest.is_some() || o.out_path.is_some();
    let dest = o.dest.unwrap_or(s.output.destination);
    if s.output.auto_copy && dest != Destination::Clipboard {
        if let Err(e) = output::clipboard::copy_image(&image) {
            log::warn!("auto-copy failed: {e:#}");
        }
    }
    if s.output.preview_in_editor && !explicit && !matches!(dest, Destination::Editor | Destination::Draw) {
        editor::open_image(image, capture_title(), None, Some(dest));
        return;
    }
    let exit_after = with_state(|st| st.exit_after_output);
    deliver_to(image, dest, o.out_path, exit_after);
}

fn capture_title() -> String {
    let t = DateTime::now_local();
    format!("Capture {:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

fn exit_if(flag: bool) {
    if flag {
        app::post(AppCommand::Exit);
    }
}

fn notify_saved(path: &Path) {
    app::notify(i18n::t("app-name"), i18n::tf("notify-saved", &[("path", &path.display().to_string())]), false);
}

fn notify_err(e: &anyhow::Error) {
    log::error!("output failed: {e:#}");
    app::notify(i18n::t("app-name"), i18n::tf("notify-error", &[("error", &format!("{e:#}"))]), true);
}

/// A file in the output folder named by the template.
fn output_path() -> PathBuf {
    let s = settings::get();
    let folder = s.output_folder();
    let ext = s.output.format.extension();
    drop(s);
    filename::next_output_path(&folder, ext)
}

/// A temporary PNG for handing to other programs (email, Office).
fn temp_path() -> PathBuf {
    let dir = std::env::temp_dir().join("SnapForge");
    let _ = std::fs::create_dir_all(&dir);
    let name = output_path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "capture".into());
    dir.join(format!("{name}.png"))
}

fn deliver_to(image: Image, dest: Destination, explicit_path: Option<PathBuf>, exit_after: bool) {
    match dest {
        Destination::Editor | Destination::Draw => {
            editor::open_image(image, capture_title(), None, None);
            // A standalone CLI run that opens the editor keeps running in the tray.
            if exit_after {
                with_state(|s| s.exit_after_output = false);
                let _ = tray::create();
            }
        }
        Destination::Clipboard => {
            match output::clipboard::copy_image(&image) {
                Ok(()) => app::notify(i18n::t("app-name"), i18n::t("notify-copied"), false),
                Err(e) => notify_err(&e),
            }
            exit_if(exit_after);
        }
        Destination::File => {
            let path = explicit_path.unwrap_or_else(output_path);
            io(move || {
                match output::save::save_image(&image, &path) {
                    Ok(()) => {
                        remember_folder(&path);
                        notify_saved(&path);
                    }
                    Err(e) => notify_err(&e),
                }
                exit_if(exit_after);
            });
        }
        Destination::Printer => {
            match output::print::print_image(&image, app::app_hwnd(), &capture_title()) {
                Ok(_) => {}
                Err(e) => notify_err(&e),
            }
            exit_if(exit_after);
        }
        Destination::Email => {
            let path = temp_path();
            io(move || {
                let r = output::save::save_image(&image, &path)
                    .and_then(|_| output::email::email_file(&path, &i18n::t("app-name")));
                if let Err(e) = r {
                    notify_err(&e);
                }
                exit_if(exit_after);
            });
        }
        Destination::Word | Destination::Excel | Destination::PowerPoint | Destination::OneNote => {
            let app_kind = match dest {
                Destination::Word => OfficeApp::Word,
                Destination::Excel => OfficeApp::Excel,
                Destination::PowerPoint => OfficeApp::PowerPoint,
                _ => OfficeApp::OneNote,
            };
            let path = temp_path();
            io(move || {
                let r = output::save::save_image(&image, &path).and_then(|_| output::office::send_to(app_kind, &path));
                if let Err(e) = r {
                    notify_err(&e);
                }
                exit_if(exit_after);
            });
        }
        Destination::Upload => {
            let up = settings::get().output.upload.clone();
            let path = if up.keep_local_copy { explicit_path.unwrap_or_else(output_path) } else { temp_path() };
            io(move || {
                let r = output::save::save_image(&image, &path).and_then(|_| output::upload::upload_file(&path, &up));
                match r {
                    Ok(Some(url)) => {
                        let _ = output::clipboard::copy_text(&url);
                        app::notify(i18n::t("app-name"), i18n::tf("notify-uploaded", &[("url", &url)]), false);
                    }
                    Ok(None) => {
                        app::notify(i18n::t("app-name"), i18n::tf("notify-uploaded", &[("url", &up.host)]), false)
                    }
                    Err(e) => notify_err(&e),
                }
                if !up.keep_local_copy {
                    let _ = std::fs::remove_file(&path);
                }
                exit_if(exit_after);
            });
        }
        Destination::Program => {
            let program = settings::get().output.external_program.clone();
            let path = explicit_path.unwrap_or_else(output_path);
            io(move || {
                let r = output::save::save_image(&image, &path).and_then(|_| {
                    if program.trim().is_empty() {
                        anyhow::bail!("no external program is configured (output.external_program)")
                    }
                    output::external::open_with(&program, &path)
                });
                if let Err(e) = r {
                    notify_err(&e);
                }
                exit_if(exit_after);
            });
        }
    }
}

/// Keep the output-folder history list (most recent first, max 10).
fn remember_folder(path: &Path) {
    let Some(dir) = path.parent().map(|d| d.display().to_string()) else {
        return;
    };
    settings::update(|s| {
        s.output.folder_history.retain(|d| !d.eq_ignore_ascii_case(&dir));
        s.output.folder_history.insert(0, dir);
        s.output.folder_history.truncate(10);
    });
}

// ------------------------------------------------------------------ idle, settings, about

fn schedule_idle() {
    // SAFETY: timer on our own message window.
    unsafe {
        SetTimer(app_hwnd(), TIMER_IDLE, IDLE_RELEASE_MS, None);
    }
}

/// Release capture resources and trim the working set after a quiet period.
fn go_idle() {
    capture::release_idle_resources();
    overlay::release_idle_resources();
    if !editor::is_running() {
        use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
        // SAFETY: trimming our own working set.
        unsafe {
            let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }
}

/// Settings UI arrives in Phase 6; until then open the TOML file and watch for edits.
fn open_settings_file() {
    let p = settings::path();
    if !p.exists() {
        settings::flush();
    }
    win::shell_open(&p.display().to_string());
    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
    with_state(|s| {
        s.settings_mtime = mtime;
        s.settings_watch_ticks = 0;
    });
    // SAFETY: timer on our own message window.
    unsafe {
        SetTimer(app_hwnd(), TIMER_SETTINGS_WATCH, 1000, None);
    }
}

fn settings_watch_tick() {
    let p = settings::path();
    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
    let (changed, expired) = with_state(|s| {
        s.settings_watch_ticks += 1;
        let changed = mtime.is_some() && mtime != s.settings_mtime;
        s.settings_mtime = mtime;
        (changed, s.settings_watch_ticks > 30 * 60)
    });
    if changed {
        match settings::load_from(&p) {
            Ok(new) if new != settings::snapshot() => {
                settings::replace(new);
                app::post(AppCommand::SettingsChanged);
                log::info!("settings reloaded from disk");
            }
            Ok(_) => {}
            Err(e) => tray::notify(&i18n::t("app-name"), &format!("settings.toml: {e}"), true),
        }
    }
    if expired {
        // SAFETY: timer on our own message window.
        unsafe {
            let _ = KillTimer(app_hwnd(), TIMER_SETTINGS_WATCH);
        }
    }
}

fn show_about() {
    let text = format!(
        "{} {}\n{}\n\n{}\nhttps://zheer.dev\n\nOpen the developer's website?",
        i18n::t("app-name"),
        env!("CARGO_PKG_VERSION"),
        i18n::t("app-tagline"),
        i18n::t("app-credit"),
    );
    let (t, c) = (wide(&text), wide(&i18n::t("tray-about")));
    // SAFETY: valid NUL-terminated strings.
    let r = unsafe { MessageBoxW(HWND::default(), pcwstr(&t), pcwstr(&c), MB_YESNO | MB_ICONINFORMATION) };
    if r == IDYES {
        win::shell_open("https://zheer.dev");
    }
}

// ------------------------------------------------------------------ uninstall

fn uninstall_cleanup() -> i32 {
    cli::attach_console();
    let run_key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value = wide("SnapForge");
    // SAFETY: deleting our own autostart value; failure (not present) is fine.
    unsafe {
        let _ = windows::Win32::System::Registry::RegDeleteKeyValueW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            pcwstr(&run_key),
            pcwstr(&value),
        );
    }
    let dir = settings::dir();
    let mut ok = true;
    if settings::is_portable() {
        for f in ["settings.toml", "settings.toml.bad"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let _ = std::fs::remove_dir_all(dir.join("logs"));
    } else if dir.exists() {
        ok = std::fs::remove_dir_all(&dir).is_ok();
    }
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("SnapForge"));
    println!("SnapForge: removed settings, logs and autostart entry ({})", if ok { "ok" } else { "partial" });
    if ok {
        0
    } else {
        1
    }
}

/// Root window under a point (used by tools that need the real window, not the overlay).
pub fn root_window_at(x: i32, y: i32) -> isize {
    // SAFETY: plain queries.
    unsafe {
        let h = WindowFromPoint(POINT { x, y });
        hwnd_to_isize(GetAncestor(h, GA_ROOT))
    }
}

/// Let the user select a region (frozen-screen overlay) and return its image and screen
/// rect. Used by Pin Capture, Capture Text and other tools. `None` if cancelled or busy.
pub(crate) fn select_region_image() -> Option<(Image, crate::common::Rect)> {
    if BUSY.with(|b| b.replace(true)) {
        return None;
    }
    let opts = GrabOptions::from_settings();
    let result = freeze(&opts).ok().and_then(|frozen| {
        let sel = overlay::select(&frozen, OverlayMode::Region)?;
        let rect = match &sel {
            Selection::Rect(r) | Selection::Window { rect: r, .. } => *r,
            Selection::Polygon(p) => {
                let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                for pt in p {
                    l = l.min(pt.x);
                    t = t.min(pt.y);
                    r = r.max(pt.x);
                    b = b.max(pt.y);
                }
                crate::common::Rect::from_ltrb(l, t, r + 1, b + 1)
            }
            Selection::Multi(rs) => rs.iter().fold(crate::common::Rect::default(), |a, r| a.union(r)),
        };
        let img = overlay::render_selection(&frozen, &sel);
        (!img.is_empty()).then_some((img, rect))
    });
    BUSY.with(|b| b.set(false));
    schedule_idle();
    result
}
