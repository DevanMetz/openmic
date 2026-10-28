//! OpenMic: local real-time microphone cleaner and soundboard for Windows
//! and Linux.

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
#[cfg(any(target_os = "linux", test))]
mod monitors;
mod playback;
mod record;
mod resample;
mod tray;
mod update;
mod virtual_mic;
#[cfg(target_os = "linux")]
mod x11_type;
mod viz;
mod widgets;

/// Keeps the Linux glutin features in Cargo.toml attached to this crate.
/// eframe creates the window; nothing here runs at startup.
#[cfg(target_os = "linux")]
#[allow(dead_code, unused_imports)]
mod linux_gl {
    use glutin as _;
    use glutin_winit as _;
}

use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // After an update, wait for the old copy to quit before claiming the
    // single-instance lock and the audio devices.
    let updated = update::wait_for_previous_version();
    let Ok(instance) = instance::acquire() else {
        return Ok(()); // the running copy shows its window instead
    };
    update::finish_update();
    logfile::init();
    logfile::info(format_args!(
        "OpenMic v{}{} started{}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { " (dev build)" } else { "" },
        if updated { " after an update" } else { "" },
    ));
    // The Linux virtual microphone must exist before devices are listed.
    if let Err(e) = virtual_mic::activate() {
        logfile::warn(format_args!("virtual microphone unavailable: {e:#}"));
    }
    let settings = config::Settings::load();
    // Windows startup launches straight into the notification area.
    // (A collapsed OpenMic opens as the strip, with the window hidden.)
    let start_hidden = settings.load_error.is_none()
        && ((settings.close_to_tray && std::env::args().any(|arg| arg == config::MINIMIZED_ARG)) || settings.collapsed);
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
    let result = eframe::run_native(
        &title,
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(initial_size(settings.window_size))
                .with_min_inner_size([MIN_SIZE.0, MIN_SIZE.1])
                .with_resizable(true)
                .with_icon(Arc::new(icon))
                .with_visible(!start_hidden),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(gui::App::new(settings, &cc.egui_ctx, start_hidden, instance, updated)))),
    );
    // Normal window close and eframe errors both release the virtual mic.
    virtual_mic::shutdown();
    result
}

/// The layout's natural size, and the smallest the window may get (the
/// tabs scroll below the natural height).
const NATURAL_SIZE: (f32, f32) = (760.0, 880.0);
const MIN_SIZE: (f32, f32) = (700.0, 420.0);

/// Fit the saved or natural size to the current screen's work area.
fn initial_size(saved: Option<[f32; 2]>) -> [f32; 2] {
    fit_window_size(saved, work_area_size())
}

fn fit_window_size(saved: Option<[f32; 2]>, available: Option<[f32; 2]>) -> [f32; 2] {
    let [width, height] = saved.unwrap_or([NATURAL_SIZE.0, NATURAL_SIZE.1]);
    let width = if width.is_finite() { width.max(MIN_SIZE.0) } else { NATURAL_SIZE.0 };
    let height = if height.is_finite() { height.max(MIN_SIZE.1) } else { NATURAL_SIZE.1 };
    match available {
        Some([max_width, max_height]) => [
            width.min(max_width - 40.0).max(MIN_SIZE.0),
            height.min(max_height - 40.0).max(MIN_SIZE.1),
        ],
        None => [width, height],
    }
}

/// Size of the primary screen's work area (minus the taskbar), in points.
#[cfg(windows)]
fn work_area_size() -> Option<[f32; 2]> {
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
    (dpi > 0).then(|| [
        (area.right - area.left) as f32 * 96.0 / dpi as f32,
        (area.bottom - area.top) as f32 * 96.0 / dpi as f32,
    ])
}

#[cfg(target_os = "linux")]
fn work_area_size() -> Option<[f32; 2]> {
    monitors::primary_work_area_points()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn work_area_size() -> Option<[f32; 2]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_windows_fit_smaller_screens_in_both_directions() {
        assert_eq!(fit_window_size(Some([2200.0, 1300.0]), Some([1280.0, 720.0])), [1240.0, 680.0]);
        assert_eq!(fit_window_size(None, Some([1024.0, 768.0])), [760.0, 728.0]);
        assert_eq!(fit_window_size(Some([744.0, 842.0]), Some([1920.0, 1080.0])), [744.0, 842.0]);
    }

    #[test]
    fn restored_sizes_remain_usable_when_preferences_or_screen_queries_fail() {
        assert_eq!(fit_window_size(Some([0.0, 50.0]), None), [MIN_SIZE.0, MIN_SIZE.1]);
        assert_eq!(fit_window_size(None, Some([600.0, 400.0])), [MIN_SIZE.0, MIN_SIZE.1]);
        for invalid in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            assert_eq!(fit_window_size(Some([invalid, invalid]), None), [NATURAL_SIZE.0, NATURAL_SIZE.1]);
        }
    }

    #[test]
    fn a_connected_screen_limits_the_restored_window() {
        let Some(area) = work_area_size() else { return };
        assert!(area[0].is_finite() && area[1].is_finite() && area[0] > 0.0 && area[1] > 0.0, "{area:?}");
        let fitted = fit_window_size(Some([10_000.0, 10_000.0]), Some(area));
        assert!(fitted[0] >= MIN_SIZE.0 && fitted[1] >= MIN_SIZE.1, "{fitted:?}");
        assert!(fitted[0] <= area[0].max(MIN_SIZE.0) && fitted[1] <= area[1].max(MIN_SIZE.1), "{fitted:?} vs {area:?}");
        if area[0] > MIN_SIZE.0 + 40.0 && area[1] > MIN_SIZE.1 + 40.0 {
            assert!(fitted[0] < 10_000.0 && fitted[1] < 10_000.0, "a huge window is fitted to {area:?}, got {fitted:?}");
        }
    }
}
