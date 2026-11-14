//! Command-line interface (automation).
//!
//! ```text
//! SnapForge.exe --capture region --out clipboard
//! SnapForge.exe --capture fullscreen --file "C:\shots\screen.png"
//! SnapForge.exe --open a.png b.jpg
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
  --open <files...>      open images in the editor
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
    Ok(c)
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
