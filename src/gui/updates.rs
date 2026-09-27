//! Updates in the header: the version number checks for a new release when
//! clicked, and the right of the title row shows what the updater is doing.
//! When there's nothing to report it shows the tagline, so the daily
//! background check never flickers the header.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use super::{open_url, short, App, AMBER, GREEN, MUTED, RED};
use crate::update::{self, Status};

const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// How long a manual check's result ("Up to date", a failure) stays up.
const RESULT_SHOWN: Duration = Duration::from_secs(8);
/// How long "Updated to …" shows after restarting into a new version.
pub(super) const UPDATED_SHOWN: Duration = Duration::from_secs(30);

impl App {
    /// The version beside the title; clicking it checks for updates now.
    pub(super) fn draw_version(&mut self, ui: &mut egui::Ui) {
        let status = self.updater.status();
        let idle = !matches!(status, Status::Checking | Status::Downloading(_) | Status::Ready(_));
        let button = ui
            .add(egui::Button::new(RichText::new(VERSION).weak()).frame(false))
            .on_hover_text(if idle { "Check for updates" } else { "Update in progress" });
        if idle && button.clicked() {
            self.update_error = None;
            self.update_checked_at = Some(Instant::now());
            self.updater.check();
        }
    }

    /// Right side of the title row, in a right-to-left layout: widgets are
    /// added from the right edge inward.
    pub(super) fn draw_update_status(&mut self, ui: &mut egui::Ui) {
        let recent_check = self.update_checked_at.is_some_and(|t| t.elapsed() < RESULT_SHOWN);
        if let Some(error) = self.update_error.clone() {
            if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                self.update_error = None;
            }
            ui.label(RichText::new(short(&error)).color(RED)).on_hover_text(error);
            return;
        }
        match self.updater.status().clone() {
            Status::Ready(release) => {
                let blocker = self.update_blocker();
                if ui
                    .add_enabled(blocker.is_none(), egui::Button::new("Restart to update"))
                    .on_hover_text("OpenMic closes and opens again as the new version (a few seconds)")
                    .on_disabled_hover_text(blocker.unwrap_or_default())
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.install_update(&ctx);
                }
                ui.label(RichText::new(format!("{} ready", release.tag)).color(GREEN))
                    .on_hover_text("Downloaded and checked against the release's SHA-256 checksum");
            }
            Status::Downloading(release) => {
                if ui.small_button("Cancel").clicked() {
                    self.updater.cancel_download();
                }
                let (done, total) = self.updater.progress().unwrap_or_default();
                let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(140.0)
                        .text(format!("{} / {} MB", done / 1_000_000, total / 1_000_000)),
                )
                .on_hover_text(format!("Downloading {}", release.tag));
            }
            Status::Available(release) => {
                if ui.link("What's new").clicked() {
                    open_url(&release.page);
                }
                if ui.button("Update").on_hover_text("Download and verify the new version").clicked() {
                    self.updater.download();
                }
                ui.label(RichText::new(format!("{} available", release.tag)).color(GREEN));
            }
            Status::Checking => {
                ui.weak("Checking for updates…");
                ui.spinner();
            }
            Status::Failed(e) if recent_check => {
                if ui.link("Releases page").clicked() {
                    open_url(update::RELEASES_PAGE);
                }
                ui.label(RichText::new("Couldn't check for updates").color(AMBER)).on_hover_text(e);
            }
            Status::UpToDate if recent_check => {
                ui.label(RichText::new(format!("✔ {VERSION} is up to date")).color(MUTED));
            }
            _ if self.updated_at.is_some_and(|t| t.elapsed() < UPDATED_SHOWN) => {
                ui.label(RichText::new(format!("✔ Updated to {VERSION}")).color(GREEN));
            }
            _ => {
                ui.weak("Clean voice. Instant sounds. Fully local.");
            }
        }
    }
}
