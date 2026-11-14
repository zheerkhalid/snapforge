//! "Start with Windows": `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\SnapForge`.

use crate::common::win::{exe_path, pcwstr, wide};
use std::ffi::c_void;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE: &str = "SnapForge";

/// The command line stored in the Run key.
pub fn command_line() -> String {
    format!("\"{}\" --tray", exe_path().display())
}

/// True when the Run value exists.
pub fn is_enabled() -> bool {
    let key = wide(RUN_KEY);
    let val = wide(VALUE);
    let mut size: u32 = 0;
    // SAFETY: size query only (no data buffer).
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, pcwstr(&key), pcwstr(&val), RRF_RT_REG_SZ, None, None, Some(&mut size))
    };
    r == ERROR_SUCCESS
}

pub fn set_enabled(enable: bool) -> anyhow::Result<()> {
    let key = wide(RUN_KEY);
    let val = wide(VALUE);
    // SAFETY: valid NUL-terminated strings; the data buffer outlives the call.
    let r = unsafe {
        if enable {
            let data = wide(&command_line());
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                pcwstr(&key),
                pcwstr(&val),
                REG_SZ.0,
                Some(data.as_ptr() as *const c_void),
                (data.len() * 2) as u32,
            )
        } else {
            let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, pcwstr(&key), pcwstr(&val));
            if r == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                ERROR_SUCCESS
            } else {
                r
            }
        }
    };
    if r != ERROR_SUCCESS {
        anyhow::bail!("registry update failed: {}", windows::core::Error::from(r.to_hresult()).message());
    }
    Ok(())
}

/// Read the real registry state and mirror it into the settings. Returns it.
pub fn sync_setting() -> bool {
    let on = is_enabled();
    if crate::settings::get().general.start_with_windows != on {
        crate::settings::update(|s| s.general.start_with_windows = on);
    }
    on
}
