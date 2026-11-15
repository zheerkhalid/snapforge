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
mod draw;
mod editor;
mod effects;
mod hotkeys;
mod i18n;
mod integration;
mod logging;
mod ocr;
mod output;
mod overlay;
mod pin;
mod settings;
mod settings_ui;
mod toolbar;
mod tools;
mod tray;
mod video_editor;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = controller::main_entry(args);
    std::process::exit(code);
}
