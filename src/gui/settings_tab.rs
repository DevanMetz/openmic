//! Settings tab: global hotkeys, speech to text, startup and the
//! notification area.

use eframe::egui::{self, RichText};

use super::{App, Binding, AMBER, CYAN, GREEN, MUTED, RED, VIOLET};
use crate::dictation::SpeechModel;
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
            },
        );
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
