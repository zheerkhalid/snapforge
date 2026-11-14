//! E-mail via Simple MAPI (mapi32.dll, loaded dynamically).

use crate::common::win::{pcwstr, wide};
use anyhow::{bail, Context, Result};
use std::ffi::c_void;
use std::path::Path;
use windows::core::{PCSTR, PWSTR};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::Globalization::{WideCharToMultiByte, CP_ACP};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const MAPI_LOGON_UI: u32 = 0x0000_0001;
const MAPI_DIALOG: u32 = 0x0000_0008;

const SUCCESS_SUCCESS: u32 = 0;
const MAPI_USER_ABORT: u32 = 1;
const MAPI_E_FAILURE: u32 = 2;
const MAPI_E_LOGIN_FAILURE: u32 = 3;
const MAPI_E_INSUFFICIENT_MEMORY: u32 = 5;
const MAPI_E_TOO_MANY_FILES: u32 = 9;
const MAPI_E_ATTACHMENT_NOT_FOUND: u32 = 11;
const MAPI_E_ATTACHMENT_OPEN_FAILURE: u32 = 12;
const MAPI_E_NOT_SUPPORTED: u32 = 26;

const NO_CLIENT_HELP: &str = "No e-mail program is set up for Simple MAPI. Install or configure a desktop mail \
     client (for example Outlook or Thunderbird) and make it the default e-mail app in Windows Settings > Apps > \
     Default apps, or save the capture and attach it manually.";

#[repr(C)]
pub struct MapiRecipDescW {
    pub ul_reserved: u32,
    pub ul_recip_class: u32,
    pub lpsz_name: PWSTR,
    pub lpsz_address: PWSTR,
    pub ul_eid_size: u32,
    pub lp_entry_id: *mut c_void,
}

#[repr(C)]
pub struct MapiFileDescW {
    pub ul_reserved: u32,
    pub fl_flags: u32,
    pub n_position: u32,
    pub lpsz_path_name: PWSTR,
    pub lpsz_file_name: PWSTR,
    pub lp_file_type: *mut c_void,
}

#[repr(C)]
pub struct MapiMessageW {
    pub ul_reserved: u32,
    pub lpsz_subject: PWSTR,
    pub lpsz_note_text: PWSTR,
    pub lpsz_message_type: PWSTR,
    pub lpsz_date_received: PWSTR,
    pub lpsz_conversation_id: PWSTR,
    pub fl_flags: u32,
    pub lp_originator: *mut MapiRecipDescW,
    pub n_recip_count: u32,
    pub lp_recips: *mut MapiRecipDescW,
    pub n_file_count: u32,
    pub lp_files: *mut MapiFileDescW,
}

// ANSI variants (same layout with narrow strings).
#[repr(C)]
struct MapiFileDescA {
    ul_reserved: u32,
    fl_flags: u32,
    n_position: u32,
    lpsz_path_name: *mut u8,
    lpsz_file_name: *mut u8,
    lp_file_type: *mut c_void,
}

#[repr(C)]
struct MapiMessageA {
    ul_reserved: u32,
    lpsz_subject: *mut u8,
    lpsz_note_text: *mut u8,
    lpsz_message_type: *mut u8,
    lpsz_date_received: *mut u8,
    lpsz_conversation_id: *mut u8,
    fl_flags: u32,
    lp_originator: *mut c_void,
    n_recip_count: u32,
    lp_recips: *mut c_void,
    n_file_count: u32,
    lp_files: *mut MapiFileDescA,
}

type SendMailW = unsafe extern "system" fn(usize, usize, *const MapiMessageW, u32, u32) -> u32;
type SendMailA = unsafe extern "system" fn(usize, usize, *const MapiMessageA, u32, u32) -> u32;

/// Owns the UTF-16 buffers a [`MapiMessageW`] points into.
pub struct WideMessage {
    subject: Vec<u16>,
    path: Vec<u16>,
    file_name: Vec<u16>,
    file: MapiFileDescW,
    pub msg: MapiMessageW,
}

impl WideMessage {
    /// Build a message with one attachment and no recipients (the user fills them in).
    pub fn new(path: &Path, subject: &str) -> Box<Self> {
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut m = Box::new(WideMessage {
            subject: wide(subject),
            path: wide(&path.to_string_lossy()),
            file_name: wide(&file_name),
            file: MapiFileDescW {
                ul_reserved: 0,
                fl_flags: 0,
                n_position: u32::MAX, // -1: attachment not placed in the body text
                lpsz_path_name: PWSTR::null(),
                lpsz_file_name: PWSTR::null(),
                lp_file_type: std::ptr::null_mut(),
            },
            msg: MapiMessageW {
                ul_reserved: 0,
                lpsz_subject: PWSTR::null(),
                lpsz_note_text: PWSTR::null(),
                lpsz_message_type: PWSTR::null(),
                lpsz_date_received: PWSTR::null(),
                lpsz_conversation_id: PWSTR::null(),
                fl_flags: 0,
                lp_originator: std::ptr::null_mut(),
                n_recip_count: 0,
                lp_recips: std::ptr::null_mut(),
                n_file_count: 1,
                lp_files: std::ptr::null_mut(),
            },
        });
        // Boxed, so the internal pointers stay valid when the box moves.
        m.file.lpsz_path_name = PWSTR(m.path.as_mut_ptr());
        m.file.lpsz_file_name = PWSTR(m.file_name.as_mut_ptr());
        m.msg.lpsz_subject = PWSTR(m.subject.as_mut_ptr());
        m.msg.lp_files = &mut m.file;
        m
    }
}

/// System ANSI code page, NUL-terminated.
fn to_ansi(s: &str) -> Vec<u8> {
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.is_empty() {
        return vec![0];
    }
    // SAFETY: valid input slice; first call sizes, second fills.
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, 0, &w, None, PCSTR::null(), None);
        if n <= 0 {
            return s.bytes().map(|b| if b.is_ascii() { b } else { b'?' }).chain([0]).collect();
        }
        let mut out = vec![0u8; n as usize + 1];
        WideCharToMultiByte(CP_ACP, 0, &w, Some(&mut out[..n as usize]), PCSTR::null(), None);
        out
    }
}

/// Human-readable explanation of a MAPI result code.
pub fn describe(code: u32) -> String {
    match code {
        MAPI_E_FAILURE | MAPI_E_LOGIN_FAILURE | MAPI_E_NOT_SUPPORTED => NO_CLIENT_HELP.to_string(),
        MAPI_E_INSUFFICIENT_MEMORY => "the mail program ran out of memory".into(),
        MAPI_E_TOO_MANY_FILES => "the mail program rejected the attachment (too many files)".into(),
        MAPI_E_ATTACHMENT_NOT_FOUND => "the attachment file was not found".into(),
        MAPI_E_ATTACHMENT_OPEN_FAILURE => "the mail program could not open the attachment".into(),
        c => format!("the mail program reported MAPI error {c}"),
    }
}

/// Unloads the library on drop.
struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: loaded by LoadLibraryW below.
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

/// Open the default mail client with a new message that has `path` attached (Simple MAPI).
pub fn email_file(path: &Path, subject: &str) -> Result<()> {
    if !path.is_file() {
        bail!("cannot e-mail {}: the file does not exist", path.display());
    }
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = strip_verbatim(&path);
    let dll = wide("mapi32.dll");
    // SAFETY: valid NUL-terminated name.
    let lib = Library(unsafe { LoadLibraryW(pcwstr(&dll)) }.context(NO_CLIENT_HELP)?);

    // SAFETY: names are NUL-terminated ASCII; the transmutes match the documented signatures.
    let (send_w, send_a) = unsafe {
        let w = GetProcAddress(lib.0, PCSTR(c"MAPISendMailW".as_ptr().cast()));
        let a = GetProcAddress(lib.0, PCSTR(c"MAPISendMail".as_ptr().cast()));
        (
            w.map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, SendMailW>(f)),
            a.map(|f| std::mem::transmute::<unsafe extern "system" fn() -> isize, SendMailA>(f)),
        )
    };

    let flags = MAPI_DIALOG | MAPI_LOGON_UI;
    let code = if let Some(send) = send_w {
        let m = WideMessage::new(&path, subject);
        log::info!("email: MAPISendMailW with {}", path.display());
        // SAFETY: `m` owns every buffer the message points to and outlives the call.
        unsafe { send(0, 0, &m.msg, flags, 0) }
    } else if let Some(send) = send_a {
        let mut subj = to_ansi(subject);
        let mut p = to_ansi(&path.to_string_lossy());
        let mut name = to_ansi(&path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        let mut file = MapiFileDescA {
            ul_reserved: 0,
            fl_flags: 0,
            n_position: u32::MAX,
            lpsz_path_name: p.as_mut_ptr(),
            lpsz_file_name: name.as_mut_ptr(),
            lp_file_type: std::ptr::null_mut(),
        };
        let msg = MapiMessageA {
            ul_reserved: 0,
            lpsz_subject: subj.as_mut_ptr(),
            lpsz_note_text: std::ptr::null_mut(),
            lpsz_message_type: std::ptr::null_mut(),
            lpsz_date_received: std::ptr::null_mut(),
            lpsz_conversation_id: std::ptr::null_mut(),
            fl_flags: 0,
            lp_originator: std::ptr::null_mut(),
            n_recip_count: 0,
            lp_recips: std::ptr::null_mut(),
            n_file_count: 1,
            lp_files: &mut file,
        };
        log::info!("email: MAPISendMail (ANSI) with {}", path.display());
        // SAFETY: all buffers live until after the call.
        unsafe { send(0, 0, &msg, flags, 0) }
    } else {
        bail!("{NO_CLIENT_HELP}");
    };

    match code {
        SUCCESS_SUCCESS => Ok(()),
        MAPI_USER_ABORT => {
            log::info!("email: cancelled by the user");
            Ok(())
        }
        c => {
            log::warn!("email: MAPI returned {c}");
            bail!("{}", describe(c))
        }
    }
}

/// `\\?\C:\x` → `C:\x` (mail clients choke on verbatim paths).
fn strip_verbatim(p: &Path) -> std::path::PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        std::path::PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        std::path::PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_mapi_h() {
        // x64 sizes from MAPI.h.
        assert_eq!(std::mem::size_of::<MapiFileDescW>(), 40);
        assert_eq!(std::mem::size_of::<MapiRecipDescW>(), 40);
        assert_eq!(std::mem::size_of::<MapiMessageW>(), 96);
        assert_eq!(std::mem::size_of::<MapiMessageA>(), 96);
    }

    #[test]
    fn message_points_at_owned_buffers() {
        let m = WideMessage::new(Path::new(r"C:\Pics\shot 1.png"), "Screenshot");
        assert_eq!(m.msg.n_file_count, 1);
        assert_eq!(m.msg.n_recip_count, 0);
        // SAFETY: the pointers refer to buffers owned by `m`.
        unsafe {
            assert_eq!(m.msg.lpsz_subject.to_string().unwrap(), "Screenshot");
            let f = &*m.msg.lp_files;
            assert_eq!(f.lpsz_path_name.to_string().unwrap(), r"C:\Pics\shot 1.png");
            assert_eq!(f.lpsz_file_name.to_string().unwrap(), "shot 1.png");
            assert_eq!(f.n_position, u32::MAX);
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(to_ansi("abc"), b"abc\0");
        assert!(describe(MAPI_E_LOGIN_FAILURE).contains("Default apps"));
        assert_eq!(strip_verbatim(Path::new(r"\\?\C:\a\b.png")), Path::new(r"C:\a\b.png"));
        assert!(email_file(Path::new(r"C:\definitely\missing\file.png"), "x").is_err());
    }
}
