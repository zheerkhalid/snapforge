//! UI strings from Fluent (`.ftl`) files.
//!
//! Supports the Fluent subset SnapForge uses: `key = value`, multi-line values (indented
//! continuation lines), comments, and `{ $var }` placeholders. English is embedded; other
//! locales load from `<exe dir>\locales\<lang>\main.ftl` when present.

use std::collections::HashMap;
use std::sync::OnceLock;

const EN_US: &str = include_str!("../locales/en-US/main.ftl");

static BUNDLE: OnceLock<HashMap<String, String>> = OnceLock::new();

pub fn parse_ftl(src: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<(String, String)> = None;
    for raw in src.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if raw.starts_with(' ') || raw.starts_with('\t') {
            if let Some((_, v)) = current.as_mut() {
                if !v.is_empty() {
                    v.push('\n');
                }
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if let Some((k0, v0)) = current.take() {
                map.insert(k0, v0);
            }
            current = Some((k.trim().to_string(), v.trim().to_string()));
        }
    }
    if let Some((k, v)) = current {
        map.insert(k, v);
    }
    map
}

/// Load the bundle for `lang` (English fills any gaps). First call wins.
pub fn init(lang: &str) {
    BUNDLE.get_or_init(|| {
        let mut map = parse_ftl(EN_US);
        if lang != "en-US" {
            let p = crate::common::win::exe_dir().join("locales").join(lang).join("main.ftl");
            if let Ok(text) = std::fs::read_to_string(p) {
                map.extend(parse_ftl(&text));
            }
        }
        map
    });
}

/// Look up a message; returns the key itself if missing.
pub fn t(key: &str) -> String {
    init("en-US");
    BUNDLE.get().and_then(|m| m.get(key).cloned()).unwrap_or_else(|| key.to_string())
}

/// Look up a message and substitute `{ $name }` placeholders.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in args {
        for pat in [format!("{{ ${k} }}"), format!("{{${k}}}")] {
            s = s.replace(&pat, v);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subset() {
        let m = parse_ftl("# c\na = Hello\nb =\n    line one\n    line two\nc = x = y\n");
        assert_eq!(m["a"], "Hello");
        assert_eq!(m["b"], "line one\nline two");
        assert_eq!(m["c"], "x = y");
    }

    #[test]
    fn embedded_english_has_core_keys() {
        let m = parse_ftl(EN_US);
        for k in ["app-name", "mode-region", "dest-clipboard", "tray-exit"] {
            assert!(m.contains_key(k), "missing {k}");
        }
        assert_eq!(tf("notify-saved", &[("path", "C:\\a.png")]), "Saved to C:\\a.png");
    }
}
