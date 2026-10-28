//! Linux desktop appearance, independent of winit's X11/Wayland theme support.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use ashpd::desktop::settings::{ColorScheme, Settings};
use eframe::egui;
use futures_channel::mpsc;
use futures_util::{FutureExt, StreamExt, select};

/// Stops the worker when the application closes. All desktop I/O stays off the UI thread.
pub(crate) struct Monitor(Arc<AtomicBool>);

impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn start(ctx: &egui::Context) -> Monitor {
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let (mut tx, mut ticks) = mpsc::unbounded();
    // Also bounds portal discovery and lets an idle signal listener shut down.
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(2));
            if tx.start_send(()).is_err() {
                break;
            }
        }
    });
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        pollster::block_on(async move {
            loop {
                let portal = async {
                    let settings = Settings::new().await?;
                    // Subscribe before reading so a change during startup is not lost.
                    let changes = settings.receive_color_scheme_changed().await?;
                    let scheme = settings.color_scheme().await?;
                    Ok::<_, ashpd::Error>((settings, changes, scheme))
                }
                .fuse();
                futures_util::pin_mut!(portal);
                let result = select! {
                    result = portal => result.ok(),
                    tick = ticks.next().fuse() => {
                        if tick.is_none() { return; }
                        None
                    }
                };
                if let Some((_settings, changes, mut scheme)) = result {
                    futures_util::pin_mut!(changes);
                    loop {
                        apply(&ctx, portal_theme(scheme).or_else(gtk_theme));
                        select! {
                            next = changes.next().fuse() => match next {
                                Some(next) => scheme = next,
                                None => break,
                            },
                            tick = ticks.next().fuse() => if tick.is_none() { return; },
                        }
                    }
                } else {
                    apply(&ctx, gtk_theme());
                    if ticks.next().await.is_none() {
                        return;
                    }
                }
            }
        })
    });
    Monitor(stopped)
}

fn apply(ctx: &egui::Context, theme: Option<egui::Theme>) {
    if let Some(theme) = theme
        && ctx.theme() != theme
    {
        // Explicit preference prevents winit's absent theme from resetting this next frame.
        ctx.set_theme(theme);
        ctx.request_repaint();
    }
}

fn portal_theme(scheme: ColorScheme) -> Option<egui::Theme> {
    match scheme {
        ColorScheme::PreferDark => Some(egui::Theme::Dark),
        ColorScheme::PreferLight => Some(egui::Theme::Light),
        ColorScheme::NoPreference => None,
    }
}

fn gsettings(key: &str) -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", key])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn gtk_theme() -> Option<egui::Theme> {
    gtk_preference(gsettings("color-scheme").as_deref(), None)
        .or_else(|| gtk_preference(None, gsettings("gtk-theme").as_deref()))
}

fn gtk_preference(scheme: Option<&str>, theme: Option<&str>) -> Option<egui::Theme> {
    let unquote = |s: &str| s.trim().trim_matches(['\'', '"']).to_owned();
    match scheme.map(unquote).as_deref() {
        Some("prefer-dark") => Some(egui::Theme::Dark),
        Some("prefer-light") => Some(egui::Theme::Light),
        _ => theme
            .map(unquote)
            .filter(|name| !name.is_empty())
            .map(|name| {
                if name.to_ascii_lowercase().ends_with("-dark") {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                }
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_preferences() {
        assert_eq!(
            portal_theme(ColorScheme::PreferDark),
            Some(egui::Theme::Dark)
        );
        assert_eq!(
            portal_theme(ColorScheme::PreferLight),
            Some(egui::Theme::Light)
        );
        assert_eq!(portal_theme(ColorScheme::NoPreference), None);
    }

    #[test]
    fn desktop_changes_survive_missing_winit_theme() {
        let ctx = egui::Context::default();
        for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            apply(&ctx, Some(theme));
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(ui.ctx().theme(), theme);
            });
        }
        // Missing desktop settings do not erase the last usable preference.
        apply(&ctx, None);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert_eq!(ui.ctx().theme(), egui::Theme::Light);
        });
    }

    #[test]
    fn gtk_preferences() {
        for (scheme, theme, expected) in [
            (
                Some("'prefer-dark'\n"),
                Some("'Adwaita'"),
                Some(egui::Theme::Dark),
            ),
            (
                Some("'prefer-light'"),
                Some("'Adwaita-dark'"),
                Some(egui::Theme::Light),
            ),
            (
                Some("'default'"),
                Some("'Adwaita-dark'\n"),
                Some(egui::Theme::Dark),
            ),
            (None, Some("\"Breeze-Dark\""), Some(egui::Theme::Dark)),
            (None, Some("'Adwaita'"), Some(egui::Theme::Light)),
            (Some("unknown"), Some("''"), None),
            (None, None, None),
        ] {
            assert_eq!(gtk_preference(scheme, theme), expected);
        }
    }
}
