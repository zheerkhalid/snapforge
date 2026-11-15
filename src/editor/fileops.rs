//! File-level helpers for the editor: Recycle Bin delete, file properties, sequence
//! names for "Save All", temp copies and the change watcher behind "Edit With".
//! Everything that touches the disk is called from worker threads.

use crate::common::win::{pcwstr, wide};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Move a file to the Recycle Bin (undoable from Explorer). Blocking: call on a worker.
pub fn recycle(path: &Path) -> Result<(), String> {
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW,
    };
    if !path.is_file() {
        return Err(format!("{} does not exist", path.display()));
    }
    // pFrom is a list of NUL-terminated strings ending with an extra NUL.
    let mut from = wide(&path.to_string_lossy());
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: pcwstr(&from),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_NOERRORUI.0 | FOF_SILENT.0) as u16,
        ..Default::default()
    };
    // SAFETY: `from` outlives the call; the struct is fully initialised.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        return Err(format!("the file could not be deleted (code {rc:#x})"));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err("the delete was cancelled".into());
    }
    Ok(())
}

/// Facts for the File Properties dialog.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileProps {
    pub path: Option<PathBuf>,
    pub size_bytes: Option<u64>,
    pub format: String,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub width: u32,
    pub height: u32,
    pub has_alpha: bool,
    /// Filled in by a worker (counting colours of a 4K image takes a moment).
    pub colors: Option<u64>,
}

/// Read file metadata (blocking: call on a worker).
pub fn file_props(path: Option<&Path>, width: u32, height: u32, has_alpha: bool) -> FileProps {
    let mut p = FileProps { width, height, has_alpha, path: path.map(Path::to_path_buf), ..Default::default() };
    if let Some(path) = path {
        if let Ok(m) = std::fs::metadata(path) {
            p.size_bytes = Some(m.len());
            p.created = m.created().ok().and_then(local_time_string);
            p.modified = m.modified().ok().and_then(local_time_string);
        }
        let head = read_head(path).unwrap_or_default();
        p.format = sniff_format(&head)
            .map(str::to_string)
            .unwrap_or_else(|| path.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default());
    }
    p
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 16];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// Image format from the first bytes of a file.
pub fn sniff_format(b: &[u8]) -> Option<&'static str> {
    Some(match b {
        [0x89, b'P', b'N', b'G', ..] => "PNG",
        [0xFF, 0xD8, 0xFF, ..] => "JPEG",
        [b'G', b'I', b'F', b'8', ..] => "GIF",
        [b'B', b'M', ..] => "BMP",
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => "TIFF",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "WebP",
        [0, 0, 1, 0, ..] => "ICO",
        [0x0A, 0..=5, 1, ..] => "PCX",
        _ => return None,
    })
}

/// `YYYY-MM-DD HH:MM:SS` in local time.
pub fn local_time_string(t: SystemTime) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME counts 100 ns intervals since 1601-01-01.
    let since_unix = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let ticks = since_unix.as_nanos() / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out pointers.
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    ))
}

/// `1.4 MB`, `820 KB`, `12 bytes`.
pub fn format_bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f >= K * K * K {
        format!("{:.2} GB", f / (K * K * K))
    } else if f >= K * K {
        format!("{:.1} MB", f / (K * K))
    } else if f >= K {
        format!("{:.0} KB", f / K)
    } else {
        format!("{n} bytes")
    }
}

/// `Base_001` style names for "Save All with sequence".
pub fn sequence_name(base: &str, n: u32, digits: u32) -> String {
    let base = base.trim();
    let sep = if base.is_empty() || base.ends_with(['_', '-', ' ']) { "" } else { "_" };
    format!("{base}{sep}{n:0width$}", width = digits.clamp(1, 9) as usize)
}

/// File names for `count` images; numbers already used in `folder` are skipped so
/// nothing is overwritten.
pub fn plan_sequence(
    count: usize,
    base: &str,
    start: u32,
    digits: u32,
    ext: &str,
    exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::with_capacity(count);
    let mut n = start;
    while out.len() < count {
        let name = format!("{}.{ext}", sequence_name(base, n, digits));
        n = n.saturating_add(1);
        if !exists(&name) {
            out.push(name);
        }
        if n == u32::MAX {
            break;
        }
    }
    out
}

/// Remove characters Windows does not allow in file names.
pub fn sanitize_name(s: &str) -> String {
    let s: String = s.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() {
        "Untitled".into()
    } else {
        s
    }
}

/// `%TEMP%\SnapForge\<sub>`, created on demand.
pub fn temp_dir(sub: &str) -> PathBuf {
    let d = std::env::temp_dir().join("SnapForge").join(sub);
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Size and modification time of a watched file.
type Stamp = Option<(u64, Option<SystemTime>)>;

/// Polls a file's size and modification time on its own thread and calls `on_change`
/// once the file changed and then stayed stable for a moment (external editors often
/// write in several steps). Dropping the watcher stops the thread.
pub struct FileWatcher {
    stop: Arc<AtomicBool>,
}

impl FileWatcher {
    pub fn start(path: PathBuf, on_change: impl Fn() + Send + 'static) -> FileWatcher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let p = path.clone();
        let res = std::thread::Builder::new().name("editor-watch".into()).spawn(move || {
            let stamp = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
            let mut last = stamp(&p);
            let mut pending: Option<(Stamp, u32)> = None;
            while !flag.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(400));
                let now = stamp(&p);
                match &mut pending {
                    None if now != last && now.is_some() => pending = Some((now, 0)),
                    Some((seen, n)) => {
                        if now != *seen {
                            *seen = now;
                            *n = 0;
                        } else {
                            *n += 1;
                            if *n >= 2 {
                                last = now;
                                pending = None;
                                on_change();
                            }
                        }
                    }
                    None => {}
                }
            }
        });
        if let Err(e) = res {
            log::warn!("editor: cannot watch {}: {e}", path.display());
        }
        FileWatcher { stop }
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_names() {
        assert_eq!(sequence_name("Shot", 7, 3), "Shot_007");
        assert_eq!(sequence_name("Shot-", 12, 2), "Shot-12");
        assert_eq!(sequence_name("Shot", 1234, 2), "Shot_1234");
        assert_eq!(sequence_name("", 1, 0), "1");
        let taken = ["Shot_002.png", "Shot_003.png"];
        let names = plan_sequence(3, "Shot", 1, 3, "png", |n| taken.contains(&n));
        assert_eq!(names, ["Shot_001.png", "Shot_004.png", "Shot_005.png"]);
        assert!(plan_sequence(0, "a", 1, 3, "png", |_| false).is_empty());
    }

    #[test]
    fn sniffing_and_sizes() {
        assert_eq!(sniff_format(&[0x89, b'P', b'N', b'G', 13, 10]), Some("PNG"));
        assert_eq!(sniff_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("WebP"));
        assert_eq!(sniff_format(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("JPEG"));
        assert_eq!(sniff_format(b"hello"), None);
        assert_eq!(format_bytes(12), "12 bytes");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024 / 2), "1.5 MB");
        assert_eq!(sanitize_name(" a:b?. "), "a_b_");
        assert_eq!(sanitize_name("..."), "Untitled");
    }

    #[test]
    fn props_and_time() {
        let dir = temp_dir("test-props");
        let p = dir.join("x.png");
        std::fs::write(&p, [0x89, b'P', b'N', b'G', 13, 10, 26, 10]).expect("write");
        let props = file_props(Some(&p), 3, 4, false);
        assert_eq!(props.format, "PNG");
        assert_eq!(props.size_bytes, Some(8));
        assert!(props.modified.as_deref().is_some_and(|s| s.len() == 19));
        let none = file_props(None, 1, 1, true);
        assert!(none.path.is_none() && none.size_bytes.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn watcher_reports_changes() {
        let dir = temp_dir("test-watch");
        let p = dir.join(format!("w{}.txt", std::process::id()));
        std::fs::write(&p, "a").expect("write");
        let (tx, rx) = std::sync::mpsc::channel();
        let w = FileWatcher::start(p.clone(), move || {
            let _ = tx.send(());
        });
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(&p, "bbbb").expect("write");
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        drop(w);
        let _ = std::fs::remove_file(&p);
    }
}
