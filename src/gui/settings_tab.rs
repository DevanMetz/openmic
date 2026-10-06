//! Settings tab: global hotkeys, speech to text, startup and the
//! notification area.

use eframe::egui::{self, RichText};

use super::{clock_time, App, Binding, Command, AMBER, CYAN, GREEN, MUTED, RED, VIOLET};
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
                                RichText::new("Press keys…").color(widgets::text_color(ui, CYAN))
                            } else {
                                RichText::new(current.as_deref().unwrap_or("Not set")).monospace()
                            }),
                        );
                        ui.horizontal(|ui| {
                            if waiting {
                                if ui.button("Cancel").clicked() {
                                    self.cancel_capture();
                                }
                            } else if ui.add_enabled(hotkeys_ok, egui::Button::new("Set…"))
                                .on_disabled_hover_text("Global shortcuts are unavailable. Restart OpenMic to retry.")
                                .clicked()
                            {
                                self.start_capture(binding.clone());
                            }
                            if current.is_some() && ui.button("Clear").clicked() {
                                self.assign_hotkey(&binding, None);
                            }
                        });
                        ui.end_row();
                    }
                });
                ui.add_space(4.0);
                ui.weak("Soundboard pads get their own hotkeys: right-click a pad.");
                if !self.hotkey_status.0.is_empty() {
                    ui.label(RichText::new(&self.hotkey_status.0).color(widgets::text_color(ui, self.hotkey_status.1)));
                }
            },
        );
        ui.add_space(8.0);

        let tray_ok = self.tray.is_some();
        widgets::card(
            ui,
            "GENERAL",
            CYAN,
            |_| {},
            |ui| {
                let startup = ui
                    .checkbox(&mut self.settings.start_with_windows, "Start with Windows")
                    .on_hover_text("Starts hidden in the notification area");
                widgets::scroll_on_focus(&startup);
                if startup.changed() {
                    self.toggle_startup(self.settings.start_with_windows);
                }
                let auto_start = ui.checkbox(&mut self.settings.auto_start, "Start processing automatically");
                widgets::scroll_on_focus(&auto_start);
                if auto_start.changed() {
                    self.touch();
                }
                let tray = ui.add_enabled(
                    tray_ok,
                    egui::Checkbox::new(
                        &mut self.settings.close_to_tray,
                        "Keep running in the notification area when the window closes",
                    ),
                );
                widgets::scroll_on_focus(&tray);
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
                let minimize = ui
                    .checkbox(&mut self.settings.minimize_to_strip, "Minimize to the strip")
                    .on_hover_text("The window's minimize button collapses OpenMic to the always-on-top strip");
                widgets::scroll_on_focus(&minimize);
                if minimize.changed() {
                    self.touch();
                }
                let strip = ui
                    .checkbox(&mut self.settings.always_show_strip, "Always show the strip")
                    .on_hover_text("Keep the strip visible when you open the full window");
                widgets::scroll_on_focus(&strip);
                if strip.changed() {
                    self.touch();
                }
                ui.add_space(4.0);
                self.draw_updates(ui);
            },
        );
    }

    /// The daily check; checking now and installing happen in the header.
    fn draw_updates(&mut self, ui: &mut egui::Ui) {
        let updates = ui
            .checkbox(&mut self.settings.check_for_updates, "Check for updates daily")
            .on_hover_text("Asks GitHub for the latest OpenMic release; nothing else is sent");
        widgets::scroll_on_focus(&updates);
        if updates.changed() {
            self.touch();
        }
        ui.weak("Click the version number at the top to check now.");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let logs = ui
                .button("Open log folder")
                .on_hover_text("openmic.log records device changes, errors and updates; attach it to a bug report");
            widgets::scroll_on_focus(&logs);
            if logs.clicked() {
                super::open_url(&crate::logfile::dir().display().to_string());
            }
        });
    }

    /// Speech to text: the model, how the shortcut works, and the result.
    pub(super) fn draw_dictation_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Shortcut").weak());
            let waiting = self.capturing.as_ref() == Some(&Binding::Dictate);
            let current = self.settings.hotkeys.dictate.clone();
            ui.label(if waiting {
                RichText::new("Press keys…").color(widgets::text_color(ui, CYAN))
            } else {
                RichText::new(current.as_deref().unwrap_or("Not set")).monospace()
            });
            if waiting {
                let cancel = ui.button("Cancel");
                widgets::scroll_on_focus(&cancel);
                if cancel.clicked() {
                    self.cancel_capture();
                }
            } else {
                let set = ui.add_enabled(self.hotkeys.is_some(), egui::Button::new("Set…"))
                    .on_disabled_hover_text("Global shortcuts are unavailable. Restart OpenMic to retry.");
                widgets::scroll_on_focus(&set);
                if set.clicked() {
                    self.start_capture(Binding::Dictate);
                }
            }
            if current.is_some() {
                let clear = ui.button("Clear");
                widgets::scroll_on_focus(&clear);
                if clear.clicked() {
                    self.assign_hotkey(&Binding::Dictate, None);
                }
            }
        });
        if (self.capturing.as_ref() == Some(&Binding::Dictate) || self.hotkeys.is_none())
            && !self.hotkey_status.0.is_empty()
        {
            ui.label(RichText::new(&self.hotkey_status.0).color(widgets::text_color(ui, self.hotkey_status.1)));
        }
        if self.capturing.is_none()
            && let Some(shortcut) = &self.settings.hotkeys.dictate
            && let Some(hotkeys) = &self.hotkeys
            && hotkeys.was_refused(shortcut, &Command::Dictate)
        {
            ui.colored_label(widgets::text_color(ui, AMBER), "This shortcut is unavailable. Choose another combination.");
        }
        ui.add_space(8.0);
        // Shortcut warnings must not change the speech controls' identities.
        ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("dictation-speech")), |ui| {
            self.draw_speech_card(ui);
        });
    }

    fn draw_speech_card(&mut self, ui: &mut egui::Ui) {
        widgets::card(
            ui,
            "SPEECH TO TEXT",
            AMBER,
            |ui| widgets::badge(ui, "Runs on this PC", GREEN),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Model").weak());
                    let before = self.settings.speech_model;
                    let choice = egui::ComboBox::from_id_salt("speech_model")
                        .width(240.0)
                        .selected_text(before.label())
                        .show_ui(ui, |ui| {
                            for model in SpeechModel::ALL {
                                let text = if model.is_downloaded() {
                                    format!("{} · downloaded", model.label())
                                } else {
                                    model.label().to_owned()
                                };
                                ui.selectable_value(&mut self.settings.speech_model, model, text);
                            }
                        }).response.on_hover_text("Applies to the next recording");
                    widgets::scroll_on_focus(&choice);
                    if self.settings.speech_model != before {
                        self.touch();
                    }
                    self.draw_model_download(ui);
                });
                ui.weak(self.settings.speech_model.hint());
                ui.add_space(4.0);

                ui.horizontal_wrapped(|ui| {
                    let hold = ui
                        .radio_value(&mut self.settings.dictation_hold, true, "Hold the shortcut while you speak")
                        .on_hover_text("Applies to the next recording");
                    widgets::scroll_on_focus(&hold);
                    let toggle = ui
                        .radio_value(&mut self.settings.dictation_hold, false, "Press to start, again to stop")
                        .on_hover_text("Applies to the next recording");
                    widgets::scroll_on_focus(&toggle);
                    if hold.changed() || toggle.changed() {
                        self.touch();
                    }
                });
                ui.weak("Your words are typed into the app you're using. Set your Shortcut above.");

                if let Some(recording) = &self.dictating {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let (text, color) = if recording.recorder.is_stopping() {
                            ("Finishing recording…".into(), CYAN)
                        } else {
                            (format!("Listening… {}", clock_time(recording.recorder.elapsed())), AMBER)
                        };
                        ui.colored_label(widgets::text_color(ui, color), text)
                            .on_hover_text(format!("Using {}", recording.model.label()));
                    });
                    let discard = ui.button("Discard recording")
                        .on_hover_text("Discard this take before it is transcribed or typed");
                    widgets::scroll_on_focus(&discard);
                    if discard.clicked() {
                        self.discard_dictation();
                    }
                }
                if self.dictation.busy() {
                    // Another take can appear or disappear while this job runs.
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("dictation-transcription")), |ui| {
                        let cancelling = self.dictation.transcription_cancelling();
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.colored_label(widgets::text_color(ui, CYAN), if cancelling { "Cancelling transcription…" } else { "Transcribing…" });
                        });
                        let cancel = ui.add_enabled(!cancelling, egui::Button::new("Cancel transcription"))
                            .on_hover_text("Discard the current transcription and queued takes before they type");
                        widgets::scroll_on_focus(&cancel);
                        if cancel.clicked() {
                            self.dictation.cancel_transcription();
                            self.dictation_status = (String::new(), MUTED);
                        }
                    });
                }
                if !self.dictation_status.0.is_empty() {
                    ui.label(RichText::new(&self.dictation_status.0).color(widgets::text_color(ui, self.dictation_status.1)));
                }
                if !self.last_dictation.is_empty() {
                    // Background status rows must not replace the copy control
                    // or the selectable text's identity.
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("dictation-transcript")), |ui| {
                        ui.horizontal(|ui| {
                            let copy = ui.small_button("Copy");
                            widgets::scroll_on_focus(&copy);
                            if copy.clicked() {
                                ui.ctx().copy_text(self.last_dictation.clone());
                            }
                            ui.add(egui::Label::new(RichText::new(&self.last_dictation).italics()).truncate())
                                .on_hover_text(&self.last_dictation);
                        });
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
            let cancelling = self.dictation.download_cancelling();
            let cancel = ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Cancelling…" } else { "Cancel" }));
            widgets::scroll_on_focus(&cancel);
            if cancel.clicked() {
                self.dictation.cancel_download();
                self.dictation_status = ("Cancelling model download…".into(), MUTED);
            }
        } else if !model.is_downloaded() {
            let download = ui.button("Download").on_hover_text("Downloads once from Hugging Face");
            widgets::scroll_on_focus(&download);
            if download.clicked() {
                self.dictation.start_download(model);
                self.dictation_status = (format!("Downloading {}…", model.label()), CYAN);
            }
        } else {
            let in_use = self.dictation.busy()
                || self.dictating.as_ref().is_some_and(|recording| recording.model == model);
            let remove = ui.add_enabled(!in_use, egui::Button::new("Remove"))
                .on_hover_text("Delete the downloaded model")
                .on_disabled_hover_text("Finish dictation before removing a speech model");
            widgets::scroll_on_focus(&remove);
            if remove.clicked() {
                self.dictation_status = match model.remove() {
                    Ok(()) => ("Model removed".into(), MUTED),
                    Err(error) => {
                        crate::logfile::warn(format_args!("speech model removal failed: {error:#}"));
                        (format!("Could not remove model: {}", error.root_cause()), RED)
                    }
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use crate::dictation::Dictation;
    use crate::record::Recorder;
    use super::super::{DictationRecording, Page};

    fn draw(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        draw_size(app, ctx, egui::vec2(700.0, 720.0), events)
    }

    fn draw_size(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    #[test]
    fn dictation_mode_keeps_focus_when_shortcut_warnings_change() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for hold in [true, false] {
                    for appears in [true, false] {
                        let mut app = App::stopped(Default::default());
                        app.page = Page::Dictation;
                        app.settings.dictation_hold = !hold;
                        if !appears { app.hotkey_status = ("Shortcut unavailable".into(), AMBER); }
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        let label = if hold { "Hold the shortcut while you speak" } else { "Press to start, again to stop" };
                        for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                        let output = draw(&mut app, &ctx, Vec::new());
                        let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                            node.role() == Role::RadioButton && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        draw(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                        })]);
                        for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                        app.hotkey_status = if appears { ("Shortcut unavailable".into(), AMBER) } else { (String::new(), MUTED) };
                        let output = draw(&mut app, &ctx, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let current = *tree.nodes.iter().find(|(_, node)| node.role() == Role::RadioButton
                            && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        assert_eq!(current, target, "dictation mode keeps its identity; appears={appears}, passes={passes}");
                        assert_eq!(tree.focus, target, "the shortcut warning keeps keyboard focus with the same control");
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none());
                        draw(&mut app, &ctx, vec![egui::Event::Key {
                            key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }]);
                        for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                        assert_eq!(app.settings.dictation_hold, hold, "Space activates the focused mode after the warning changes");
                        assert!(app.dirty_since.is_some() && app.dictating.is_none() && !app.dictation.busy());
                    }
                }
            }
        }
    }

    fn pending_dictation() -> (App, crossbeam_channel::Sender<()>) {
        let (dictation, release) = Dictation::delayed_for_test();
        let mut app = App::stopped(Default::default());
        app.page = Page::Dictation;
        app.dictation = dictation;
        app.hotkey_status = ("This shortcut is unavailable. Choose another combination to retry.".into(), AMBER);
        app.last_dictation = "Previous words café\nSecond line.".into();
        app.dictation_status = ("Copy the previous transcript below while your next take is being transcribed.".into(), AMBER);
        let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
        app.dictation.transcribe(app.settings.speech_model, take);
        app.dictating = Some(DictationRecording {
            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
            model: app.settings.speech_model, hold_to_talk: false,
        });
        (app, release)
    }

    #[test]
    fn transcription_cancel_keeps_focus_when_recording_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for starts in [true, false] {
                    let (mut app, release) = pending_dictation();
                    if starts { app.dictating = None; }
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let transcript = app.last_dictation.clone();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    let output = draw(&mut app, &ctx, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some("Cancel transcription")).map(|(id, _)| id).unwrap();
                    draw(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    if starts {
                        app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
                            model: app.settings.speech_model, hold_to_talk: false,
                        });
                    } else { app.discard_dictation(); }
                    let output = draw(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let current = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                        && node.label() == Some("Cancel transcription")).map(|(id, _)| id).unwrap();
                    assert_eq!(current, target, "recording changes preserve the cancellation control; starts={starts}, passes={passes}");
                    assert_eq!(tree.focus, target);
                    assert!(app.dictation.busy() && !app.dictation.transcription_cancelling());
                    draw(&mut app, &ctx, vec![egui::Event::Key {
                        key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }]);
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    assert!(app.dictation.transcription_cancelling());
                    assert_eq!(app.dictating.is_some(), starts, "cancelling the previous transcription preserves a new take");
                    assert_eq!(app.last_dictation, transcript);
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none());
                    release.send(()).unwrap();
                }
            }
        }
    }

    #[test]
    fn navigation_reveals_dictation_actions_in_a_short_window() {
        use egui::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};
        // At 125% zoom, the minimum-size native window has fewer UI points.
        let zoom = 1.25;
        let size = egui::vec2(700.0, 420.0) / zoom;
        let key = |key, modifiers| vec![egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        let bounds = |output: &egui::FullOutput, target: NodeId| {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32))
        };
        let visible = |output: &egui::FullOutput, target| {
            let rect = bounds(output, target);
            output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect) && match &shape.shape {
                egui::Shape::Rect(shape) => shape.rect.intersects(rect),
                egui::Shape::Text(shape) => shape.visual_bounding_rect().intersects(rect),
                _ => false,
            })
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    for label in ["Discard recording", "Cancel transcription", "Copy"] {
                        let (mut app, release) = pending_dictation();
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let transcript = app.last_dictation.clone();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = zoom;
                        });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        for (id, node) in &output.platform_output.accesskit_update.as_ref().unwrap().nodes {
                            if matches!(node.role(), Role::RadioButton | Role::ComboBox)
                                || matches!(node.label(), Some("Download" | "Remove"))
                            {
                                assert!(visible(&output, *id), "the zoomed speech control stays within the viewport: {:?}", node.label());
                            }
                        }
                        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_, node)|
                            node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        if label == "Copy" { assert!(!visible(&output, target), "this case starts with Copy below the viewport"); }
                        let original_bounds = bounds(&draw_size(&mut app, &ctx, size, Vec::new()), target);
                        if assistive {
                            draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                            })]);
                        } else {
                            let mut reached = false;
                            for _ in 0..64 {
                                let output = draw_size(&mut app, &ctx, size, key(egui::Key::Tab, egui::Modifiers::NONE));
                                if output.platform_output.accesskit_update.unwrap().focus == target { reached = true; break; }
                            }
                            assert!(reached, "Tab reaches {label}");
                        }
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert!(visible(&output, target), "{label} is fully visible; dark={dark}, passes={passes}, assistive={assistive}");
                        for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                            draw_size(&mut app, &ctx, size, key(egui::Key::Tab, modifiers));
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert!(visible(&output, target));
                        for delta in [1000.0, -1000.0] {
                            draw_size(&mut app, &ctx, size, vec![
                                egui::Event::PointerMoved(egui::pos2(size.x - 14.0, size.y - 60.0)),
                                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                    modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
                            ]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                            if delta > 0.0 { assert_eq!(bounds(&output, target).center(), original_bounds.center(), "focus allows manually scrolling to the page top"); }
                            else { assert!(visible(&output, target)); }
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && app.dictating.is_some() && app.dictation.busy()
                            && !app.dictation.transcription_cancelling());
                        let mut output = draw_size(&mut app, &ctx, size, key(egui::Key::Enter, egui::Modifiers::NONE));
                        let mut commands = output.platform_output.commands;
                        for _ in 0..3 {
                            output = draw_size(&mut app, &ctx, size, Vec::new());
                            commands.extend(output.platform_output.commands);
                        }
                        match label {
                            "Discard recording" => assert!(app.dictating.is_none() && !app.dictation.transcription_cancelling()),
                            "Cancel transcription" => assert!(app.dictating.is_some() && app.dictation.transcription_cancelling()),
                            _ => assert_eq!(commands.iter().filter_map(|command| match command {
                                egui::OutputCommand::CopyText(text) => Some(text.as_str()), _ => None,
                            }).collect::<Vec<_>>(), [transcript.as_str()], "Enter copies the full previous transcript once"),
                        }
                        assert_eq!(app.last_dictation, transcript);
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        app.dictation.cancel_transcription();
                        release.send(()).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes Dictation keyboard focus screenshots; needs Python with Pillow"]
    fn snapshot_dictation_keyboard_focus() {
        use egui::accesskit::Role;
        for (zoom, size_label) in [(1.0, "standard"), (1.25, "zoomed")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for (control, label) in [("cancel", "Cancel transcription"), ("copy", "Copy")] {
                    let (mut app, release) = pending_dictation();
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let mut events = vec![Vec::new(); 3];
                    let mut focused = false;
                    for _ in 0..64 {
                        let tab = vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }];
                        let output = draw_size(&mut app, &ctx, size, tab.clone());
                        events.push(tab);
                        if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                    }
                    assert!(focused);
                    events.extend(vec![Vec::new(); 8]);
                    // Matching reduced UI points and pixel scale reproduces native zoom.
                    let image = super::super::snapshot::snapshot_with_events(&mut app,
                        &format!("dictation-keyboard-{control}-{size_label}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.dictating.is_some() && !app.dictation.transcription_cancelling());
                    app.dictation.cancel_transcription();
                    release.send(()).unwrap();
                }
            }
        }
    }

    #[test]
    fn keyboard_navigation_reveals_general_settings_in_a_short_window() {
        use egui::accesskit::{NodeId, Role};
        let size = egui::vec2(700.0, 420.0);
        let key = |modifiers| vec![egui::Event::Key {
            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
        }];
        let bounds = |output: &egui::FullOutput, target: NodeId| {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32))
        };
        let visible = |output: &egui::FullOutput, target| {
            let rect = bounds(output, target);
            output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect) && match &shape.shape {
                egui::Shape::Rect(shape) => shape.rect.intersects(rect),
                egui::Shape::Text(shape) => shape.visual_bounding_rect().intersects(rect),
                _ => false,
            })
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for warning in [false, true] {
                    for label in ["Start with Windows", "Start processing automatically", "Minimize to the strip",
                        "Always show the strip", "Check for updates daily", "Open log folder"] {
                        let mut app = App::stopped(Default::default());
                        app.page = Page::Settings;
                        if warning { app.hotkey_status = ("Some shortcuts are unavailable. Choose another combination for each affected action.".into(), AMBER); }
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let target = *tree.nodes.iter().find(|(_, node)| matches!(node.role(), Role::Button | Role::CheckBox)
                            && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        let original_bounds = bounds(&output, target);
                        let mut focused = false;
                        for _ in 0..64 {
                            let output = draw_size(&mut app, &ctx, size, key(egui::Modifiers::NONE));
                            if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                        }
                        assert!(focused, "Tab reaches {label}");
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert!(visible(&output, target), "{label} is fully visible when focused; dark={dark}, warning={warning}, passes={passes}");
                        for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                            draw_size(&mut app, &ctx, size, key(modifiers));
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert!(visible(&output, target), "returning with Shift+Tab and Tab reveals {label}");
                        for delta in [1000.0, -1000.0] {
                            draw_size(&mut app, &ctx, size, vec![
                                egui::Event::PointerMoved(egui::pos2(size.x - 14.0, size.y - 60.0)),
                                egui::Event::MouseWheel {
                                    unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                    modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                                },
                            ]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                            if delta > 0.0 { assert_eq!(bounds(&output, target), original_bounds, "focus allows scrolling back to the page top"); }
                            else { assert!(visible(&output, target), "scrolling down reveals {label} again"); }
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original, "navigation never changes saved preferences");
                        assert!(app.dirty_since.is_none() && app.capturing.is_none() && app.engine.is_none() && !app.dictation.busy());
                    }
                }
            }
        }
    }

    #[test]
    fn clicking_general_settings_keeps_the_page_position() {
        use egui::accesskit::Role;
        let size = egui::vec2(700.0, 420.0);
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = App::stopped(Default::default());
                app.page = Page::Settings;
                app.settings.always_show_strip = false;
                let mut expected = app.settings.clone();
                expected.always_show_strip = true;
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                draw_size(&mut app, &ctx, size, vec![
                    egui::Event::PointerMoved(egui::pos2(size.x - 14.0, size.y - 60.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -20.0),
                        modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                    },
                ]);
                for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                let output = draw_size(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let (target, node) = tree.nodes.iter().find(|(_, node)| node.role() == Role::CheckBox
                    && node.label() == Some("Always show the strip")).unwrap();
                let (target, bounds) = (*target, node.bounds().unwrap());
                let pos = egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32);
                for pressed in [true, false] {
                    let mut output = draw_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }]);
                    if !pressed {
                        for _ in 0..8 { output = draw_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert_eq!(tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds(), Some(bounds),
                        "pointer focus never scrolls the page during a click");
                }
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), serde_json::to_value(&expected).unwrap());
                assert!(app.dirty_since.is_some() && app.capturing.is_none() && app.engine.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes Settings keyboard focus screenshots; needs Python with Pillow"]
    fn snapshot_settings_keyboard_focus() {
        use egui::accesskit::Role;
        let size = egui::vec2(700.0, 420.0);
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (control, label) in [("updates", "Check for updates daily"), ("logs", "Open log folder")] {
                let mut app = App::stopped(Default::default());
                app.page = Page::Settings;
                app.hotkey_status = ("Some shortcuts are unavailable. Choose another combination for each affected action.".into(), AMBER);
                let original = serde_json::to_value(&app.settings).unwrap();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let output = draw_size(&mut app, &ctx, size, Vec::new());
                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                    matches!(node.role(), Role::Button | Role::CheckBox) && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                let mut events = vec![Vec::new()];
                let mut focused = false;
                for _ in 0..64 {
                    let tab = vec![egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }];
                    let output = draw_size(&mut app, &ctx, size, tab.clone());
                    events.push(tab);
                    if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
                }
                assert!(focused);
                events.extend(vec![Vec::new(); 8]);
                let image = super::super::snapshot::snapshot_with_events(&mut app, &format!("settings-keyboard-{control}-{theme}"),
                    size, 1.25, dark, events);
                println!("SNAPSHOT {}", image.display());
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                assert!(app.dirty_since.is_none());
            }
        }
    }

    #[test]
    fn selected_transcript_words_survive_status_and_shortcut_warning_changes() {
        let transcript = "Previous words café";
        for dark in [true, false] {
            for passes in [1, 2] {
                for shortcut_warning in [false, true] {
                    let mut app = App::stopped(Default::default());
                    app.page = Page::Dictation;
                    app.last_dictation = transcript.into();
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    let output = draw(&mut app, &ctx, Vec::new());
                    let (start, end) = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == transcript => Some((
                            text.pos + text.galley.pos_from_cursor(egui::text::CCursor::new(0)).center().to_vec2(),
                            text.pos + text.galley.pos_from_cursor(egui::text::CCursor::new(8)).center().to_vec2(),
                        )),
                        _ => None,
                    }).unwrap();
                    draw(&mut app, &ctx, vec![egui::Event::PointerMoved(start), egui::Event::PointerButton {
                        pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE,
                    }]);
                    draw(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
                    draw(&mut app, &ctx, vec![egui::Event::PointerButton {
                        pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE,
                    }]);
                    let copy = |app: &mut App| {
                        let output = draw(app, &ctx, vec![egui::Event::Copy]);
                        let words: Vec<_> = output.platform_output.commands.iter().filter_map(|command| match command {
                            egui::OutputCommand::CopyText(text) => Some(text.as_str()), _ => None,
                        }).collect();
                        assert_eq!(words, ["Previous"], "copy keeps the mouse-selected word; passes={passes}");
                    };
                    copy(&mut app);
                    for visible in [true, false] {
                        let status = if visible { ("Shortcut unavailable".into(), AMBER) } else { (String::new(), MUTED) };
                        if shortcut_warning { app.hotkey_status = status; } else { app.dictation_status = status; }
                        for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                        copy(&mut app);
                    }
                    assert_eq!(app.last_dictation, transcript);
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.dictating.is_none() && !app.dictation.busy());
                }
            }
        }
    }

    #[test]
    fn copy_transcript_keeps_focus_through_background_status_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let transcript = "Previous words café\nSecond line.";
        for dark in [true, false] {
            for passes in [1, 2] {
                for scene in ["listening starts", "recording discarded", "status appears", "status clears",
                    "transcription starts", "transcription finishes", "shortcut warning appears", "shortcut warning clears"] {
                    let (dictation, release) = Dictation::delayed_for_test();
                    let mut release = Some(release);
                    let mut app = App::stopped(Default::default());
                    app.dictation = dictation;
                    app.page = Page::Dictation;
                    app.last_dictation = transcript.into();
                    if scene == "recording discarded" {
                        app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
                            model: app.settings.speech_model, hold_to_talk: false,
                        });
                    }
                    if scene == "status clears" { app.dictation_status = ("Typed".into(), GREEN); }
                    if scene == "shortcut warning clears" { app.hotkey_status = ("Shortcut unavailable".into(), AMBER); }
                    if scene == "transcription finishes" {
                        let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
                        app.dictation.transcribe(app.settings.speech_model, take);
                        app.dictation.cancel_transcription();
                    }
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    let output = draw(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let copy = *tree.nodes.iter()
                        .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Copy")).map(|(id, _)| id).unwrap();
                    let text = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Label && node.value() == Some(transcript))
                        .map(|(id, _)| id).unwrap();
                    draw(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: copy, data: None,
                    })]);
                    for _ in 0..3 { draw(&mut app, &ctx, Vec::new()); }
                    match scene {
                        "listening starts" => app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]),
                            model: app.settings.speech_model, hold_to_talk: false,
                        }),
                        "recording discarded" => app.discard_dictation(),
                        "status appears" => app.dictation_status = ("Model ready".into(), GREEN),
                        "status clears" => app.dictation_status = (String::new(), MUTED),
                        "shortcut warning appears" => app.hotkey_status = ("Shortcut unavailable".into(), AMBER),
                        "shortcut warning clears" => app.hotkey_status = (String::new(), MUTED),
                        "transcription starts" => {
                            let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
                            app.dictation.transcribe(app.settings.speech_model, take);
                        }
                        _ => {
                            release.take().unwrap().send(()).unwrap();
                            let deadline = Instant::now() + Duration::from_secs(3);
                            while app.dictation.busy() || app.dictation_status.0 != "Transcription cancelled" {
                                app.poll_dictation();
                                assert!(Instant::now() < deadline, "the cancelled worker finishes");
                                std::thread::sleep(Duration::from_millis(1));
                            }
                        }
                    }
                    let output = draw(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let current = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                        && node.label() == Some("Copy")).map(|(id, _)| id).unwrap();
                    assert_eq!(current, copy, "Copy keeps its identity when {scene}; passes={passes}");
                    assert_eq!(tree.focus, copy, "Copy stays focused when {scene}");
                    assert!(tree.nodes.iter().any(|(id, node)| *id == text && node.value() == Some(transcript)),
                        "the transcript's identity stays with its selectable text");
                    for key in [egui::Key::Enter, egui::Key::Space] {
                        let output = draw(&mut app, &ctx, vec![egui::Event::Key {
                            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }]);
                        let mut copied = output.platform_output.commands;
                        for _ in 0..3 { copied.extend(draw(&mut app, &ctx, Vec::new()).platform_output.commands); }
                        let words: Vec<_> = copied.iter().filter_map(|command| match command {
                            egui::OutputCommand::CopyText(text) => Some(text.as_str()), _ => None,
                        }).collect();
                        assert_eq!(words, [transcript], "{key:?} copies the complete previous transcript exactly once");
                    }
                    assert_eq!(app.last_dictation, transcript);
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.playing.is_empty() && app.clip_loads.is_none());
                    app.dictation.cancel_transcription();
                    if let Some(release) = release { release.send(()).unwrap(); }
                }
            }
        }
    }
}
