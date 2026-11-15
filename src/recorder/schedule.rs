//! Scheduled recording: start and end at local wall-clock times, optionally sleeping the
//! PC afterwards. The schedule lives in `settings.recorder.schedule` (edited by the
//! settings window); [`start_watcher`] checks it once per second on a background thread.
//! A schedule is one-shot: it is disabled when it starts (or when it has expired).

use crate::common::time::DateTime;
use crate::common::Rect;
use crate::settings::recorder::{ScheduleSettings, ScheduleTarget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Parse `YYYY-MM-DD HH:MM` (seconds optional) into a sortable key.
pub fn parse(s: &str) -> Option<(u16, u8, u8, u8, u8)> {
    let s = s.trim();
    let (date, time) = s.split_once([' ', 'T'])?;
    let mut d = date.split('-');
    let y: u16 = d.next()?.parse().ok()?;
    let mo: u8 = d.next()?.parse().ok()?;
    let da: u8 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: u8 = t.next()?.parse().ok()?;
    let mi: u8 = t.next()?.parse().ok()?;
    ((1..=12).contains(&mo) && (1..=31).contains(&da) && h < 24 && mi < 60).then_some((y, mo, da, h, mi))
}

pub fn key_of(t: &DateTime) -> (u16, u8, u8, u8, u8) {
    (t.year, t.month, t.day, t.hour, t.minute)
}

/// `YYYY-MM-DD HH:MM`, the form [`parse`] reads.
pub fn format_key(k: (u16, u8, u8, u8, u8)) -> String {
    format!("{:04}-{:02}-{:02} {:02}:{:02}", k.0, k.1, k.2, k.3, k.4)
}

fn days_in_month(y: u16, m: u8) -> u8 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

/// `k` plus `minutes` (non-negative) on the calendar (local time, no DST adjustment).
pub fn add_minutes(k: (u16, u8, u8, u8, u8), minutes: u32) -> (u16, u8, u8, u8, u8) {
    let (mut y, mut mo, mut d) = (k.0, k.1.clamp(1, 12), k.2.max(1));
    let total = k.3 as u32 * 60 + k.4 as u32 + minutes;
    let mut days = total / (24 * 60);
    let rest = total % (24 * 60);
    while days > 0 {
        if d < days_in_month(y, mo) {
            d += 1;
        } else {
            d = 1;
            if mo == 12 {
                mo = 1;
                y = y.saturating_add(1);
            } else {
                mo += 1;
            }
        }
        days -= 1;
    }
    (y, mo, d, (rest / 60) as u8, (rest % 60) as u8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Start,
    Stop,
    /// The window has passed (or the schedule is invalid): disable it.
    Expire,
}

/// Decide what to do now. `active` = a scheduled recording is running.
pub fn decide(now: (u16, u8, u8, u8, u8), s: &ScheduleSettings, active: bool, recording: bool) -> Action {
    let end = parse(&s.end);
    if active {
        return match end {
            Some(e) if now < e && recording => Action::Nothing,
            _ => Action::Stop,
        };
    }
    if !s.enabled {
        return Action::Nothing;
    }
    let (Some(start), Some(end)) = (parse(&s.start), end) else { return Action::Expire };
    if end <= start || now >= end {
        return Action::Expire;
    }
    if now >= start && !recording {
        return Action::Start;
    }
    Action::Nothing
}

static ACTIVE: Mutex<Option<ScheduleSettings>> = Mutex::new(None);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Start the once-per-second schedule check (idempotent). Call at startup.
pub fn start_watcher() {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("rec-schedule".into()).spawn(|| loop {
        tick();
        std::thread::sleep(Duration::from_secs(1));
    });
}

/// Replace the schedule (settings UI) and persist it.
pub fn set(s: ScheduleSettings) {
    crate::settings::update(|st| st.recorder.schedule = s);
}

/// Cancel the pending schedule (a running scheduled recording keeps going).
pub fn cancel() {
    crate::settings::update(|st| st.recorder.schedule.enabled = false);
}

/// Validation message key for the schedule dialog, or `None` if the schedule is usable.
pub fn validate(s: &ScheduleSettings) -> Option<&'static str> {
    validate_at(s, key_of(&DateTime::now_local()))
}

/// [`validate`] at a given local time.
pub fn validate_at(s: &ScheduleSettings, now: (u16, u8, u8, u8, u8)) -> Option<&'static str> {
    let (Some(a), Some(b)) = (parse(&s.start), parse(&s.end)) else { return Some("rec-schedule-invalid") };
    if b <= a {
        return Some("rec-schedule-order");
    }
    if b <= now {
        return Some("rec-schedule-past");
    }
    match s.target {
        ScheduleTarget::Region if s.region[2] < MIN_REGION || s.region[3] < MIN_REGION => Some("rec-schedule-region"),
        ScheduleTarget::Window if s.window_title.trim().is_empty() => Some("rec-schedule-window"),
        _ => None,
    }
}

/// Smallest scheduled region side, in pixels.
pub const MIN_REGION: i32 = 16;

/// The window a scheduled recording targets: an exact title match first, then the first
/// window (in z-order) whose title contains `title`, ignoring case.
pub fn find_window(windows: &[crate::capture::WindowEntry], title: &str) -> Option<isize> {
    let want = title.trim();
    if want.is_empty() {
        return None;
    }
    let lower = want.to_lowercase();
    windows
        .iter()
        .find(|w| w.title == want)
        .or_else(|| windows.iter().find(|w| w.title.to_lowercase().contains(&lower)))
        .map(|w| w.hwnd)
}

fn tick() {
    let sched = crate::settings::get().recorder.schedule.clone();
    let active = ACTIVE.lock().map(|a| a.clone()).unwrap_or(None);
    let now = key_of(&DateTime::now_local());
    let recording = super::is_recording();
    let s = active.clone().unwrap_or(sched);
    match decide(now, &s, active.is_some(), recording) {
        Action::Nothing => {}
        Action::Expire => {
            log::info!("recorder: schedule {}..{} expired", s.start, s.end);
            super::schedule::cancel();
        }
        Action::Start => {
            cancel();
            let area = match s.target {
                ScheduleTarget::FullScreen => None,
                ScheduleTarget::Region => {
                    let [x, y, w, h] = s.region;
                    Some(super::Area::Rect(Rect::new(x, y, w, h)))
                }
                ScheduleTarget::Window => {
                    let found = find_window(&crate::capture::snapshot_windows(&[]), &s.window_title);
                    if found.is_none() {
                        log::warn!("recorder: no window titled {:?}; recording the full screen", s.window_title);
                        crate::app::notify(
                            crate::i18n::t("app-name"),
                            crate::i18n::tf("rec-schedule-no-window", &[("title", &s.window_title)]),
                            true,
                        );
                    }
                    found.map(super::Area::Window)
                }
            };
            let opts = super::StartOptions { sleep_after: s.sleep_after, ..Default::default() };
            match super::start_with(area, opts) {
                Ok(()) => {
                    log::info!("recorder: scheduled recording started (until {})", s.end);
                    if let Ok(mut a) = ACTIVE.lock() {
                        *a = Some(s);
                    }
                }
                Err(e) => {
                    log::error!("recorder: scheduled recording failed: {e:#}");
                    crate::app::notify(crate::i18n::t("app-name"), format!("{e:#}"), true);
                }
            }
        }
        Action::Stop => {
            if let Ok(mut a) = ACTIVE.lock() {
                *a = None;
            }
            if recording {
                log::info!("recorder: scheduled recording ends");
                super::stop();
            }
        }
    }
}

/// Put the PC to sleep (after a scheduled recording, when requested).
pub fn sleep_now() {
    log::info!("recorder: going to sleep after the scheduled recording");
    // SAFETY: plain call; fails harmlessly without the privilege.
    unsafe {
        let _ = windows::Win32::System::Power::SetSuspendState(false, false, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(start: &str, end: &str) -> ScheduleSettings {
        ScheduleSettings { enabled: true, start: start.into(), end: end.into(), ..Default::default() }
    }

    #[test]
    fn parse_times() {
        assert_eq!(parse("2026-11-14 09:05"), Some((2026, 11, 14, 9, 5)));
        assert_eq!(parse("2026-11-14T23:59:30"), Some((2026, 11, 14, 23, 59)));
        assert_eq!(parse("2026-13-01 10:00"), None);
        assert_eq!(parse("garbage"), None);
    }

    #[test]
    fn decisions() {
        let s = sched("2026-11-14 10:00", "2026-11-14 11:00");
        let at = |h, m| (2026, 11, 14, h, m);
        assert_eq!(decide(at(9, 59), &s, false, false), Action::Nothing);
        assert_eq!(decide(at(10, 0), &s, false, false), Action::Start);
        // Missed start (PC was off): still starts while inside the window.
        assert_eq!(decide(at(10, 30), &s, false, false), Action::Start);
        // Busy with a manual recording: wait.
        assert_eq!(decide(at(10, 30), &s, false, true), Action::Nothing);
        assert_eq!(decide(at(11, 0), &s, false, false), Action::Expire);
        // Running scheduled recording stops at the end, or if the user stopped it.
        assert_eq!(decide(at(10, 59), &s, true, true), Action::Nothing);
        assert_eq!(decide(at(11, 0), &s, true, true), Action::Stop);
        assert_eq!(decide(at(10, 40), &s, true, false), Action::Stop);
        // Disabled or invalid.
        let mut off = s.clone();
        off.enabled = false;
        assert_eq!(decide(at(10, 30), &off, false, false), Action::Nothing);
        assert_eq!(decide(at(10, 30), &sched("x", "y"), false, false), Action::Expire);
        assert_eq!(decide(at(10, 30), &sched("2026-11-14 11:00", "2026-11-14 10:00"), false, false), Action::Expire);
    }

    #[test]
    fn calendar_math() {
        assert_eq!(add_minutes((2026, 11, 14, 9, 55), 10), (2026, 11, 14, 10, 5));
        assert_eq!(add_minutes((2026, 11, 30, 23, 30), 60), (2026, 12, 1, 0, 30));
        assert_eq!(add_minutes((2026, 12, 31, 23, 59), 1), (2027, 1, 1, 0, 0));
        assert_eq!(add_minutes((2028, 2, 28, 12, 0), 24 * 60), (2028, 2, 29, 12, 0));
        assert_eq!(add_minutes((2026, 2, 28, 12, 0), 24 * 60), (2026, 3, 1, 12, 0));
        assert_eq!(format_key((2026, 3, 1, 7, 5)), "2026-03-01 07:05");
        assert_eq!(parse(&format_key((2026, 3, 1, 7, 5))), Some((2026, 3, 1, 7, 5)));
    }

    #[test]
    fn validation() {
        let now = (2026, 11, 14, 9, 0);
        let mut s = sched("2026-11-14 10:00", "2026-11-14 11:00");
        assert_eq!(validate_at(&s, now), None);
        assert_eq!(validate_at(&s, (2026, 11, 14, 11, 0)), Some("rec-schedule-past"));
        assert_eq!(validate_at(&sched("2026-11-14 10:00", "x"), now), Some("rec-schedule-invalid"));
        assert_eq!(validate_at(&sched("2026-11-14 10:00", "2026-11-14 10:00"), now), Some("rec-schedule-order"));
        s.target = ScheduleTarget::Region;
        s.region = [0, 0, 8, 600];
        assert_eq!(validate_at(&s, now), Some("rec-schedule-region"));
        s.region = [0, 0, 800, 600];
        assert_eq!(validate_at(&s, now), None);
        s.target = ScheduleTarget::Window;
        assert_eq!(validate_at(&s, now), Some("rec-schedule-window"));
        s.window_title = "Notepad".into();
        assert_eq!(validate_at(&s, now), None);
    }

    #[test]
    fn finds_windows_by_title() {
        let w = |hwnd: isize, title: &str| crate::capture::WindowEntry {
            hwnd,
            rect: Rect::new(0, 0, 10, 10),
            title: title.into(),
            class: String::new(),
        };
        let list = [w(1, "notes.txt - Notepad"), w(2, "Notepad"), w(3, "Browser")];
        assert_eq!(find_window(&list, "Notepad"), Some(2));
        assert_eq!(find_window(&list, "notepad"), Some(1));
        assert_eq!(find_window(&list, " browser "), Some(3));
        assert_eq!(find_window(&list, "Paint"), None);
        assert_eq!(find_window(&list, "  "), None);
    }
}
