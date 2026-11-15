//! `SnapForge.exe --bench`: measures the performance budgets.
//!
//! The parent spawns a child (`--bench-child`, fresh settings folder) that starts exactly
//! like a normal launch, then drives itself through the real code paths and prints
//! `KEY value` lines on stdout. The parent formats the report.

use crate::app::{self, AppCommand};
use crate::common::time::{qpc_ms, qpc_now};
use crate::common::win;
use crate::settings::CaptureMode;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ parent

pub fn run_parent() -> i32 {
    let exe = win::exe_path();
    let dir = std::env::temp_dir().join(format!("SnapForge-bench-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    println!("SnapForge {} performance report", env!("CARGO_PKG_VERSION"));
    println!("(a temporary instance runs with default settings; please don't touch the mouse or keyboard)\n");
    let spawn_at = Instant::now();
    let child = Command::new(&exe)
        .arg("--bench-child")
        .arg("--settings-dir")
        .arg(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not start the benchmark child: {e}");
            return 1;
        }
    };
    let mut lines: Vec<(String, String)> = Vec::new();
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let (k, v) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            if k == "DONE" {
                break;
            }
            if k == "READY" {
                lines.push(("SPAWN_TO_READY".into(), format!("{:.1}", spawn_at.elapsed().as_secs_f64() * 1000.0)));
            }
            lines.push((k.to_string(), v.to_string()));
            if spawn_at.elapsed() > Duration::from_secs(120) {
                break;
            }
        }
    }
    let _ = child.wait_timeout_or_kill(Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);

    let get = |k: &str| lines.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let num = |k: &str| get(k).and_then(|v| v.split_whitespace().next().and_then(|n| n.parse::<f64>().ok()));
    let row = |name: &str, measured: String, target: &str, pass: Option<bool>| {
        let verdict = match pass {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "-",
        };
        println!("{name:<44} {measured:<28} {target:<12} {verdict}");
    };
    println!("{:<44} {:<28} {:<12} Result", "Budget", "Measured", "Target");
    println!("{}", "-".repeat(92));

    match num("READY") {
        Some(ms) => row(
            "Cold start -> tray ready",
            format!("{ms:.0} ms (wall {:.0} ms)", num("SPAWN_TO_READY").unwrap_or(0.0)),
            "< 300 ms",
            Some(ms < 300.0),
        ),
        None => row("Cold start -> tray ready", "no data".into(), "< 300 ms", Some(false)),
    }
    if let Some(v) = get("MEM_IDLE") {
        let mut it = v.split_whitespace().filter_map(|x| x.parse::<f64>().ok());
        let (ws, pb) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0));
        row("Idle RAM in tray (working set / private)", format!("{ws:.1} MB / {pb:.1} MB"), "< 30 MB", Some(ws < 30.0));
    }
    if let Some(v) = get("OVERLAY") {
        let mut xs: Vec<f64> = v.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        xs.sort_by(|a, b| a.total_cmp(b));
        if xs.is_empty() {
            row("Hotkey -> capture overlay visible", "no data (overlay not painted)".into(), "< 80 ms", Some(false));
        } else {
            let med = xs[xs.len() / 2];
            row(
                "Hotkey -> capture overlay visible",
                format!("{med:.0} ms median (n={})", xs.len()),
                "< 80 ms",
                Some(med < 80.0),
            );
        }
    }
    if let Some(v) = get("EDITOR") {
        let mut it = v.split_whitespace();
        let cold: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
        let warm: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
        let size = it.next().unwrap_or("?");
        row(
            &format!("Capture -> editor showing image ({size})"),
            format!("{warm:.0} ms warm, {cold:.0} ms first open"),
            "< 150 ms",
            Some(warm < 150.0),
        );
    }
    if let Some(v) = get("MEM_EDITOR") {
        row("RAM with editor open", format!("{v} MB (working set)"), "-", None);
    }
    let size_mb = std::fs::metadata(&exe).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
    row("Portable EXE size", format!("{size_mb:.2} MB"), "< 20 MB", Some(size_mb < 20.0));
    match get("REC60") {
        Some(v) => {
            let f: Vec<&str> = v.split_whitespace().collect();
            let total: f64 = f.first().and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
            let core: f64 = f.get(1).and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
            let dropped: u64 = f.get(2).and_then(|x| x.parse().ok()).unwrap_or(u64::MAX);
            let frames = f.get(3).copied().unwrap_or("?");
            let size = f.get(4).copied().unwrap_or("?");
            let enc = f.get(5).copied().unwrap_or("?");
            row(
                &format!("{size}@60 recording CPU ({enc} encoder)"),
                format!("{total:.1} % ({core:.0} % of a core)"),
                "< 10 %",
                Some(total < 10.0),
            );
            row("Recording dropped frames (5 s at 60 fps)", format!("{dropped} of {frames}"), "0", Some(dropped == 0));
        }
        None => {
            let why = get("REC60_ERR").unwrap_or_else(|| "no data".into());
            row("1080p60 recording CPU", format!("failed: {why}"), "< 10 %", Some(false));
        }
    }
    if let Some(ms) = num("STITCH20") {
        row(
            "Scrolling capture, 20 screens",
            format!("stitching {ms:.0} ms (+ scroll waits)"),
            "< 5 s",
            Some(ms < 5000.0),
        );
    }
    let engines: Vec<String> = lines.iter().filter(|(k, _)| k == "ENGINE").map(|(_, v)| v.clone()).collect();
    if !engines.is_empty() {
        println!("\nCapture engines (full primary monitor):");
        for e in engines {
            println!("  {e}");
        }
    }
    if cfg!(debug_assertions) {
        println!("\nNote: this is a DEBUG build; measure with `cargo build --release`.");
    }
    0
}

trait WaitKill {
    fn wait_timeout_or_kill(&mut self, d: Duration) -> std::io::Result<()>;
}

impl WaitKill for std::process::Child {
    fn wait_timeout_or_kill(&mut self, d: Duration) -> std::io::Result<()> {
        let start = Instant::now();
        while start.elapsed() < d {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.kill()
    }
}

// ------------------------------------------------------------------ child

fn emit(key: &str, value: impl std::fmt::Display) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{key} {value}");
    let _ = out.flush();
}

/// Milliseconds since this process was created.
fn ms_since_process_start() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid out pointers for our own process.
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).is_err() {
            return f64::NAN;
        }
        let now = GetSystemTimeAsFileTime();
        let to64 = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        (to64(now).saturating_sub(to64(c))) as f64 / 10_000.0
    }
}

/// (working set MB, private bytes MB)
pub fn memory_mb() -> (f64, f64) {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut pmc = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: the EX struct begins with PROCESS_MEMORY_COUNTERS and cb is set accordingly.
    unsafe {
        let _ = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            pmc.cb,
        );
    }
    (pmc.WorkingSetSize as f64 / 1_048_576.0, pmc.PrivateUsage as f64 / 1_048_576.0)
}

fn wait_change(atom: &std::sync::atomic::AtomicI64, old: i64, timeout: Duration) -> Option<i64> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let v = atom.load(Ordering::Acquire);
        if v != old {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

fn hide_editor_window() {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_HIDE};
    let title = win::wide("SnapForge Editor");
    // SAFETY: plain window lookup and hide.
    unsafe {
        if let Ok(h) = FindWindowW(windows::core::PCWSTR::null(), win::pcwstr(&title)) {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// Kernel + user CPU time of this process, in 100 ns units.
fn process_cpu_100ns() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid out pointers for our own process.
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    f(k) + f(u)
}

fn logical_cpus() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

/// CPU use while recording: `cpu_100ns` over `wall_secs`, as (% of all logical CPUs,
/// % of one core).
pub fn cpu_percent(cpu_100ns: u64, wall_secs: f64, cpus: u32) -> (f64, f64) {
    if wall_secs <= 0.0 {
        return (0.0, 0.0);
    }
    let core = cpu_100ns as f64 / 1e7 / wall_secs * 100.0;
    (core / cpus.max(1) as f64, core)
}

/// Record 5 s of the primary monitor (at most 1920×1080) at 60 fps without audio, the same
/// way a user recording runs (control bar hidden). Returns
/// `"<% total> <% one core> <dropped> <frames> <WxH> <hw|sw> <backend>"`.
fn bench_recording(monitor: crate::common::Rect) -> anyhow::Result<String> {
    use crate::recorder::{self, Area, StartOptions};
    let rect = crate::common::Rect::new(monitor.x, monitor.y, monitor.w.min(1920), monitor.h.min(1080));
    let rs = crate::settings::recorder::RecorderSettings {
        fps: 60,
        record_system_audio: false,
        record_microphone: false,
        ..crate::settings::get().recorder.clone()
    };
    let out = std::env::temp_dir().join(format!("SnapForge-bench-rec-{}.mp4", std::process::id()));
    let opts = StartOptions { out_path: Some(out.clone()), headless: true, settings: Some(rs), sleep_after: false };
    recorder::start_with(Some(Area::Rect(rect)), opts)?;
    // Let the pipeline settle (encoder start, first frames) before measuring.
    std::thread::sleep(Duration::from_millis(500));
    let (cpu0, t0) = (process_cpu_100ns(), Instant::now());
    std::thread::sleep(Duration::from_secs(5));
    let (cpu1, wall) = (process_cpu_100ns(), t0.elapsed().as_secs_f64());
    let status = recorder::status();
    let finished = recorder::stop_blocking();
    let _ = std::fs::remove_file(&out);
    let f = finished?;
    let _ = std::fs::remove_file(&f.path);
    let (total, core) = cpu_percent(cpu1.saturating_sub(cpu0), wall, logical_cpus());
    Ok(format!(
        "{total:.1} {core:.1} {} {} {}x{} {} {}",
        status.dropped.max(f.dropped),
        f.frames,
        rect.w & !1,
        rect.h & !1,
        if f.hardware_encoder { "hw" } else { "sw" },
        f.backend
    ))
}

/// Called on the UI thread right before the message loop starts.
pub fn start_child() {
    emit("READY", format!("{:.1}", ms_since_process_start()));
    let _ = std::thread::Builder::new().name("bench".into()).spawn(child_script);
}

fn child_script() {
    std::thread::sleep(Duration::from_millis(1500));
    let (ws, pb) = memory_mb();
    emit("MEM_IDLE", format!("{ws:.1} {pb:.1}"));

    // Hotkey -> overlay: the same command a hotkey posts.
    crate::overlay::BENCH_AUTOCANCEL.store(true, Ordering::Release);
    let mut samples = Vec::new();
    for _ in 0..5 {
        let before = crate::overlay::LAST_FIRST_PAINT_QPC.load(Ordering::Acquire);
        let t0 = qpc_now();
        app::post(AppCommand::Capture(CaptureMode::Region));
        if let Some(painted) = wait_change(&crate::overlay::LAST_FIRST_PAINT_QPC, before, Duration::from_secs(3)) {
            samples.push(format!("{:.1}", qpc_ms(t0, painted)));
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    crate::overlay::BENCH_AUTOCANCEL.store(false, Ordering::Release);
    emit("OVERLAY", samples.join(" "));

    // Capture -> editor (cold, then warm after hide).
    let opts = crate::capture::GrabOptions::from_settings();
    let rect = win::monitors().into_iter().find(|m| m.primary).map(|m| m.rect).unwrap_or_else(win::virtual_screen);
    if let Ok(img) = crate::capture::grab_rect(rect, &opts) {
        let size = format!("{}x{}", img.width, img.height);
        let mut times = Vec::new();
        for i in 0..2 {
            if i == 1 {
                hide_editor_window();
                std::thread::sleep(Duration::from_millis(600));
            }
            let before = crate::editor::LAST_IMAGE_PRESENTED_QPC.load(Ordering::Acquire);
            let t0 = qpc_now();
            crate::editor::open_image(img.clone(), "bench".into(), None, None);
            match wait_change(&crate::editor::LAST_IMAGE_PRESENTED_QPC, before, Duration::from_secs(10)) {
                Some(t) => times.push(qpc_ms(t0, t)),
                None => times.push(f64::NAN),
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        emit("EDITOR", format!("{:.1} {:.1} {size}", times[0], times.get(1).copied().unwrap_or(f64::NAN)));
        emit("MEM_EDITOR", format!("{:.1}", memory_mb().0));
        hide_editor_window();

        for (name, r) in crate::capture::bench_engines(rect) {
            match r {
                Ok(ms) => emit("ENGINE", format!("{name}: {ms:.1} ms")),
                Err(e) => emit("ENGINE", format!("{name}: unavailable ({e})")),
            }
        }
    } else {
        emit("EDITOR", "NaN NaN capture-failed");
    }

    emit("STITCH20", format!("{:.1}", crate::capture::stitch::bench_synthetic(20)));
    match bench_recording(rect) {
        Ok(v) => emit("REC60", v),
        Err(e) => emit("REC60_ERR", format!("{e:#}")),
    }
    emit("DONE", "");
    app::post(AppCommand::Exit);
}

#[cfg(test)]
mod tests {
    use super::cpu_percent;

    #[test]
    fn cpu_share() {
        // 1 s of CPU over 5 s on 8 logical CPUs: 20 % of a core, 2.5 % of the machine.
        let (total, core) = cpu_percent(10_000_000, 5.0, 8);
        assert!((core - 20.0).abs() < 1e-9 && (total - 2.5).abs() < 1e-9);
        assert_eq!(cpu_percent(5, 0.0, 4), (0.0, 0.0));
    }
}
