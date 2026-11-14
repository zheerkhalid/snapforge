//! SnapForge: fast, native Windows screen capture, annotation and recording.
//! Developed by Zheer Khalid (https://zheer.dev).

#![windows_subsystem = "windows"]
// Temporary while Phase 1 modules are being filled in; removed at integration.
#![allow(dead_code)]

mod app;
mod bench;
mod capture;
mod cli;
mod common;
mod compose;
mod controller;
mod editor;
mod effects;
mod hotkeys;
mod i18n;
mod integration;
mod logging;
mod output;
mod overlay;
mod settings;
mod toolbar;
mod tray;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = controller::main_entry(args);
    std::process::exit(code);
}
