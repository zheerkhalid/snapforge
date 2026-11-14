//! "Open with" an external program.

use anyhow::{bail, Context, Result};
use std::os::windows::process::CommandExt;
use std::path::Path;

/// Split a program string into the executable and the raw argument string, substituting
/// the file path. `%1` (optionally already in quotes) is replaced with the quoted path;
/// without a placeholder the quoted path is appended.
///
/// The executable may be quoted (`"C:\Program Files\X\x.exe" -a %1`) or unquoted; for an
/// unquoted one containing spaces the longest prefix that names an existing file wins.
pub fn build_command(program: &str, path: &Path, exists: impl Fn(&str) -> bool) -> Result<(String, String)> {
    let program = program.trim();
    if program.is_empty() {
        bail!("no external program is configured");
    }
    let quoted = format!("\"{}\"", path.display());

    let (exe, rest) = if let Some(stripped) = program.strip_prefix('"') {
        match stripped.find('"') {
            Some(end) => (stripped[..end].to_string(), stripped[end + 1..].trim_start().to_string()),
            None => bail!("the external program has an unmatched quote: {program}"),
        }
    } else {
        // Candidate split points: every whitespace boundary, longest first.
        let mut splits: Vec<usize> =
            program.char_indices().filter(|(_, c)| c.is_whitespace()).map(|(i, _)| i).collect();
        splits.push(program.len());
        let found = splits.iter().rev().copied().find(|&i| {
            let cand = &program[..i];
            !cand.contains("%1") && (exists(cand) || exists(&format!("{cand}.exe")))
        });
        let i = found.unwrap_or_else(|| splits[0]);
        (program[..i].to_string(), program[i..].trim_start().to_string())
    };
    if exe.is_empty() {
        bail!("the external program setting has no executable: {program}");
    }

    let args = if rest.contains("%1") {
        rest.replace("\"%1\"", &quoted).replace("%1", &quoted)
    } else if rest.is_empty() {
        quoted
    } else {
        format!("{rest} {quoted}")
    };
    Ok((exe, args))
}

/// Launch `program` with the file path as its argument (does not wait for it).
pub fn open_with(program: &str, path: &Path) -> Result<()> {
    let (exe, args) = build_command(program, path, |p| Path::new(p).is_file())?;
    log::info!("external: {exe} {args}");
    std::process::Command::new(&exe).raw_arg(&args).spawn().with_context(|| format!("could not start \"{exe}\""))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = r"C:\Shots\a b.png";

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn appends_quoted_path_without_placeholder() {
        let (e, a) = build_command("mspaint.exe", Path::new(P), none).unwrap();
        assert_eq!(e, "mspaint.exe");
        assert_eq!(a, r#""C:\Shots\a b.png""#);
        let (e, a) = build_command(r#""C:\Program Files\X\x.exe" /edit"#, Path::new(P), none).unwrap();
        assert_eq!(e, r"C:\Program Files\X\x.exe");
        assert_eq!(a, r#"/edit "C:\Shots\a b.png""#);
    }

    #[test]
    fn substitutes_placeholder() {
        let (e, a) = build_command(r#""C:\X\x.exe" --open %1 --wait"#, Path::new(P), none).unwrap();
        assert_eq!(e, r"C:\X\x.exe");
        assert_eq!(a, r#"--open "C:\Shots\a b.png" --wait"#);
        // Already-quoted placeholder is not double-quoted.
        let (_, a) = build_command(r#"x.exe "%1""#, Path::new(P), none).unwrap();
        assert_eq!(a, r#""C:\Shots\a b.png""#);
    }

    #[test]
    fn unquoted_exe_with_spaces() {
        let exists = |p: &str| p == r"C:\Program Files\Paint Tool\pt.exe";
        let (e, a) = build_command(r"C:\Program Files\Paint Tool\pt.exe -n %1", Path::new(P), exists).unwrap();
        assert_eq!(e, r"C:\Program Files\Paint Tool\pt.exe");
        assert_eq!(a, r#"-n "C:\Shots\a b.png""#);
    }

    #[test]
    fn errors() {
        assert!(build_command("   ", Path::new(P), none).is_err());
        assert!(build_command(r#""C:\x.exe"#, Path::new(P), none).is_err());
        assert!(open_with(r"C:\definitely\not\here\nothing.exe", Path::new(P)).is_err());
    }
}
