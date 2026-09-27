//! Settings tab: global hotkeys, speech to text, startup and the
//! notification area.

use eframe::egui::{self, RichText};

use super::{open_url, short, App, Binding, AMBER, CYAN, GREEN, MUTED, RED, VIOLET};
use crate::dictation::SpeechModel;
use crate::update::{self, Status};
use crate::widgets;

impl App {
    pub(super) fn draw_settings_tab(&mut self, ui: &mut egui::Ui) {
        let hotkeys_ok = self.hotkeys.is_some();
        widgets::card(
            ui,
            "HOTKEYS",
            VIOLET,
            |ui| {
                if hotkeys_ok {
                    widgets::badge(ui, "Work in any app", GREEN);
                }
            },
            |ui| {
                egui::Grid::new("hotkeys").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
                    let h = &self.settings.hotkeys;
                    let rows = [
                        ("Mute mic", Binding::Mute, h.mute.clone()),
                        ("Bypass processing", Binding::Bypass, h.bypass.clone()),
                        ("Stop all clips", Binding::StopClips, h.stop_clips.clone()),
                        ("Speech to text", Binding::Dictate, h.dictate.clone()),
                    ];
                    for (label, binding, current) in rows {
                        ui.label(RichText::new(label).weak());
                        let waiting = self.capturing.as_ref() == Some(&binding);
                        ui.add_sized(
                            [150.0, 18.0],
                            egui::Label::new(if waiting {
                                RichText::new("Press keys…").color(CYAN)
                            } else {
                                RichText::new(current.as_deref().unwrap_or("Not set")).monospace()
                            }),
                        );
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(hotkeys_ok, |ui| {
                                if waiting {
                                    if ui.button("Cancel").clicked() {
                                        self.capturing = None;
                                        self.hotkey_status = ("Shortcut unchanged".into(), super::MUTED);
                                    }
                                } else if ui.button("Set…").clicked() {
                                    self.start_capture(binding.clone());
                                }
                                if current.is_some() && ui.button("Clear").clicked() {
                                    self.assign_hotkey(&binding, None);
                                }
                            });
                        });
                        ui.end_row();
                    }
                });
                ui.add_space(4.0);
                ui.weak("Soundboard pads get their own hotkeys: right-click a pad.");
                if !self.hotkey_status.0.is_empty() {
                    ui.label(RichText::new(&self.hotkey_status.0).color(self.hotkey_status.1));
                }
            },
        );
        ui.add_space(8.0);
        self.draw_speech_card(ui);
        ui.add_space(8.0);

        let tray_ok = self.tray.is_some();
        widgets::card(
            ui,
            "GENERAL",
            CYAN,
            |_| {},
            |ui| {
                if ui
                    .checkbox(&mut self.settings.start_with_windows, "Start with Windows")
                    .on_hover_text("Starts hidden in the notification area")
                    .changed()
                {
                    self.toggle_startup(self.settings.start_with_windows);
                }
                if ui
                    .checkbox(&mut self.settings.auto_start, "Start processing automatically")
                    .changed()
                {
                    self.touch();
                }
                let tray = ui.add_enabled(
                    tray_ok,
                    egui::Checkbox::new(
                        &mut self.settings.close_to_tray,
                        "Keep running in the notification area when the window closes",
                    ),
                );
                if tray.changed() {
                    self.touch();
                }
                ui.weak(if !tray_ok {
                    "The notification area isn't available, so closing the window quits OpenMic."
                } else if self.settings.close_to_tray {
                    "Click the tray icon to reopen OpenMic; quit from its right-click menu."
                } else {
                    "Closing the window quits OpenMic."
                });
                ui.add_space(4.0);
                self.draw_updates(ui);
            },
        );
    }

    /// Update check toggle, then what the updater is doing and what to do next.
    fn draw_updates(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut self.settings.check_for_updates, "Check for updates daily")
                .on_hover_text("Asks GitHub for the latest OpenMic release; nothing else is sent")
                .changed()
            {
                self.touch();
            }
            let status = self.updater.status().clone();
            let busy = matches!(status, Status::Checking | Status::Downloading(_));
            if !busy && !matches!(status, Status::Ready(_)) && ui.small_button("Check now").clicked() {
                self.update_error = None;
                self.updater.check();
            }
        });

        let current = format!("v{}", env!("CARGO_PKG_VERSION"));
        ui.horizontal(|ui| match self.updater.status().clone() {
            Status::Idle if self.just_updated => {
                ui.label(RichText::new(format!("Updated to {current}")).color(GREEN));
            }
            Status::Idle => {
                ui.weak(format!("OpenMic {current}"));
            }
            Status::Checking => {
                ui.spinner();
                ui.weak("Checking for updates…");
            }
            Status::UpToDate if self.just_updated => {
                ui.label(RichText::new(format!("Updated to {current}, the latest version")).color(GREEN));
            }
            Status::UpToDate => {
                ui.label(RichText::new(format!("{current} is the latest version")).color(MUTED));
            }
            Status::Failed(e) => {
                ui.label(RichText::new(short(&e)).color(AMBER)).on_hover_text(&e);
                if ui.small_button("Download page").clicked() {
                    open_url(update::RELEASES_PAGE);
                }
            }
            Status::Available(release) => {
                ui.label(RichText::new(format!("{} is available", release.tag)).color(GREEN));
                if ui.button("Download update").clicked() {
                    self.update_error = None;
                    self.updater.download();
                }
                if ui.small_button("What's new").clicked() {
                    open_url(&release.page);
                }
            }
            Status::Downloading(release) => {
                let (done, total) = self.updater.progress().unwrap_or_default();
                let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(150.0)
                        .text(format!("{} / {} MB", done / 1_000_000, total / 1_000_000)),
                )
                .on_hover_text(format!("Downloading {}", release.tag));
                if ui.button("Cancel").clicked() {
                    self.updater.cancel_download();
                }
            }
            Status::Ready(release) => {
                ui.label(RichText::new(format!("{} is downloaded and verified", release.tag)).color(GREEN));
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
            }
        });
        if let Some(error) = &self.update_error {
            ui.label(RichText::new(error).color(RED));
        }
    }

    fn draw_speech_card(&mut self, ui: &mut egui::Ui) {
        widgets::card(
            ui,
            "SPEECH TO TEXT",
            AMBER,
            |ui| widgets::badge(ui, "Runs on this PC", GREEN),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Model").weak());
                    let before = self.settings.speech_model;
                    egui::ComboBox::from_id_salt("speech_model")
                        .width(240.0)
                        .selected_text(before.label())
                        .show_ui(ui, |ui| {
                            for model in SpeechModel::ALL {
                                let text = if model.is_downloaded() {
                                    format!("{} ✔", model.label())
                                } else {
                                    model.label().to_owned()
                                };
                                ui.selectable_value(&mut self.settings.speech_model, model, text);
                            }
                        });
                    if self.settings.speech_model != before {
                        self.touch();
                    }
                    self.draw_model_download(ui);
                });
                ui.weak(self.settings.speech_model.hint());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    let mut changed = ui
                        .radio_value(&mut self.settings.dictation_hold, true, "Hold the shortcut while you speak")
                        .changed();
                    changed |= ui
                        .radio_value(&mut self.settings.dictation_hold, false, "Press to start, again to stop")
                        .changed();
                    if changed {
                        self.touch();
                    }
                });
                ui.weak("Your words are typed into the app you're using. Set the shortcut under Hotkeys.");

                let working = self.dictating.is_some() || self.dictation.busy();
                if working || !self.dictation_status.0.is_empty() {
                    ui.horizontal(|ui| {
                        if working {
                            ui.spinner();
                        }
                        ui.label(RichText::new(&self.dictation_status.0).color(self.dictation_status.1));
                    });
                }
                if !self.last_dictation.is_empty() {
                    ui.horizontal(|ui| {
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(self.last_dictation.clone());
                        }
                        ui.add(egui::Label::new(RichText::new(&self.last_dictation).italics()).truncate())
                            .on_hover_text(&self.last_dictation);
                    });
                }
            },
        );
    }

    /// Download progress, or the button to download or remove the model.
    fn draw_model_download(&mut self, ui: &mut egui::Ui) {
        let model = self.settings.speech_model;
        if let Some((downloading, done, total)) = self.dictation.download_progress() {
            let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(150.0)
                    .text(format!("{} MB", done / 1_000_000)),
            )
            .on_hover_text(format!("Downloading {}", downloading.label()));
            if ui.button("Cancel").clicked() {
                self.dictation.cancel_download();
            }
        } else if !model.is_downloaded() {
            if ui.button("Download").on_hover_text("Downloads once from Hugging Face").clicked() {
                self.dictation.start_download(model);
                self.dictation_status = (format!("Downloading {}…", model.label()), CYAN);
            }
        } else if ui.button("Remove").on_hover_text("Delete the downloaded model").clicked() {
            self.dictation_status = match model.remove() {
                Ok(()) => ("Model removed".into(), MUTED),
                // Windows locks a model that's loaded.
                Err(_) => ("In use: restart OpenMic to remove this model".into(), RED),
            };
        }
    }
}
