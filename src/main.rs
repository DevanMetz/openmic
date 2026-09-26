//! OpenMic: local real-time microphone cleaner and soundboard for Windows.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod config;
mod decode;
mod default_mic;
mod dictation;
mod denoise;
mod dsp;
mod engine;
mod gui;
mod hotkeys;
mod icon;
mod instance;
mod record;
mod resample;
mod tray;
mod viz;
mod widgets;

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let Ok(instance) = instance::acquire() else {
        return Ok(()); // the running copy shows its window instead
    };
    let settings = config::Settings::load();
    // Windows startup launches straight into the notification area.
    let start_hidden = settings.close_to_tray
        && std::env::args().any(|arg| arg == config::MINIMIZED_ARG);
    let icon = egui::IconData {
        rgba: icon::rgba(64, false),
        width: 64,
        height: 64,
    };
    eframe::run_native(
        &format!("OpenMic v{}", env!("CARGO_PKG_VERSION")),
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([760.0, 880.0])
                .with_resizable(false)
                .with_icon(Arc::new(icon))
                .with_visible(!start_hidden),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(gui::App::new(settings, &cc.egui_ctx, start_hidden, instance)))),
    )
}
