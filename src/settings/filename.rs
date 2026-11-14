//! File-name templates.
//!
//! | Token | Meaning |
//! |---|---|
//! | `$Y` `$M` `$D` | year (4 digits), month, day (2 digits) |
//! | `$H` `$N` `$S` | hour (24h), minute, second (2 digits) |
//! | `$$` | a literal `$` |
//! | `###` | counter, zero-padded to the number of `#` |
//! | `@` | one random character `[a-z0-9]` |
//! | `%USERNAME%` `%COMPUTERNAME%` `%DESKTOP%` `%DOCUMENTS%` `%PICTURES%` | user / machine / folders (any env var also works) |

use super::FileNameSettings;
use crate::common::rng::Rng;
use crate::common::time::DateTime;
use crate::common::win;
use std::path::{Path, PathBuf};

const RANDOM_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Default resolver for `%NAME%` tokens.
pub fn default_env(name: &str) -> Option<String> {
    match name.to_ascii_uppercase().as_str() {
        "DESKTOP" => Some(win::desktop_dir().display().to_string()),
        "DOCUMENTS" | "MYDOCUMENTS" => Some(win::documents_dir().display().to_string()),
        "PICTURES" => Some(win::pictures_dir().display().to_string()),
        other => std::env::var(other).ok(),
    }
}

/// Expand tokens in folder settings (`%DESKTOP%\Shots\$Y-$M` works).
pub fn expand_env_tokens(s: &str) -> String {
    let mut rng = Rng::from_entropy();
    expand(s, &DateTime::now_local(), 0, &mut rng, &default_env, false)
}

fn is_invalid_name_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32
}

/// Expand a template. With `sanitize`, characters that are illegal in file names are
/// replaced by `-` (path separators are kept, so templates may contain sub-folders);
/// text inserted from `%NAME%` tokens is inserted verbatim (it may be a full path).
pub fn expand(
    template: &str,
    now: &DateTime,
    counter: u32,
    rng: &mut Rng,
    env: &dyn Fn(&str) -> Option<String>,
    sanitize: bool,
) -> String {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len() + 16);
    let push_lit = |out: &mut String, c: char| {
        if sanitize && is_invalid_name_char(c) {
            out.push('-');
        } else {
            out.push(c);
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '$' if i + 1 < chars.len() => {
                let t = chars[i + 1];
                let rep = match t {
                    'Y' => Some(format!("{:04}", now.year)),
                    'M' => Some(format!("{:02}", now.month)),
                    'D' => Some(format!("{:02}", now.day)),
                    'H' => Some(format!("{:02}", now.hour)),
                    'N' => Some(format!("{:02}", now.minute)),
                    'S' => Some(format!("{:02}", now.second)),
                    '$' => Some("$".to_string()),
                    _ => None,
                };
                match rep {
                    Some(r) => {
                        out.push_str(&r);
                        i += 2;
                    }
                    None => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            '#' => {
                let mut n = 0;
                while i + n < chars.len() && chars[i + n] == '#' {
                    n += 1;
                }
                out.push_str(&format!("{:0width$}", counter, width = n));
                i += n;
            }
            '@' => {
                out.push(RANDOM_CHARS[rng.below(RANDOM_CHARS.len() as u64) as usize] as char);
                i += 1;
            }
            '%' => {
                // Find closing '%' with a plausible variable name in between.
                let close = chars[i + 1..].iter().position(|&ch| ch == '%').map(|p| i + 1 + p);
                let name: Option<String> = close.and_then(|cl| {
                    let n: String = chars[i + 1..cl].iter().collect();
                    let ok = !n.is_empty() && n.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
                    ok.then_some(n)
                });
                match (name, close) {
                    (Some(n), Some(cl)) => {
                        match env(&n) {
                            Some(v) => out.push_str(&v),
                            None => {
                                for ch in chars[i..=cl].iter() {
                                    push_lit(&mut out, *ch);
                                }
                            }
                        }
                        i = cl + 1;
                    }
                    _ => {
                        push_lit(&mut out, '%');
                        i += 1;
                    }
                }
            }
            _ => {
                push_lit(&mut out, c);
                i += 1;
            }
        }
    }
    out
}

/// Pure path builder. Returns the chosen path and the updated counter state.
///
/// * Applies the daily counter reset.
/// * Relative results are placed in `folder`; absolute results (e.g. `%DESKTOP%\x`) are kept.
/// * Never returns an existing file: appends ` (2)`, ` (3)`… if needed.
#[allow(clippy::too_many_arguments)]
pub fn build_path(
    fs: &FileNameSettings,
    folder: &Path,
    ext: &str,
    now: &DateTime,
    rng: &mut Rng,
    env: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&Path) -> bool,
) -> (PathBuf, FileNameSettings) {
    let mut next = fs.clone();
    let today = now.date_key();
    if next.reset_daily && next.last_date != today {
        next.counter = 1;
    }
    let uses_counter = fs.template.contains('#');
    let mut name = expand(&fs.template, now, next.counter, rng, env, true);
    if name.trim().is_empty() {
        name = "capture".into();
    }
    let rel = PathBuf::from(&name);
    let base = if rel.is_absolute() { rel } else { folder.join(rel) };
    let with_ext = |p: &Path, suffix: &str| -> PathBuf {
        let mut s = p.as_os_str().to_os_string();
        s.push(suffix);
        s.push(".");
        s.push(ext);
        PathBuf::from(s)
    };
    let mut candidate = with_ext(&base, "");
    let mut n = 2;
    while exists(&candidate) && n < 100_000 {
        candidate = with_ext(&base, &format!(" ({n})"));
        n += 1;
    }
    if uses_counter {
        next.counter = next.counter.saturating_add(1);
    }
    next.last_date = today;
    (candidate, next)
}

/// Next output path using the live settings; persists the counter.
pub fn next_output_path(folder: &Path, ext: &str) -> PathBuf {
    let fs = super::get().filename.clone();
    let mut rng = Rng::from_entropy();
    let now = DateTime::now_local();
    let (p, next) = build_path(&fs, folder, ext, &now, &mut rng, &default_env, &|p| p.exists());
    if next != fs {
        super::update(|s| s.filename = next);
    }
    p
}

/// Text for the optional date/time stamp drawn on captures.
pub fn timestamp_text(format: &str) -> String {
    let mut rng = Rng::from_entropy();
    expand(format, &DateTime::now_local(), 0, &mut rng, &default_env, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> DateTime {
        DateTime { year: 2026, month: 3, day: 7, hour: 9, minute: 5, second: 2, millis: 0 }
    }

    fn env(n: &str) -> Option<String> {
        match n {
            "USERNAME" => Some("alice".into()),
            "COMPUTERNAME" => Some("BOX".into()),
            "DESKTOP" => Some("C:\\Users\\alice\\Desktop".into()),
            _ => None,
        }
    }

    fn ex(tpl: &str, counter: u32) -> String {
        let mut rng = Rng::new(42);
        expand(tpl, &t(), counter, &mut rng, &env, true)
    }

    #[test]
    fn date_tokens() {
        assert_eq!(ex("$Y-$M-$D $H.$N.$S", 0), "2026-03-07 09.05.02");
        assert_eq!(ex("cost$$", 0), "cost$");
        assert_eq!(ex("$X", 0), "$X");
    }

    #[test]
    fn counter_padding() {
        assert_eq!(ex("img###", 7), "img007");
        assert_eq!(ex("#", 12), "12");
        assert_eq!(ex("a##b#", 3), "a03b3");
    }

    #[test]
    fn random_chars() {
        let s = ex("@@@@", 0);
        assert_eq!(s.len(), 4);
        assert!(s.bytes().all(|b| RANDOM_CHARS.contains(&b)));
        // Deterministic for a fixed seed.
        assert_eq!(s, ex("@@@@", 0));
    }

    #[test]
    fn env_tokens() {
        assert_eq!(ex("%USERNAME%@%COMPUTERNAME%", 0).len(), "alice".len() + 1 + "BOX".len());
        assert!(ex("%USERNAME%_x", 0).starts_with("alice_"));
        assert_eq!(ex("%NOPE%", 0), "%NOPE%");
        assert_eq!(ex("100%", 0), "100%");
    }

    #[test]
    fn sanitizes_but_keeps_env_paths() {
        assert_eq!(ex("a:b?c*", 0), "a-b-c-");
        assert_eq!(ex("%DESKTOP%\\shot", 0), "C:\\Users\\alice\\Desktop\\shot");
    }

    #[test]
    fn build_path_counter_and_daily_reset() {
        let fs = FileNameSettings {
            template: "cap_###".into(),
            counter: 5,
            reset_daily: true,
            last_date: "2026-03-07".into(),
        };
        let mut rng = Rng::new(1);
        let folder = Path::new("C:\\out");
        let (p, next) = build_path(&fs, folder, "png", &t(), &mut rng, &env, &|_| false);
        assert_eq!(p, PathBuf::from("C:\\out\\cap_005.png"));
        assert_eq!(next.counter, 6);

        let old = FileNameSettings { last_date: "2026-03-06".into(), ..fs.clone() };
        let (p, next) = build_path(&old, folder, "png", &t(), &mut rng, &env, &|_| false);
        assert_eq!(p, PathBuf::from("C:\\out\\cap_001.png"));
        assert_eq!(next.counter, 2);
        assert_eq!(next.last_date, "2026-03-07");

        let no_reset = FileNameSettings { reset_daily: false, ..old };
        let (p, _) = build_path(&no_reset, folder, "png", &t(), &mut rng, &env, &|_| false);
        assert_eq!(p, PathBuf::from("C:\\out\\cap_005.png"));
    }

    #[test]
    fn build_path_avoids_existing_and_absolute() {
        let fs = FileNameSettings { template: "shot".into(), counter: 1, reset_daily: false, last_date: String::new() };
        let mut rng = Rng::new(1);
        let taken = |p: &Path| p == Path::new("C:\\out\\shot.png") || p == Path::new("C:\\out\\shot (2).png");
        let (p, next) = build_path(&fs, Path::new("C:\\out"), "png", &t(), &mut rng, &env, &taken);
        assert_eq!(p, PathBuf::from("C:\\out\\shot (3).png"));
        assert_eq!(next.counter, 1, "no ### in template → counter unchanged");

        let abs = FileNameSettings { template: "%DESKTOP%\\x".into(), ..fs };
        let (p, _) = build_path(&abs, Path::new("C:\\out"), "jpg", &t(), &mut rng, &env, &|_| false);
        assert_eq!(p, PathBuf::from("C:\\Users\\alice\\Desktop\\x.jpg"));
    }
}
