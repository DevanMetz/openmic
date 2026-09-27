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
mod logfile;
mod record;
mod resample;
mod tray;
mod update;
mod viz;
mod widgets;

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // After an update, wait for the old copy to quit before claiming the
    // single-instance lock and the audio devices.
    let updated = update::finish_update();
    let Ok(instance) = instance::acquire() else {
        return Ok(()); // the running copy shows its window instead
    };
    logfile::init();
    logfile::info(format_args!(
        "OpenMic v{}{} started{}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { " (dev build)" } else { "" },
        if updated { " after an update" } else { "" },
    ));
    let settings = config::Settings::load();
    // Windows startup launches straight into the notification area.
    let start_hidden = settings.close_to_tray
        && std::env::args().any(|arg| arg == config::MINIMIZED_ARG);
    let icon = egui::IconData {
        rgba: icon::rgba(64, false),
        width: 64,
        height: 64,
    };
    let title = if cfg!(debug_assertions) {
        format!("OpenMic v{} (dev build)", env!("CARGO_PKG_VERSION"))
    } else {
        format!("OpenMic v{}", env!("CARGO_PKG_VERSION"))
    };
    eframe::run_native(
        &title,
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(initial_size())
                .with_min_inner_size([MIN_SIZE.0, MIN_SIZE.1])
                .with_resizable(true)
                .with_icon(Arc::new(icon))
                .with_visible(!start_hidden),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(gui::App::new(settings, &cc.egui_ctx, start_hidden, instance, updated)))),
    )
}

/// The layout's natural size, and the smallest the window may get (the
/// tabs scroll below the natural height).
const NATURAL_SIZE: (f32, f32) = (760.0, 880.0);
const MIN_SIZE: (f32, f32) = (700.0, 420.0);

/// The natural size, shortened to fit the screen: at 150% scaling a 1080p
/// laptop has only about 690 points of height to spare.
fn initial_size() -> [f32; 2] {
    let (width, height) = NATURAL_SIZE;
    match work_area_height() {
        Some(available) => [width, height.min(available - 40.0).max(MIN_SIZE.1)],
        None => [width, height],
    }
}

/// Height of the primary screen's work area (minus the taskbar), in points.
#[cfg(windows)]
fn work_area_height() -> Option<f32> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut area = RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT to the pointer we pass.
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(std::ptr::from_mut(&mut area).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    // SAFETY: no arguments; returns the system DPI (96 = 100%).
    let dpi = unsafe { GetDpiForSystem() };
    (dpi > 0).then(|| (area.bottom - area.top) as f32 * 96.0 / dpi as f32)
}

#[cfg(not(windows))]
fn work_area_height() -> Option<f32> {
    None
}
