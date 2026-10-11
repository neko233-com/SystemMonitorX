#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
extern crate gpui_kit as gpui;

#[cfg(windows)]
mod ui;

fn main() {
    #[cfg(not(windows))]
    compile_error!("SystemMonitorX is Windows-only. Build for x86_64-pc-windows-msvc.");
    #[cfg(windows)]
    ui::run();
}
