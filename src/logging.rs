//! Rotating file logger: `<settings dir>\logs\snapforge.log`, rotated at 1 MiB, 3 old files kept.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 1024 * 1024;
const KEEP: usize = 3;

struct FileLogger {
    path: PathBuf,
    file: Mutex<Option<File>>,
    level: log::LevelFilter,
}

impl FileLogger {
    fn rotate_if_needed(&self, guard: &mut Option<File>) {
        let too_big = guard.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > MAX_BYTES);
        if !too_big {
            return;
        }
        *guard = None;
        for i in (1..KEEP).rev() {
            let _ = std::fs::rename(
                self.path.with_extension(format!("log.{i}")),
                self.path.with_extension(format!("log.{}", i + 1)),
            );
        }
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        *guard = OpenOptions::new().create(true).append(true).open(&self.path).ok();
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        // Third-party crates only at warn and above.
        if !r.target().starts_with("snapforge") && !r.target().starts_with("SnapForge") && r.level() > log::Level::Warn
        {
            return;
        }
        let now = crate::common::time::DateTime::now_local();
        let line = format!(
            "{}.{:03} {:5} [{}] {}\r\n",
            now.display(),
            now.millis,
            r.level(),
            std::thread::current().name().unwrap_or("?"),
            r.args()
        );
        if let Ok(mut g) = self.file.lock() {
            self.rotate_if_needed(&mut g);
            if let Some(f) = g.as_mut() {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut g) = self.file.lock() {
            if let Some(f) = g.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

pub fn log_dir() -> PathBuf {
    crate::settings::dir().join("logs")
}

/// Install the logger. Best-effort: failures are silent.
pub fn init(level: &str) {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("snapforge.log");
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let level = level.parse().unwrap_or(log::LevelFilter::Info);
    let logger = Box::new(FileLogger { path, file: Mutex::new(file), level });
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level);
    }
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
        log::logger().flush();
    }));
}
