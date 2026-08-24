//! OpenMic: local real-time microphone cleaner and soundboard for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod config;
mod resample;
mod decode;
mod dsp;
mod engine;
use eframe::egui;
mod gui;

fn main() -> eframe::Result<()> {
    let settings = config::Settings::load();
    eframe::run_native(
        &format!("OpenMic v{}", env!("CARGO_PKG_VERSION")),
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([760.0, 880.0])
                .with_resizable(false),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(gui::App::new(settings)))),
    )
}
