//! Settings tab: global hotkeys, speech to text, startup and the
//! notification area.

use eframe::egui::{self, RichText};

use super::{clock_time, App, Binding, Command, AMBER, CYAN, GREEN, MUTED, RED, VIOLET};
use crate::dictation::SpeechModel;
use crate::widgets;

#[cfg(windows)]
fn startup_label() -> &'static str {
    "Start with Windows"
}

#[cfg(not(windows))]
fn startup_label() -> &'static str {
    "Start on login"
}

#[cfg(windows)]
fn startup_hint() -> &'static str {
    "Starts hidden in the notification area"
}

#[cfg(not(windows))]
fn startup_hint() -> &'static str {
    "Starts minimized"
}

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
                let h = &self.settings.hotkeys;
                let rows = [
                    ("Mute mic", Binding::Mute, h.mute.clone()),
                    ("Bypass processing", Binding::Bypass, h.bypass.clone()),
                    ("Stop all clips", Binding::StopClips, h.stop_clips.clone()),
                    ("Speech to text", Binding::Dictate, h.dictate.clone()),
                ];
                for (label, binding, current) in rows {
                    self.draw_shortcut_row(ui, binding, label, current.as_deref(), hotkeys_ok);
                    ui.add_space(4.0);
                }
                ui.add_space(4.0);
                ui.weak("Soundboard pads get their own hotkeys: right-click a pad.");
                if !self.hotkey_status.0.is_empty() {
                    ui.label(RichText::new(&self.hotkey_status.0).color(widgets::text_color(ui, self.hotkey_status.1)));
                }
            },
        );
        ui.add_space(8.0);

        widgets::card(
            ui,
            "GENERAL",
            CYAN,
            |_| {},
            |ui| {
                let startup = ui
                    .checkbox(&mut self.settings.start_with_windows, startup_label())
                    .on_hover_text(startup_hint());
                widgets::scroll_on_focus(&startup);
                if startup.changed() {
                    self.toggle_startup(self.settings.start_with_windows);
                }
                let auto_start = ui.checkbox(&mut self.settings.auto_start, "Start processing automatically");
                widgets::scroll_on_focus(&auto_start);
                if auto_start.changed() {
                    self.touch();
                }
                #[cfg(windows)]
                {
                    let tray_ok = self.tray.is_some();
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
                }
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

    /// The daily check, where the in-app updater exists. The log folder stays
    /// available either way; checking now and installing happen in the header.
    fn draw_updates(&mut self, ui: &mut egui::Ui) {
        if crate::update::updates_supported() {
            let updates = ui
                .checkbox(&mut self.settings.check_for_updates, "Check for updates daily")
                .on_hover_text("Asks GitHub for the latest OpenMic release; nothing else is sent");
            widgets::scroll_on_focus(&updates);
            if updates.changed() {
                self.touch();
            }
            ui.weak("Click the version number at the top to check now.");
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            let logs = ui
                .button("Open log folder")
                .on_hover_text("openmic.log records device changes, errors and updates; attach it to a bug report");
            widgets::scroll_on_focus(&logs);
            if logs.clicked() {
                super::open_url(ui.ctx(), &crate::logfile::dir().display().to_string());
            }
        });
    }

    /// Stable shortcut actions wrap within the page, including during capture.
    fn draw_shortcut_row(&mut self, ui: &mut egui::Ui, binding: Binding, label: &str, current: Option<&str>, available: bool) {
        let id = egui::Id::new(("shortcut-row", &binding));
        let action = if binding == Binding::Dictate { "Speech to text" } else { label };
        let waiting = self.capturing.as_ref() == Some(&binding);
        let cancelled = !waiting && ui.data(|data| data.get_temp::<bool>(id.with("capture"))) == Some(true)
            && ui.input(|input| input.focused && input.key_pressed(egui::Key::Escape));
        let mut controls = Vec::new();
        ui.scope_builder(egui::UiBuilder::new().id(id), |ui| ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(label).weak());
            let value = if waiting { "Press keys…" } else { current.unwrap_or("Not set") };
            let text = if waiting { RichText::new(value).color(widgets::text_color(ui, CYAN)) }
                else { RichText::new(value).monospace() };
            ui.add_sized([ui.max_rect().width().min(150.0), ui.spacing().interact_size.y], egui::Label::new(text).truncate())
                .on_hover_text(value);
            let edit = ui.add_enabled(waiting || available, egui::Button::new(if waiting { "Cancel" } else { "Set…" }))
                .on_disabled_hover_text("Global shortcuts are unavailable. Restart OpenMic to retry.");
            edit.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, edit.enabled(),
                format!("{} shortcut: {action}", if waiting { "Cancel" } else { "Set" })));
            ui.data_mut(|data| data.insert_temp(id.with("edit"), edit.id));
            if cancelled && edit.enabled() { edit.request_focus(); }
            if edit.clicked() {
                widgets::finish_keyboard_activation(ui.ctx());
                if waiting { self.cancel_capture(); } else { self.start_capture(binding.clone()); }
            }
            if !waiting && !edit.clicked() { super::shortcut_capture_opener(&edit); }
            if current.is_some() {
                let clear = ui.button("Clear");
                clear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("Clear shortcut: {action}")));
                if clear.clicked() {
                    self.assign_hotkey(&binding, None);
                    widgets::finish_keyboard_activation(ui.ctx());
                    if available { edit.request_focus(); }
                    else {
                        clear.surrender_focus();
                        ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::Next));
                    }
                }
                controls.push(clear);
            }
            controls.push(edit);
        }));
        if cancelled && !available {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::Next));
        }
        ui.data_mut(|data| data.insert_temp(id.with("capture"), self.capturing.as_ref() == Some(&binding)));
        for control in &controls { widgets::scroll_on_focus(control); }
    }

    /// Speech to text: the model, how the shortcut works, and the result.
    pub(super) fn draw_dictation_page(&mut self, ui: &mut egui::Ui) {
        let current = self.settings.hotkeys.dictate.clone();
        self.draw_shortcut_row(ui, Binding::Dictate, "Shortcut", current.as_deref(), self.hotkeys.is_some());
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
        let busy = self.dictation.busy();
        let cancelling = self.dictation.transcription_cancelling();
        let focused = ui.memory(|memory| memory.focused());
        let next_focus = ui.data(|data| data.get_temp::<(Option<egui::Id>, Option<egui::Id>, Option<egui::Id>, bool)>(
            egui::Id::new("dictation-controls")))
            .and_then(|(discard, cancel, progress, was_cancelling)| {
                let owner = focused?;
                if (self.dictating.is_none() && discard == Some(owner))
                    || (!busy && (cancel == Some(owner) || progress == Some(owner)))
                {
                    Some((owner, false))
                } else if cancelling && !was_cancelling && cancel == Some(owner) {
                    Some((owner, true))
                } else { None }
            }).filter(|_| ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released()));
        let mut model_control = None;
        let mut download_control = None;
        let mut copy_control = None;
        let mut discard_control = None;
        let mut cancel_control = None;
        let mut progress_control = None;
        widgets::card(
            ui,
            "SPEECH TO TEXT",
            AMBER,
            |ui| widgets::badge(ui, "Runs on this PC", GREEN),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(RichText::new("Model").weak());
                    let before = self.settings.speech_model;
                    let mut selected = false;
                    let choice = egui::ComboBox::from_id_salt("speech_model")
                        .width(ui.available_rect_before_wrap().width().min(240.0))
                        .selected_text(before.label())
                        .truncate()
                        .show_ui(ui, |ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            for model in SpeechModel::ALL {
                                let text = if model.is_downloaded() {
                                    format!("{} · downloaded", model.label())
                                } else {
                                    model.label().to_owned()
                                };
                                let choice = ui.selectable_value(&mut self.settings.speech_model, model, text.as_str()).on_hover_text(text);
                                widgets::scroll_on_focus(&choice);
                                if choice.clicked() {
                                    selected = true;
                                    ui.close();
                                }
                            }
                        }).response.on_hover_text(format!("{}\nApplies to the next recording", before.label())).labelled_by(label.id);
                    if selected { choice.request_focus(); }
                    widgets::scroll_on_focus(&choice);
                    model_control = Some(choice.id);
                    if self.settings.speech_model != before {
                        self.touch();
                    }
                    download_control = self.draw_model_download(ui, choice.id);
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
                ui.weak(if cfg!(windows) {
                    "Your words are typed into the app you're using. Set your Shortcut above."
                } else {
                    "Your words appear below to copy into the app you're using. Set your Shortcut above."
                });

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
                    discard_control = Some(discard.id);
                    if discard.clicked() {
                        if ui.input(|input| input.pointer.any_down() || input.pointer.any_released()) { discard_control = None; }
                        widgets::finish_keyboard_activation(ui.ctx());
                        self.discard_dictation();
                        ui.ctx().request_repaint();
                    }
                }
                if busy {
                    // Another take can appear or disappear while this job runs.
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("dictation-transcription")), |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            let progress = ui.add(egui::Label::new(RichText::new(
                                if cancelling { "Cancelling transcription…" } else { "Transcribing…" }).color(widgets::text_color(ui, CYAN)))
                                .sense(egui::Sense::focusable_noninteractive()).selectable(false));
                            widgets::scroll_on_focus(&progress);
                            progress_control = Some(progress.id);
                        });
                        let cancel = ui.add_enabled(!cancelling, egui::Button::new("Cancel transcription"))
                            .on_hover_text("Discard the current transcription and queued takes before they type");
                        widgets::scroll_on_focus(&cancel);
                        cancel_control = Some(cancel.id);
                        if cancel.clicked() {
                            if ui.input(|input| input.pointer.any_down() || input.pointer.any_released()) { cancel_control = None; }
                            widgets::finish_keyboard_activation(ui.ctx());
                            self.dictation.cancel_transcription();
                            self.dictation_status = (String::new(), MUTED);
                            ui.ctx().request_repaint();
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
                            copy_control = Some(copy.id);
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
        // Record what was rendered, including a control clicked this frame.
        ui.data_mut(|data| data.insert_temp(egui::Id::new("dictation-controls"),
            (discard_control, cancel_control, progress_control, cancelling)));
        let layout = (ui.min_size().round(), ui.clip_rect().size().round());
        let reflowed = ui.data_mut(|data| {
            let id = egui::Id::new("dictation-layout");
            let previous = data.get_temp::<(egui::Vec2, egui::Vec2)>(id);
            data.insert_temp(id, layout);
            previous.is_some_and(|previous| previous != layout)
        });
        if reflowed
            && ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released())
            && let Some(focused) = ui.memory(|memory| memory.focused())
            && [model_control, download_control, copy_control, discard_control, cancel_control, progress_control].contains(&Some(focused))
            && let Some(response) = ui.ctx().read_response(focused)
        {
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if let Some((owner, progress)) = next_focus
            && ui.memory(|memory| memory.focused().is_none_or(|focused| focused == owner))
            && let Some(target) = if progress { progress_control } else { copy_control.or(model_control) }
            && let Some(response) = ui.ctx().read_response(target)
            && response.enabled() && response.sense.is_focusable()
        {
            widgets::finish_keyboard_activation(ui.ctx());
            response.request_focus();
            response.scroll_to_me(Some(egui::Align::Center));
        }
    }

    /// Download progress, or the button to download or remove the model.
    fn draw_model_download(&mut self, ui: &mut egui::Ui, selector: egui::Id) -> Option<egui::Id> {
        let model = self.settings.speech_model;
        let progress = self.dictation.download_progress();
        let active = progress.is_some();
        let cancelling = self.dictation.download_cancelling();
        let keyboard = ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released());
        let previous = ui.data(|data| data.get_temp::<(bool, bool, bool, Option<egui::Id>)>(egui::Id::new("model-download-controls")));
        // Disabled widgets surrender focus as they are drawn.
        let owner = previous.and_then(|(_, _, _, control)| control)
            .filter(|owner| keyboard && ui.memory(|memory| memory.has_focus(*owner)));
        let mut next_focus = previous.filter(|(was_active, was_cancelling, started, _)|
            *started || *was_active != active || (cancelling && !was_cancelling))
            .and(owner).map(|owner| (owner, !active || cancelling));
        let mut started = false;
        let mut control;
        if let Some((downloading, done, total)) = progress {
            let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(150.0)
                    .text(format!("{} MB", done / 1_000_000)),
            )
            .on_hover_text(format!("Downloading {}", downloading.label()));
            let cancel = ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Cancelling…" } else { "Cancel" }));
            widgets::scroll_on_focus(&cancel);
            control = Some(cancel.id);
            if cancel.clicked() {
                if cancel.has_focus() && keyboard { next_focus = Some((cancel.id, true)); }
                cancel.surrender_focus();
                if !keyboard { control = None; }
                widgets::finish_keyboard_activation(ui.ctx());
                self.dictation.cancel_download();
                self.dictation_status = ("Cancelling model download…".into(), MUTED);
                ui.ctx().request_repaint();
            }
        } else if !model.is_downloaded() {
            let download = ui.button("Download").on_hover_text("Downloads once from Hugging Face");
            widgets::scroll_on_focus(&download);
            control = Some(download.id);
            if download.clicked() {
                started = download.has_focus() && keyboard;
                if !keyboard { download.surrender_focus(); control = None; }
                widgets::finish_keyboard_activation(ui.ctx());
                self.dictation.start_download(model);
                self.dictation_status = (format!("Downloading {}…", model.label()), CYAN);
                ui.ctx().request_repaint();
            }
        } else {
            let in_use = self.dictation.busy()
                || self.dictating.as_ref().is_some_and(|recording| recording.model == model);
            let remove = ui.add_enabled(!in_use, egui::Button::new("Remove"))
                .on_hover_text("Delete the downloaded model")
                .on_disabled_hover_text("Finish dictation before removing a speech model");
            widgets::scroll_on_focus(&remove);
            control = Some(remove.id);
            if in_use && owner == Some(remove.id) { next_focus = Some((remove.id, true)); }
            if remove.clicked() {
                if remove.has_focus() && keyboard { next_focus = Some((remove.id, true)); }
                remove.surrender_focus();
                if !keyboard { control = None; }
                widgets::finish_keyboard_activation(ui.ctx());
                self.dictation_status = match model.remove() {
                    Ok(()) => ("Model removed".into(), MUTED),
                    Err(error) => {
                        crate::logfile::warn(format_args!("speech model removal failed: {error:#}"));
                        (format!("Could not remove model: {}", error.root_cause()), RED)
                    }
                };
                ui.ctx().request_repaint();
            }
        }
        ui.data_mut(|data| data.insert_temp(egui::Id::new("model-download-controls"), (active, cancelling, started, control)));
        if let Some((owner, return_to_model)) = next_focus
            && ui.memory(|memory| memory.focused().is_none_or(|focused| focused == owner))
            && let Some(target) = if return_to_model { Some(selector) } else { control }
            && let Some(response) = ui.ctx().read_response(target)
            && response.enabled() && response.sense.is_focusable()
        {
            widgets::finish_keyboard_activation(ui.ctx());
            response.request_focus();
            response.scroll_to_me(Some(egui::Align::Center));
        }
        control
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
    fn shortcut_controls_fit_in_the_zoomed_settings_and_dictation_pages() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for page in [Page::Settings, Page::Dictation] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for zoom in [1.0, 1.25, 1.5] {
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+Shift+ArrowRight".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Alt+Shift+BracketRight".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+Shift+PageDown".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+Shift+F24".into());
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let names = if page == Page::Settings {
                            vec!["Ctrl+Alt+Shift+ArrowRight", "Ctrl+Alt+Shift+BracketRight", "Ctrl+Alt+Shift+PageDown", "Ctrl+Alt+Shift+F24"]
                        } else { vec!["Ctrl+Alt+Shift+F24"] };
                        for name in names {
                            assert!(output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().any(|(_, node)|
                                node.role() == Role::Label && node.value() == Some(name)), "the complete combination stays accessible");
                            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                                if text.galley.text() == name && text.visual_bounding_rect().width() <= 150.0)),
                                "a long combination stays within its value column");
                        }
                        let controls: Vec<_> = output.platform_output.accesskit_update.unwrap().nodes.into_iter()
                            .filter(|(_, node)| node.role() == Role::Button && node.label().is_some_and(|label|
                                label == "Set…" || label == "Clear" || label.starts_with("Set shortcut:") || label.starts_with("Clear shortcut:")))
                            .collect();
                        assert_eq!(controls.len(), if page == Page::Settings { 8 } else { 2 });
                        for (target, node) in controls {
                            let bounds = node.bounds().unwrap();
                            assert!(bounds.x0 >= 176.0 && bounds.x1 <= f64::from(size.x),
                                "{} fits at {zoom}x on {page:?}, passes={passes}: {bounds:?}", node.label().unwrap());
                            if node.is_disabled() { continue; }
                            draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                            })]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                            let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                            assert!(rect.top() >= 44.0 && rect.bottom() <= size.y
                                && output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                    && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                                "focused shortcut action is fully visible: {rect:?}");
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && app.capturing.is_none() && app.engine.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn clearing_a_shortcut_by_keyboard_keeps_the_other_rows_and_a_usable_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (page, binding, label, slot) in [
            (Page::Settings, Binding::Mute, "Mute mic", "mute"),
            (Page::Settings, Binding::Bypass, "Bypass processing", "bypass"),
            (Page::Settings, Binding::StopClips, "Stop all clips", "stop_clips"),
            (Page::Settings, Binding::Dictate, "Speech to text", "dictate"),
            (Page::Dictation, Binding::Dictate, "Speech to text", "dictate"),
        ] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    let mut app = App::stopped(Default::default());
                    app.page = page;
                    app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                    app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                    app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                    app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                    app.settings.strength = 0.71347;
                    let mut expected = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    let mut size = egui::vec2(700.0, 420.0);
                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(format!("Clear shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                    draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    for zoom in [1.5, 1.25, 1.0] {
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        size = egui::vec2(700.0, 420.0) / zoom;
                        app.hotkey_status = ("Another shortcut is unavailable. Choose another key combination.".into(), AMBER);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        assert_eq!(tree.focus, target, "zoom and shortcut warnings preserve {label}'s action");
                        let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                        assert!(bounds.x0 >= 176.0 && bounds.x1 <= f64::from(size.x) && bounds.y0 >= 44.0 && bounds.y1 <= f64::from(size.y));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    }
                    for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                        draw_size(&mut app, &ctx, size, vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
                        }]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    draw_size(&mut app, &ctx, size, vec![egui::Event::Key {
                        key: if passes == 1 { egui::Key::Enter } else { egui::Key::Space },
                        physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }]);
                    for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    let focused = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
                    assert!(!focused.is_disabled() && matches!(focused.role(), Role::Button | Role::CheckBox | Role::ComboBox | Role::RadioButton),
                        "clearing an unavailable shortcut leaves a usable control focused: {focused:?}");
                    assert_ne!(tree.focus, target, "the removed Clear control does not retain focus");
                    expected["hotkeys"][slot] = serde_json::Value::Null;
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected, "only the chosen binding changes");
                    assert!(app.binding_slot(&binding).unwrap().is_none());
                    assert!(app.capturing.is_none() && app.dirty_since.is_some() && app.engine.is_none());
                }
            }
        }
    }

    #[test]
    fn clearing_shortcuts_does_not_reuse_a_held_key_on_the_next_control() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let rows = [("Mute mic", Binding::Mute, "mute"), ("Bypass processing", Binding::Bypass, "bypass"),
            ("Stop all clips", Binding::StopClips, "stop_clips"), ("Speech to text", Binding::Dictate, "dictate")];
        let key = |key, pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        for (page, label, binding, slot) in [(Page::Settings, "Mute mic", Binding::Mute, "mute"),
            (Page::Settings, "Speech to text", Binding::Dictate, "dictate"),
            (Page::Dictation, "Speech to text", Binding::Dictate, "dictate")]
        {
            for available in [false, true] {
                for dark in [true, false] {
                    for passes in [1, 2] {
                        for held in [egui::Key::Enter, egui::Key::Space] {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                            app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                            app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                            app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                            app.settings.strength = 0.71347;
                            app.settings.highpass_hz = 132.456;
                            let mut expected = serde_json::to_value(&app.settings).unwrap();
                            expected["hotkeys"][slot] = serde_json::Value::Null;
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let continued = std::cell::Cell::new(0);
                            let whole_page = !available && page == Page::Settings && binding == Binding::Mute;
                            // Exercise available rows and the final row without OS shortcut,
                            // startup-registration or speech-download side effects.
                            let draw = |app: &mut App, events| if !whole_page {
                                ctx.run_ui(egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(280.0, 220.0))),
                                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                                }, |ui| {
                                    widgets::consume_held_activation(ui.ctx());
                                    app.capture_hotkey(ui.ctx());
                                    egui::ScrollArea::vertical().show(ui, |ui| {
                                        for (label, binding, _) in &rows {
                                            if page == Page::Dictation && *binding != Binding::Dictate { continue; }
                                            let current = app.binding_slot(binding).unwrap().clone();
                                            app.draw_shortcut_row(ui, binding.clone(), label, current.as_deref(), available);
                                            ui.add_space(4.0);
                                        }
                                        if ui.button("Continue").clicked() { continued.set(continued.get() + 1); }
                                    });
                                })
                            } else { draw_size(app, &ctx, egui::vec2(700.0, 420.0) / 1.5, events) };
                            for _ in 0..3 { draw(&mut app, Vec::new()); }
                            let output = draw(&mut app, Vec::new());
                            let clear = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.label() == Some(format!("Clear shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                            draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: clear, data: None,
                            })]);
                            for _ in 0..8 { draw(&mut app, Vec::new()); }
                            draw(&mut app, vec![key(held, true)]);
                            for _ in 0..8 { draw(&mut app, Vec::new()); }
                            let target = draw(&mut app, Vec::new()).platform_output.accesskit_update.unwrap().focus;
                            assert_ne!(target, clear);
                            for _ in 0..3 {
                                let mut input = vec![key(held, true)];
                                if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                let output = draw(&mut app, input);
                                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected,
                                    "held {held:?} must only clear {slot}: page={page:?}, available={available}, passes={passes}");
                                assert!(app.capturing.is_none(), "clearing does not reuse its held key to begin shortcut capture");
                                assert_eq!(continued.get(), 0, "the next action stays idle while the key is held");
                            }
                            assert!(app.engine.is_none() && app.dirty_since.is_some());
                            if available {
                                draw(&mut app, vec![key(held, false), key(held, true), key(held, false)]);
                                assert_eq!(app.capturing, Some(binding.clone()), "a new press in the release batch opens capture");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            } else if !whole_page {
                                draw(&mut app, vec![key(held, false), key(held, true), key(held, false)]);
                                assert_eq!(continued.get(), 1, "a fresh activation after release reaches the next action");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            } else { draw(&mut app, vec![key(held, false)]); }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn captured_enter_and_space_do_not_operate_the_focused_control() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["other.wav", "target.wav"].into_iter().map(|name| {
            let path = dir.path().join(name);
            Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            path
        }).collect();
        // UI frames never call sync_hotkeys, so this manager registers no shortcuts.
        let (commands, _receiver) = crossbeam_channel::unbounded();
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let mut cases = ["Play target", "Remove target", "Rename target", "Remove target from the strip"].into_iter()
            .map(|label| (Page::Soundboard, Binding::Pad(paths[1].clone()), None, label)).collect::<Vec<_>>();
        cases.extend([
            (Page::Settings, Binding::Mute, Some("mute"), "Cancel shortcut: Mute mic"),
            (Page::Settings, Binding::Bypass, Some("bypass"), "Cancel shortcut: Bypass processing"),
            (Page::Settings, Binding::StopClips, Some("stop_clips"), "Cancel shortcut: Stop all clips"),
            (Page::Settings, Binding::Dictate, Some("dictate"), "Cancel shortcut: Speech to text"),
            (Page::Dictation, Binding::Dictate, Some("dictate"), "Cancel shortcut: Speech to text"),
        ]);
        let key = |key, pressed, repeat| egui::Event::Key {
            key, physical_key: Some(key), pressed, repeat, modifiers: egui::Modifiers::CTRL,
        };
        for (page, binding, slot, label) in cases {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for activation in [egui::Key::Enter, egui::Key::Space] {
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.hotkeys = manager.take();
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Enter".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        app.settings.sound_volume = 0.61347;
                        app.settings.sounds = paths.iter().cloned().map(crate::config::Pad::new).collect();
                        app.settings.sounds[1].name = Some("target".into());
                        app.settings.sounds[1].starred = true;
                        app.settings.sounds[1].volume = 0.43561;
                        app.settings.sounds[1].start = 0.12345;
                        app.settings.sounds[1].end = Some(0.812345);
                        app.settings.sounds[1].hotkey = Some("Ctrl+Alt+P".into());
                        let combo = if activation == egui::Key::Enter { "Ctrl+Enter" } else { "Ctrl+Space" };
                        let mut expected = serde_json::to_value(&app.settings).unwrap();
                        if activation == egui::Key::Enter { expected["hotkeys"]["bypass"] = serde_json::Value::Null; }
                        if let Some(slot) = slot { expected["hotkeys"][slot] = serde_json::json!(combo); }
                        else { expected["sounds"][1]["hotkey"] = serde_json::json!(combo); }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        if page == Page::Soundboard {
                            app.engine = Some(crate::engine::Engine::for_test());
                            for path in &paths { app.cache_clip(path.clone(), super::super::CachedClip::read(path, &std::sync::atomic::AtomicBool::new(false)).unwrap()); }
                            app.play_clip(0, &ctx);
                            app.toggle_clip(0, &ctx);
                        }
                        let playing = app.playing.clone();
                        let progress = app.pad_progress.clone();
                        let status = app.sound_status.clone();
                        app.start_capture(binding.clone());
                        let size = egui::vec2(700.0, 420.0) / 1.5;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let owner = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: owner, data: None,
                        })]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let mut input = vec![key(activation, true, false)];
                        if activation == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                        draw_size(&mut app, &ctx, size, input);
                        for _ in 0..4 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        for repeat in 0..4 {
                            if repeat > 0 {
                                let mut input = vec![key(activation, true, true)];
                                if activation == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                draw_size(&mut app, &ctx, size, input);
                            }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            assert!(app.capturing.is_none(), "captured {combo} must finish capture without reopening Set: {label}, passes={passes}, repeat={repeat}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected,
                                "captured {combo} must only reassign its binding: {label}, passes={passes}, repeat={repeat}");
                            assert_eq!(app.playing, playing, "a captured shortcut must not play or stop a clip: {label}");
                            assert_eq!(app.pad_progress, progress);
                            assert_eq!(app.sound_status, status);
                            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner);
                            assert!(app.renaming_pad.is_none() && app.clip_loads.is_none() && app.dirty_since.is_some());
                            assert_eq!(app.page, page);
                        }
                        draw_size(&mut app, &ctx, size, vec![key(activation, false, false)]);
                        assert_eq!(app.hotkey_status.0, format!("{combo} set"));
                        assert!(!app.hotkey_bindings().is_empty(), "the saved shortcuts are available again");
                        assert!(paths.iter().all(|path| path.is_file()));
                        manager = app.hotkeys.take();
                    }
                }
            }
        }
    }

    #[test]
    fn shortcut_capture_opens_and_finishes_once_per_keyboard_activation() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let (commands, _receiver) = crossbeam_channel::unbounded();
        // Full UI frames do not register this manager's shortcuts with Windows.
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let cases = [(Page::Settings, Binding::Mute, "mute", "Mute mic"),
            (Page::Settings, Binding::Bypass, "bypass", "Bypass processing"),
            (Page::Settings, Binding::StopClips, "stop_clips", "Stop all clips"),
            (Page::Settings, Binding::Dictate, "dictate", "Speech to text"),
            (Page::Dictation, Binding::Dictate, "dictate", "Speech to text")];
        let key = |key, pressed, repeat, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed, repeat, modifiers };
        for (page, binding, slot, label) in cases {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for opener in [egui::Key::Enter, egui::Key::Space] {
                        for finish in ["save-enter", "save-space", "cancel-enter", "cancel-space", "escape"] {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            app.hotkeys = manager.take();
                            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                            app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                            app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                            app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                            app.settings.strength = 0.71347;
                            app.settings.highpass_hz = 132.456;
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let size = egui::vec2(700.0, 420.0) / 1.5;
                            for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            let owner = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.label() == Some(format!("Set shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                            draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: owner, data: None,
                            })]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let mut input = vec![key(opener, true, false, egui::Modifiers::NONE)];
                            if opener == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            draw_size(&mut app, &ctx, size, input);
                            for repeat in 0..4 {
                                if repeat > 0 {
                                    let mut input = vec![key(opener, true, true, egui::Modifiers::NONE)];
                                    if opener == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                    draw_size(&mut app, &ctx, size, input);
                                }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                assert_eq!(app.capturing.as_ref(), Some(&binding), "held Set must not activate Cancel: {label}, {opener:?}, {finish}, passes={passes}, repeat={repeat}");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none() && app.hotkey_bindings().is_empty());
                                let tree = output.platform_output.accesskit_update.unwrap();
                                assert_eq!(tree.focus, owner);
                                assert_eq!(tree.nodes.iter().find(|(id, _)| *id == owner).unwrap().1.label(), Some(format!("Cancel shortcut: {label}").as_str()));
                            }
                            let completion = if finish == "escape" { egui::Key::Escape }
                                else if finish.ends_with("space") { egui::Key::Space } else { egui::Key::Enter };
                            let save = finish.starts_with("save");
                            let modifiers = if save { egui::Modifiers::CTRL } else { egui::Modifiers::NONE };
                            let mut input = vec![key(opener, false, false, egui::Modifiers::NONE), key(completion, true, false, modifiers)];
                            if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            if completion == egui::Key::Escape { input.push(key(completion, false, false, modifiers)); }
                            draw_size(&mut app, &ctx, size, input);
                            let mut expected = original;
                            let combo = if completion == egui::Key::Space { "Ctrl+Space" } else { "Ctrl+Enter" };
                            if save { expected["hotkeys"][slot] = serde_json::json!(combo); }
                            for repeat in 0..4 {
                                if repeat > 0 && completion != egui::Key::Escape {
                                    let mut input = vec![key(completion, true, true, modifiers)];
                                    if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                    draw_size(&mut app, &ctx, size, input);
                                }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                assert!(app.capturing.is_none(), "held completion must not reopen Set: {label}, {finish}, passes={passes}, repeat={repeat}");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                                assert_eq!(app.dirty_since.is_some(), save);
                                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner, "completion preserves focus: {label}, {finish}, passes={passes}, repeat={repeat}");
                                assert_eq!(app.page, page);
                                assert!(app.engine.is_none() && app.dictating.is_none() && !app.dictation.busy());
                            }
                            assert_eq!(app.hotkey_status.0, if save { format!("{combo} set") } else { "Shortcut unchanged".into() });
                            assert!(!app.hotkey_bindings().is_empty());
                            // A new activation after releasing the completion key opens capture normally.
                            draw_size(&mut app, &ctx, size, vec![key(completion, false, false, modifiers), key(egui::Key::Enter, true, false, egui::Modifiers::NONE)]);
                            assert_eq!(app.capturing.as_ref(), Some(&binding));
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            manager = app.hotkeys.take();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn opening_capture_preserves_a_combination_in_the_same_input_frame() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let (commands, _receiver) = crossbeam_channel::unbounded();
        // Only App::ui runs; this manager never registers shortcuts with Windows.
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let cases = [(Page::Settings, Binding::Mute, "mute", "Mute mic"),
            (Page::Settings, Binding::Bypass, "bypass", "Bypass processing"),
            (Page::Settings, Binding::StopClips, "stop_clips", "Stop all clips"),
            (Page::Settings, Binding::Dictate, "dictate", "Speech to text"),
            (Page::Dictation, Binding::Dictate, "dictate", "Speech to text")];
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE };
        let key = |key, pressed, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers };
        let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action, target_tree: TreeId::ROOT, target_node, data: None,
        });
        for (page, binding, slot, label) in cases {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for opener in ["enter", "space", "assistive", "pointer"] {
                        for completion in [egui::Key::Q, egui::Key::Enter, egui::Key::Space, egui::Key::Escape] {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            app.hotkeys = manager.take();
                            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                            app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                            app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                            app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                            app.settings.strength = 0.71347;
                            app.settings.highpass_hz = 132.456;
                            let mut expected = serde_json::to_value(&app.settings).unwrap();
                            let save = completion != egui::Key::Escape;
                            let modifiers = if save { ctrl } else { egui::Modifiers::NONE };
                            let combo = save.then(|| crate::hotkeys::combo(completion, modifiers).unwrap());
                            if let Some(combo) = &combo { expected["hotkeys"][slot] = serde_json::json!(combo); }
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let size = egui::vec2(700.0, 420.0) / 1.5;
                            for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            let owner = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.label() == Some(format!("Set shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                            draw_size(&mut app, &ctx, size, vec![action(Action::Focus, owner)]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let edit = ctx.data(|data| data.get_temp::<egui::Id>(egui::Id::new(("shortcut-row", &binding)).with("edit"))).unwrap();
                            let pos = ctx.read_response(edit).unwrap().rect.center();
                            let pointer = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                            let mut input = match opener {
                                "assistive" => vec![action(Action::Click, owner)],
                                "pointer" => {
                                    draw_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), pointer(true)]);
                                    vec![pointer(false)]
                                }
                                _ => {
                                    let opening_key = if opener == "space" { egui::Key::Space } else { egui::Key::Enter };
                                    let mut input = vec![key(opening_key, true, egui::Modifiers::NONE)];
                                    if opening_key == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                    input.push(key(opening_key, false, egui::Modifiers::NONE));
                                    input
                                }
                            };
                            input.push(key(completion, true, modifiers));
                            if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            draw_size(&mut app, &ctx, size, input);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            assert!(app.capturing.is_none(), "the same-frame combination completes capture: {label}, {opener}, {completion:?}, passes={passes}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert_eq!(app.dirty_since.is_some(), save);
                            assert_eq!(app.hotkey_status.0, combo.map_or_else(|| "Shortcut unchanged".into(), |combo| format!("{combo} set")));
                            for _ in 0..3 {
                                let mut repeat = vec![key(completion, true, modifiers)];
                                if completion == egui::Key::Space { repeat.push(egui::Event::Text(" ".into())); }
                                draw_size(&mut app, &ctx, size, repeat);
                            }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            assert!(app.capturing.is_none());
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner);
                            assert_eq!(app.page, page);
                            assert!(app.engine.is_none() && app.dictating.is_none() && !app.dictation.busy());
                            manager = app.hotkeys.take();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn capture_processes_rapid_corrections_and_cancellation_in_order() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..12).map(|index| {
            let path = dir.path().join(format!("capture-correction-{index}.wav"));
            Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            path
        }).collect();
        let (commands, _receiver) = crossbeam_channel::unbounded();
        // Only App::ui runs; this manager never registers shortcuts with Windows.
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let cases = [(Page::Settings, Binding::Mute, "mute", "Set shortcut: Mute mic"),
            (Page::Settings, Binding::Bypass, "bypass", "Set shortcut: Bypass processing"),
            (Page::Settings, Binding::StopClips, "stop_clips", "Set shortcut: Stop all clips"),
            (Page::Settings, Binding::Dictate, "dictate", "Set shortcut: Speech to text"),
            (Page::Dictation, Binding::Dictate, "dictate", "Set shortcut: Speech to text"),
            (Page::Soundboard, Binding::Pad(paths[11].clone()), "pad", "search"),
            (Page::Soundboard, Binding::Pad(paths[11].clone()), "pad", "volume"),
            (Page::Soundboard, Binding::Pad(paths[11].clone()), "pad", "Play target")];
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE };
        let key = |key, physical_key, pressed, modifiers| egui::Event::Key { key, physical_key, pressed, repeat: false, modifiers };
        let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action, target_tree: TreeId::ROOT, target_node, data: None,
        });
        for (page, binding, slot, label) in cases {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for mode in ["save-letter", "save-enter", "save-space", "cancel", "physical-held"] {
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.hotkeys = manager.take();
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        app.settings.sound_volume = 0.61347;
                        app.settings.sounds = paths.iter().cloned().map(crate::config::Pad::new).collect();
                        app.settings.sounds[11].name = Some("target".into());
                        app.settings.sounds[11].starred = true;
                        app.settings.sounds[11].hotkey = Some("Ctrl+Alt+P".into());
                        app.settings.sounds[11].volume = 0.43561;
                        app.settings.sounds[11].start = 0.12345;
                        app.settings.sounds[11].end = Some(0.812345);
                        app.pad_filter = "tar".into();
                        let mut expected = serde_json::to_value(&app.settings).unwrap();
                        let save = mode != "cancel";
                        let physical_collision = mode == "physical-held";
                        let logical = match mode { "save-enter" => egui::Key::Enter, "save-space" => egui::Key::Space,
                            "cancel" => egui::Key::Escape, "physical-held" => egui::Key::T, _ => egui::Key::Q };
                        let physical = if physical_collision { egui::Key::Q } else { logical };
                        let modifiers = if physical_collision { egui::Modifiers { alt: true, ..ctrl } }
                            else if save { ctrl } else { egui::Modifiers::NONE };
                        let combo = save.then(|| crate::hotkeys::combo(physical, modifiers).unwrap());
                        if let Some(combo) = &combo {
                            if page == Page::Soundboard { expected["sounds"][11]["hotkey"] = serde_json::json!(combo); }
                            else { expected["hotkeys"][slot] = serde_json::json!(combo); }
                        }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        if page == Page::Soundboard {
                            app.engine = Some(crate::engine::Engine::for_test());
                            for path in [&paths[0], &paths[11]] { app.cache_clip(path.clone(), super::super::CachedClip::read(path, &std::sync::atomic::AtomicBool::new(false)).unwrap()); }
                            app.play_clip(0, &ctx);
                            app.toggle_clip(0, &ctx);
                        }
                        let playing = app.playing.clone();
                        let progress = app.pad_progress.clone();
                        let status = app.sound_status.clone();
                        let size = egui::vec2(700.0, 420.0) / 1.5;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        let owner = *tree.nodes.iter().find(|(_, node)| match label {
                            "search" => node.placeholder() == Some("Search clips"),
                            "volume" => node.role() == Role::Slider && node.numeric_value() == Some(f64::from(0.61347f32)),
                            _ => node.label() == Some(label),
                        }).map(|(id, _)| id).unwrap();
                        let set = if page == Page::Soundboard {
                            let pad = *tree.nodes.iter().find(|(_, node)| node.label() == Some("Play target")).map(|(id, _)| id).unwrap();
                            draw_size(&mut app, &ctx, size, vec![action(Action::Focus, pad)]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            draw_size(&mut app, &ctx, size, vec![key(egui::Key::F10, Some(egui::Key::F10), true, egui::Modifiers::SHIFT), key(egui::Key::F10, Some(egui::Key::F10), false, egui::Modifiers::SHIFT)]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)| node.label() == Some("Set…")).map(|(id, _)| id).unwrap()
                        } else { owner };
                        draw_size(&mut app, &ctx, size, vec![action(Action::Focus, set)]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        draw_size(&mut app, &ctx, size, vec![action(Action::Click, set)]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        draw_size(&mut app, &ctx, size, vec![action(Action::Focus, owner)]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        assert_eq!(app.capturing.as_ref(), Some(&binding));
                        let cursor = (label == "search").then(|| egui::TextEdit::load_state(&ctx, egui::Id::new("soundboard-search")).unwrap().cursor.char_range());
                        let mut input = vec![key(egui::Key::T, Some(egui::Key::T), true, egui::Modifiers::NONE), egui::Event::Text("t".into())];
                        if !physical_collision { input.push(key(egui::Key::T, Some(egui::Key::T), false, egui::Modifiers::NONE)); }
                        for rejected in [egui::Key::Tab, egui::Key::ArrowRight, egui::Key::Backspace] {
                            input.extend([key(rejected, Some(rejected), true, egui::Modifiers::NONE), key(rejected, Some(rejected), false, egui::Modifiers::NONE)]);
                        }
                        input.push(key(logical, Some(physical), true, modifiers));
                        if logical == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                        if physical_collision { input.push(egui::Event::Text("@".into())); }
                        draw_size(&mut app, &ctx, size, input);
                        for _ in 0..12 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(app.capturing.is_none(), "capture must process the correction: {label}, {mode}, passes={passes}");
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(app.hotkey_status.0, combo.map_or_else(|| "Shortcut unchanged".into(), |combo| format!("{combo} set")));
                        assert_eq!(app.dirty_since.is_some(), save);
                        for _ in 0..3 {
                            let mut input = Vec::new();
                            if physical_collision { input.extend([key(egui::Key::T, Some(egui::Key::T), true, egui::Modifiers::NONE), egui::Event::Text("t".into())]); }
                            input.push(key(logical, Some(physical), true, modifiers));
                            if logical == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            if physical_collision { input.push(egui::Event::Text("@".into())); }
                            draw_size(&mut app, &ctx, size, input);
                        }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert!(app.capturing.is_none());
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner);
                        assert_eq!(app.pad_filter, "tar");
                        if let Some(cursor) = cursor { assert_eq!(egui::TextEdit::load_state(&ctx, egui::Id::new("soundboard-search")).unwrap().cursor.char_range(), cursor); }
                        assert_eq!(app.playing, playing);
                        assert_eq!(app.pad_progress, progress);
                        assert_eq!(app.sound_status, status);
                        assert!(app.renaming_pad.is_none() && app.clip_loads.is_none() && app.dictating.is_none() && !app.dictation.busy());
                        if physical_collision {
                            // Releasing the rejected physical key must not release the captured one.
                            draw_size(&mut app, &ctx, size, vec![key(egui::Key::T, Some(egui::Key::T), false, egui::Modifiers::NONE)]);
                            for _ in 0..3 { draw_size(&mut app, &ctx, size, vec![key(logical, Some(physical), true, modifiers), egui::Event::Text("@".into())]); }
                            assert_eq!(app.pad_filter, "tar", "the other physical key remains owned until its own release");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert_eq!(app.playing, playing);
                        }
                        let mut fresh = vec![key(logical, Some(physical), false, modifiers)];
                        let next = match label { "search" => egui::Key::M, "volume" => egui::Key::ArrowRight,
                            "Play target" => egui::Key::Enter, _ => egui::Key::Space };
                        fresh.push(key(next, Some(next), true, egui::Modifiers::NONE));
                        if label == "search" { fresh.push(egui::Event::Text("m".into())); }
                        if next == egui::Key::Space { fresh.push(egui::Event::Text(" ".into())); }
                        fresh.push(key(next, Some(next), false, egui::Modifiers::NONE));
                        draw_size(&mut app, &ctx, size, fresh);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        if label == "search" { assert_eq!(app.pad_filter, "tarm"); }
                        else if label == "volume" {
                            assert!((app.settings.sound_volume - 0.62).abs() < 0.001);
                            expected["sound_volume"] = serde_json::json!(app.settings.sound_volume);
                        } else if label == "Play target" { assert_ne!(app.playing, playing, "a fresh activation plays after release"); }
                        else {
                            assert_eq!(app.capturing.as_ref(), Some(&binding), "a fresh activation opens Set after release");
                            draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, Some(egui::Key::Escape), true, egui::Modifiers::NONE), key(egui::Key::Escape, Some(egui::Key::Escape), false, egui::Modifiers::NONE)]);
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        manager = app.hotkeys.take();
                    }
                }
            }
        }
    }

    #[test]
    fn only_the_capture_cancel_button_accepts_plain_activation_keys() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let (commands, _receiver) = crossbeam_channel::unbounded();
        // App::ui never registers this manager's shortcuts with Windows.
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
        });
        let key = |key, pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        for (page, binding, action, label, setting) in [
            (Page::Settings, Binding::Mute, "Mute mic", "Start processing automatically", "auto_start"),
            (Page::Settings, Binding::Dictate, "Speech to text", "Start processing automatically", "auto_start"),
            (Page::Dictation, Binding::Dictate, "Speech to text", "Press to start, again to stop", "dictation_hold"),
        ] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for held in [egui::Key::Enter, egui::Key::Space] {
                        for assistive_cancel in [true, false] {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            app.hotkeys = manager.take();
                            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                            app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                            app.settings.strength = 0.71347;
                            let original = serde_json::to_value(&app.settings).unwrap();
                            app.start_capture(binding.clone());
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            ctx.enable_accesskit();
                            let size = egui::vec2(700.0, 420.0) / 1.5;
                            for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.unwrap();
                            let owner = *tree.nodes.iter().find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                            let cancel = *tree.nodes.iter().find(|(_, node)| node.label() == Some(format!("Cancel shortcut: {action}").as_str())).map(|(id, _)| id).unwrap();
                            draw_size(&mut app, &ctx, size, vec![focus(owner)]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            for _ in 0..4 {
                                let mut input = vec![key(held, true)];
                                if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                draw_size(&mut app, &ctx, size, input);
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                assert_eq!(app.capturing.as_ref(), Some(&binding), "another control must not act during capture: {label}");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, owner);
                                assert!(app.dirty_since.is_none());
                            }
                            let completion = if assistive_cancel {
                                draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Click, target_tree: TreeId::ROOT, target_node: cancel, data: None,
                                })]);
                                held
                            } else {
                                draw_size(&mut app, &ctx, size, vec![focus(cancel)]);
                                draw_size(&mut app, &ctx, size, vec![key(held, false), key(egui::Key::Space, true), egui::Event::Text(" ".into())]);
                                egui::Key::Space
                            };
                            for _ in 0..3 {
                                draw_size(&mut app, &ctx, size, vec![key(completion, true)]);
                                assert!(app.capturing.is_none(), "held cancellation must not reopen Set");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none());
                            }
                            draw_size(&mut app, &ctx, size, vec![key(completion, false), focus(owner)]);
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            draw_size(&mut app, &ctx, size, vec![key(egui::Key::Space, true), key(egui::Key::Space, false)]);
                            for _ in 0..4 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let mut expected = original;
                            expected[setting] = serde_json::json!(false);
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected, "a fresh activation works after cancellation: {label}, {held:?}, assistive={assistive_cancel}, passes={passes}");
                            assert!(app.dirty_since.is_some() && app.engine.is_none() && app.dictating.is_none() && !app.dictation.busy());
                            manager = app.hotkeys.take();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn enabled_shortcut_rows_keep_focus_through_capture_cancel_and_save() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let rows = [("Mute mic", Binding::Mute, "mute"), ("Bypass processing", Binding::Bypass, "bypass"),
            ("Stop all clips", Binding::StopClips, "stop_clips"), ("Speech to text", Binding::Dictate, "dictate")];
        let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action, target_tree: TreeId::ROOT, target_node, data: None,
        });
        let key = |key, modifiers| [true, false].into_iter().map(|pressed| egui::Event::Key {
            key, physical_key: Some(key), pressed, repeat: false, modifiers,
        }).collect();
        for (label, binding, slot) in &rows {
            for dark in [true, false] {
                for passes in [1, 2] {
                    let mut app = App::stopped(Default::default());
                    app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                    app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                    app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                    app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                    app.settings.strength = 0.71347;
                    app.settings.highpass_hz = 132.456;
                    let mut expected = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = 1.5; });
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    // Exercise the enabled UI without registering shortcuts with the operating system.
                    let draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(280.0, 220.0))),
                        time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                    }, |ui| {
                        app.capture_hotkey(ui.ctx());
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (label, binding, _) in &rows {
                                let current = app.binding_slot(binding).unwrap().clone();
                                app.draw_shortcut_row(ui, binding.clone(), label, current.as_deref(), true);
                                ui.add_space(4.0);
                            }
                            ui.label(&app.hotkey_status.0);
                        });
                    });
                    for _ in 0..3 { draw(&mut app, Vec::new()); }
                    let output = draw(&mut app, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(format!("Set shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                    draw(&mut app, vec![action(Action::Focus, target)]);
                    for _ in 0..8 { draw(&mut app, Vec::new()); }
                    draw(&mut app, key(egui::Key::Enter, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert_eq!(app.capturing.as_ref(), Some(binding));
                    assert!(app.hotkey_bindings().is_empty());
                    let output = draw(&mut app, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    assert_eq!(tree.focus, target);
                    assert_eq!(tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.label(), Some(format!("Cancel shortcut: {label}").as_str()));
                    draw(&mut app, key(egui::Key::M, egui::Modifiers::NONE));
                    assert_eq!(app.capturing.as_ref(), Some(binding), "a plain typing key keeps capture open");
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    draw(&mut app, key(egui::Key::Escape, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert!(app.capturing.is_none() && app.dirty_since.is_none(),
                        "Escape cancels {binding:?}, dark={dark}, passes={passes}: capture={:?}, dirty={}, status={:?}",
                        app.capturing, app.dirty_since.is_some(), app.hotkey_status);
                    assert_eq!(draw(&mut app, Vec::new()).platform_output.accesskit_update.unwrap().focus, target);
                    draw(&mut app, key(egui::Key::Space, egui::Modifiers::NONE));
                    assert_eq!(app.capturing.as_ref(), Some(binding));
                    draw(&mut app, key(egui::Key::Escape, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert!(app.capturing.is_none() && app.dirty_since.is_none(),
                        "immediate Escape cancels {binding:?}, dark={dark}, passes={passes}: capture={:?}, dirty={}, status={:?}",
                        app.capturing, app.dirty_since.is_some(), app.hotkey_status);
                    assert_eq!(draw(&mut app, Vec::new()).platform_output.accesskit_update.unwrap().focus, target,
                        "Escape on the very next frame returns to Set");
                    draw(&mut app, key(egui::Key::Space, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert_eq!(app.capturing.as_ref(), Some(binding));
                    draw(&mut app, key(egui::Key::F24, egui::Modifiers::CTRL | egui::Modifiers::ALT | egui::Modifiers::SHIFT));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    expected["hotkeys"][*slot] = serde_json::json!("Ctrl+Alt+Shift+F24");
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    assert!(app.capturing.is_none() && app.dirty_since.is_some());
                    let output = draw(&mut app, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    assert_eq!(tree.focus, target, "saving returns to the same Set action");
                    let clear = *tree.nodes.iter().find(|(_, node)| node.label() == Some(format!("Clear shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                    draw(&mut app, vec![action(Action::Focus, clear)]);
                    draw(&mut app, key(egui::Key::Enter, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    expected["hotkeys"][*slot] = serde_json::Value::Null;
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    assert_eq!(draw(&mut app, Vec::new()).platform_output.accesskit_update.unwrap().focus, target,
                        "clearing returns to Set, ready for a replacement shortcut");
                    app.dirty_since = None;
                    draw(&mut app, key(egui::Key::Enter, egui::Modifiers::NONE));
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert_eq!(app.capturing.as_ref(), Some(binding));
                    draw(&mut app, vec![action(Action::Click, target)]);
                    for _ in 0..4 { draw(&mut app, Vec::new()); }
                    assert!(app.capturing.is_none() && app.dirty_since.is_none());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    assert_eq!(draw(&mut app, Vec::new()).platform_output.accesskit_update.unwrap().focus, target);
                    assert_eq!(app.hotkey_bindings().len(), 3, "the other shortcuts return after capture");
                }
            }
        }
    }

    #[test]
    fn pointer_shortcut_edits_preserve_the_scroll_position() {
        use egui::accesskit::{Action, ActionRequest, NodeId, TreeId, TreeUpdate};
        let rows = [("Mute mic", Binding::Mute, "mute"), ("Bypass processing", Binding::Bypass, "bypass"),
            ("Stop all clips", Binding::StopClips, "stop_clips"), ("Speech to text", Binding::Dictate, "dictate")];
        let bounds = |tree: &TreeUpdate, target: NodeId| tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
        for (label, binding, slot, width) in [
            ("Mute mic", Binding::Mute, "mute", 420.0),
            ("Speech to text", Binding::Dictate, "dictate", 240.0),
        ] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for y in [40.0, 170.0] {
                        for cancel in [true, false] {
                            let mut app = App::stopped(Default::default());
                            app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                            app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                            app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                            app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                            app.settings.strength = 0.71347;
                            let mut expected = serde_json::to_value(&app.settings).unwrap();
                            if cancel { app.start_capture(binding.clone()); }
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            ctx.enable_accesskit();
                            let draw = |app: &mut App, events, scroll_offset: Option<f32>| {
                                let mut offset = 0.0;
                                let output = ctx.run_ui(egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 220.0))),
                                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                                }, |ui| {
                                    app.capture_hotkey(ui.ctx());
                                    let mut scroll = egui::ScrollArea::vertical().id_salt("shortcut-pointer");
                                    if let Some(offset) = scroll_offset { scroll = scroll.vertical_scroll_offset(offset); }
                                    let result = scroll.show(ui, |ui| {
                                        ui.add_space(450.0);
                                        for (label, binding, _) in &rows {
                                            let current = app.binding_slot(binding).unwrap().clone();
                                            app.draw_shortcut_row(ui, binding.clone(), label, current.as_deref(), true);
                                            ui.add_space(4.0);
                                        }
                                        ui.add_space(600.0);
                                    });
                                    offset = result.state.offset.y;
                                });
                                (output, offset)
                            };
                            for _ in 0..3 { draw(&mut app, Vec::new(), Some(0.0)); }
                            let (output, _) = draw(&mut app, Vec::new(), Some(0.0));
                            let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                                node.label() == Some(format!("{} shortcut: {label}", if cancel { "Cancel" } else { "Clear" }).as_str()))
                                .map(|(id, _)| id).unwrap();
                            let (output, _) = draw(&mut app, Vec::new(), Some(0.0));
                            let rect = bounds(output.platform_output.accesskit_update.as_ref().unwrap(), target);
                            let scroll_offset = ((rect.y0 + rect.y1) / 2.0) as f32 - y;
                            draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                            })], None);
                            for _ in 0..8 { draw(&mut app, Vec::new(), None); }
                            for _ in 0..3 { draw(&mut app, Vec::new(), Some(scroll_offset)); }
                            for _ in 0..8 { draw(&mut app, Vec::new(), None); }
                            let (output, before) = draw(&mut app, Vec::new(), None);
                            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target,
                                "manual scrolling preserves the existing keyboard focus before clicking");
                            let rect = bounds(output.platform_output.accesskit_update.as_ref().unwrap(), target);
                            assert!(rect.y0 >= 0.0 && rect.y1 <= 220.0, "the clicked action starts within the viewport: {rect:?}");
                            let pos = egui::pos2(((rect.x0 + rect.x1) / 2.0) as f32, ((rect.y0 + rect.y1) / 2.0) as f32);
                            for pressed in [true, false] {
                                let (_, offset) = draw(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                                    pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                                }], None);
                                assert!((offset - before).abs() < 0.1, "press and release keep the page position");
                            }
                            for _ in 0..8 { draw(&mut app, Vec::new(), None); }
                            let (output, after) = draw(&mut app, Vec::new(), None);
                            assert!((after - before).abs() < 0.1,
                                "pointer editing stays at the user's scroll position: {label}, width={width}, cancel={cancel}, y={y}, passes={passes}: {before} -> {after}");
                            let tree = output.platform_output.accesskit_update.unwrap();
                            let focused = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
                            assert_eq!(focused.label(), Some(format!("Set shortcut: {label}").as_str()),
                                "pointer action focuses Set: cancel={cancel}, capture={:?}, pos={pos:?}, bounds={rect:?}, focused={focused:?}", app.capturing);
                            if !cancel { expected["hotkeys"][slot] = serde_json::Value::Null; }
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert!(app.capturing.is_none());
                            assert_eq!(app.dirty_since.is_some(), !cancel);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn settings_focus_stays_visible_when_only_the_window_height_changes() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for label in ["Check for updates daily", "Open log folder"] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for zoom in [1.0, 1.5] {
                        let mut app = App::stopped(Default::default());
                        app.page = Page::Settings;
                        app.hotkey_status = ("Some shortcuts are unavailable. Choose another combination for each affected action.".into(), AMBER);
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        let mut size = egui::vec2(700.0, 720.0) / zoom;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                        draw_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                        })]);
                        for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        for height in [420.0, 720.0, 420.0] {
                            size.y = height / zoom;
                            for _ in 0..8 { draw_size(&mut app, &ctx, size, Vec::new()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                            let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                            assert!(rect.top() >= 44.0 && rect.bottom() <= size.y
                                && output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                    && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                                "{label} stays visible after changing only height: height={height}, zoom={zoom}, passes={passes}: {rect:?}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && app.capturing.is_none() && app.engine.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes Settings focus after height-only resizing; needs Python with Pillow"]
    fn snapshot_settings_focus_after_resizing() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for (control, label) in [("updates", "Check for updates daily"), ("logs", "Open log folder")] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for zoom in [1.0, 1.5] {
                    let mut app = App::stopped(Default::default());
                    app.page = Page::Settings;
                    app.hotkey_status = ("Some shortcuts are unavailable. Choose another combination for each affected action.".into(), AMBER);
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.enable_accesskit();
                    let tall = egui::vec2(700.0, 720.0) / zoom;
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    for _ in 0..3 { draw_size(&mut app, &ctx, tall, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, tall, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let input = |size, events| egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default()
                    };
                    let mut inputs: Vec<_> = (0..3).map(|_| input(tall, Vec::new())).collect();
                    inputs.push(input(tall, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]));
                    inputs.extend((0..8).map(|_| input(tall, Vec::new())));
                    inputs.extend((0..8).map(|_| input(size, Vec::new())));
                    let image = super::super::snapshot::snapshot_with_inputs(&mut app,
                        &format!("settings-resize-{control}-{}-{theme}", (zoom * 100.0) as u32), size, zoom, dark, inputs);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.capturing.is_none() && app.engine.is_none());
                }
            }
        }
    }

    #[test]
    #[ignore = "writes shortcut clearing screenshots; needs Python with Pillow"]
    fn snapshot_shortcut_focus_after_clearing() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for (page, page_name, label, slot) in [(Page::Settings, "settings", "Mute mic", "mute"),
            (Page::Dictation, "dictation", "Speech to text", "dictate")]
        {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for zoom in [1.0, 1.25, 1.5] {
                    for (held, key_name) in [(egui::Key::Enter, "enter"), (egui::Key::Space, "space")] {
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        let mut expected = serde_json::to_value(&app.settings).unwrap();
                        expected["hotkeys"][slot] = serde_json::Value::Null;
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        ctx.enable_accesskit();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let clear = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(format!("Clear shortcut: {label}").as_str())).map(|(id, _)| id).unwrap();
                        let key = |pressed| egui::Event::Key { key: held, physical_key: Some(held),
                            pressed, repeat: false, modifiers: egui::Modifiers::NONE };
                        let mut events = vec![Vec::new(); 3];
                        events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: clear, data: None,
                        })]);
                        events.extend(vec![Vec::new(); 8]);
                        events.push(vec![key(true)]);
                        for _ in 0..3 {
                            let mut input = vec![key(true)];
                            if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            events.push(input);
                        }
                        events.push(vec![key(false)]);
                        events.extend(vec![Vec::new(); 8]);
                        let image = super::super::snapshot::snapshot_with_events(&mut app,
                            &format!("shortcut-{page_name}-cleared-{key_name}-{}-{theme}", (zoom * 100.0) as u32), size, zoom, dark, events);
                        println!("SNAPSHOT {}", image.display());
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert!(app.capturing.is_none() && app.engine.is_none() && app.dirty_since.is_some());
                        assert!(!app.dictation.busy() && app.dictation.download_progress().is_none());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes shortcut capture workflow screenshots; needs Python with Pillow"]
    fn snapshot_shortcut_capture_keyboard_workflow() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["other.wav", "target.wav"].into_iter().map(|name| {
            let path = dir.path().join(name);
            Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().unwrap().save_wav(&path).unwrap();
            path
        }).collect();
        let (commands, _receiver) = crossbeam_channel::unbounded();
        let mut manager = Some(crate::hotkeys::Hotkeys::new(&egui::Context::default(), commands).unwrap());
        let focus = |target_node| vec![egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
        })];
        let key = |key, pressed, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers };
        for (page, page_name, binding, owner_label) in [
            (Page::Settings, "settings", Binding::Mute, "Set shortcut: Mute mic"),
            (Page::Dictation, "dictation", Binding::Dictate, "Set shortcut: Speech to text"),
            (Page::Soundboard, "pad", Binding::Pad(paths[1].clone()), "Play target"),
        ] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for zoom in [1.0, 1.25, 1.5] {
                    for state in ["waiting", "saved", "batch-saved", "batch-cancelled", "corrected", "correction-cancelled"] {
                        let saved = matches!(state, "saved" | "batch-saved" | "corrected");
                        let batch = state.starts_with("batch-");
                        let correction = matches!(state, "corrected" | "correction-cancelled");
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.hotkeys = manager.take();
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+M".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Alt+B".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+S".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+D".into());
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        app.settings.sounds = paths.iter().cloned().map(crate::config::Pad::new).collect();
                        app.settings.sounds[1].name = Some("target".into());
                        app.settings.sounds[1].starred = true;
                        app.settings.sounds[1].volume = 0.43561;
                        app.settings.sounds[1].start = 0.12345;
                        app.settings.sounds[1].end = Some(0.812345);
                        app.settings.sounds[1].hotkey = Some("Ctrl+Alt+P".into());
                        app.settings.sound_volume = 0.61347;
                        let mut expected = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        if page == Page::Soundboard {
                            app.engine = Some(crate::engine::Engine::for_test());
                            for path in &paths { app.cache_clip(path.clone(), super::super::CachedClip::read(path, &std::sync::atomic::AtomicBool::new(false)).unwrap()); }
                            app.play_clip(0, &ctx);
                            app.toggle_clip(0, &ctx);
                        }
                        let playing = app.playing.clone();
                        let progress = app.pad_progress.clone();
                        let status = app.sound_status.clone();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let owner = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(owner_label)).map(|(id, _)| id).unwrap();
                        let mut events = vec![Vec::new(); 3];
                        events.push(focus(owner));
                        events.extend(vec![Vec::new(); 8]);
                        let set = if page == Page::Soundboard {
                            events.push(vec![key(egui::Key::F10, true, egui::Modifiers::SHIFT), key(egui::Key::F10, false, egui::Modifiers::SHIFT)]);
                            events.extend(vec![Vec::new(); 8]);
                            for input in &events { draw_size(&mut app, &ctx, size, input.clone()); }
                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                            *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.label() == Some("Set…")).map(|(id, _)| id).unwrap()
                        } else { owner };
                        events.push(focus(set));
                        events.extend(vec![Vec::new(); 8]);
                        let held = if zoom == 1.25 { egui::Key::Space } else { egui::Key::Enter };
                        if saved {
                            let combo = if held == egui::Key::Space { "Ctrl+Space" } else { "Ctrl+Enter" };
                            if page == Page::Soundboard { expected["sounds"][1]["hotkey"] = serde_json::json!(combo); }
                            else { expected["hotkeys"][if page == Page::Settings { "mute" } else { "dictate" }] = serde_json::json!(combo); }
                        }
                        if batch {
                            let completion = if saved { held } else { egui::Key::Escape };
                            let modifiers = if saved { egui::Modifiers::CTRL } else { egui::Modifiers::NONE };
                            let mut input = vec![key(held, true, egui::Modifiers::NONE)];
                            if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            input.extend([key(held, false, egui::Modifiers::NONE), key(completion, true, modifiers)]);
                            if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                            events.push(input);
                            for _ in 0..3 {
                                let mut input = vec![key(completion, true, modifiers)];
                                if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                events.push(input);
                            }
                            events.push(vec![key(completion, false, modifiers)]);
                        } else {
                            for _ in 0..4 {
                                let mut input = vec![key(held, true, egui::Modifiers::NONE)];
                                if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                events.push(input);
                            }
                            events.push(vec![key(held, false, egui::Modifiers::NONE)]);
                            if correction {
                                let completion = if saved { held } else { egui::Key::Escape };
                                let modifiers = if saved { egui::Modifiers::CTRL } else { egui::Modifiers::NONE };
                                let mut input = vec![key(egui::Key::T, true, egui::Modifiers::NONE), egui::Event::Text("t".into()), key(egui::Key::T, false, egui::Modifiers::NONE)];
                                for rejected in [egui::Key::Tab, egui::Key::ArrowRight, egui::Key::Backspace] {
                                    input.extend([key(rejected, true, egui::Modifiers::NONE), key(rejected, false, egui::Modifiers::NONE)]);
                                }
                                input.push(key(completion, true, modifiers));
                                if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                events.push(input);
                                for _ in 0..3 {
                                    let mut input = vec![key(completion, true, modifiers)];
                                    if completion == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                    events.push(input);
                                }
                                events.push(vec![key(completion, false, modifiers)]);
                            } else if saved {
                                for _ in 0..4 {
                                    let mut input = vec![key(held, true, egui::Modifiers::CTRL)];
                                    if held == egui::Key::Space { input.push(egui::Event::Text(" ".into())); }
                                    events.push(input);
                                }
                                events.push(vec![key(held, false, egui::Modifiers::CTRL)]);
                            }
                        }
                        events.extend(vec![Vec::new(); 8]);
                        let image = super::super::snapshot::snapshot_with_events(&mut app,
                            &format!("shortcut-workflow-{page_name}-{state}-{}-{theme}", (zoom * 100.0) as u32), size, zoom, dark, events);
                        println!("SNAPSHOT {}", image.display());
                        assert_eq!(app.capturing.as_ref(), (state == "waiting").then_some(&binding));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(app.dirty_since.is_some(), saved);
                        assert_eq!(app.playing, playing);
                        assert_eq!(app.pad_progress, progress);
                        assert_eq!(app.sound_status, status);
                        assert!(app.renaming_pad.is_none() && app.clip_loads.is_none() && !app.dictation.busy());
                        manager = app.hotkeys.take();
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes responsive shortcut editor screenshots; needs Python with Pillow"]
    fn snapshot_responsive_shortcut_editors() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for (page, page_name, label) in [(Page::Settings, "settings", "Stop all clips"), (Page::Dictation, "dictation", "Speech to text")] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for zoom in [1.0, 1.5] {
                    for waiting in [false, true] {
                        let mut app = App::stopped(Default::default());
                        app.page = page;
                        app.settings.hotkeys.mute = Some("Ctrl+Alt+Shift+ArrowRight".into());
                        app.settings.hotkeys.bypass = Some("Ctrl+Alt+Shift+BracketRight".into());
                        app.settings.hotkeys.stop_clips = Some("Ctrl+Alt+Shift+PageDown".into());
                        app.settings.hotkeys.dictate = Some("Ctrl+Alt+Shift+F24".into());
                        if waiting { app.start_capture(if page == Page::Settings { Binding::StopClips } else { Binding::Dictate }); }
                        let ctx = egui::Context::default();
                        ctx.enable_accesskit();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                            node.label() == Some(format!("{} shortcut: {label}", if waiting { "Cancel" } else { "Clear" }).as_str())).map(|(id, _)| id).unwrap();
                        let mut events = vec![vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                        })]];
                        events.extend(std::iter::repeat_n(Vec::new(), 8));
                        let state = if waiting { "capture" } else { "saved" };
                        let image = super::super::snapshot::snapshot_with_events(&mut app,
                            &format!("shortcut-{page_name}-{state}-{}-{theme}", (zoom * 100.0) as u32), size, zoom, dark, events);
                        println!("SNAPSHOT {}", image.display());
                    }
                }
            }
        }
    }

    #[test]
    fn speech_model_menu_selection_updates_only_the_next_take() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for recording in [false, true] {
                        let mut app = App::stopped(Default::default());
                        app.page = Page::Dictation;
                        let before = app.settings.speech_model;
                        if recording {
                            app.dictating = Some(DictationRecording {
                                recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: before, hold_to_talk: false,
                            });
                        }
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                        let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                            action, target_tree: TreeId::ROOT, target_node, data: None,
                        });
                        let key = |key| egui::Event::Key {
                            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        };
                        draw_size(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let popup_id = ctx.memory(|memory| memory.focused()).unwrap().with("popup");
                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(egui::Popup::is_any_open(&ctx));
                        let menu = egui::AreaState::load(&ctx, popup_id).unwrap().rect();
                        assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(menu),
                            "the speech-model menu fits the zoomed viewport; zoom={zoom}, passes={passes}: {menu:?}");
                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape)]);
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        draw_size(&mut app, &ctx, size, vec![action(Action::Focus, target), key(egui::Key::Enter)]);
                        for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let model = SpeechModel::Small;
                        let choice = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.label().is_some_and(|name| name.starts_with(model.label())))
                            .map(|(id, _)| id).unwrap();
                        if passes == 1 {
                            let mut reached = false;
                            for _ in 0..10 {
                                let output = draw_size(&mut app, &ctx, size, vec![key(egui::Key::Tab)]);
                                if output.platform_output.accesskit_update.unwrap().focus == choice { reached = true; break; }
                            }
                            assert!(reached, "Tab reaches the multilingual model inside the menu");
                            draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        } else { draw_size(&mut app, &ctx, size, vec![action(Action::Click, choice)]); }
                        for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        let mut expected = original;
                        expected["speech_model"] = serde_json::to_value(model).unwrap();
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(app.dictating.as_ref().map(|take| take.model), recording.then_some(before));
                        assert!(!app.dictation.busy() && app.dictation.download_progress().is_none());
                        assert!(app.dirty_since.is_some() && !egui::Popup::is_any_open(&ctx));
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        assert_eq!(tree.focus, target);
                        let node = &tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1;
                        assert_eq!(node.value(), Some(model.label()));
                        let bounds = node.bounds().unwrap();
                        let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                            && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                            "the selected model and focus outline are visible; zoom={zoom}, passes={passes}, recording={recording}: {rect:?}");
                        let label = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Label && node.value() == Some("Model"))
                            .map(|(id, _)| id).unwrap();
                        assert!(node.labelled_by().contains(&label), "the model selector keeps its accessible label");
                        // Selecting the saved model again must close the menu without scheduling a save.
                        app.dirty_since = None;
                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        draw_size(&mut app, &ctx, size, vec![action(Action::Click, choice)]);
                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(!egui::Popup::is_any_open(&ctx) && app.dirty_since.is_none());
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(app.dictating.as_ref().map(|take| take.model), recording.then_some(before));
                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes speech-model menu and focus screenshots; needs Python with Pillow"]
    fn snapshot_dictation_model_menu() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (zoom, scale) in [(1.0, "standard"), (1.5, "zoomed")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for menu in [true, false] {
                    let mut app = App::stopped(Default::default());
                    app.page = Page::Dictation;
                    if !menu { app.settings.speech_model = SpeechModel::Small; }
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    events.extend(vec![Vec::new(); 8]);
                    if menu { events.push(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None,
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]); }
                    let scene = if menu { "menu" } else { "focus" };
                    let image = super::super::snapshot::snapshot_with_events(&mut app,
                        &format!("dictation-model-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none() && app.dictating.is_none() && !app.dictation.busy());
                }
            }
        }
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

    fn model_files(model: SpeechModel) {
        let directory = model.directory_for_test();
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["config.json", "tokenizer.json", "model.safetensors"] {
            std::fs::write(directory.join(name), b"isolated UI model fixture").unwrap();
        }
        assert!(model.is_downloaded());
    }

    fn finish_model_download(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.dictation.download_progress().is_some() {
            app.poll_dictation();
            assert!(Instant::now() < deadline, "the model download finishes");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn active_dictation_preserves_model_controls_and_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let directory = tempfile::tempdir().unwrap();
        let _model_directory = crate::dictation::use_model_directory_for_test(directory.path());
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for (index, model) in SpeechModel::ALL.into_iter().enumerate() {
                        for scene in ["recording", "finishing", "another model", "transcribing"] {
                            for focus in ["remove", "mute", "cleared"] {
                                model_files(model);
                                let mut app = App::stopped(Default::default());
                                app.page = Page::Dictation;
                                app.settings.speech_model = model;
                                app.last_dictation = "Previous words café".into();
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                let action = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                let button = |output: &egui::FullOutput, label: &str| *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let remove = button(&output, "Remove");
                                let mute = button(&output, "Mute");
                                let picker = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                                draw_size(&mut app, &ctx, size, vec![action(if focus == "mute" { mute } else { remove })]);
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                if focus == "cleared" {
                                    draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                }
                                let release = if scene == "transcribing" {
                                    let (dictation, release) = Dictation::delayed_for_test();
                                    app.dictation = dictation;
                                    let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
                                    app.dictation.transcribe(model, take);
                                    Some(release)
                                } else {
                                    let mut recorder = Recorder::from_samples_for_test(&[0.25; 96]);
                                    if scene == "finishing" { recorder.stop_capture(); }
                                    app.dictating = Some(DictationRecording {
                                        recorder, model: if scene == "another model" { SpeechModel::ALL[(index + 1) % SpeechModel::ALL.len()] } else { model },
                                        hold_to_talk: false,
                                    });
                                    None
                                };
                                for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                let disabled = scene != "another model";
                                assert_eq!(tree.nodes.iter().find(|(id, _)| *id == remove).unwrap().1.is_disabled(), disabled);
                                if focus == "cleared" { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                else {
                                    let target = if focus == "mute" { mute } else if disabled { picker } else { remove };
                                    assert_eq!(tree.focus, target, "model controls preserve focus: {scene}, {focus}, zoom={zoom}, passes={passes}");
                                    if focus == "remove" && disabled {
                                        let bounds = tree.nodes.iter().find(|(id, _)| *id == picker).unwrap().1.bounds().unwrap();
                                        let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                            && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))));
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                        assert!(egui::Popup::is_any_open(&ctx));
                                    }
                                }
                                assert!(model.is_downloaded() && app.dictation.download_progress().is_none());
                                for name in ["config.json", "tokenizer.json", "model.safetensors"] {
                                    assert_eq!(std::fs::read(model.directory_for_test().join(name)).unwrap(), b"isolated UI model fixture");
                                }
                                assert_eq!(app.last_dictation, "Previous words café");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none());
                                app.dictation.cancel_transcription();
                                if let Some(release) = release { release.send(()).unwrap(); }
                                model.remove().unwrap();
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn model_downloads_keep_focus_through_completion_and_cancellation() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let directory = tempfile::tempdir().unwrap();
        let _model_directory = crate::dictation::use_model_directory_for_test(directory.path());
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for model in SpeechModel::ALL {
                        for outcome in ["ready", "failed", "cancelled"] {
                            for early in [true, false] {
                                if early && outcome == "cancelled" { continue; }
                                for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                                    let mut app = App::stopped(Default::default());
                                    app.page = Page::Dictation;
                                    app.settings.speech_model = model;
                                    app.last_dictation = "Previous words café".into();
                                    let completion = app.dictation.hold_next_download_for_test(50_000_000, 100_000_000);
                                    let original = serde_json::to_value(&app.settings).unwrap();
                                    let ctx = egui::Context::default();
                                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                    widgets::configure_text_styles(&ctx);
                                    ctx.enable_accesskit();
                                    let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                        action, target_tree: TreeId::ROOT, target_node, data: None,
                                    });
                                    let key = |key, pressed| egui::Event::Key {
                                        key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                    };
                                    let pointer = activation.contains("pointer");
                                    let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                    let held_events = || {
                                        let mut events = vec![key(held, true)];
                                        if held == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
                                        events
                                    };
                                    let click = |app: &mut App, target| {
                                        if pointer {
                                            let output = draw_size(app, &ctx, size, Vec::new());
                                            let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter()
                                                .find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                            let pos = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
                                            assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos));
                                            for pressed in [true, false] {
                                                draw_size(app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                                                    pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                                                }]);
                                            }
                                        } else {
                                            draw_size(app, &ctx, size, if activation == "accessible" { vec![action(Action::Click, target)] } else { held_events() });
                                        }
                                    };
                                    let visible = |output: &egui::FullOutput, target| {
                                        let bounds = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                            .find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                        let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                            && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                                            "focused model control is visible: {outcome}, {activation}, early={early}, zoom={zoom}, passes={passes}");
                                    };
                                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    let download = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                                        && node.label() == Some("Download")).map(|(id, _)| id).unwrap();
                                    let picker = *tree.nodes.iter().find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                                    if activation != "pointer" { draw_size(&mut app, &ctx, size, vec![action(Action::Focus, download)]); }
                                    for _ in 0..4 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    click(&mut app, download);
                                    assert_eq!(app.dictation.download_progress(), Some((model, 50_000_000, 100_000_000)));
                                    let _retry = app.dictation.hold_next_download_for_test(0, 100_000_000);
                                    if !early {
                                        for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                        let cancel = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                                            && node.label() == Some("Cancel")).map(|(id, _)| id).unwrap();
                                        if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                        else {
                                            assert_eq!(tree.focus, cancel, "starting a download returns focus to its cancellation control");
                                            visible(&output, cancel);
                                        }
                                        if matches!(activation, "enter" | "space") {
                                            let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                            assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)));
                                            for _ in 0..3 { draw_size(&mut app, &ctx, size, held_events()); }
                                            assert!(!app.dictation.download_cancelling(), "the starting key must not cancel the download");
                                        }
                                        if outcome == "cancelled" {
                                            if matches!(activation, "enter" | "space") {
                                                draw_size(&mut app, &ctx, size, vec![key(held, false)]);
                                            }
                                            if activation == "focused pointer" {
                                                draw_size(&mut app, &ctx, size, vec![action(Action::Focus, cancel)]);
                                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                            }
                                            click(&mut app, cancel);
                                            for _ in 0..5 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                            assert!(app.dictation.download_cancelling());
                                            let output = draw_size(&mut app, &ctx, size, Vec::new());
                                            if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                            else {
                                                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, picker);
                                                visible(&output, picker);
                                                if matches!(activation, "enter" | "space") {
                                                    for _ in 0..3 { draw_size(&mut app, &ctx, size, held_events()); }
                                                    assert!(!egui::Popup::is_any_open(&ctx), "the cancellation key leaves the model selector closed");
                                                }
                                            }
                                        }
                                    }
                                    if outcome == "ready" { model_files(model); }
                                    completion.send(if outcome == "failed" { Err(anyhow::anyhow!("Model download fixture interrupted")) } else { Ok(()) }).unwrap();
                                    finish_model_download(&mut app);
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                    else {
                                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, picker,
                                            "download completion returns to Model: {outcome}, {activation}, early={early}, zoom={zoom}, passes={passes}");
                                        visible(&output, picker);
                                        if matches!(activation, "enter" | "space") {
                                            let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                            assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)));
                                            for _ in 0..3 { draw_size(&mut app, &ctx, size, held_events()); }
                                            draw_size(&mut app, &ctx, size, vec![key(held, false)]);
                                        }
                                        assert!(!egui::Popup::is_any_open(&ctx));
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                        assert!(egui::Popup::is_any_open(&ctx));
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    }
                                    assert!(app.dictation.download_progress().is_none() && !app.dictation.busy());
                                    assert_eq!(model.is_downloaded(), outcome == "ready");
                                    match outcome {
                                        "ready" => assert_eq!(app.dictation_status, (format!("{} is ready", model.label()), GREEN)),
                                        "failed" => assert_eq!(app.dictation_status, ("Model download fixture interrupted".into(), RED)),
                                        _ => assert_eq!(app.dictation_status, ("Model download cancelled".into(), MUTED)),
                                    }
                                    assert_eq!(app.last_dictation, "Previous words café");
                                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                    assert!(app.dirty_since.is_none());
                                    model.remove().unwrap();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn model_download_completion_preserves_navigation_and_the_next_model() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let directory = tempfile::tempdir().unwrap();
        let _model_directory = crate::dictation::use_model_directory_for_test(directory.path());
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for outcome in ["ready", "failed", "cancelled"] {
                        for early in [true, false] {
                            if early && outcome == "cancelled" { continue; }
                            for mode in ["unchanged", "focus moved", "page switched", "result focus queued", "result page queued",
                                "result focus cleared", "model focus moved", "model chosen", "recording starts", "recording discarded"] {
                                let mut app = App::stopped(Default::default());
                                app.page = Page::Dictation;
                                let model = app.settings.speech_model;
                                let completion = app.dictation.hold_next_download_for_test(50_000_000, 100_000_000);
                                app.last_dictation = "Previous words café".into();
                                if mode.starts_with("recording") { model_files(SpeechModel::TinyEn); }
                                if mode == "recording discarded" {
                                    app.dictating = Some(DictationRecording {
                                        recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: SpeechModel::TinyEn, hold_to_talk: false,
                                    });
                                }
                                let mut expected = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                let button = |output: &egui::FullOutput, label: &str| *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let picker = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                                let download = button(&output, "Download");
                                let switched = mode.contains("page");
                                let other = if mode == "model focus moved" { picker } else { button(&output, if switched { "Soundboard" } else { "Mute" }) };
                                let move_focus = |app: &mut App| {
                                    let mut events = vec![action(Action::Focus, other)];
                                    if switched { events.push(action(Action::Click, other)); }
                                    draw_size(app, &ctx, size, events);
                                    for _ in 0..3 { draw_size(app, &ctx, size, Vec::new()); }
                                };
                                draw_size(&mut app, &ctx, size, vec![action(Action::Focus, download)]);
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                let _retry = app.dictation.hold_next_download_for_test(0, 100_000_000);
                                if !early {
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, button(&output, "Cancel"));
                                    if outcome == "cancelled" {
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                        assert!(app.dictation.download_cancelling());
                                        for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    }
                                }
                                match mode {
                                    "focus moved" | "page switched" | "model focus moved" => move_focus(&mut app),
                                    "model chosen" => {
                                        draw_size(&mut app, &ctx, size, vec![action(Action::Focus, picker), key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                        for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                        let output = draw_size(&mut app, &ctx, size, Vec::new());
                                        let choice = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                            .find(|(_, node)| node.label().is_some_and(|label| label.starts_with(SpeechModel::Small.label())))
                                            .map(|(id, _)| id).unwrap();
                                        draw_size(&mut app, &ctx, size, vec![action(Action::Click, choice)]);
                                        for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                        assert_eq!(app.settings.speech_model, SpeechModel::Small);
                                        expected["speech_model"] = serde_json::to_value(SpeechModel::Small).unwrap();
                                    }
                                    "recording starts" => app.dictating = Some(DictationRecording {
                                        recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: SpeechModel::TinyEn, hold_to_talk: false,
                                    }),
                                    "recording discarded" => app.discard_dictation(),
                                    _ => {}
                                }
                                if mode.starts_with("recording") && !early {
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                }
                                if outcome == "ready" { model_files(model); }
                                completion.send(if outcome == "failed" { Err(anyhow::anyhow!("Model download fixture interrupted")) } else { Ok(()) }).unwrap();
                                finish_model_download(&mut app);
                                if mode.starts_with("result") {
                                    if mode == "result focus cleared" {
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    } else { move_focus(&mut app); }
                                }
                                for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                if mode == "result focus cleared" { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                else {
                                    let expected_focus = if matches!(mode, "unchanged" | "model chosen" | "recording starts" | "recording discarded") { picker } else { other };
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, expected_focus,
                                        "download completion preserves focus: {mode}, {outcome}, early={early}, zoom={zoom}, passes={passes}");
                                }
                                if switched {
                                    assert_eq!(app.page, Page::Soundboard);
                                    let dictation = button(&output, "Dictation");
                                    draw_size(&mut app, &ctx, size, vec![action(Action::Focus, dictation), action(Action::Click, dictation)]);
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, dictation,
                                        "returning to Dictation preserves its navigation focus");
                                }
                                assert_eq!(app.page, Page::Dictation);
                                assert_eq!(app.dictating.as_ref().map(|take| take.model), (mode == "recording starts").then_some(SpeechModel::TinyEn));
                                assert_eq!(app.last_dictation, "Previous words café");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                                assert_eq!(app.dirty_since.is_some(), mode == "model chosen");
                                assert_eq!(model.is_downloaded(), outcome == "ready");
                                assert!(!SpeechModel::Small.is_downloaded() && app.dictation.download_progress().is_none() && !app.dictation.busy());
                                model.remove().unwrap();
                                SpeechModel::TinyEn.remove().unwrap();
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn removing_a_model_keeps_focus_without_starting_a_download() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        #[cfg(windows)]
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let _model_directory = crate::dictation::use_model_directory_for_test(directory.path());
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for model in SpeechModel::ALL {
                        for failed in [false, true] {
                            for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                                assert!(model.directory_for_test().starts_with(directory.path()));
                                model_files(model);
                                #[cfg(windows)]
                                let held_file = failed.then(|| std::fs::File::options().read(true).share_mode(1)
                                    .open(model.directory_for_test().join("model.safetensors")).unwrap());
                                #[cfg(not(windows))]
                                let held_permissions = failed.then(|| {
                                    use std::os::unix::fs::PermissionsExt;
                                    let folder = model.directory_for_test();
                                    let permissions = std::fs::metadata(&folder).unwrap().permissions();
                                    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
                                    permissions
                                });
                                let mut app = App::stopped(Default::default());
                                app.page = Page::Dictation;
                                app.settings.speech_model = model;
                                app.last_dictation = "Previous words café".into();
                                let _download = app.dictation.hold_next_download_for_test(50_000_000, 100_000_000);
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                let target = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button
                                    && node.label() == Some("Remove")).map(|(id, _)| id).unwrap();
                                let picker = *tree.nodes.iter().find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                                if activation != "pointer" { draw_size(&mut app, &ctx, size, vec![action(Action::Focus, target)]); }
                                for _ in 0..5 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let pointer = activation.contains("pointer");
                                let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                let held_events = || {
                                    let mut events = vec![key(held, true)];
                                    if held == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
                                    events
                                };
                                if pointer {
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter()
                                        .find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                    let pos = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
                                    assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos));
                                    for pressed in [true, false] {
                                        draw_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                                            pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                                        }]);
                                    }
                                } else {
                                    let events = if activation == "accessible" { vec![action(Action::Click, target)] } else { held_events() };
                                    draw_size(&mut app, &ctx, size, events);
                                }
                                for _ in 0..5 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none(), "mouse removal must not focus Download"); }
                                else {
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, picker,
                                        "model removal returns focus: {activation}, failed={failed}, zoom={zoom}, passes={passes}");
                                    if matches!(activation, "enter" | "space") {
                                        let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                        assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)));
                                        for _ in 0..3 { draw_size(&mut app, &ctx, size, held_events()); }
                                        draw_size(&mut app, &ctx, size, vec![key(held, false)]);
                                    }
                                    assert!(!egui::Popup::is_any_open(&ctx), "the removal key leaves the model menu closed");
                                    draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    assert!(egui::Popup::is_any_open(&ctx), "a fresh Enter opens the model selector");
                                    draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                }
                                assert!(app.dictation.download_progress().is_none() && !app.dictation.busy());
                                if failed {
                                    assert_eq!(app.dictation_status.1, RED);
                                    assert!(app.dictation_status.0.starts_with("Could not remove model:"));
                                    assert_eq!(std::fs::read(model.directory_for_test().join("model.safetensors")).unwrap(), b"isolated UI model fixture");
                                } else {
                                    assert_eq!(app.dictation_status.0, "Model removed");
                                    assert!(!model.directory_for_test().exists());
                                }
                                assert_eq!(app.last_dictation, "Previous words café");
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none());
                                #[cfg(windows)]
                                drop(held_file);
                                #[cfg(not(windows))]
                                if let Some(permissions) = held_permissions {
                                    std::fs::set_permissions(model.directory_for_test(), permissions).unwrap();
                                }
                                model.remove().unwrap();
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "writes model download and removal focus screenshots; needs Python with Pillow"]
    fn snapshot_dictation_download_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        use std::os::windows::fs::OpenOptionsExt;
        struct ModelControls {
            app: App,
            completion: Option<crossbeam_channel::Sender<anyhow::Result<()>>>,
            scene: &'static str,
            changed: bool,
            copied: bool,
        }
        impl eframe::App for ModelControls {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if ui.ctx().cumulative_frame_nr() >= 18 && !self.changed {
                    self.changed = true;
                    if self.scene == "in-use" {
                        self.app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: self.app.settings.speech_model, hold_to_talk: false,
                        });
                    }
                    if matches!(self.scene, "ready" | "failed" | "cancelled") {
                        if self.scene == "ready" { model_files(self.app.settings.speech_model); }
                        self.completion.take().unwrap().send(if self.scene == "failed" {
                            Err(anyhow::anyhow!("Model download fixture interrupted"))
                        } else { Ok(()) }).unwrap();
                        finish_model_download(&mut self.app);
                        self.completion = Some(self.app.dictation.hold_next_download_for_test(0, 100_000_000));
                    }
                }
                eframe::App::ui(&mut self.app, ui, frame);
                self.copied |= ui.ctx().output(|output| output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let _model_directory = crate::dictation::use_model_directory_for_test(directory.path());
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["downloading", "cancelling", "ready", "failed", "cancelled", "removed", "remove-failed", "in-use"] {
                    let mut app = App::stopped(Default::default());
                    app.page = Page::Dictation;
                    app.last_dictation = "Previous words café\nSecond line.".into();
                    let model = app.settings.speech_model;
                    let installed = matches!(scene, "removed" | "remove-failed" | "in-use");
                    if installed { model_files(model); }
                    let held_file = (scene == "remove-failed").then(|| std::fs::File::options().read(true).share_mode(1)
                        .open(model.directory_for_test().join("model.safetensors")).unwrap());
                    let completion = app.dictation.hold_next_download_for_test(50_000_000, 100_000_000);
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some(if installed { "Remove" } else { "Download" }))
                        .map(|(id, _)| id).unwrap();
                    let mut controls = ModelControls { app, completion: Some(completion), scene, changed: false, copied: false };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    events.extend(vec![Vec::new(); 6]);
                    let press = vec![egui::Event::Key { key: egui::Key::Space, physical_key: Some(egui::Key::Space),
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }, egui::Event::Text(" ".into())];
                    events.push(if scene == "in-use" { Vec::new() } else { press.clone() });
                    events.extend(vec![Vec::new(); 3]);
                    events.push(if matches!(scene, "cancelling" | "cancelled") { vec![egui::Event::Key {
                        key: egui::Key::Space, physical_key: Some(egui::Key::Space), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE,
                    }] } else { Vec::new() });
                    events.push(if matches!(scene, "cancelling" | "cancelled") { press.clone() } else { Vec::new() });
                    events.extend(vec![Vec::new(); 5]);
                    events.extend(if scene == "in-use" { vec![Vec::new(); 3] } else { vec![press; 3] });
                    events.extend(vec![Vec::new(); 3]);
                    let image = super::super::snapshot::snapshot_with_events(&mut controls,
                        &format!("dictation-download-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&controls.app.settings).unwrap(), original);
                    assert_eq!(controls.app.last_dictation, "Previous words café\nSecond line.");
                    assert!(!controls.copied && controls.app.dirty_since.is_none() && !controls.app.dictation.busy());
                    assert_eq!(controls.app.dictation.download_progress().is_some(), matches!(scene, "downloading" | "cancelling"));
                    assert_eq!(controls.app.dictation.download_cancelling(), scene == "cancelling");
                    assert_eq!(controls.app.dictating.is_some(), scene == "in-use");
                    assert_eq!(model.is_downloaded(), matches!(scene, "ready" | "in-use"));
                    drop(held_file);
                    model.remove().unwrap();
                }
            }
        }
    }

    #[test]
    fn dictation_cancellation_keeps_focus_without_replaying_the_key() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for transcript in ["Previous words café\nSecond line.", ""] {
                        for scene in ["cancel", "cancel listening", "cancel finishing", "discard listening", "discard finishing",
                            "discard listening busy", "discard finishing busy"] {
                            for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                                let (dictation, release) = Dictation::delayed_for_test();
                                let mut app = App::stopped(Default::default());
                                app.page = Page::Dictation;
                                app.dictation = dictation;
                                app.last_dictation = transcript.into();
                                let cancel = scene.starts_with("cancel");
                                let busy = cancel || scene.ends_with("busy");
                                if busy {
                                    let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
                                    app.dictation.transcribe(app.settings.speech_model, take);
                                }
                                let writer = if scene.contains("finishing") {
                                    let (mut recorder, writer) = Recorder::delayed_writer_for_test(&[0.25; 96]);
                                    recorder.stop_capture();
                                    app.dictating = Some(DictationRecording { recorder, model: app.settings.speech_model, hold_to_talk: false });
                                    Some(writer)
                                } else {
                                    if scene != "cancel" {
                                        app.dictating = Some(DictationRecording {
                                            recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: app.settings.speech_model, hold_to_talk: false,
                                        });
                                    }
                                    None
                                };
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                let label = if cancel { "Cancel transcription" } else { "Discard recording" };
                                let pointer = activation.contains("pointer");
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                                    node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                                if activation != "pointer" {
                                    draw_size(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                                } else {
                                    draw_size(&mut app, &ctx, size, vec![
                                        egui::Event::PointerMoved(egui::pos2(size.x - 20.0, size.y - 60.0)),
                                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -2000.0),
                                            modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
                                    ]);
                                }
                                for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                let held_events = || {
                                    let mut events = vec![key(held, true)];
                                    if held == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
                                    events
                                };
                                let mut commands = Vec::new();
                                if pointer {
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                        .find(|(id, _)| *id == target).unwrap().1;
                                    let bounds = node.bounds().unwrap();
                                    let pos = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
                                    assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos));
                                    for pressed in [true, false] {
                                        commands.extend(draw_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos),
                                            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
                                        ]).platform_output.commands);
                                    }
                                } else {
                                    let events = if activation == "accessible" { vec![action(Action::Click, target)] } else { held_events() };
                                    commands.extend(draw_size(&mut app, &ctx, size, events).platform_output.commands);
                                }
                                for _ in 0..6 { commands.extend(draw_size(&mut app, &ctx, size, Vec::new()).platform_output.commands); }
                                let visible = |output: &egui::FullOutput, target| {
                                    let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                        .find(|(id, _)| *id == target).unwrap().1;
                                    let bounds = node.bounds().unwrap();
                                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect) && match &shape.shape {
                                        egui::Shape::Rect(shape) => shape.rect.intersects(rect),
                                        egui::Shape::Text(shape) => shape.visual_bounding_rect().intersects(rect), _ => false,
                                    }), "focused Dictation control is visible: {scene}, {activation}, zoom={zoom}, passes={passes}");
                                };
                                if cancel {
                                    assert!(app.dictation.busy() && app.dictation.transcription_cancelling());
                                    assert_eq!(app.dictating.is_some(), scene != "cancel", "cancelling transcription keeps the new recording");
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    let (status, node) = tree.nodes.iter().find(|(_, node)| node.role() == Role::Label
                                        && node.value() == Some("Cancelling transcription…")).unwrap();
                                    if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                    else {
                                        assert_eq!(tree.focus, *status, "cancellation keeps focus: {scene}, {activation}, zoom={zoom}, passes={passes}");
                                        assert!(!node.is_disabled() && node.supports_action(Action::Focus));
                                        visible(&output, *status);
                                    }
                                    release.send(()).unwrap();
                                    let deadline = Instant::now() + Duration::from_secs(3);
                                    while app.dictation.busy() || app.dictation_status.0 != "Transcription cancelled" {
                                        assert!(Instant::now() < deadline, "the cancelled transcription finishes");
                                        app.poll_dictation();
                                        std::thread::sleep(Duration::from_millis(1));
                                    }
                                } else {
                                    assert!(app.dictating.is_none() && !app.dictation.transcription_cancelling());
                                    assert_eq!(app.dictation.busy(), busy, "discarding a recording keeps previous transcription");
                                    app.dictation.cancel_transcription();
                                    release.send(()).unwrap();
                                }
                                for _ in 0..6 { commands.extend(draw_size(&mut app, &ctx, size, Vec::new()).platform_output.commands); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                let target = *tree.nodes.iter().find(|(_, node)| if transcript.is_empty() { node.role() == Role::ComboBox }
                                    else { node.role() == Role::Button && node.label() == Some("Copy") }).map(|(id, _)| id).unwrap();
                                if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                else {
                                    assert_eq!(tree.focus, target, "completion restores focus: {scene}, {activation}, zoom={zoom}, passes={passes}");
                                    visible(&output, target);
                                }
                                if matches!(activation, "enter" | "space") {
                                    let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                    assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)), "guard held activation before replay");
                                    for _ in 0..3 { commands.extend(draw_size(&mut app, &ctx, size, held_events()).platform_output.commands); }
                                    assert!(!egui::Popup::is_any_open(&ctx), "the held cancellation key leaves the model selector closed");
                                    draw_size(&mut app, &ctx, size, vec![key(held, false)]);
                                }
                                assert!(!commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(_))),
                                    "the cancellation key must not copy the previous transcript");
                                if !pointer && !transcript.is_empty() {
                                    let mut copied = Vec::new();
                                    for pressed in [true, false] {
                                        copied.extend(draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, pressed)]).platform_output.commands);
                                    }
                                    for _ in 0..3 { copied.extend(draw_size(&mut app, &ctx, size, Vec::new()).platform_output.commands); }
                                    assert_eq!(copied.iter().filter_map(|command| match command {
                                        egui::OutputCommand::CopyText(text) => Some(text.as_str()), _ => None,
                                    }).collect::<Vec<_>>(), [transcript], "a fresh Enter copies the transcript once");
                                } else if !pointer {
                                    draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    assert!(egui::Popup::is_any_open(&ctx), "a fresh Enter opens the model selector");
                                    draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    assert!(!egui::Popup::is_any_open(&ctx));
                                }
                                assert_eq!(app.last_dictation, transcript);
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none() && app.dictation.download_progress().is_none() && app.playing.is_empty());
                                drop(writer);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn transcription_completion_preserves_navigation_and_new_recordings() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for transcript in ["Previous words café\nSecond line.", ""] {
                        for early in [true, false] {
                            for mode in ["unchanged", "focus moved", "page switched", "result focus queued", "result page queued",
                                "result focus cleared", "model focus moved", "recording starts", "recording discarded"] {
                                let (mut app, release) = pending_dictation();
                                app.last_dictation = transcript.into();
                                if mode != "recording discarded" { app.dictating = None; }
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                let button = |output: &egui::FullOutput, label: &str| *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                                for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let cancel = button(&output, "Cancel transcription");
                                let model = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                                let switched = mode.contains("page");
                                let other = if mode == "model focus moved" { model } else { button(&output, if switched { "Soundboard" } else { "Mute" }) };
                                let move_focus = |app: &mut App| {
                                    let mut events = vec![action(Action::Focus, other)];
                                    if switched { events.push(action(Action::Click, other)); }
                                    draw_size(app, &ctx, size, events);
                                    for _ in 0..3 { draw_size(app, &ctx, size, Vec::new()); }
                                };
                                draw_size(&mut app, &ctx, size, vec![action(Action::Focus, cancel)]);
                                for _ in 0..4 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                draw_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                assert!(app.dictation.transcription_cancelling());
                                if !early {
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                }
                                match mode {
                                    "focus moved" | "page switched" | "model focus moved" => move_focus(&mut app),
                                    "recording starts" => app.dictating = Some(DictationRecording {
                                        recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: app.settings.speech_model, hold_to_talk: false,
                                    }),
                                    "recording discarded" => app.discard_dictation(),
                                    _ => {}
                                }
                                if !early && mode.starts_with("recording") {
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    let (status, node) = tree.nodes.iter().find(|(_, node)| node.role() == Role::Label
                                        && node.value() == Some("Cancelling transcription…")).unwrap();
                                    assert_eq!(tree.focus, *status, "a recording change preserves cancellation focus");
                                    let bounds = node.bounds().unwrap();
                                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                        && matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Cancelling transcription…")),
                                        "cancellation focus remains visible when {mode}; zoom={zoom}, passes={passes}");
                                    draw_size(&mut app, &ctx, size, vec![
                                        egui::Event::PointerMoved(egui::pos2(size.x - 20.0, size.y - 60.0)),
                                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 2000.0),
                                            modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
                                    ]);
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, *status);
                                    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                                        if text.galley.text() == "Model" && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                                        "focus still permits scrolling back to the model selector after reflow");
                                }
                                release.send(()).unwrap();
                                let deadline = Instant::now() + Duration::from_secs(3);
                                while app.dictation.busy() || app.dictation_status.0 != "Transcription cancelled" {
                                    app.poll_dictation();
                                    assert!(Instant::now() < deadline, "the cancelled worker finishes");
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                                if mode.starts_with("result") {
                                    if mode == "result focus cleared" {
                                        draw_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    } else { move_focus(&mut app); }
                                }
                                for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                let output = draw_size(&mut app, &ctx, size, Vec::new());
                                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                if mode == "result focus cleared" {
                                    assert!(ctx.memory(|memory| memory.focused()).is_none(), "completion respects Escape");
                                } else {
                                    let expected = if matches!(mode, "unchanged" | "recording starts" | "recording discarded") {
                                        if transcript.is_empty() { model } else { button(&output, "Copy") }
                                    } else { other };
                                    assert_eq!(tree.focus, expected, "completion respects focus: {mode}, early={early}, zoom={zoom}, passes={passes}");
                                }
                                if switched {
                                    assert_eq!(app.page, Page::Soundboard);
                                    let dictation = button(&output, "Dictation");
                                    draw_size(&mut app, &ctx, size, vec![action(Action::Focus, dictation), action(Action::Click, dictation)]);
                                    for _ in 0..6 { draw_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                                    assert_eq!(app.page, Page::Dictation);
                                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, dictation,
                                        "returning to Dictation keeps focus on its navigation button");
                                }
                                assert_eq!(app.dictating.is_some(), mode == "recording starts");
                                assert_eq!(app.last_dictation, transcript);
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(!app.dictation.busy() && app.dictation.download_progress().is_none() && app.dirty_since.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes Dictation cancellation focus screenshots; needs Python with Pillow"]
    fn snapshot_dictation_cancellation_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        struct CancelledDictation {
            app: App,
            release: Option<crossbeam_channel::Sender<()>>,
            complete: bool,
            new_take: bool,
            copied: bool,
        }
        impl eframe::App for CancelledDictation {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if ui.ctx().cumulative_frame_nr() >= 16 {
                    if self.complete && let Some(release) = self.release.take() {
                        release.send(()).unwrap();
                        let deadline = Instant::now() + Duration::from_secs(3);
                        while self.app.dictation.busy() || self.app.dictation_status.0 != "Transcription cancelled" {
                            self.app.poll_dictation();
                            assert!(Instant::now() < deadline, "the cancelled snapshot job finishes");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                    if self.new_take {
                        self.app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: self.app.settings.speech_model, hold_to_talk: false,
                        });
                        self.new_take = false;
                    }
                }
                eframe::App::ui(&mut self.app, ui, frame);
                self.copied |= ui.ctx().output(|output| output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(_))));
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["discard-copy", "discard-model", "cancelling", "cancelled-copy", "cancelled-model", "new-take"] {
                    let (dictation, release) = Dictation::delayed_for_test();
                    let mut app = App::stopped(Default::default());
                    app.dictation = dictation;
                    app.page = Page::Dictation;
                    if !scene.ends_with("model") { app.last_dictation = "Previous words café\nSecond line.".into(); }
                    let transcript = app.last_dictation.clone();
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let discard = scene.starts_with("discard");
                    if discard {
                        app.dictating = Some(DictationRecording {
                            recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: app.settings.speech_model, hold_to_talk: false,
                        });
                    } else {
                        let take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().unwrap();
                        app.dictation.transcribe(app.settings.speech_model, take);
                    }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw_size(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_size(&mut app, &ctx, size, Vec::new());
                    let label = if discard { "Discard recording" } else { "Cancel transcription" };
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let mut cancelled = CancelledDictation { app, release: Some(release), complete: scene.starts_with("cancelled"),
                        new_take: scene == "new-take", copied: false };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    events.extend(vec![Vec::new(); 6]);
                    let press = vec![egui::Event::Key { key: egui::Key::Space, physical_key: Some(egui::Key::Space),
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }, egui::Event::Text(" ".into())];
                    events.push(press.clone());
                    events.extend(vec![Vec::new(); 5]);
                    events.extend(vec![press; 3]);
                    events.extend(vec![Vec::new(); 3]);
                    let image = super::super::snapshot::snapshot_with_events(&mut cancelled,
                        &format!("dictation-focus-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(cancelled.app.last_dictation, transcript);
                    assert_eq!(serde_json::to_value(&cancelled.app.settings).unwrap(), original);
                    assert!(!cancelled.copied && cancelled.app.dirty_since.is_none() && cancelled.app.dictation.download_progress().is_none());
                    assert_eq!(cancelled.app.dictating.is_some(), scene == "new-take");
                    if matches!(scene, "cancelling" | "new-take") {
                        assert!(cancelled.app.dictation.busy() && cancelled.app.dictation.transcription_cancelling());
                    } else { assert!(!cancelled.app.dictation.busy()); }
                }
            }
        }
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
                    for label in [startup_label(), "Start processing automatically", "Minimize to the strip",
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
