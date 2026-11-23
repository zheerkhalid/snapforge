//! Command-line interface (automation).
//!
//! ```text
//! SnapForge.exe --capture region --out clipboard
//! SnapForge.exe --capture fullscreen --file "C:\shots\screen.png"
//! SnapForge.exe --open a.png b.jpg
//! SnapForge.exe --open recording.mp4
//! ```

use crate::settings::{CaptureMode, Destination};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cli {
    pub capture: Option<CaptureMode>,
    pub out: Option<Destination>,
    pub file: Option<PathBuf>,
    pub delay: Option<u32>,
    pub open: Vec<PathBuf>,
    pub tray: bool,
    pub show_toolbar: bool,
    pub bench: bool,
    pub bench_child: bool,
    pub uninstall_cleanup: bool,
    pub settings_dir: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
}

pub const HELP: &str = "\
SnapForge - fast screen capture, annotation and recording
Developed by Zheer Khalid - https://zheer.dev

USAGE:
  SnapForge.exe [OPTIONS]

CAPTURE:
  --capture <mode>       active | window | region | freehand | fullscreen | scrolling | fixed
  --out <destination>    editor | draw | clipboard | file | printer | email | word | excel |
                         powerpoint | onenote | upload | program   (default: settings)
  --file <path>          save to this file (implies --out file; format from extension)
  --delay <seconds>      wait before capturing (shows a countdown)

OTHER:
  --open <files...>      open images or .sfcap projects in the editor, and
                         .mp4 / .wmv videos or .sfvid projects in the video editor
  --tray                 start in the tray without showing the toolbar
  --toolbar              show the capture toolbar
  --bench                measure and print the performance budgets
  --settings-dir <dir>   use another settings folder
  --uninstall-cleanup    remove settings, logs and the autostart entry
  --version, --help

If SnapForge is already running, the command is forwarded to the running instance.
";

#[derive(Debug, PartialEq, Eq)]
pub struct CliError(pub String);

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn parse(args: &[String]) -> Result<Cli, CliError> {
    let mut c = Cli::default();
    let mut i = 0;
    let value = |i: &mut usize, name: &str| -> Result<String, CliError> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| CliError(format!("{name} needs a value")))
    };
    while i < args.len() {
        let a = args[i].as_str();
        let key = a.trim_start_matches('-').to_ascii_lowercase();
        match key.as_str() {
            "capture" | "c" => {
                let v = value(&mut i, a)?;
                c.capture =
                    Some(CaptureMode::from_id(&v).ok_or_else(|| CliError(format!("unknown capture mode '{v}'")))?);
            }
            "out" | "o" => {
                let v = value(&mut i, a)?;
                c.out = Some(Destination::from_id(&v).ok_or_else(|| CliError(format!("unknown destination '{v}'")))?);
            }
            "file" | "f" => {
                c.file = Some(PathBuf::from(value(&mut i, a)?));
            }
            "delay" | "d" => {
                let v = value(&mut i, a)?;
                c.delay = Some(v.parse().map_err(|_| CliError(format!("invalid delay '{v}'")))?);
            }
            "open" => {
                while i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    c.open.push(PathBuf::from(&args[i]));
                }
            }
            "tray" | "minimized" => c.tray = true,
            "toolbar" => c.show_toolbar = true,
            "bench" => c.bench = true,
            "bench-child" => c.bench_child = true,
            "uninstall-cleanup" => c.uninstall_cleanup = true,
            "settings-dir" => c.settings_dir = Some(PathBuf::from(value(&mut i, a)?)),
            "help" | "h" | "?" => c.help = true,
            "version" | "v" => c.version = true,
            _ if !a.starts_with('-') => c.open.push(PathBuf::from(a)),
            _ => return Err(CliError(format!("unknown option '{a}' (see --help)"))),
        }
        i += 1;
    }
    if c.file.is_some() && c.out.is_none() {
        c.out = Some(Destination::File);
    }
    if c.file.is_some() && c.capture.is_none() {
        return Err(CliError("--file needs --capture".into()));
    }
    if let Some(f) = &c.file {
        // Without a known extension the save would fail later, where nobody sees it.
        let ext = f.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if crate::settings::ImageFormat::from_extension(&ext).is_none() {
            return Err(CliError(format!(
                "--file needs an image file name (png, jpg, bmp, gif, tif, tga, pcx or webp): '{}'",
                f.display()
            )));
        }
    }
    Ok(c)
}

/// `args` with every file argument made absolute against `cwd` (the values of `--file`,
/// `--open` and `--settings-dir`, and bare file names). A command line forwarded to the
/// running instance is parsed there, where relative paths would otherwise resolve against
/// that instance's working folder.
pub fn absolutize(args: &[String], cwd: &std::path::Path) -> Vec<String> {
    let abs = |a: &str| -> String {
        let p = std::path::Path::new(a);
        if a.is_empty() || p.is_absolute() || p.has_root() {
            a.to_string()
        } else {
            cwd.join(p).to_string_lossy().into_owned()
        }
    };
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if !a.starts_with('-') {
            out.push(abs(a));
            i += 1;
            continue;
        }
        out.push(a.to_string());
        match a.trim_start_matches('-').to_ascii_lowercase().as_str() {
            "file" | "f" | "settings-dir" => {
                if let Some(v) = args.get(i + 1) {
                    out.push(abs(v));
                    i += 1;
                }
            }
            // Plain values: copied as they are.
            "capture" | "c" | "out" | "o" | "delay" | "d" => {
                if let Some(v) = args.get(i + 1) {
                    out.push(v.clone());
                    i += 1;
                }
            }
            // `--open a b c`: the files that follow are bare arguments, handled above.
            _ => {}
        }
        i += 1;
    }
    out
}

/// Make console output visible when launched from a terminal (GUI-subsystem EXE).
pub fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain call; fails harmlessly when there is no parent console.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cli, CliError> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn file_needs_a_known_image_extension() {
        assert!(p("--capture fullscreen --file C:\\shots\\out").is_err());
        assert!(p("--capture fullscreen --file out.txt").is_err());
        for ok in ["a.png", "a.JPG", "a.jpeg", "a.bmp", "a.gif", "a.tif", "a.tga", "a.pcx", "a.webp"] {
            assert!(p(&format!("--capture fullscreen --file {ok}")).is_ok(), "{ok}");
        }
    }

    #[test]
    fn forwarded_paths_become_absolute() {
        let cwd = std::path::Path::new("D:\\work");
        let abs = |s: &str| absolutize(&s.split_whitespace().map(String::from).collect::<Vec<_>>(), cwd).join(" ");
        assert_eq!(abs("--capture region --file shot.png"), "--capture region --file D:\\work\\shot.png");
        assert_eq!(abs("-c region -f sub\\shot.png -d 3"), "-c region -f D:\\work\\sub\\shot.png -d 3");
        assert_eq!(abs("--open a.png b.sfcap --toolbar"), "--open D:\\work\\a.png D:\\work\\b.sfcap --toolbar");
        assert_eq!(abs("picture.png"), "D:\\work\\picture.png");
        // Already absolute (drive or UNC), option values and flags stay as they are.
        assert_eq!(abs("--open C:\\x\\a.png \\\\server\\share\\b.png"), "--open C:\\x\\a.png \\\\server\\share\\b.png");
        assert_eq!(abs("--capture window --out clipboard --delay 2"), "--capture window --out clipboard --delay 2");
        assert_eq!(abs("--settings-dir cfg --tray"), "--settings-dir D:\\work\\cfg --tray");
        assert_eq!(abs("--file"), "--file");
        assert!(absolutize(&[], cwd).is_empty());
        // What the running instance parses is unchanged apart from the paths.
        let fwd = absolutize(&["--capture".into(), "active".into(), "--file".into(), "s.png".into()], cwd);
        let c = parse(&fwd).expect("parse");
        assert_eq!(c.file, Some(PathBuf::from("D:\\work\\s.png")));
        assert_eq!(c.capture, Some(CaptureMode::ActiveWindow));
    }

    #[test]
    fn capture_and_out() {
        let c = p("--capture region --out clipboard").expect("parse");
        assert_eq!(c.capture, Some(CaptureMode::Region));
        assert_eq!(c.out, Some(Destination::Clipboard));
    }

    #[test]
    fn file_implies_out_file() {
        let c = p("--capture fullscreen --file C:\\x.png --delay 3").expect("parse");
        assert_eq!(c.out, Some(Destination::File));
        assert_eq!(c.delay, Some(3));
        assert!(p("--file C:\\x.png").is_err());
    }

    #[test]
    fn open_and_bare_paths() {
        let c = p("--open a.png b.png --tray").expect("parse");
        assert_eq!(c.open.len(), 2);
        assert!(c.tray);
        assert_eq!(p("c.png").expect("parse").open, vec![PathBuf::from("c.png")]);
    }

    #[test]
    fn errors() {
        assert!(p("--capture nope").is_err());
        assert!(p("--out").is_err());
        assert!(p("--frobnicate").is_err());
    }
}
