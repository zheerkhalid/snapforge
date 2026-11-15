//! Crash recovery.
//!
//! While recording, the MP4 is written as *fragmented* MP4 (`ftyp`, `moov` with `mvex`,
//! then one `moof`+`mdat` pair per key-frame interval) to `<name>.recording.mp4`, and a
//! small JSON journal in `<settings dir>\recordings\` names the partial and final paths.
//! A clean stop remuxes the fragments into a regular MP4 (no re-encode) at the final path
//! and deletes the journal. After a crash or power loss, [`scan_pending`] finds journals
//! whose partial file still exists; recovery cuts the file after the last complete
//! fragment ([`fmp4_complete_len`]) and remuxes it the same way. If the remux fails, the
//! truncated fragmented file itself is kept (it is playable) under the final name.
//! WMV (ASF) and MP3 files are streamable as written, so they are only renamed.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journal {
    /// File being written.
    pub partial: PathBuf,
    /// Where the finished recording goes.
    pub final_path: PathBuf,
    /// "mp4" | "wmv" | "mp3".
    pub format: String,
    pub started: String,
    pub pid: u32,
}

pub fn journal_dir() -> PathBuf {
    crate::settings::dir().join("recordings")
}

impl Journal {
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        let stem = self.final_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        dir.join(format!("{stem}.journal.json"))
    }

    pub fn write(&self, dir: &Path) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let p = self.path_in(dir);
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(p)
    }
}

/// Journals in `dir` left behind by a process other than `own_pid`.
pub fn scan_pending(dir: &Path, own_pid: u32) -> Vec<(PathBuf, Journal)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.to_string_lossy().ends_with(".journal.json") {
            continue;
        }
        match std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Journal>(&b).ok()) {
            Some(j) if j.pid != own_pid => v.push((p, j)),
            Some(_) => {}
            None => {
                log::warn!("recorder: unreadable journal {}", p.display());
                let _ = std::fs::rename(&p, p.with_extension("bad"));
            }
        }
    }
    v
}

/// A unique final path: `base.ext`, `base (2).ext`, ...
pub fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    for i in 2..10_000 {
        let c = dir.join(format!("{stem} ({i}).{ext}"));
        if !c.exists() {
            return c;
        }
    }
    p.to_path_buf()
}

/// The partial file for a final path: `name.mp4` -> `name.recording.mp4`.
pub fn partial_for(final_path: &Path) -> PathBuf {
    let stem = final_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = final_path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    final_path.with_file_name(format!("{stem}.recording.{ext}"))
}

// ------------------------------------------------------------------ fMP4 scanning

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxHeader {
    pub kind: [u8; 4],
    pub offset: u64,
    /// Total size including the header; `None` = runs to the end of the file (size 0).
    pub size: Option<u64>,
    pub header_len: u64,
}

/// Parse a box header at the start of `b`.
pub fn parse_box_header(b: &[u8], offset: u64) -> Option<BoxHeader> {
    if b.len() < 8 {
        return None;
    }
    let size32 = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
    let kind = [b[4], b[5], b[6], b[7]];
    match size32 {
        0 => Some(BoxHeader { kind, offset, size: None, header_len: 8 }),
        1 => {
            if b.len() < 16 {
                return None;
            }
            let large = u64::from_be_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
            (large >= 16).then_some(BoxHeader { kind, offset, size: Some(large), header_len: 16 })
        }
        s if s >= 8 => Some(BoxHeader { kind, offset, size: Some(s), header_len: 8 }),
        _ => None,
    }
}

/// Length of the longest playable prefix of a fragmented MP4 that was cut off at
/// `file_len`: everything up to the end of the last complete `moof`+`mdat` pair.
/// `read_at(offset, buf)` fills `buf` from the file (returns bytes read).
/// Returns `None` if there is no `moov` or no complete fragment.
pub fn fmp4_complete_len(file_len: u64, mut read_at: impl FnMut(u64, &mut [u8]) -> usize) -> Option<u64> {
    let mut off = 0u64;
    let mut have_moov = false;
    let mut last_good: Option<u64> = None;
    let mut pending_moof = false;
    let mut hdr = [0u8; 16];
    while off + 8 <= file_len {
        let n = read_at(off, &mut hdr);
        let Some(h) = parse_box_header(&hdr[..n], off) else { break };
        let size = h.size.unwrap_or(file_len - off);
        let end = off.checked_add(size)?;
        if end > file_len {
            break; // truncated box
        }
        match &h.kind {
            b"moov" => have_moov = true,
            b"moof" => pending_moof = true,
            b"mdat" => {
                if pending_moof && have_moov {
                    last_good = Some(end);
                }
                pending_moof = false;
            }
            b"mfra" | b"free" | b"skip" | b"sidx" | b"styp" | b"ftyp" | b"uuid" | b"meta" => {}
            _ => {}
        }
        if h.size.is_none() {
            break;
        }
        off = end;
    }
    if have_moov {
        last_good
    } else {
        None
    }
}

pub fn fmp4_complete_len_of(path: &Path) -> std::io::Result<Option<u64>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    Ok(fmp4_complete_len(len, |off, buf| {
        if f.seek(SeekFrom::Start(off)).is_err() {
            return 0;
        }
        let mut got = 0;
        while got < buf.len() {
            match f.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        got
    }))
}

/// What recovery should do with one journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryPlan {
    /// Partial file is gone (the recording finished, only the journal was left).
    DropJournal,
    /// Truncate the fragmented MP4 to `len` bytes, then remux to the final path.
    TruncateAndRemux { len: u64 },
    /// Keep the file as it is under the final name (ASF/MP3 are streamable).
    Rename,
    /// Nothing playable in it.
    Discard,
}

pub fn plan(j: &Journal, partial_exists: bool, complete_len: Option<u64>) -> RecoveryPlan {
    if !partial_exists {
        return RecoveryPlan::DropJournal;
    }
    match j.format.as_str() {
        "mp4" => match complete_len {
            Some(len) => RecoveryPlan::TruncateAndRemux { len },
            None => RecoveryPlan::Discard,
        },
        _ => RecoveryPlan::Rename,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], payload: usize) -> Vec<u8> {
        let mut v = ((payload + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend(std::iter::repeat(0u8).take(payload));
        v
    }

    fn scan(data: &[u8]) -> Option<u64> {
        fmp4_complete_len(data.len() as u64, |off, buf| {
            let off = off as usize;
            let n = buf.len().min(data.len().saturating_sub(off));
            buf[..n].copy_from_slice(&data[off..off + n]);
            n
        })
    }

    #[test]
    fn complete_fragments_only() {
        let mut f = bx(b"ftyp", 16);
        f.extend(bx(b"moov", 100));
        let after_moov = f.len() as u64;
        assert_eq!(scan(&f), None, "no fragment yet");
        f.extend(bx(b"moof", 50));
        f.extend(bx(b"mdat", 1000));
        let one = f.len() as u64;
        f.extend(bx(b"moof", 50));
        f.extend(bx(b"mdat", 1000));
        let two = f.len() as u64;
        assert_eq!(scan(&f), Some(two));
        // Power loss in the middle of the third mdat.
        let mut cut = f.clone();
        cut.extend(bx(b"moof", 50));
        cut.extend(bx(b"mdat", 1000));
        cut.truncate(cut.len() - 300);
        assert_eq!(scan(&cut), Some(two));
        // Cut inside a moof.
        let mut cut2 = f[..one as usize].to_vec();
        cut2.extend_from_slice(&bx(b"moof", 50)[..20]);
        assert_eq!(scan(&cut2), Some(one));
        // A moof without its mdat does not count.
        let mut cut3 = f[..one as usize].to_vec();
        cut3.extend(bx(b"moof", 50));
        assert_eq!(scan(&cut3), Some(one));
        // No moov at all.
        let mut nomoov = bx(b"ftyp", 16);
        nomoov.extend(bx(b"moof", 8));
        nomoov.extend(bx(b"mdat", 8));
        assert_eq!(scan(&nomoov), None);
        assert!(after_moov < one);
    }

    #[test]
    fn large_size_header() {
        let mut b = vec![0, 0, 0, 1];
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&40u64.to_be_bytes());
        let h = parse_box_header(&b, 0).expect("hdr");
        assert_eq!((h.size, h.header_len), (Some(40), 16));
        assert!(parse_box_header(&[0, 0, 0, 3, b'a', b'b', b'c', b'd'], 0).is_none());
    }

    #[test]
    fn journal_roundtrip_and_scan() {
        let dir = std::env::temp_dir().join(format!("sf-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let j = Journal {
            partial: dir.join("a.recording.mp4"),
            final_path: dir.join("a.mp4"),
            format: "mp4".into(),
            started: "2026-01-01 10:00:00".into(),
            pid: 1,
        };
        let p = j.write(&dir).expect("write");
        assert!(p.exists());
        let found = scan_pending(&dir, 999);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, j);
        // Our own live journals are skipped.
        assert!(scan_pending(&dir, 1).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plans() {
        let mut j = Journal {
            partial: "x.recording.mp4".into(),
            final_path: "x.mp4".into(),
            format: "mp4".into(),
            started: String::new(),
            pid: 1,
        };
        assert_eq!(plan(&j, false, None), RecoveryPlan::DropJournal);
        assert_eq!(plan(&j, true, Some(10)), RecoveryPlan::TruncateAndRemux { len: 10 });
        assert_eq!(plan(&j, true, None), RecoveryPlan::Discard);
        j.format = "wmv".into();
        assert_eq!(plan(&j, true, None), RecoveryPlan::Rename);
        assert_eq!(partial_for(Path::new("C:\\v\\a.mp4")), PathBuf::from("C:\\v\\a.recording.mp4"));
    }
}
