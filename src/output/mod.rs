//! Output destinations: clipboard, files, printer, email, Office, upload, external program,
//! and the date/time stamp.
//!
//! CONTRACT: functions are blocking. The app calls everything except `clipboard` and
//! `print` (which show UI / need the UI thread) on its I/O worker thread.

pub mod clipboard;
mod dib;
pub mod email;
pub mod external;
pub mod office;
pub mod pcx;
pub mod print;
pub mod save;
pub mod stamp;
pub mod upload;
