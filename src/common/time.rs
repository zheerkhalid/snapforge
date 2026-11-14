//! Local wall-clock time without pulling in a date crate.

use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

impl DateTime {
    pub fn now_local() -> Self {
        // SAFETY: plain query.
        let t = unsafe { GetLocalTime() };
        Self {
            year: t.wYear,
            month: t.wMonth as u8,
            day: t.wDay as u8,
            hour: t.wHour as u8,
            minute: t.wMinute as u8,
            second: t.wSecond as u8,
            millis: t.wMilliseconds,
        }
    }

    /// `YYYY-MM-DD`, used for the daily counter reset.
    pub fn date_key(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `YYYY-MM-DD HH:MM:SS`
    pub fn display(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// High-resolution timestamp (QueryPerformanceCounter ticks).
pub fn qpc_now() -> i64 {
    let mut t = 0i64;
    // SAFETY: valid out pointer.
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut t);
    }
    t
}

/// Milliseconds between two [`qpc_now`] values.
pub fn qpc_ms(from: i64, to: i64) -> f64 {
    let mut f = 0i64;
    // SAFETY: valid out pointer.
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
    }
    (to - from) as f64 * 1000.0 / f.max(1) as f64
}
