//! Voice page: first-run checklist, routing (which devices your voice
//! comes from and goes to), presets and the mic test, and each setting as a
//! row that shows what it does.

use std::time::Duration;

use eframe::egui::{self, ComboBox, RichText};

#[cfg(windows)]
use super::routes::VB_CABLE_URL;
use super::routes::cable_input;
use super::{AMBER, App, CYAN, GREEN, MUTED, VIOLET};
use crate::config;
use crate::denoise::ModelState;
use crate::engine::Engine;
use crate::viz;
use crate::widgets;

/// Platform words for the virtual-cable setup. Linux has no download; it
/// creates and names its own microphone instead.
#[cfg(windows)]
mod text {
    pub const CABLE_STEP: &str = "Set up VB-Cable";
    pub const CABLE_ACTION: &str = "Get VB-Cable";
    pub const CABLE_ACTION_HINT: Option<&str> = Some("Run the installer as administrator");
    pub const CABLE_STEP_HELP: Option<&str> = Some("If installed, enable CABLE Input in Windows");
    pub const CABLE_MISSING: &str = "VB-Cable input isn't available.";
    pub const CABLE_INSTALL_HELP: Option<&str> = Some(
        "If installed, enable CABLE Input in Windows. Otherwise, run VB-Cable's setup as administrator.",
    );
    pub const CABLE_USE: &str = "Use VB-Cable";
    pub const CABLE_OTHER_HELP: Option<&str> =
        Some("For another virtual output, choose its matching input in Discord.");
    pub const DEFAULT_MIC: &str = "Make CABLE Output my Windows default mic while running";
    pub const DEFAULT_MIC_HELP: bool = true;
    pub const DEFAULT_MIC_ON: &str = "In Discord: leave Voice & Video > Input Device on Default";
    pub const DEFAULT_MIC_OFF: &str = "In Discord: Voice & Video > Input Device > CABLE Output";
    pub const BADGE_READY: &str = "VB-Cable connected";
    pub const BADGE_MISSING: &str = "VB-Cable unavailable";
    pub const DISCORD_MIC: &str = "In Discord, set Voice & Video > Input Device to CABLE Output";
}

#[cfg(not(windows))]
mod text {
    pub const CABLE_STEP: &str = "Set up OpenMic Microphone";
    pub const CABLE_ACTION: &str = "Create virtual mic";
    pub const CABLE_ACTION_HINT: Option<&str> = None;
    pub const CABLE_STEP_HELP: Option<&str> = None;
    pub const CABLE_MISSING: &str = "OpenMic Microphone isn't available.";
    pub const CABLE_INSTALL_HELP: Option<&str> = None;
    pub const CABLE_USE: &str = "Use OpenMic Virtual Cable";
    pub const CABLE_OTHER_HELP: Option<&str> = None;
    pub const DEFAULT_MIC: &str = "Make OpenMic Microphone my default mic while running";
    pub const DEFAULT_MIC_HELP: bool = false;
    pub const DEFAULT_MIC_ON: &str = "In Discord: leave Voice & Video > Input Device on Default";
    pub const DEFAULT_MIC_OFF: &str =
        "In Discord: Voice & Video > Input Device > OpenMic Microphone";
    pub const BADGE_READY: &str = "OpenMic Microphone ready";
    pub const BADGE_MISSING: &str = "OpenMic Microphone unavailable";
    pub const DISCORD_MIC: &str =
        "In Discord, set Voice & Video > Input Device to OpenMic Microphone";
}

/// A first-run checklist step: whether it's done, its title, and what it
/// asks the user to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Cable,
    Microphone,
    Discord,
    Test,
}

impl App {
    pub(super) fn draw_mic_tab(&mut self, ui: &mut egui::Ui) {
        if self.show_checklist() {
            self.draw_checklist(ui);
            ui.add_space(8.0);
        }
        self.draw_routing(ui);
        ui.add_space(8.0);
        self.draw_presets(ui);
        ui.add_space(8.0);
        self.draw_voice_rows(ui);
    }

    // ---- first-run checklist ----------------------------------------------

    /// Which steps are done. VB-Cable and the microphone are checked live;
    /// Discord's setting can't be seen from here, so the user ticks it.
    pub(super) fn steps(&self) -> [(Step, bool); 4] {
        let s = &self.settings;
        let mic = !s.microphone.is_empty() && self.inputs.contains(&s.microphone);
        [
            (Step::Cable, cable_input(&self.outputs).is_some()),
            (Step::Microphone, mic),
            (Step::Discord, s.setup.discord),
            (Step::Test, s.setup.tested),
        ]
    }

    pub(super) fn show_checklist(&self) -> bool {
        let done = self.steps().iter().all(|(_, done)| *done);
        !done && !self.settings.setup.dismissed
    }

    fn draw_checklist(&mut self, ui: &mut egui::Ui) {
        let steps = self.steps();
        let done = steps.iter().filter(|(_, done)| *done).count();
        let mut hide = false;
        widgets::card(
            ui,
            "GET SET UP",
            VIOLET,
            |ui| {
                hide = ui
                    .small_button("Hide")
                    .on_hover_text("Put the checklist away")
                    .clicked();
                widgets::badge(
                    ui,
                    &format!("{done} of {}", steps.len()),
                    if done == steps.len() { GREEN } else { VIOLET },
                );
            },
            |ui| {
                // The first step not yet done is the one to do now.
                let current = steps.iter().position(|(_, done)| !done);
                for (i, (step, done)) in steps.into_iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        step_mark(ui, i + 1, done);
                        let active = current == Some(i);
                        let title = RichText::new(step_title(step, self.settings.default_mic));
                        ui.label(if done {
                            title.weak()
                        } else if active {
                            title.strong()
                        } else {
                            title
                        });
                        if !done {
                            self.step_action(ui, step);
                        }
                    });
                }
            },
        );
        if hide {
            self.settings.setup.dismissed = true;
            self.touch();
        }
    }

    fn step_action(&mut self, ui: &mut egui::Ui, step: Step) {
        match step {
            Step::Cable => {
                self.cable_action_button(ui, text::CABLE_ACTION, text::CABLE_ACTION_HINT, true);
                if let Some(help) = text::CABLE_STEP_HELP {
                    ui.weak(help);
                }
            }
            Step::Microphone => {
                ui.weak("pick it under Routing below");
            }
            Step::Discord => {
                if ui
                    .small_button("Done")
                    .on_hover_text("Discord > User Settings > Voice & Video")
                    .clicked()
                {
                    self.settings.setup.discord = true;
                    self.touch();
                }
            }
            Step::Test => {
                if !self.mic_test.busy() && ui.small_button("Test").clicked() {
                    self.start_mic_test();
                }
            }
        }
    }

    /// "Get VB-Cable" on Windows, the built-in virtual microphone on Linux.
    fn cable_action_button(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        hover: Option<&str>,
        small: bool,
    ) -> egui::Response {
        let button = if small {
            ui.small_button(label)
        } else {
            ui.button(label)
        };
        let button = match hover {
            Some(hover) => button.on_hover_text(hover),
            None => button,
        };
        if button.clicked() {
            #[cfg(windows)]
            super::open_url(ui.ctx(), VB_CABLE_URL);
            #[cfg(not(windows))]
            self.create_virtual_mic();
        }
        button
    }

    #[cfg(not(windows))]
    fn create_virtual_mic(&mut self) {
        match crate::virtual_mic::activate() {
            Ok(()) => self.refresh_devices(true),
            Err(e) => {
                crate::logfile::warn(format_args!("virtual microphone not created: {e:#}"));
                self.status = (super::short(&format!("Virtual mic: {e:#}")), super::RED);
            }
        }
    }

    // ---- routing -------------------------------------------------------------

    /// Guide the VB-Cable route, or explain how a custom virtual output reaches
    /// Discord. Device enumeration cannot tell whether a driver is installed.
    fn draw_cable_hint(&mut self, ui: &mut egui::Ui) {
        let cable = cable_input(&self.outputs).cloned();
        let tone = match &cable {
            Some(c) if self.settings.output == *c => GREEN,
            _ => AMBER,
        };
        if cable.is_none() && self.show_checklist() {
            return;
        }
        egui::Frame::new()
            .fill(tone.gamma_multiply(0.08))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                match cable {
                    None => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(text::CABLE_MISSING)
                                    .color(widgets::text_color(ui, AMBER)),
                            );
                            self.cable_action_button(ui, text::CABLE_ACTION, None, false);
                        });
                        if let Some(help) = text::CABLE_INSTALL_HELP {
                            ui.weak(help);
                        }
                    }
                    Some(cable) if self.settings.output != cable => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("A different processed output is selected.").color(widgets::text_color(ui, AMBER)));
                            if ui.button(text::CABLE_USE).clicked() {
                                self.settings.output = cable;
                                self.touch();
                                if self.running() {
                                    self.restart_engine();
                                }
                            }
                        });
                        if let Some(help) = text::CABLE_OTHER_HELP {
                            ui.weak(help);
                        }
                    }
                    Some(_) => {
                        let changed = ui
                            .checkbox(
                                &mut self.settings.default_mic,
                                text::DEFAULT_MIC,
                            )
                            .on_hover_text(
                                "Apps set to the default microphone (Discord's default) hear your \
                                 cleaned voice. Your previous default comes back when OpenMic stops.",
                            )
                            .changed();
                        if changed {
                            self.touch();
                        }
                        if text::DEFAULT_MIC_HELP {
                            ui.weak(if self.settings.default_mic {
                                text::DEFAULT_MIC_ON
                            } else {
                                text::DEFAULT_MIC_OFF
                            });
                        }
                    }
                }
            });
    }

    fn draw_routing(&mut self, ui: &mut egui::Ui) {
        let cable = cable_input(&self.outputs).is_some();
        let mut refresh = false;
        widgets::card(
            ui,
            "ROUTING",
            CYAN,
            |ui| {
                refresh = ui.small_button("Refresh").clicked();
                if cable {
                    widgets::badge(ui, text::BADGE_READY, GREEN);
                } else {
                    widgets::badge(ui, text::BADGE_MISSING, AMBER);
                }
            },
            |ui| {
                let mut changed = false;
                egui::Grid::new("routing")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for (label, id, field, pool) in [
                            ("Microphone", 0, &mut self.settings.microphone, &self.inputs),
                            (
                                "Processed output",
                                1,
                                &mut self.settings.output,
                                &self.outputs,
                            ),
                            (
                                "Monitor output",
                                2,
                                &mut self.settings.monitor_output,
                                &self.outputs,
                            ),
                        ] {
                            let label = ui.label(RichText::new(label).weak());
                            changed |= combo(ui, id, field, pool).labelled_by(label.id).changed();
                            ui.end_row();
                        }
                    });
                if changed {
                    self.touch();
                    if self.running() {
                        self.restart_engine();
                    }
                }
                ui.add_space(4.0);
                self.draw_cable_hint(ui);
            },
        );
        if refresh {
            self.refresh_devices(true);
        }
    }

    // ---- voice settings --------------------------------------------------------

    /// Presets as one-click chips, with Bypass and the mic test beside them.
    fn draw_presets(&mut self, ui: &mut egui::Ui) {
        let current = self.settings.processing();
        let builtins = config::builtin_presets();
        let mut apply = None;
        let mut delete = None;
        let mut save = false;
        let mut controls = Vec::new();
        let mut save_button = None;
        let reveal_id = egui::Id::new("voice-preset-reveal");
        let reveal = ui
            .data_mut(|data| data.remove_temp::<bool>(reveal_id))
            .unwrap_or(false);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Preset").weak());
            for preset in &builtins {
                let chip = preset_chip(ui, preset, &current, false, 0);
                if chip.clicked() {
                    apply = Some(preset.processing.clone());
                }
                controls.push(chip);
            }
            let mut occurrences = std::collections::HashMap::new();
            for (i, preset) in self.settings.presets.iter().enumerate() {
                let occurrence = occurrences.entry(preset.name.as_str()).or_insert(0);
                let chip = preset_chip(ui, preset, &current, true, *occurrence);
                *occurrence += 1;
                if chip.clicked() {
                    apply = Some(preset.processing.clone());
                }
                let menu_id = egui::Popup::default_response_id(&chip);
                let keyboard_id = menu_id.with("keyboard");
                let keyboard = chip.has_focus() && ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F10));
                if keyboard {
                    egui::Popup::open_id(ui.ctx(), menu_id);
                    ui.data_mut(|data| data.insert_temp(keyboard_id, true));
                } else if chip.secondary_clicked() {
                    ui.data_mut(|data| data.remove::<bool>(keyboard_id));
                }
                let mut popup = egui::Popup::context_menu(&chip);
                if ui.data(|data| data.get_temp::<bool>(keyboard_id)).unwrap_or(false) {
                    popup = popup.anchor(&chip);
                }
                let menu = popup.show(|ui| {
                    let remove = ui.button("Delete preset");
                    if keyboard { remove.request_focus(); }
                    if remove.clicked() {
                        delete = Some(i);
                        ui.close();
                    }
                });
                if !egui::Popup::is_id_open(ui.ctx(), menu_id) {
                    let keyboard = ui.data_mut(|data| {
                        let keyboard = data.get_temp::<bool>(keyboard_id).unwrap_or(false);
                        data.remove::<bool>(keyboard_id);
                        keyboard
                    });
                    if keyboard && menu.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                        chip.request_focus();
                    }
                }
                controls.push(chip);
            }
            let open = ui.allocate_ui_with_layout(egui::vec2(22.0, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center), |ui|
                ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("save-current-preset")), |ui| ui.button("+")).inner).inner
                .on_hover_text("Save your current settings as a preset. Right-click or Shift+F10 on a saved preset to delete it.");
            open.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Save preset"));
            let menu_id = egui::Popup::default_response_id(&open);
            let opening = open.clicked() && !egui::Popup::is_id_open(ui.ctx(), menu_id);
            if opening { widgets::finish_keyboard_activation(ui.ctx()); }
            let menu = egui::Popup::menu(&open).width(240.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.label("Save your settings as a preset");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.preset_name).id_salt("preset-name").hint_text("Preset name").desired_width(160.0),
                    );
                    if opening { field.request_focus(); }
                    let named = !self.preset_name.trim().is_empty();
                    let enter = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    save = ui.add_enabled(named, egui::Button::new("Save")).clicked()
                        || (named && enter);
                    if enter && !named { field.request_focus(); }
                    if save {
                        ui.close();
                    }
                });
            if menu.is_some() && !egui::Popup::is_id_open(ui.ctx(), menu_id)
                && (save || ui.input(|input| input.key_pressed(egui::Key::Escape)))
            {
                open.request_focus();
            }
            controls.push(open.clone());
            save_button = Some(open);
        });
        if let Some(i) = delete {
            self.settings.presets.remove(i);
            self.touch();
            if let Some(button) = &save_button {
                button.request_focus();
            }
        }
        if save {
            let name = self.preset_name.trim().to_owned();
            let preset = config::Preset {
                name: name.clone(),
                processing: current,
            };
            match self
                .settings
                .presets
                .iter_mut()
                .find(|p| p.name.eq_ignore_ascii_case(&name))
            {
                Some(existing) => *existing = preset,
                None => self.settings.presets.push(preset),
            }
            self.preset_name.clear();
            self.touch();
        }
        if let Some(processing) = apply {
            self.settings.apply_processing(&processing);
            self.apply_live();
        }
        if save || delete.is_some() {
            widgets::finish_keyboard_activation(ui.ctx());
            // The next frame lays out the edited preset list and its save control.
            let keyboard =
                ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released());
            ui.data_mut(|data| data.insert_temp(reveal_id, keyboard));
        }
        if reveal
            && let Some(button) = &save_button
            && button.has_focus()
            && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
        {
            button.scroll_to_me(Some(egui::Align::Center));
        }
        for control in &controls {
            widgets::scroll_on_focus(control);
        }
        ui.add_space(4.0);
        ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("mic-test-controls")), |ui| {
            ui.horizontal_wrapped(|ui| {
                let bypass = widgets::pill(ui, &mut self.settings.bypass, "Bypass", MUTED)
                    .on_hover_text("Skip noise reduction and both gates. Mic level, rumble filter, output level and mute still apply.");
                widgets::scroll_on_focus(&bypass);
                if bypass.changed() {
                    self.apply_live();
                }
                self.draw_mic_test(ui);
            });
        });
    }

    /// The settings, one row each, in the order your voice passes through them.
    fn draw_voice_rows(&mut self, ui: &mut egui::Ui) {
        #[allow(unused_mut)]
        let mut frames = self.engine.as_ref().map(Engine::scope).unwrap_or_default();
        #[cfg(test)]
        if frames.is_empty() {
            frames = self.demo_frames.clone();
        }
        let bypassed = self.settings.bypass;
        let mut changed = viz::Changed::default();
        widgets::card(
            ui,
            "YOUR VOICE, STEP BY STEP",
            CYAN,
            |ui| {
                let hint = "drag or scroll · Tab, then arrows · double-click to reset";
                ui.add(egui::Label::new(RichText::new(hint).weak()).truncate())
                    .on_hover_text(hint);
            },
            |ui| {
                if bypassed {
                    ui.colored_label(
                        widgets::text_color(ui, AMBER),
                        "Bypass is on: noise reduction and both gates are skipped.",
                    )
                    .on_hover_text("Mic level, rumble filter, output level and mute still apply.");
                }
                // Keep native switch and link IDs stable when other page sections change.
                changed = ui
                    .scope_builder(
                        egui::UiBuilder::new().id(egui::Id::new("voice-settings")),
                        |ui| {
                            viz::voice_rows(
                                ui,
                                &frames,
                                &mut self.settings,
                                &mut self.strength_before_off,
                            )
                        },
                    )
                    .inner;
            },
        );
        if changed.settings {
            self.apply_live();
        }
        // The monitor device wasn't connected when processing started: open it now.
        if changed.monitor
            && self.settings.monitor
            && self.engine.as_ref().is_some_and(|e| !e.has_monitor())
        {
            self.restart_engine();
        }
    }

    /// The footer's "Running · voice 42%", unless a warning is showing.
    pub(super) fn update_running_status(&mut self) {
        let Some(stats) = self.engine.as_ref().map(Engine::stats) else {
            return;
        };
        let warning_fresh = self
            .warn_until
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(2));
        if warning_fresh {
            return;
        }
        let voice = format!("voice {:.0}%", stats.prob * 100.0);
        self.status = match stats.model {
            ModelState::DeepFilterLoading => (format!("Loading DeepFilterNet… · {voice}"), AMBER),
            ModelState::DeepFilterFailed => (
                format!("DeepFilterNet unavailable, using RNNoise · {voice}"),
                AMBER,
            ),
            ModelState::DeepFilter | ModelState::Rnnoise => (format!("Running · {voice}"), GREEN),
        };
    }
}

pub(super) fn combo(
    ui: &mut egui::Ui,
    id: usize,
    value: &mut String,
    pool: &[String],
) -> egui::Response {
    let mut changed = false;
    let mut selected = false;
    let mut response = ComboBox::from_id_salt(id)
        .width(ui.available_width().min(360.0))
        .selected_text(value.as_str())
        .truncate()
        .show_ui(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for name in pool {
                let choice = ui
                    .selectable_value(value, name.clone(), name)
                    .on_hover_text(name);
                widgets::scroll_on_focus(&choice);
                changed |= choice.changed();
                if choice.clicked() {
                    selected = true;
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(value.as_str());
    if selected {
        response.request_focus();
    }
    widgets::scroll_on_focus(&response);
    if changed {
        response.mark_changed();
    }
    response
}

/// A checklist step's marker: its number in a ring, or a check in a
/// filled circle once done. Drawn, because egui's fonts have no check mark.
fn step_mark(ui: &mut egui::Ui, number: usize, done: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let center = rect.center();
    let painter = ui.painter();
    if done {
        painter.circle_filled(center, 8.0, GREEN);
        let tick = [
            center + egui::vec2(-4.0, 0.0),
            center + egui::vec2(-1.0, 3.0),
            center + egui::vec2(4.5, -3.5),
        ];
        painter.add(egui::Shape::line(
            tick.to_vec(),
            egui::Stroke::new(2.0, egui::Color32::from_gray(18)),
        ));
    } else {
        painter.circle_stroke(center, 8.0, egui::Stroke::new(1.0, MUTED));
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            number.to_string(),
            egui::FontId::proportional(11.0),
            widgets::text_color(ui, MUTED),
        );
    }
}

fn preset_chip(
    ui: &mut egui::Ui,
    preset: &config::Preset,
    current: &config::Processing,
    saved: bool,
    occurrence: usize,
) -> egui::Response {
    let selected = preset.processing == *current;
    let id = egui::Id::new(("voice-preset", saved, &preset.name, occurrence));
    let mut size = ui
        .painter()
        .layout_no_wrap(
            preset.name.clone(),
            egui::TextStyle::Button.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        + ui.spacing().button_padding * 2.0;
    size.x = size.x.min(ui.max_rect().width());
    size.y = size.y.max(ui.spacing().interact_size.y);
    let chip = ui
        .allocate_ui_with_layout(
            size,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.scope_builder(egui::UiBuilder::new().id(id), |ui| {
                    ui.add(
                        egui::Button::selectable(selected, &preset.name)
                            .truncate()
                            .min_size(size),
                    )
                })
                .inner
            },
        )
        .inner
        .on_hover_text(&preset.name);
    chip.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            format!(
                "Apply {}preset: {}",
                if saved { "saved " } else { "" },
                preset.name
            ),
        )
    });
    chip
}

fn step_title(step: Step, default_mic: bool) -> &'static str {
    match step {
        Step::Cable => text::CABLE_STEP,
        Step::Microphone => "Choose your microphone",
        Step::Discord if default_mic => "In Discord, set Voice & Video > Input Device to Default",
        Step::Discord => text::DISCORD_MIC,
        Step::Test => "Test your mic: hear yourself raw and cleaned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_settings_heading_and_keyboard_hint_fit_at_zoom() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let hint = "drag or scroll · Tab, then arrows · double-click to reset";
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (route, dark) in [
                ("missing", true),
                ("missing", false),
                ("connected", true),
                ("connected", false),
                ("custom", true),
                ("custom", false),
            ] {
                for passes in [1, 2] {
                    let mut app = App::stopped(config::Settings::default());
                    app.settings.setup.dismissed = true;
                    if route != "missing" {
                        let cable = "CABLE Input (VB-Audio Virtual Cable)".to_owned();
                        app.settings.output = if route == "connected" {
                            cable.clone()
                        } else {
                            "Headphones".into()
                        };
                        app.outputs = vec![cable, "Headphones".into()];
                    }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| {
                        options.max_passes = passes.try_into().unwrap();
                        options.zoom_factor = zoom;
                    });
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let target_node = *output
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::CheckBox && node.label() == Some("Bypass")
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node,
                            data: None,
                        })],
                    );
                    for _ in 0..8 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let text_rect = |label| {
                        output
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::Text(text) if text.galley.text() == label => {
                                    Some((text.visual_bounding_rect(), shape.clip_rect))
                                }
                                _ => None,
                            })
                            .unwrap()
                    };
                    let (heading, heading_clip) = text_rect("YOUR VOICE, STEP BY STEP");
                    let (hint, hint_clip) = text_rect(hint);
                    assert!(
                        !heading.intersects(hint),
                        "heading and hint overlap at {zoom}: {heading:?}, {hint:?}"
                    );
                    for (rect, clip) in [(heading, heading_clip), (hint, hint_clip)] {
                        assert!(
                            rect.left() >= clip.left() && rect.right() <= clip.right(),
                            "header fits at {zoom}: {rect:?}, {clip:?}"
                        );
                    }
                    assert!(app.dirty_since.is_none());
                }
            }
        }
    }

    fn draw_voice(
        app: &mut App,
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2),
                events,
                ..Default::default()
            },
            |ui| app.ui(ui, &mut frame),
        )
    }

    fn routing_app(long_names: bool) -> App {
        let mut app = App::stopped(config::Settings::default());
        app.settings.setup.dismissed = true;
        let suffix = if long_names {
            " with a long manufacturer and device description".repeat(3)
        } else {
            String::new()
        };
        app.settings.microphone = format!("USB microphone{suffix}");
        app.settings.output = format!("CABLE Input (VB-Audio Virtual Cable){suffix}");
        app.settings.monitor_output = format!("Headphones{suffix}");
        app.inputs = vec![app.settings.microphone.clone()];
        app.outputs = vec![
            app.settings.output.clone(),
            app.settings.monitor_output.clone(),
        ];
        app
    }

    #[test]
    fn routing_selectors_fit_and_have_named_purposes_at_zoom() {
        use egui::accesskit::Role;
        for dark in [true, false] {
            for passes in [1, 2] {
                for long_names in [false, true] {
                    let mut app = routing_app(long_names);
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for zoom in [1.0, 1.25, 1.5, 1.0] {
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        assert_eq!(
                            tree.nodes
                                .iter()
                                .filter(|(_, node)| node.role() == Role::ComboBox)
                                .count(),
                            3
                        );
                        for (purpose, value) in [
                            ("Microphone", &app.settings.microphone),
                            ("Processed output", &app.settings.output),
                            ("Monitor output", &app.settings.monitor_output),
                        ] {
                            let (_, node) = tree
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == Role::ComboBox
                                        && node.value() == Some(value.as_str())
                                })
                                .unwrap();
                            let bounds = node.bounds().unwrap();
                            assert!(
                                bounds.x0 >= 176.0 && bounds.x1 <= f64::from(size.x),
                                "{purpose} fits the viewport; zoom={zoom}, passes={passes}, long_names={long_names}: {bounds:?}"
                            );
                            let label = *tree
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == Role::Label && node.value() == Some(purpose)
                                })
                                .map(|(id, _)| id)
                                .unwrap();
                            assert!(
                                node.labelled_by().contains(&label),
                                "screen readers identify {purpose}, including its full device name"
                            );
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.engine.is_none() && app.dirty_since.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn routing_focus_reveals_devices_below_the_checklist() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let size = egui::vec2(700.0, 420.0) / 1.5;
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    for index in 0..3 {
                        let mut app = routing_app(true);
                        app.settings.setup.dismissed = false;
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let value = [
                            &app.settings.microphone,
                            &app.settings.output,
                            &app.settings.monitor_output,
                        ][index]
                            .clone();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = 1.5;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let target = *output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                node.role() == Role::ComboBox
                                    && node.value() == Some(value.as_str())
                            })
                            .map(|(id, _)| id)
                            .unwrap();
                        let key = |modifiers| egui::Event::Key {
                            key: egui::Key::Tab,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        };
                        if assistive {
                            draw_voice(
                                &mut app,
                                &ctx,
                                size,
                                vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus,
                                    target_tree: TreeId::ROOT,
                                    target_node: target,
                                    data: None,
                                })],
                            );
                        } else {
                            let mut reached = false;
                            for _ in 0..64 {
                                let output = draw_voice(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![key(egui::Modifiers::NONE)],
                                );
                                if output.platform_output.accesskit_update.unwrap().focus == target
                                {
                                    reached = true;
                                    break;
                                }
                            }
                            assert!(reached, "Tab reaches device {index}");
                        }
                        for modifiers in [
                            None,
                            Some(egui::Modifiers::SHIFT),
                            Some(egui::Modifiers::NONE),
                        ] {
                            if let Some(modifiers) = modifiers {
                                draw_voice(&mut app, &ctx, size, vec![key(modifiers)]);
                            }
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        assert_eq!(tree.focus, target);
                        let bounds = tree
                            .nodes
                            .iter()
                            .find(|(id, _)| *id == target)
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                        );
                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                            && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                            "focused device {index} is visible; passes={passes}, assistive={assistive}: {rect:?}");
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.engine.is_none() && app.dirty_since.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn long_device_menus_fit_and_selection_changes_only_the_requested_route() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    let mut app = routing_app(true);
                    let alternate = format!(
                        "Second USB microphone{}",
                        " with a long manufacturer and device description".repeat(3)
                    );
                    app.inputs.push(alternate.clone());
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| {
                        options.max_passes = passes.try_into().unwrap();
                        options.zoom_factor = zoom;
                    });
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let target = *output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::ComboBox
                                && node.value() == Some(app.settings.microphone.as_str())
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    let action = |action, target_node| {
                        egui::Event::AccessKitActionRequest(ActionRequest {
                            action,
                            target_tree: TreeId::ROOT,
                            target_node,
                            data: None,
                        })
                    };
                    let key = |key| egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    };
                    draw_voice(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let popup_id = ctx.memory(|memory| memory.focused()).unwrap().with("popup");
                    draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    assert!(egui::Popup::is_any_open(&ctx));
                    let rect = egui::AreaState::load(&ctx, popup_id).unwrap().rect();
                    assert!(
                        rect.left() >= 0.0 && rect.right() <= size.x,
                        "long device names keep the menu within the window; zoom={zoom}, passes={passes}: {rect:?}"
                    );
                    draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Escape)]);
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    assert!(!egui::Popup::is_any_open(&ctx));
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        vec![action(Action::Focus, target), key(egui::Key::Enter)],
                    );
                    for _ in 0..6 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let choice = *output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| node.label() == Some(alternate.as_str()))
                        .map(|(id, _)| id)
                        .unwrap();
                    if passes == 1 {
                        let mut reached = false;
                        for _ in 0..8 {
                            let output =
                                draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Tab)]);
                            if output.platform_output.accesskit_update.unwrap().focus == choice {
                                reached = true;
                                break;
                            }
                        }
                        assert!(
                            reached,
                            "Tab reaches the alternate microphone inside the menu"
                        );
                        draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                    } else {
                        draw_voice(&mut app, &ctx, size, vec![action(Action::Click, choice)]);
                    }
                    for _ in 0..6 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let mut expected = original;
                    expected["microphone"] = alternate.into();
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                    assert!(app.engine.is_none() && app.dirty_since.is_some());
                    assert!(!egui::Popup::is_any_open(&ctx));
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert_eq!(
                        output.platform_output.accesskit_update.unwrap().focus,
                        target
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "writes routing and device-menu screenshots; needs Python with Pillow"]
    fn snapshot_zoomed_device_routing() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (zoom, scale) in [(1.0, "standard"), (1.5, "zoomed")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in [
                    "routing",
                    "checklist",
                    "microphone-focus",
                    "monitor-focus",
                    "menu",
                ] {
                    let mut app = routing_app(true);
                    app.settings.setup.dismissed = matches!(scene, "routing" | "menu");
                    app.inputs.push(format!(
                        "Second USB microphone{}",
                        " with a long manufacturer and device description".repeat(3)
                    ));
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let mut events = vec![Vec::new(); 3];
                    if matches!(scene, "microphone-focus" | "monitor-focus" | "menu") {
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let value = if scene == "monitor-focus" {
                            &app.settings.monitor_output
                        } else {
                            &app.settings.microphone
                        };
                        let target = *output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                node.role() == Role::ComboBox
                                    && node.value() == Some(value.as_str())
                            })
                            .map(|(id, _)| id)
                            .unwrap();
                        events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node: target,
                            data: None,
                        })]);
                        events.extend(vec![Vec::new(); 8]);
                        if scene == "menu" {
                            events.push(vec![egui::Event::Key {
                                key: egui::Key::Enter,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            }]);
                        }
                    }
                    let image = super::super::snapshot::snapshot_with_events(
                        &mut app,
                        &format!("voice-{scene}-{scale}-{theme}"),
                        size,
                        zoom,
                        dark,
                        events,
                    );
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.engine.is_none() && app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    fn assistive_focus_and_keyboard_activation_change_only_the_requested_control() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for (old_role, old_label, new_role, new_label, key, field) in [
                    (
                        Role::Button,
                        "Soundboard",
                        Role::CheckBox,
                        "Remove noise",
                        egui::Key::Space,
                        "noise",
                    ),
                    (
                        Role::CheckBox,
                        "Remove rumble",
                        Role::CheckBox,
                        "Remove noise",
                        egui::Key::Space,
                        "noise",
                    ),
                    (
                        Role::CheckBox,
                        "Remove noise",
                        Role::CheckBox,
                        "Remove rumble",
                        egui::Key::Space,
                        "rumble",
                    ),
                    (
                        Role::CheckBox,
                        "Remove rumble",
                        Role::Link,
                        "DeepFilterNet",
                        egui::Key::Enter,
                        "model",
                    ),
                    (
                        Role::Link,
                        "DeepFilterNet",
                        Role::CheckBox,
                        "Only my voice",
                        egui::Key::Enter,
                        "voice",
                    ),
                    (
                        Role::CheckBox,
                        "Only my voice",
                        Role::Link,
                        "DeepFilterNet",
                        egui::Key::Space,
                        "model",
                    ),
                ] {
                    let mut app = App::stopped(config::Settings {
                        strength: 0.74321,
                        highpass_hz: 137.25,
                        monitor_volume: 0.43561,
                        ..Default::default()
                    });
                    app.settings.setup.dismissed = true;
                    let page = app.page;
                    let original = app.settings.processing();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    ctx.enable_accesskit();
                    let size = egui::vec2(760.0, 880.0);
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let [old, new] =
                        [(old_role, old_label), (new_role, new_label)].map(|(role, label)| {
                            *tree
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == role && node.label() == Some(label)
                                })
                                .map(|(id, _)| id)
                                .unwrap()
                        });
                    let focus = |target_node| {
                        egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node,
                            data: None,
                        })
                    };
                    draw_voice(&mut app, &ctx, size, vec![focus(old)]);
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert_eq!(
                        output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .focus,
                        old
                    );
                    assert!(app.dirty_since.is_none());
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        vec![
                            focus(new),
                            egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    );
                    for _ in 0..7 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let mut expected = original;
                    match field {
                        "noise" => expected.strength = 0.0,
                        "rumble" => expected.highpass = !expected.highpass,
                        "model" => expected.model = crate::dsp::Model::Rnnoise,
                        "voice" => expected.voice_gate = !expected.voice_gate,
                        _ => unreachable!(),
                    }
                    assert_eq!(
                        app.page, page,
                        "the key must not activate the old {old_label} sidebar button"
                    );
                    assert_eq!(
                        app.settings.processing(),
                        expected,
                        "{key:?} belongs only to the requested {new_label}"
                    );
                    assert_eq!(app.settings.monitor_volume, 0.43561);
                    assert!(!app.settings.monitor);
                    assert!(app.dirty_since.is_some());
                    assert_eq!(
                        output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .focus,
                        new
                    );
                }
            }
        }
    }

    #[test]
    fn escape_cancels_activation_waiting_for_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for (role, label, modifiers) in [
                (Role::Button, "Soundboard", None),
                (Role::Slider, "Remove rumble", Some(egui::Modifiers::NONE)),
                (Role::Link, "DeepFilterNet", Some(egui::Modifiers::SHIFT)),
            ] {
                let mut app = App::stopped(config::Settings {
                    strength: 0.74321,
                    ..Default::default()
                });
                app.settings.setup.dismissed = true;
                let page = app.page;
                let original = app.settings.processing();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.enable_accesskit();
                let size = egui::vec2(760.0, 880.0);
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let [old, new] =
                    [(role, label), (Role::CheckBox, "Remove noise")].map(|(role, label)| {
                        *tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.role() == role && node.label() == Some(label))
                            .map(|(id, _)| id)
                            .unwrap()
                    });
                let focus = |target_node| {
                    egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus,
                        target_tree: TreeId::ROOT,
                        target_node,
                        data: None,
                    })
                };
                let key = |key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                };
                draw_voice(&mut app, &ctx, size, vec![focus(old)]);
                for _ in 0..2 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let movement = modifiers.map_or_else(
                    || focus(new),
                    |modifiers| egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    },
                );
                draw_voice(&mut app, &ctx, size, vec![movement, key(egui::Key::Space)]);
                assert_eq!(
                    app.settings.processing(),
                    original,
                    "the command waits for focus"
                );
                draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Escape)]);
                for _ in 0..4 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                assert_eq!(
                    app.settings.processing(),
                    original,
                    "Escape discards the unseen activation"
                );
                assert_eq!(app.page, page);
                assert!(app.dirty_since.is_none());
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![focus(new), key(egui::Key::Space)],
                );
                for _ in 0..5 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let mut expected = original;
                expected.strength = 0.0;
                assert_eq!(
                    app.settings.processing(),
                    expected,
                    "a fresh command still works after cancellation"
                );
                assert_eq!(app.page, page);
                assert!(app.dirty_since.is_some());
            }
        }
    }

    #[test]
    fn batched_tab_activation_uses_the_new_focus_once() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for key in [egui::Key::Space, egui::Key::Enter] {
                    for (old_role, old_label, new_role, new_label, backwards, field) in [
                        (
                            Role::Link,
                            "DeepFilterNet",
                            Role::CheckBox,
                            "Only my voice",
                            false,
                            "voice",
                        ),
                        (
                            Role::CheckBox,
                            "Only my voice",
                            Role::Link,
                            "DeepFilterNet",
                            true,
                            "model",
                        ),
                        (
                            Role::Slider,
                            "Remove rumble",
                            Role::CheckBox,
                            "Remove noise",
                            false,
                            "noise",
                        ),
                        (
                            Role::Slider,
                            "Only my voice",
                            Role::CheckBox,
                            "Quiet cut-off",
                            false,
                            "gate",
                        ),
                        (
                            Role::Slider,
                            "Quiet cut-off",
                            Role::CheckBox,
                            "Quiet cut-off",
                            true,
                            "gate",
                        ),
                        (
                            Role::CheckBox,
                            "Remove rumble",
                            Role::Slider,
                            "Your mic",
                            true,
                            "none",
                        ),
                        (
                            Role::Link,
                            "DeepFilterNet",
                            Role::CheckBox,
                            "Only my voice",
                            false,
                            "both",
                        ),
                        (
                            Role::CheckBox,
                            "Only my voice",
                            Role::Link,
                            "DeepFilterNet",
                            true,
                            "both",
                        ),
                    ] {
                        let mut app = App::stopped(config::Settings {
                            strength: 0.74321,
                            highpass_hz: 137.25,
                            voice_threshold: 0.61347,
                            monitor_volume: 0.43561,
                            ..Default::default()
                        });
                        app.settings.setup.dismissed = true;
                        let page = app.page;
                        let original = app.settings.processing();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        ctx.enable_accesskit();
                        let size = egui::vec2(760.0, 880.0);
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let [old, new] =
                            [(old_role, old_label), (new_role, new_label)].map(|(role, label)| {
                                *tree
                                    .nodes
                                    .iter()
                                    .find(|(_, node)| {
                                        node.role() == role && node.label() == Some(label)
                                    })
                                    .map(|(id, _)| id)
                                    .unwrap()
                            });
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: old,
                                data: None,
                            })],
                        );
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let mut clicked = Vec::new();
                        let command = |pressed| egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        };
                        let mut events = if field == "both" {
                            vec![command(true), command(false)]
                        } else {
                            Vec::new()
                        };
                        events.extend([
                            egui::Event::Key {
                                key: egui::Key::Tab,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: if backwards {
                                    egui::Modifiers::SHIFT
                                } else {
                                    egui::Modifiers::NONE
                                },
                            },
                            command(true),
                        ]);
                        for _ in 0..8 {
                            let output =
                                draw_voice(&mut app, &ctx, size, std::mem::take(&mut events));
                            clicked.extend(output.platform_output.events.iter().filter_map(
                                |event| match event {
                                    egui::output::OutputEvent::Clicked(info) => info.label.clone(),
                                    _ => None,
                                },
                            ));
                        }
                        let mut expected = original;
                        match field {
                            "voice" => expected.voice_gate = !expected.voice_gate,
                            "model" => expected.model = crate::dsp::Model::Rnnoise,
                            "noise" => expected.strength = 0.0,
                            "gate" => expected.gate = !expected.gate,
                            "both" => {
                                expected.model = crate::dsp::Model::Rnnoise;
                                expected.voice_gate = !expected.voice_gate;
                            }
                            "none" => {}
                            _ => unreachable!(),
                        }
                        assert_eq!(
                            app.settings.processing(),
                            expected,
                            "{key:?} after Tab belongs to {new_label}; backwards={backwards}, passes={passes}"
                        );
                        let expected_clicks = if field == "both" {
                            vec![old_label, new_label]
                        } else {
                            vec![new_label]
                        };
                        assert_eq!(
                            clicked, expected_clicks,
                            "each control reports its activation once, in input order"
                        );
                        assert_eq!(app.page, page);
                        assert_eq!(app.settings.monitor_volume, 0.43561);
                        assert!(!app.settings.monitor);
                        assert_eq!(app.dirty_since.is_some(), field != "none");
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        assert_eq!(
                            output
                                .platform_output
                                .accesskit_update
                                .as_ref()
                                .unwrap()
                                .focus,
                            new
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn voice_controls_fit_and_keep_identity_when_text_size_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let controls = [
            (Role::Slider, "Your mic", Some("+3.4 dB")),
            (Role::Slider, "Remove rumble", Some("137 Hz")),
            (Role::Slider, "Remove noise", Some("74%")),
            (Role::Slider, "Only my voice", Some("61%")),
            (Role::Slider, "Quiet cut-off", Some("-43 dB")),
            (Role::Slider, "To Discord", Some("-2.3 dB")),
            (Role::Slider, "Hear yourself", Some("44%")),
            (Role::CheckBox, "Remove rumble", None),
            (Role::CheckBox, "Remove noise", None),
            (Role::CheckBox, "Only my voice", None),
            (Role::CheckBox, "Quiet cut-off", None),
            (Role::CheckBox, "Hear yourself", None),
            (Role::Link, "DeepFilterNet", None),
        ];
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    let mut app = App::stopped(config::Settings {
                        input_gain_db: 3.37,
                        highpass_hz: 137.25,
                        strength: 0.74321,
                        voice_threshold: 0.61347,
                        gate_threshold_db: -43.27,
                        output_gain_db: -2.33,
                        monitor_volume: 0.43561,
                        ..Default::default()
                    });
                    app.settings.setup.dismissed = true;
                    app.settings.output = "CABLE Input (VB-Audio Virtual Cable)".into();
                    app.outputs = vec![app.settings.output.clone()];
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    let mut ids = Vec::new();
                    let mut size = egui::vec2(700.0, 420.0);
                    for (transition, zoom) in [1.0, 1.25, 1.0, 1.5, 1.0].into_iter().enumerate() {
                        let keep = (transition > 0).then(|| {
                            let index = [0, 11, 12, 6][transition - 1];
                            (ids[index], controls[index].0, controls[index].1)
                        });
                        if let Some((target_node, _, _)) = keep {
                            draw_voice(
                                &mut app,
                                &ctx,
                                size,
                                vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus,
                                    target_tree: TreeId::ROOT,
                                    target_node,
                                    data: None,
                                })],
                            );
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                        }
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        if let Some((target, role, label)) = keep {
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == target)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            let rect = egui::Rect::from_min_max(
                                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                            );
                            assert!(
                                output.shapes.iter().any(|shape| shape
                                    .clip_rect
                                    .contains_rect(rect)
                                    && match &shape.shape {
                                        egui::Shape::Rect(outline) if role != Role::Link =>
                                            outline.rect == rect && outline.stroke.width == 2.0,
                                        egui::Shape::Text(link) if role == Role::Link =>
                                            link.galley.text() == label
                                                && link.underline.width > 0.0,
                                        _ => false,
                                    }),
                                "{label} stays visible when text size changes to {zoom}: {rect:?}"
                            );
                        }
                        for (index, (role, label, value)) in controls.into_iter().enumerate() {
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let target = *output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == role && node.label() == Some(label)
                                })
                                .map(|(id, _)| id)
                                .unwrap();
                            if ids.len() == index {
                                ids.push(target);
                            } else {
                                assert_eq!(
                                    ids[index], target,
                                    "{label} keeps its identity at {zoom}"
                                );
                            }
                            let tab = |modifiers| egui::Event::Key {
                                key: egui::Key::Tab,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers,
                            };
                            if assistive {
                                draw_voice(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                        action: Action::Focus,
                                        target_tree: TreeId::ROOT,
                                        target_node: target,
                                        data: None,
                                    })],
                                );
                            } else {
                                let mut reached = false;
                                for _ in 0..80 {
                                    let output = draw_voice(
                                        &mut app,
                                        &ctx,
                                        size,
                                        vec![tab(egui::Modifiers::NONE)],
                                    );
                                    if output.platform_output.accesskit_update.unwrap().focus
                                        == target
                                    {
                                        reached = true;
                                        break;
                                    }
                                }
                                assert!(reached, "Tab reaches {label} at {zoom}");
                            }
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == target)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            let rect = egui::Rect::from_min_max(
                                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                            );
                            assert!(
                                output.shapes.iter().any(|shape| shape
                                    .clip_rect
                                    .contains_rect(rect)
                                    && match &shape.shape {
                                        egui::Shape::Rect(outline) if role != Role::Link =>
                                            outline.rect == rect && outline.stroke.width == 2.0,
                                        egui::Shape::Text(link) if role == Role::Link =>
                                            link.galley.text() == label
                                                && link.underline.width > 0.0,
                                        _ => false,
                                    }),
                                "{role:?} {label} fits at {zoom}; passes={passes}, assistive={assistive}: {rect:?}"
                            );
                            if role == Role::Slider {
                                for shape in &output.shapes {
                                    if shape.clip_rect == rect
                                        && let egui::Shape::Text(text) = &shape.shape
                                    {
                                        assert!(
                                            rect.contains_rect(text.visual_bounding_rect()),
                                            "{label}'s plot hint fits at {zoom}"
                                        );
                                    }
                                }
                            }
                            if let Some(value) = value {
                                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                                    if text.galley.text() == value && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                                    "{label}'s displayed {value} is visible beside its control at {zoom}");
                            }
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn voice_row_focus_stays_visible_when_only_the_window_height_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let controls = [
            (Role::Slider, "Your mic"),
            (Role::Slider, "Remove rumble"),
            (Role::Slider, "Remove noise"),
            (Role::Slider, "Only my voice"),
            (Role::Slider, "Quiet cut-off"),
            (Role::Slider, "To Discord"),
            (Role::Slider, "Hear yourself"),
            (Role::CheckBox, "Remove rumble"),
            (Role::CheckBox, "Remove noise"),
            (Role::CheckBox, "Only my voice"),
            (Role::CheckBox, "Quiet cut-off"),
            (Role::CheckBox, "Hear yourself"),
            (Role::Link, "DeepFilterNet"),
        ];
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.5] {
                    for width in [700.0, 900.0] {
                        for (role, label) in controls {
                            let mut app = routing_app(false);
                            app.settings.input_gain_db = 3.37;
                            app.settings.highpass_hz = 137.25;
                            app.settings.strength = 0.74321;
                            app.settings.voice_threshold = 0.61347;
                            app.settings.gate_threshold_db = -43.27;
                            app.settings.output_gain_db = -2.33;
                            app.settings.monitor_volume = 0.43561;
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| {
                                options.max_passes = passes.try_into().unwrap();
                                options.zoom_factor = zoom;
                            });
                            ctx.set_visuals(if dark {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let mut size = egui::vec2(width, 880.0) / zoom;
                            for _ in 0..3 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let target = *output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == role && node.label() == Some(label)
                                })
                                .map(|(id, _)| id)
                                .unwrap();
                            draw_voice(
                                &mut app,
                                &ctx,
                                size,
                                vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus,
                                    target_tree: TreeId::ROOT,
                                    target_node: target,
                                    data: None,
                                })],
                            );
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            for height in [420.0, 880.0, 420.0] {
                                size.y = height / zoom;
                                for _ in 0..8 {
                                    draw_voice(&mut app, &ctx, size, Vec::new());
                                }
                                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                                let tree =
                                    output.platform_output.accesskit_update.as_ref().unwrap();
                                assert_eq!(tree.focus, target);
                                let bounds = tree
                                    .nodes
                                    .iter()
                                    .find(|(id, _)| *id == target)
                                    .unwrap()
                                    .1
                                    .bounds()
                                    .unwrap();
                                let rect = egui::Rect::from_min_max(
                                    egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                    egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                                );
                                assert!(
                                    rect.top() >= 44.0
                                        && rect.bottom() <= size.y
                                        && output.shapes.iter().any(|shape| shape
                                            .clip_rect
                                            .contains_rect(rect)
                                            && match &shape.shape {
                                                egui::Shape::Rect(outline)
                                                    if role != Role::Link =>
                                                    outline.rect == rect
                                                        && outline.stroke.width == 2.0,
                                                egui::Shape::Text(link) if role == Role::Link =>
                                                    link.galley.text() == label
                                                        && link.underline.width > 0.0,
                                                _ => false,
                                            }),
                                    "{role:?} {label} fits after a height-only resize: width={width}, height={height}, zoom={zoom}, passes={passes}: {rect:?}"
                                );
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none() && app.engine.is_none());
                            }
                            draw_voice(
                                &mut app,
                                &ctx,
                                size,
                                vec![
                                    egui::Event::PointerMoved(egui::pos2(184.0, size.y * 0.5)),
                                    egui::Event::MouseWheel {
                                        unit: egui::MouseWheelUnit::Point,
                                        delta: egui::vec2(0.0, 1000.0),
                                        modifiers: egui::Modifiers::NONE,
                                        phase: egui::TouchPhase::Move,
                                    },
                                ],
                            );
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let before = output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == target)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            if role == Role::Slider && label == "Remove noise" {
                                assert!(
                                    before.y1 > f64::from(size.y),
                                    "manual scrolling can leave the focused noise slider off screen"
                                );
                            }
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.unwrap();
                            assert_eq!(tree.focus, target);
                            assert_eq!(
                                tree.nodes
                                    .iter()
                                    .find(|(id, _)| *id == target)
                                    .unwrap()
                                    .1
                                    .bounds()
                                    .unwrap(),
                                before,
                                "manual scrolling does not snap back to {label}"
                            );
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && app.engine.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn voice_keyboard_controls_scroll_into_view_and_apply_batched_input() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let controls = [
            (Role::Slider, "Your mic"),
            (Role::Slider, "Remove rumble"),
            (Role::Slider, "Remove noise"),
            (Role::Slider, "Only my voice"),
            (Role::Slider, "Quiet cut-off"),
            (Role::Slider, "To Discord"),
            (Role::Slider, "Hear yourself"),
            (Role::CheckBox, "Remove rumble"),
            (Role::CheckBox, "Remove noise"),
            (Role::CheckBox, "Only my voice"),
            (Role::CheckBox, "Quiet cut-off"),
            (Role::CheckBox, "Hear yourself"),
            (Role::Link, "DeepFilterNet"),
        ];
        for (dark, zoom) in [
            (true, 1.0),
            (false, 1.0),
            (true, 1.25),
            (false, 1.25),
            (true, 1.5),
            (false, 1.5),
        ] {
            let mut app = App::stopped(config::Settings::default());
            app.settings.setup.dismissed = true;
            app.settings.input_gain_db = 3.37;
            app.settings.highpass_hz = 137.25;
            app.settings.monitor_volume = 0.43561;
            let original = app.settings.processing();
            let ctx = egui::Context::default();
            ctx.options_mut(|options| {
                options.max_passes = 1.try_into().unwrap();
                options.zoom_factor = zoom;
            });
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            ctx.enable_accesskit();
            let size = egui::vec2(700.0, 420.0) / zoom;
            for _ in 0..3 {
                draw_voice(&mut app, &ctx, size, Vec::new());
            }
            let mut visited = std::collections::HashSet::new();
            let key = |key, modifiers| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            for _ in 0..80 {
                let output = draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
                );
                if ctx.memory(|memory| memory.focused()).is_none() {
                    continue;
                }
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let node = &tree
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == tree.focus)
                    .unwrap()
                    .1;
                let role = node.role();
                if !node
                    .label()
                    .is_some_and(|label| controls.contains(&(role, label)))
                {
                    continue;
                }
                let name = node.label().unwrap().to_owned();
                for _ in 0..6 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let node = &tree
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == tree.focus)
                    .unwrap()
                    .1;
                assert_eq!(node.label(), Some(name.as_str()));
                assert_eq!(node.role(), role);
                let bounds = node.bounds().unwrap();
                let rect = egui::Rect::from_min_max(
                    egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                    egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                );
                assert!(
                    output
                        .shapes
                        .iter()
                        .any(|shape| shape.clip_rect.contains_rect(rect)
                            && match &shape.shape {
                                egui::Shape::Rect(outline) if role != Role::Link =>
                                    outline.rect == rect && outline.stroke.width == 2.0,
                                egui::Shape::Text(link) if role == Role::Link =>
                                    link.galley.text() == name && link.underline.width > 0.0,
                                _ => false,
                            }),
                    "the focused {role:?} {name} and its focus indicator are fully visible"
                );
                visited.insert((role, name));
                if visited.len() == controls.len() {
                    break;
                }
            }
            assert_eq!(
                visited,
                controls
                    .into_iter()
                    .map(|(role, label)| (role, label.to_owned()))
                    .collect()
            );
            assert_eq!(app.settings.processing(), original);
            assert_eq!(app.settings.monitor_volume, 0.43561);
            assert!(app.dirty_since.is_none(), "Tab preserves saved values");
            for _ in 0..2 {
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![key(egui::Key::Tab, egui::Modifiers::SHIFT)],
                );
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
            }
            let output = draw_voice(&mut app, &ctx, size, Vec::new());
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            assert_eq!(
                tree.nodes
                    .iter()
                    .find(|(id, _)| *id == tree.focus)
                    .unwrap()
                    .1
                    .label(),
                Some("To Discord")
            );
            let rumble_switch = *tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == Role::CheckBox && node.label() == Some("Remove rumble")
                })
                .map(|(id, _)| id)
                .unwrap();
            draw_voice(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus,
                    target_tree: TreeId::ROOT,
                    target_node: rumble_switch,
                    data: None,
                })],
            );
            for _ in 0..2 {
                draw_voice(&mut app, &ctx, size, Vec::new());
            }
            draw_voice(
                &mut app,
                &ctx,
                size,
                vec![
                    key(egui::Key::Tab, egui::Modifiers::NONE),
                    key(egui::Key::ArrowUp, egui::Modifiers::NONE),
                ],
            );
            for _ in 0..6 {
                draw_voice(&mut app, &ctx, size, Vec::new());
            }
            let mut expected = original;
            expected.highpass_hz = 138.0;
            assert_eq!(
                app.settings.processing(),
                expected,
                "a batched Tab and arrow edits the new control"
            );
            assert_eq!(app.settings.monitor_volume, 0.43561);
            assert!(
                app.dirty_since.is_some(),
                "keyboard edits enter the normal settings save path"
            );
        }
    }

    #[test]
    #[ignore]
    fn snapshot_responsive_voice_rows() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (zoom, scale) in [(1.0, "standard"), (1.25, "medium"), (1.5, "zoomed")] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for live in [false, true] {
                    for (role, label, scene) in [
                        (Role::Slider, "Your mic", "mic"),
                        (Role::Link, "DeepFilterNet", "model"),
                        (Role::Slider, "Hear yourself", "monitor"),
                        (Role::Slider, "Your mic", "overview"),
                    ] {
                        if scene == "overview" && zoom != 1.5 {
                            continue;
                        }
                        let mut app = App::stopped(config::Settings {
                            microphone: "Test microphone".into(),
                            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
                            input_gain_db: 3.37,
                            highpass_hz: 137.25,
                            strength: 0.74321,
                            voice_threshold: 0.61347,
                            gate_threshold_db: -43.27,
                            output_gain_db: -2.33,
                            monitor_volume: 0.43561,
                            ..Default::default()
                        });
                        app.settings.setup.dismissed = true;
                        app.inputs = vec![app.settings.microphone.clone()];
                        app.outputs = vec![app.settings.output.clone()];
                        if live {
                            app.demo_frames = super::super::snapshot::demo_frames();
                        }
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let size =
                            egui::vec2(700.0, if scene == "overview" { 1600.0 } else { 420.0 })
                                / zoom;
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let mut events = vec![Vec::new(); 3];
                        if scene != "overview" {
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let target_node = *output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(_, node)| {
                                    node.role() == role && node.label() == Some(label)
                                })
                                .map(|(id, _)| id)
                                .unwrap();
                            events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node,
                                data: None,
                            })]);
                            events.extend(vec![Vec::new(); 8]);
                        }
                        let state = if live { "live" } else { "stopped" };
                        let image = super::super::snapshot::snapshot_with_events(
                            &mut app,
                            &format!("voice-responsive-{scene}-{state}-{scale}-{theme}"),
                            size,
                            zoom,
                            dark,
                            events,
                        );
                        println!("SNAPSHOT {}", image.display());
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes Voice row focus after height-only resizing; needs Python with Pillow"]
    fn snapshot_voice_row_focus_after_resizing() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for zoom in [1.0, 1.5] {
                for width in [700.0, 900.0] {
                    for (role, label, scene) in [
                        (Role::Slider, "Remove noise", "noise"),
                        (Role::CheckBox, "Hear yourself", "switch"),
                        (Role::Link, "DeepFilterNet", "model"),
                    ] {
                        let mut app = routing_app(false);
                        app.settings.input_gain_db = 3.37;
                        app.settings.highpass_hz = 137.25;
                        app.settings.strength = 0.74321;
                        app.settings.voice_threshold = 0.61347;
                        app.settings.gate_threshold_db = -43.27;
                        app.settings.output_gain_db = -2.33;
                        app.settings.monitor_volume = 0.43561;
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let initial = egui::vec2(width, 880.0) / zoom;
                        let size = egui::vec2(width, 420.0) / zoom;
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        ctx.enable_accesskit();
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, initial, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, initial, Vec::new());
                        let target = *output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| node.role() == role && node.label() == Some(label))
                            .map(|(id, _)| id)
                            .unwrap();
                        let input = |size, events| egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            events,
                            ..Default::default()
                        };
                        let mut inputs: Vec<_> =
                            (0..3).map(|_| input(initial, Vec::new())).collect();
                        inputs.push(input(
                            initial,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: target,
                                data: None,
                            })],
                        ));
                        inputs.extend((0..8).map(|_| input(initial, Vec::new())));
                        inputs.extend((0..8).map(|_| input(size, Vec::new())));
                        let image = super::super::snapshot::snapshot_with_inputs(
                            &mut app,
                            &format!(
                                "voice-row-resize-{scene}-{}-{}-{theme}",
                                width as u32,
                                (zoom * 100.0) as u32
                            ),
                            size,
                            zoom,
                            dark,
                            inputs,
                        );
                        println!("SNAPSHOT {}", image.display());
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && app.engine.is_none());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn snapshot_voice_keyboard_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (role, label, name, size, action) in [
            (
                Role::Slider,
                "Your mic",
                "voice-keyboard",
                egui::vec2(760.0, 880.0),
                Action::Focus,
            ),
            (
                Role::Slider,
                "Hear yourself",
                "voice-keyboard-small",
                egui::vec2(700.0, 420.0),
                Action::Focus,
            ),
            (
                Role::CheckBox,
                "Remove rumble",
                "voice-switch",
                egui::vec2(760.0, 880.0),
                Action::Focus,
            ),
            (
                Role::CheckBox,
                "Hear yourself",
                "voice-switch-small",
                egui::vec2(700.0, 420.0),
                Action::Focus,
            ),
            (
                Role::Link,
                "DeepFilterNet",
                "voice-model-small",
                egui::vec2(700.0, 420.0),
                Action::Focus,
            ),
            (
                Role::CheckBox,
                "Remove noise",
                "voice-noise-off",
                egui::vec2(760.0, 880.0),
                Action::Click,
            ),
        ] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                let mut app = App::stopped(config::Settings::default());
                app.settings.setup.dismissed = true;
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let target_node = *output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == role && node.label() == Some(label))
                    .map(|(id, _)| id)
                    .unwrap();
                let mut events = vec![vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action,
                    target_tree: TreeId::ROOT,
                    target_node,
                    data: None,
                })]];
                events.extend(vec![Vec::new(); 6]);
                let path = super::super::snapshot::snapshot_with_events(
                    &mut app,
                    &format!("{name}-{theme}"),
                    size,
                    1.5,
                    dark,
                    events,
                );
                if action == Action::Click {
                    assert_eq!(app.settings.strength, 0.0);
                }
                println!("SNAPSHOT {}", path.display());
            }
        }
    }

    #[test]
    fn a_preset_name_accepts_a_batched_backward_tab_edit_and_enter() {
        use eframe::App as _;
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = App::stopped(config::Settings::default());
                app.settings.setup.dismissed = true;
                app.preset_name = "Old name".into();
                let processing = app.settings.processing();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.enable_accesskit();
                let mut frame = eframe::Frame::_new_kittest();
                let mut draw = |app: &mut App, events| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(760.0, 880.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| app.ui(ui, &mut frame),
                    )
                };
                for _ in 0..3 {
                    draw(&mut app, Vec::new());
                }
                let output = draw(&mut app, Vec::new());
                let pos = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == "+" => {
                            Some(text.visual_bounding_rect().center())
                        }
                        _ => None,
                    })
                    .unwrap();
                for pressed in [true, false] {
                    draw(
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    );
                }
                for _ in 0..3 {
                    draw(&mut app, Vec::new());
                }
                let output = draw(&mut app, Vec::new());
                let save = *output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some("Save"))
                    .map(|(id, _)| id)
                    .unwrap();
                draw(
                    &mut app,
                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus,
                        target_tree: TreeId::ROOT,
                        target_node: save,
                        data: None,
                    })],
                );
                for _ in 0..2 {
                    draw(&mut app, Vec::new());
                }
                let key = |key, modifiers| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                };
                draw(
                    &mut app,
                    vec![
                        key(egui::Key::Tab, egui::Modifiers::SHIFT),
                        key(egui::Key::A, egui::Modifiers::COMMAND),
                        egui::Event::Text("Studio".into()),
                        key(egui::Key::ArrowLeft, egui::Modifiers::NONE),
                        egui::Event::Text("X".into()),
                        key(egui::Key::Enter, egui::Modifiers::NONE),
                    ],
                );
                for _ in 0..4 {
                    draw(&mut app, Vec::new());
                }
                let preset = app.settings.presets.iter().find(|preset| preset.name == "StudiXo").unwrap_or_else(|| panic!(
                    "the edited name is saved; names={:?}, text={:?}, popup={}, focus={:?}, passes={passes}",
                    app.settings.presets.iter().map(|preset| &preset.name).collect::<Vec<_>>(), app.preset_name,
                    egui::Popup::is_any_open(&ctx), ctx.memory(|memory| memory.focused())));
                assert_eq!(preset.processing, processing);
                assert_eq!(app.settings.processing(), processing);
                assert!(app.preset_name.is_empty() && app.dirty_since.is_some());
                assert!(!egui::Popup::is_any_open(&ctx));
            }
        }
    }

    #[test]
    fn saved_preset_chips_fit_and_keep_focus_when_the_page_reflows() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let name =
            "Studio — evening meetings and remote interviews with the microphone on the desk "
                .repeat(4);
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = routing_app(false);
                app.settings.presets = (0..8)
                    .map(|index| config::Preset {
                        name: if index == 7 {
                            name.clone()
                        } else {
                            format!("Studio {index}")
                        },
                        processing: config::Processing {
                            input_gain_db: index as f32 + 0.12345,
                            ..Default::default()
                        },
                    })
                    .collect();
                let original = serde_json::to_value(&app.settings).unwrap();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.enable_accesskit();
                let size = egui::vec2(700.0, 420.0);
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let target = *output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == Role::Button
                            && node.label().is_some_and(|label| label.contains(&name))
                    })
                    .map(|(id, _)| id)
                    .unwrap();
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus,
                        target_tree: TreeId::ROOT,
                        target_node: target,
                        data: None,
                    })],
                );
                for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        vec![egui::Event::Key {
                            key: egui::Key::Tab,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        }],
                    );
                    for _ in 0..4 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                }
                assert_eq!(
                    draw_voice(&mut app, &ctx, size, Vec::new())
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .focus,
                    target,
                    "Shift+Tab and Tab return to the same saved preset"
                );
                for zoom in [1.0, 1.5, 1.25, 1.0] {
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    for _ in 0..8 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    assert_eq!(
                        tree.focus, target,
                        "resizing keeps focus on the saved preset"
                    );
                    let node = &tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1;
                    assert!(
                        node.label().unwrap().contains(&name),
                        "the full preset name stays accessible"
                    );
                    let bounds = node.bounds().unwrap();
                    assert!(
                        bounds.x0 >= 176.0
                            && bounds.x1 <= f64::from(size.x)
                            && bounds.y0 >= 44.0
                            && bounds.y1 <= f64::from(size.y),
                        "focused preset fits at {zoom}x, passes={passes}: {bounds:?}"
                    );
                    assert!(
                        bounds.y1 - bounds.y0 <= 30.0,
                        "long saved names stay on one chip row"
                    );
                    assert!(output.shapes.iter().any(
                        |shape| matches!(&shape.shape, egui::Shape::Text(text)
                        if text.galley.text() == name && text.galley.elided
                            && shape.clip_rect.contains_rect(text.visual_bounding_rect()))
                    ));
                    assert_eq!(
                        serde_json::to_value(&app.settings).unwrap(),
                        original,
                        "navigation leaves every setting unchanged"
                    );
                    assert!(app.engine.is_none() && app.dirty_since.is_none());
                }
                for (delta, visible) in [(-1000.0, false), (1000.0, true)] {
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        vec![
                            egui::Event::PointerMoved(egui::pos2(198.0, 300.0)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                delta: egui::vec2(0.0, delta),
                                modifiers: egui::Modifiers::NONE,
                                phase: egui::TouchPhase::Move,
                            },
                        ],
                    );
                    for _ in 0..8 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    assert_eq!(tree.focus, target);
                    let bounds = tree
                        .nodes
                        .iter()
                        .find(|(id, _)| *id == target)
                        .unwrap()
                        .1
                        .bounds()
                        .unwrap();
                    assert_eq!(
                        bounds.y0 >= 44.0 && bounds.y1 <= f64::from(size.y),
                        visible,
                        "manual scrolling can leave and return to a focused preset without snapping back: {bounds:?}"
                    );
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                }
                app.settings.presets.remove(0);
                for _ in 0..5 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                assert_eq!(
                    output.platform_output.accesskit_update.unwrap().focus,
                    target,
                    "removing an earlier preset keeps the focused name"
                );
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                assert_eq!(
                    app.settings.processing(),
                    app.settings.presets.last().unwrap().processing,
                    "Enter applies the preset that stayed focused"
                );
            }
        }
    }

    #[test]
    fn keyboard_preset_deletion_targets_the_saved_preset_and_restores_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let key = |key, modifiers| {
            [true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers,
                })
                .collect()
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = routing_app(false);
                app.settings.presets = vec![
                    config::Preset {
                        name: "Shared studio".into(),
                        processing: config::Processing {
                            input_gain_db: 1.2345,
                            ..Default::default()
                        },
                    },
                    config::Preset {
                        name: "Shared studio".into(),
                        processing: config::Processing {
                            input_gain_db: 2.3456,
                            ..Default::default()
                        },
                    },
                    config::Preset {
                        name: "Balanced".into(),
                        processing: config::Processing {
                            strength: 0.71347,
                            ..Default::default()
                        },
                    },
                ];
                let original = app.settings.clone();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| {
                    options.max_passes = passes.try_into().unwrap();
                    options.zoom_factor = 1.5;
                });
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.enable_accesskit();
                let size = egui::vec2(700.0, 420.0) / 1.5;
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.unwrap();
                let mut duplicates: Vec<_> = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| {
                        node.role() == Role::Button
                            && node.label() == Some("Apply saved preset: Shared studio")
                    })
                    .collect();
                duplicates.sort_by(|left, right| {
                    let left = left.1.bounds().unwrap();
                    let right = right.1.bounds().unwrap();
                    left.y0
                        .total_cmp(&right.y0)
                        .then(left.x0.total_cmp(&right.x0))
                });
                assert_eq!(
                    duplicates.len(),
                    2,
                    "duplicate names have distinct controls"
                );
                let target = duplicates[1].0;
                let save = *tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some("Save preset"))
                    .map(|(id, _)| id)
                    .unwrap();
                let builtin = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some("Apply preset: Balanced"))
                    .unwrap()
                    .0;
                let saved = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some("Apply saved preset: Balanced"))
                    .unwrap()
                    .0;
                assert_ne!(builtin, saved, "saved names can also match a built-in name");
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus,
                        target_tree: TreeId::ROOT,
                        target_node: target,
                        data: None,
                    })],
                );
                for _ in 0..8 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                for cancel in [true, false] {
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        key(egui::Key::F10, egui::Modifiers::SHIFT),
                    );
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    let (remove, node) = tree
                        .nodes
                        .iter()
                        .find(|(_, node)| node.label() == Some("Delete preset"))
                        .expect("Shift+F10 opens the saved preset menu without a pointer");
                    assert_eq!(
                        tree.focus, *remove,
                        "the keyboard menu focuses Delete preset"
                    );
                    let bounds = node.bounds().unwrap();
                    assert!(
                        bounds.x0 >= 0.0
                            && bounds.y0 >= 0.0
                            && bounds.x1 <= f64::from(size.x)
                            && bounds.y1 <= f64::from(size.y)
                    );
                    draw_voice(
                        &mut app,
                        &ctx,
                        size,
                        key(
                            if cancel {
                                egui::Key::Escape
                            } else {
                                egui::Key::Enter
                            },
                            egui::Modifiers::NONE,
                        ),
                    );
                    for _ in 0..5 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert!(!egui::Popup::is_any_open(&ctx));
                    assert_eq!(
                        output.platform_output.accesskit_update.unwrap().focus,
                        if cancel { target } else { save }
                    );
                    let mut expected = original.clone();
                    if !cancel {
                        expected.presets.remove(1);
                    }
                    assert_eq!(
                        serde_json::to_value(&app.settings).unwrap(),
                        serde_json::to_value(expected).unwrap(),
                        "deleting the second duplicate preserves the first preset and every active setting"
                    );
                    assert_eq!(app.dirty_since.is_some(), !cancel);
                    assert!(app.engine.is_none());
                }
            }
        }
    }

    #[test]
    fn the_named_save_preset_control_supports_keyboard_save_overwrite_and_escape() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let key = |key| {
            [true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect()
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = routing_app(false);
                app.settings.strength = 0.71347;
                app.settings.highpass_hz = 132.456;
                let ctx = egui::Context::default();
                ctx.options_mut(|options| {
                    options.max_passes = passes.try_into().unwrap();
                    options.zoom_factor = 1.5;
                });
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ctx.enable_accesskit();
                let size = egui::vec2(700.0, 420.0) / 1.5;
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let save = *output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == Role::Button && node.label() == Some("Save preset")
                    })
                    .map(|(id, _)| id)
                    .expect("the plus button announces its action");
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus,
                        target_tree: TreeId::ROOT,
                        target_node: save,
                        data: None,
                    })],
                );
                for (name, activation) in [
                    ("  Café studio  ", egui::Key::Enter),
                    ("café STUDIO", egui::Key::Space),
                ] {
                    let processing = app.settings.processing();
                    let count = app.settings.presets.len();
                    draw_voice(&mut app, &ctx, size, key(activation));
                    for _ in 0..4 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    let (field, node) = tree
                        .nodes
                        .iter()
                        .find(|(_, node)| node.role() == Role::TextInput)
                        .unwrap();
                    assert_eq!(tree.focus, *field, "opening focuses the preset name");
                    assert_eq!(node.value(), Some(""));
                    draw_voice(&mut app, &ctx, size, key(egui::Key::Enter));
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert!(egui::Popup::is_any_open(&ctx));
                    assert_eq!(
                        app.settings.presets.len(),
                        count,
                        "an empty name does not create or replace a preset"
                    );
                    assert_eq!(
                        output.platform_output.accesskit_update.unwrap().focus,
                        *field,
                        "Enter on an empty name keeps editing"
                    );
                    draw_voice(&mut app, &ctx, size, vec![egui::Event::Text(name.into())]);
                    draw_voice(&mut app, &ctx, size, key(egui::Key::Enter));
                    for _ in 0..5 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert!(!egui::Popup::is_any_open(&ctx));
                    assert_eq!(
                        output.platform_output.accesskit_update.unwrap().focus,
                        save,
                        "saving returns to the stable plus control"
                    );
                    assert_eq!(
                        app.settings.presets.len(),
                        1,
                        "an ASCII case match replaces the saved preset"
                    );
                    assert_eq!(app.settings.presets[0].name, name.trim());
                    assert_eq!(app.settings.presets[0].processing, processing);
                    assert_eq!(
                        app.settings.processing(),
                        processing,
                        "saving does not apply or round a preset"
                    );
                    assert!(app.preset_name.is_empty());
                    app.settings.input_gain_db = 0.98765;
                }
                let original = serde_json::to_value(&app.settings).unwrap();
                draw_voice(&mut app, &ctx, size, key(egui::Key::Enter));
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                draw_voice(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::Text("Unfinished draft".into())],
                );
                draw_voice(&mut app, &ctx, size, key(egui::Key::Escape));
                for _ in 0..3 {
                    draw_voice(&mut app, &ctx, size, Vec::new());
                }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                assert!(!egui::Popup::is_any_open(&ctx));
                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, save);
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                assert_eq!(
                    app.preset_name, "Unfinished draft",
                    "Escape keeps the draft for the next opening"
                );
            }
        }
    }

    #[test]
    fn opening_save_preset_with_a_held_key_preserves_the_unfinished_draft() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let key = |key, pressed, repeat| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        };
        let press = |activation| {
            [true, false]
                .into_iter()
                .map(|pressed| key(activation, pressed, false))
                .collect()
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.25, 1.5] {
                    for activation in [egui::Key::Enter, egui::Key::Space] {
                        for initial in [
                            "",
                            "  Café studio — evening meetings and remote interviews  ",
                            "café STUDIO",
                        ] {
                            let mut app = routing_app(false);
                            app.settings.presets = vec![
                                config::Preset {
                                    name: "Café studio".into(),
                                    processing: config::Processing {
                                        strength: 0.41357,
                                        ..Default::default()
                                    },
                                },
                                config::Preset {
                                    name: "Untouched room".into(),
                                    processing: config::Processing {
                                        input_gain_db: 2.3456,
                                        ..Default::default()
                                    },
                                },
                            ];
                            app.settings.strength = 0.71347;
                            app.settings.highpass_hz = 132.456;
                            app.settings.input_gain_db = 0.98765;
                            app.settings.output_gain_db = -2.3456;
                            let mut expected = app.settings.clone();
                            let original = serde_json::to_value(&expected).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| {
                                options.max_passes = passes.try_into().unwrap();
                                options.zoom_factor = zoom;
                            });
                            ctx.set_visuals(if dark {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let size = egui::vec2(700.0, 420.0) / zoom;
                            for _ in 0..3 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let save = *output
                                .platform_output
                                .accesskit_update
                                .unwrap()
                                .nodes
                                .iter()
                                .find(|(_, node)| node.label() == Some("Save preset"))
                                .map(|(id, _)| id)
                                .unwrap();
                            draw_voice(
                                &mut app,
                                &ctx,
                                size,
                                vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                    action: Action::Focus,
                                    target_tree: TreeId::ROOT,
                                    target_node: save,
                                    data: None,
                                })],
                            );
                            draw_voice(&mut app, &ctx, size, press(egui::Key::Enter));
                            for _ in 0..4 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            if !initial.is_empty() {
                                draw_voice(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![egui::Event::Text(initial.into())],
                                );
                            }
                            draw_voice(&mut app, &ctx, size, press(egui::Key::Escape));
                            for _ in 0..4 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            assert!(!egui::Popup::is_any_open(&ctx));
                            assert_eq!(
                                app.preset_name, initial,
                                "Escape preserves the user's draft"
                            );
                            let mut events = vec![key(activation, true, false)];
                            if activation == egui::Key::Space {
                                events.push(egui::Event::Text(" ".into()));
                            }
                            draw_voice(&mut app, &ctx, size, events);
                            for _ in 0..4 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let mut draft = initial.to_owned();
                            let mut field = None;
                            for repeat in 0..4 {
                                if repeat > 0 {
                                    let mut events = vec![key(activation, true, true)];
                                    if activation == egui::Key::Space {
                                        events.push(egui::Event::Text(" ".into()));
                                    }
                                    draw_voice(&mut app, &ctx, size, events);
                                }
                                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                                assert!(
                                    egui::Popup::is_any_open(&ctx),
                                    "holding {activation:?} opens Save once and cannot submit the draft: initial={initial:?}, zoom={zoom}, passes={passes}, repeat={repeat}"
                                );
                                let tree = output.platform_output.accesskit_update.unwrap();
                                let (id, node) = tree
                                    .nodes
                                    .iter()
                                    .find(|(_, node)| node.role() == Role::TextInput)
                                    .unwrap();
                                if let Some(field) = field {
                                    assert_eq!(*id, field, "repeats keep the same name field");
                                } else {
                                    field = Some(*id);
                                }
                                assert_eq!(tree.focus, *id, "the unfinished name keeps focus");
                                assert_eq!(
                                    node.value(),
                                    Some(draft.as_str()),
                                    "the opening Space never becomes part of the name"
                                );
                                let bounds = node.bounds().unwrap();
                                assert!(
                                    bounds.x0 >= 0.0
                                        && bounds.x1 <= f64::from(size.x)
                                        && bounds.y0 >= 0.0
                                        && bounds.y1 <= f64::from(size.y),
                                    "the focused name fits the viewport: {bounds:?}"
                                );
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert_eq!(app.preset_name, draft);
                                assert!(app.dirty_since.is_none() && app.engine.is_none());
                                if repeat == 0 && draft.is_empty() {
                                    draft = "Evening café".into();
                                    draw_voice(
                                        &mut app,
                                        &ctx,
                                        size,
                                        vec![egui::Event::Text(draft.clone())],
                                    );
                                    assert_eq!(
                                        app.preset_name, draft,
                                        "other typing works while the opening key is held"
                                    );
                                }
                            }
                            let mut events =
                                vec![key(activation, false, false), key(activation, true, false)];
                            if activation == egui::Key::Space {
                                events.push(egui::Event::Text(" ".into()));
                            }
                            draw_voice(&mut app, &ctx, size, events);
                            if activation == egui::Key::Space {
                                draft.push(' ');
                                assert!(egui::Popup::is_any_open(&ctx));
                                assert_eq!(
                                    app.preset_name, draft,
                                    "a fresh Space edits the name normally"
                                );
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                draw_voice(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![
                                        key(egui::Key::Space, false, false),
                                        key(egui::Key::Enter, true, false),
                                    ],
                                );
                            }
                            let preset = config::Preset {
                                name: draft.trim().into(),
                                processing: expected.processing(),
                            };
                            match expected
                                .presets
                                .iter_mut()
                                .find(|preset| preset.name.eq_ignore_ascii_case(draft.trim()))
                            {
                                Some(existing) => *existing = preset,
                                None => expected.presets.push(preset),
                            }
                            for _ in 0..5 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            assert!(
                                !egui::Popup::is_any_open(&ctx),
                                "a fresh Enter saves the finished name"
                            );
                            assert_eq!(
                                output.platform_output.accesskit_update.unwrap().focus,
                                save
                            );
                            assert_eq!(
                                serde_json::to_value(&app.settings).unwrap(),
                                serde_json::to_value(expected).unwrap()
                            );
                            assert!(
                                app.preset_name.is_empty()
                                    && app.dirty_since.is_some()
                                    && app.engine.is_none()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn saving_a_preset_by_pointer_preserves_the_manual_scroll_position() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for width in [420.0, 240.0] {
            for dark in [true, false] {
                for passes in [1, 2] {
                    for y in [40.0, 170.0] {
                        let mut app = routing_app(false);
                        app.settings.presets = (0..8)
                            .map(|index| config::Preset {
                                name: format!("Studio {index}"),
                                processing: Default::default(),
                            })
                            .collect();
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        let name = "New studio — evening meetings and remote interviews "
                            .repeat(4)
                            .trim()
                            .to_owned();
                        app.preset_name = name.clone();
                        let mut expected = app.settings.clone();
                        expected.presets.push(config::Preset {
                            name: name.clone(),
                            processing: app.settings.processing(),
                        });
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        ctx.enable_accesskit();
                        let draw = |app: &mut App, events, scroll_offset: Option<f32>| {
                            let mut offset = 0.0;
                            let output = ctx.run_ui(
                                egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(
                                        egui::Pos2::ZERO,
                                        egui::vec2(width, 220.0),
                                    )),
                                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2),
                                    events,
                                    ..Default::default()
                                },
                                |ui| {
                                    let mut scroll =
                                        egui::ScrollArea::vertical().id_salt("preset-pointer");
                                    if let Some(offset) = scroll_offset {
                                        scroll = scroll.vertical_scroll_offset(offset);
                                    }
                                    let result = scroll.show(ui, |ui| {
                                        ui.add_space(450.0);
                                        app.draw_presets(ui);
                                        ui.add_space(600.0);
                                    });
                                    offset = result.state.offset.y;
                                },
                            );
                            (output, offset)
                        };
                        for _ in 0..3 {
                            draw(&mut app, Vec::new(), Some(0.0));
                        }
                        let (output, _) = draw(&mut app, Vec::new(), Some(0.0));
                        let tree = output.platform_output.accesskit_update.unwrap();
                        let (target, node) = tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Save preset"))
                            .unwrap();
                        let target = *target;
                        let rect = node.bounds().unwrap();
                        let offset = ((rect.y0 + rect.y1) / 2.0) as f32 - y;
                        draw(
                            &mut app,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: target,
                                data: None,
                            })],
                            None,
                        );
                        for _ in 0..8 {
                            draw(&mut app, Vec::new(), None);
                        }
                        for _ in 0..3 {
                            draw(&mut app, Vec::new(), Some(offset));
                        }
                        for _ in 0..8 {
                            draw(&mut app, Vec::new(), None);
                        }
                        let (output, before) = draw(&mut app, Vec::new(), None);
                        assert_eq!(
                            output
                                .platform_output
                                .accesskit_update
                                .as_ref()
                                .unwrap()
                                .focus,
                            target
                        );
                        let rect = output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(id, _)| *id == target)
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        let click = |app: &mut App, rect: egui::accesskit::Rect| {
                            let pos = egui::pos2(
                                ((rect.x0 + rect.x1) / 2.0) as f32,
                                ((rect.y0 + rect.y1) / 2.0) as f32,
                            );
                            assert!(
                                rect.x0 >= 0.0
                                    && rect.y0 >= 0.0
                                    && rect.x1 <= f64::from(width)
                                    && rect.y1 <= 220.0,
                                "the clicked control is visible: {rect:?}"
                            );
                            for pressed in [true, false] {
                                draw(
                                    app,
                                    vec![
                                        egui::Event::PointerMoved(pos),
                                        egui::Event::PointerButton {
                                            pos,
                                            button: egui::PointerButton::Primary,
                                            pressed,
                                            modifiers: egui::Modifiers::NONE,
                                        },
                                    ],
                                    None,
                                );
                            }
                        };
                        click(&mut app, rect);
                        for _ in 0..4 {
                            draw(&mut app, Vec::new(), None);
                        }
                        let (output, opened) = draw(&mut app, Vec::new(), None);
                        assert!(
                            (opened - before).abs() < 0.1,
                            "opening the menu preserves the manual scroll position"
                        );
                        let rect = output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Save"))
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        click(&mut app, rect);
                        for _ in 0..8 {
                            draw(&mut app, Vec::new(), None);
                        }
                        let (output, after) = draw(&mut app, Vec::new(), None);
                        assert!(
                            (after - before).abs() < 0.1,
                            "saving by pointer does not recenter the changed toolbar: width={width}, y={y}, passes={passes}: {before} -> {after}"
                        );
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(
                            output.platform_output.accesskit_update.unwrap().focus,
                            target
                        );
                        assert_eq!(
                            serde_json::to_value(&app.settings).unwrap(),
                            serde_json::to_value(&expected).unwrap()
                        );
                        assert!(
                            app.preset_name.is_empty()
                                && app.dirty_since.is_some()
                                && app.engine.is_none()
                        );
                        // Removing the newly saved row must preserve the same scroll position.
                        let (output, _) = draw(&mut app, Vec::new(), None);
                        let rect = output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                node.label() == Some(format!("Apply saved preset: {name}").as_str())
                            })
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        assert!(
                            rect.y0 >= 0.0 && rect.y1 <= 220.0,
                            "the preset to remove is visible: {rect:?}"
                        );
                        let pos = egui::pos2(
                            ((rect.x0 + rect.x1) / 2.0) as f32,
                            ((rect.y0 + rect.y1) / 2.0) as f32,
                        );
                        for pressed in [true, false] {
                            draw(
                                &mut app,
                                vec![
                                    egui::Event::PointerMoved(pos),
                                    egui::Event::PointerButton {
                                        pos,
                                        button: egui::PointerButton::Secondary,
                                        pressed,
                                        modifiers: egui::Modifiers::NONE,
                                    },
                                ],
                                None,
                            );
                        }
                        for _ in 0..3 {
                            draw(&mut app, Vec::new(), None);
                        }
                        let (output, _) = draw(&mut app, Vec::new(), None);
                        let rect = output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Delete preset"))
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        click(&mut app, rect);
                        for _ in 0..8 {
                            draw(&mut app, Vec::new(), None);
                        }
                        let (output, after) = draw(&mut app, Vec::new(), None);
                        assert!(
                            (after - before).abs() < 0.1,
                            "pointer deletion does not recenter the smaller toolbar"
                        );
                        assert_eq!(
                            output.platform_output.accesskit_update.unwrap().focus,
                            target
                        );
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(app.settings.presets.len(), 8);
                        assert!(
                            !app.settings
                                .presets
                                .iter()
                                .any(|preset| preset.name == name)
                        );
                        expected.presets.pop();
                        assert_eq!(
                            serde_json::to_value(&app.settings).unwrap(),
                            serde_json::to_value(expected).unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn preset_focus_stays_visible_when_only_the_window_height_changes() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.5] {
                    for save_button in [true, false] {
                        let mut app = routing_app(false);
                        app.settings.presets = (0..20)
                            .map(|index| config::Preset {
                                name: format!(
                                    "Studio {index} — evening meetings and remote interviews "
                                )
                                .repeat(4),
                                processing: Default::default(),
                            })
                            .collect();
                        let label = if save_button {
                            "Save preset".to_owned()
                        } else {
                            format!(
                                "Apply saved preset: {}",
                                app.settings.presets.last().unwrap().name
                            )
                        };
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = zoom;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        ctx.enable_accesskit();
                        let mut size = egui::vec2(700.0, 880.0) / zoom;
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let target = *output
                            .platform_output
                            .accesskit_update
                            .unwrap()
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some(label.as_str()))
                            .map(|(id, _)| id)
                            .unwrap();
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: target,
                                data: None,
                            })],
                        );
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        for height in [420.0, 880.0, 420.0] {
                            size.y = height / zoom;
                            for _ in 0..8 {
                                draw_voice(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert_eq!(tree.focus, target);
                            let bounds = tree
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == target)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            let rect = egui::Rect::from_min_max(
                                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                            );
                            assert!(rect.top() >= 44.0 && rect.bottom() <= size.y
                                && output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                    && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                                "focused preset fits after a height-only resize: height={height}, zoom={zoom}, passes={passes}, save={save_button}: {rect:?}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && app.engine.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn keyboard_preset_edits_reveal_save_after_the_list_changes() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let key = |key, modifiers| {
            [true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed,
                    repeat: false,
                    modifiers,
                })
                .collect()
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.5] {
                    for deleting in [false, true] {
                        let mut app = routing_app(false);
                        app.settings.presets = (0..20)
                            .map(|index| config::Preset {
                                name: format!(
                                    "Studio {index} — evening meetings and remote interviews "
                                )
                                .repeat(4),
                                processing: Default::default(),
                            })
                            .collect();
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        app.preset_name =
                            "New studio — evening meetings and remote interviews ".repeat(4);
                        let mut expected = app.settings.clone();
                        let label = if deleting {
                            format!(
                                "Apply saved preset: {}",
                                app.settings.presets.last().unwrap().name
                            )
                        } else {
                            "Save preset".to_owned()
                        };
                        if deleting {
                            expected.presets.pop();
                        } else {
                            expected.presets.push(config::Preset {
                                name: app.preset_name.trim().into(),
                                processing: app.settings.processing(),
                            });
                        }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = zoom;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        ctx.enable_accesskit();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        let target = *tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some(label.as_str()))
                            .map(|(id, _)| id)
                            .unwrap();
                        let save = *tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Save preset"))
                            .map(|(id, _)| id)
                            .unwrap();
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus,
                                target_tree: TreeId::ROOT,
                                target_node: target,
                                data: None,
                            })],
                        );
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            key(
                                if deleting {
                                    egui::Key::F10
                                } else {
                                    egui::Key::Enter
                                },
                                if deleting {
                                    egui::Modifiers::SHIFT
                                } else {
                                    egui::Modifiers::NONE
                                },
                            ),
                        );
                        for _ in 0..4 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        assert!(egui::Popup::is_any_open(&ctx));
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            key(egui::Key::Enter, egui::Modifiers::NONE),
                        );
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        assert_eq!(
                            tree.focus, save,
                            "keyboard edits return to the stable save control"
                        );
                        let bounds = tree
                            .nodes
                            .iter()
                            .find(|(id, _)| *id == save)
                            .unwrap()
                            .1
                            .bounds()
                            .unwrap();
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                        );
                        assert!(rect.top() >= 44.0 && rect.bottom() <= size.y
                            && output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                            "save remains visible after editing the tall toolbar: deleting={deleting}, zoom={zoom}, passes={passes}: {rect:?}");
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(
                            serde_json::to_value(&app.settings).unwrap(),
                            serde_json::to_value(expected).unwrap()
                        );
                        assert!(app.dirty_since.is_some() && app.engine.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn preset_edits_wait_for_the_activation_key_to_be_released() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers,
        };
        let focus = |target_node| {
            egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node,
                data: None,
            })
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.25, 1.5] {
                    for (scene, activation, field) in [
                        ("saved", egui::Key::Enter, true),
                        ("saved", egui::Key::Enter, false),
                        ("saved", egui::Key::Space, false),
                        ("overwritten", egui::Key::Enter, false),
                        ("overwritten", egui::Key::Space, false),
                        ("deleted", egui::Key::Enter, false),
                        ("deleted", egui::Key::Space, false),
                    ] {
                        let mut app = routing_app(false);
                        app.settings.presets = (0..20)
                            .map(|index| config::Preset {
                                name: format!(
                                    "Studio {index} — evening meetings and remote interviews "
                                )
                                .repeat(4)
                                .trim()
                                .into(),
                                processing: config::Processing {
                                    input_gain_db: index as f32 + 0.12345,
                                    ..Default::default()
                                },
                            })
                            .collect();
                        app.settings.strength = 0.71347;
                        app.settings.highpass_hz = 132.456;
                        app.settings.input_gain_db = 0.98765;
                        app.settings.output_gain_db = -2.3456;
                        let name = if scene == "overwritten" {
                            format!(
                                "  {}  ",
                                app.settings
                                    .presets
                                    .last()
                                    .unwrap()
                                    .name
                                    .replace("Studio", "STUDIO")
                                    .trim()
                            )
                        } else {
                            "  Café studio — evening meetings and remote interviews  ".into()
                        };
                        let mut expected = app.settings.clone();
                        let preset = config::Preset {
                            name: name.trim().into(),
                            processing: app.settings.processing(),
                        };
                        match scene {
                            "deleted" => {
                                expected.presets.pop();
                            }
                            "overwritten" => {
                                *expected.presets.last_mut().unwrap() = preset;
                            }
                            _ => expected.presets.push(preset),
                        }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| {
                            options.max_passes = passes.try_into().unwrap();
                            options.zoom_factor = zoom;
                        });
                        ctx.set_visuals(if dark {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        let size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..3 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        let save = *tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.label() == Some("Save preset"))
                            .map(|(id, _)| id)
                            .unwrap();
                        let label = format!(
                            "Apply saved preset: {}",
                            app.settings.presets.last().unwrap().name
                        );
                        let target = if scene == "deleted" {
                            *tree
                                .nodes
                                .iter()
                                .find(|(_, node)| node.label() == Some(label.as_str()))
                                .map(|(id, _)| id)
                                .unwrap()
                        } else {
                            save
                        };
                        draw_voice(&mut app, &ctx, size, vec![focus(target)]);
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let opening = if scene == "deleted" {
                            egui::Key::F10
                        } else {
                            egui::Key::Enter
                        };
                        let modifiers = if scene == "deleted" {
                            egui::Modifiers::SHIFT
                        } else {
                            egui::Modifiers::NONE
                        };
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![
                                key(opening, true, false, modifiers),
                                key(opening, false, false, modifiers),
                            ],
                        );
                        for _ in 0..4 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        assert!(egui::Popup::is_any_open(&ctx));
                        if scene != "deleted" {
                            draw_voice(&mut app, &ctx, size, vec![egui::Event::Text(name)]);
                            if !field {
                                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                                let target = *output
                                    .platform_output
                                    .accesskit_update
                                    .unwrap()
                                    .nodes
                                    .iter()
                                    .find(|(_, node)| {
                                        node.role() == Role::Button && node.label() == Some("Save")
                                    })
                                    .map(|(id, _)| id)
                                    .unwrap();
                                draw_voice(&mut app, &ctx, size, vec![focus(target)]);
                            }
                        }
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![key(activation, true, false, egui::Modifiers::NONE)],
                        );
                        for _ in 0..8 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        for repeat in 0..4 {
                            if repeat > 0 {
                                let mut events =
                                    vec![key(activation, true, true, egui::Modifiers::NONE)];
                                if activation == egui::Key::Space {
                                    events.push(egui::Event::Text(" ".into()));
                                }
                                draw_voice(&mut app, &ctx, size, events);
                            }
                            let output = draw_voice(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            assert!(
                                !egui::Popup::is_any_open(&ctx),
                                "holding {activation:?} after {scene} must not reopen Save: field={field}, zoom={zoom}, passes={passes}, repeat={repeat}"
                            );
                            assert_eq!(tree.focus, save, "the completed edit keeps focus on +");
                            let bounds = tree
                                .nodes
                                .iter()
                                .find(|(id, _)| *id == save)
                                .unwrap()
                                .1
                                .bounds()
                                .unwrap();
                            assert!(
                                bounds.y0 >= 44.0
                                    && bounds.y1 <= f64::from(size.y)
                                    && bounds.x0 >= 176.0
                                    && bounds.x1 <= f64::from(size.x),
                                "the focused save control stays visible: {bounds:?}"
                            );
                            assert_eq!(
                                serde_json::to_value(&app.settings).unwrap(),
                                serde_json::to_value(&expected).unwrap(),
                                "only the selected saved preset changes; active processing keeps its precision: scene={scene}, activation={activation:?}, field={field}, zoom={zoom}, passes={passes}"
                            );
                            assert!(
                                app.preset_name.is_empty()
                                    && app.dirty_since.is_some()
                                    && app.engine.is_none()
                            );
                        }
                        draw_voice(
                            &mut app,
                            &ctx,
                            size,
                            vec![
                                key(activation, false, false, egui::Modifiers::NONE),
                                key(activation, true, false, egui::Modifiers::NONE),
                            ],
                        );
                        for _ in 0..4 {
                            draw_voice(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        let (field, node) = tree
                            .nodes
                            .iter()
                            .find(|(_, node)| node.role() == Role::TextInput)
                            .unwrap();
                        assert!(
                            egui::Popup::is_any_open(&ctx),
                            "releasing and pressing again in one batch opens Save"
                        );
                        assert_eq!(tree.focus, *field);
                        assert_eq!(node.value(), Some(""));
                        assert_eq!(
                            serde_json::to_value(&app.settings).unwrap(),
                            serde_json::to_value(&expected).unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes preset focus after edits and height-only resizing; needs Python with Pillow"]
    fn snapshot_preset_focus_after_edits_and_resizing() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let key_event = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers,
        };
        let key = |key, modifiers| {
            [true, false]
                .into_iter()
                .map(|pressed| key_event(key, pressed, false, modifiers))
                .collect()
        };
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for zoom in [1.0, 1.25, 1.5] {
                for state in [
                    "resize-save",
                    "resize-chip",
                    "saved",
                    "overwritten",
                    "deleted",
                ] {
                    let mut app = routing_app(false);
                    app.settings.presets = (0..20)
                        .map(|index| config::Preset {
                            name: format!(
                                "Studio {index} — evening meetings and remote interviews "
                            )
                            .repeat(4),
                            processing: Default::default(),
                        })
                        .collect();
                    app.settings.strength = 0.71347;
                    app.settings.highpass_hz = 132.456;
                    app.preset_name =
                        "New studio — evening meetings and remote interviews ".repeat(4);
                    if state == "overwritten" {
                        let preset = app.settings.presets.last_mut().unwrap();
                        preset.name = preset.name.trim().into();
                        app.preset_name = preset.name.replace("Studio", "STUDIO");
                    }
                    let mut expected = app.settings.clone();
                    if state == "saved" {
                        expected.presets.push(config::Preset {
                            name: app.preset_name.trim().into(),
                            processing: app.settings.processing(),
                        });
                    } else if state == "overwritten" {
                        *expected.presets.last_mut().unwrap() = config::Preset {
                            name: app.preset_name.trim().into(),
                            processing: app.settings.processing(),
                        };
                    } else if state == "deleted" {
                        expected.presets.pop();
                    }
                    let label = if state == "deleted" || state == "resize-chip" {
                        format!(
                            "Apply saved preset: {}",
                            app.settings.presets.last().unwrap().name
                        )
                    } else {
                        "Save preset".to_owned()
                    };
                    let resize = state.starts_with("resize");
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    let initial = if resize {
                        egui::vec2(700.0, 880.0) / zoom
                    } else {
                        size
                    };
                    let ctx = egui::Context::default();
                    ctx.enable_accesskit();
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, initial, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, initial, Vec::new());
                    let target = *output
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| node.label() == Some(label.as_str()))
                        .map(|(id, _)| id)
                        .unwrap();
                    let input = |size, events| egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    };
                    let mut inputs: Vec<_> = (0..3).map(|_| input(initial, Vec::new())).collect();
                    inputs.push(input(
                        initial,
                        vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node: target,
                            data: None,
                        })],
                    ));
                    inputs.extend((0..8).map(|_| input(initial, Vec::new())));
                    if !resize {
                        inputs.push(input(
                            size,
                            key(
                                if state == "deleted" {
                                    egui::Key::F10
                                } else {
                                    egui::Key::Enter
                                },
                                if state == "deleted" {
                                    egui::Modifiers::SHIFT
                                } else {
                                    egui::Modifiers::NONE
                                },
                            ),
                        ));
                        inputs.extend((0..4).map(|_| input(size, Vec::new())));
                        let activation = if zoom == 1.25 || state == "overwritten" {
                            egui::Key::Space
                        } else {
                            egui::Key::Enter
                        };
                        if activation == egui::Key::Space && state != "deleted" {
                            inputs.push(input(size, key(egui::Key::Tab, egui::Modifiers::NONE)));
                            inputs.extend((0..3).map(|_| input(size, Vec::new())));
                        }
                        inputs.push(input(
                            size,
                            vec![key_event(activation, true, false, egui::Modifiers::NONE)],
                        ));
                        inputs.extend((0..8).map(|_| input(size, Vec::new())));
                        for _ in 0..3 {
                            let mut events =
                                vec![key_event(activation, true, true, egui::Modifiers::NONE)];
                            if activation == egui::Key::Space {
                                events.push(egui::Event::Text(" ".into()));
                            }
                            inputs.push(input(size, events));
                        }
                        inputs.push(input(
                            size,
                            vec![key_event(activation, false, false, egui::Modifiers::NONE)],
                        ));
                    }
                    inputs.extend((0..8).map(|_| input(size, Vec::new())));
                    let image = super::super::snapshot::snapshot_with_inputs(
                        &mut app,
                        &format!(
                            "voice-preset-reflow-{state}-{}-{theme}",
                            (zoom * 100.0) as u32
                        ),
                        size,
                        zoom,
                        dark,
                        inputs,
                    );
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(
                        serde_json::to_value(&app.settings).unwrap(),
                        serde_json::to_value(expected).unwrap()
                    );
                    assert_eq!(app.dirty_since.is_some(), !resize);
                    assert!(app.engine.is_none());
                    if state == "saved" || state == "overwritten" {
                        assert!(app.preset_name.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes preset chip and keyboard menu screenshots; needs Python with Pillow"]
    fn snapshot_voice_presets_and_keyboard_menus() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let name =
            "Studio — evening meetings and remote interviews with the microphone on the desk "
                .repeat(4);
        let key = |key, pressed, repeat, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers,
        };
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for zoom in [1.0, 1.25, 1.5] {
                for state in ["chips", "save", "save-space", "save-empty", "delete"] {
                    let mut app = routing_app(false);
                    app.settings.presets = (0..8)
                        .map(|index| config::Preset {
                            name: if index == 7 {
                                name.clone()
                            } else {
                                format!("Studio {index}")
                            },
                            processing: config::Processing {
                                input_gain_db: index as f32 + 0.12345,
                                ..Default::default()
                            },
                        })
                        .collect();
                    let draft = if state == "save-empty" {
                        ""
                    } else {
                        "Café studio"
                    };
                    app.preset_name = draft.into();
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let saving = state.starts_with("save");
                    let ctx = egui::Context::default();
                    ctx.enable_accesskit();
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    for _ in 0..3 {
                        draw_voice(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let target = *output
                        .platform_output
                        .accesskit_update
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            if saving {
                                node.label() == Some("Save preset")
                            } else {
                                node.label().is_some_and(|label| label.contains(&name))
                            }
                        })
                        .map(|(id, _)| id)
                        .unwrap();
                    let mut events =
                        vec![vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus,
                            target_tree: TreeId::ROOT,
                            target_node: target,
                            data: None,
                        })]];
                    events.extend(std::iter::repeat_n(Vec::new(), 8));
                    if saving {
                        let activation = if state == "save-space" || (state == "save-empty" && dark)
                        {
                            egui::Key::Space
                        } else {
                            egui::Key::Enter
                        };
                        let mut opening = vec![key(activation, true, false, egui::Modifiers::NONE)];
                        if activation == egui::Key::Space {
                            opening.push(egui::Event::Text(" ".into()));
                        }
                        events.push(opening);
                        events.extend(std::iter::repeat_n(Vec::new(), 4));
                        for _ in 0..3 {
                            let mut repeat =
                                vec![key(activation, true, true, egui::Modifiers::NONE)];
                            if activation == egui::Key::Space {
                                repeat.push(egui::Event::Text(" ".into()));
                            }
                            events.push(repeat);
                        }
                        events.push(vec![key(activation, false, false, egui::Modifiers::NONE)]);
                    } else if state == "delete" {
                        events.push(
                            [true, false]
                                .into_iter()
                                .map(|pressed| {
                                    key(egui::Key::F10, pressed, false, egui::Modifiers::SHIFT)
                                })
                                .collect(),
                        );
                    }
                    let path = super::super::snapshot::snapshot_with_events(
                        &mut app,
                        &format!("voice-presets-{state}-{}-{theme}", (zoom * 100.0) as u32),
                        size,
                        zoom,
                        dark,
                        events,
                    );
                    println!("SNAPSHOT {}", path.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert_eq!(
                        app.preset_name, draft,
                        "opening and holding the activation key preserves the draft"
                    );
                    assert!(app.dirty_since.is_none() && app.engine.is_none());
                }
            }
        }
    }

    #[test]
    fn selecting_a_route_reports_a_change() {
        let ctx = egui::Context::default();
        let pool: Vec<String> = ["First mic", "Second mic"].map(String::from).into();
        let mut value = pool[0].clone();
        let mut frame = |events| {
            let mut response = None;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760.0, 880.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    response = Some(combo(ui, 0, &mut value, &pool));
                },
            );
            response.unwrap()
        };
        let response = frame(Vec::new());
        let popup_id = response.id.with("popup");
        egui::Popup::open_id(&ctx, popup_id);
        frame(Vec::new());
        // Let the popup finish its sizing pass before interacting with it.
        frame(Vec::new());
        let menu = egui::AreaState::load(&ctx, popup_id).unwrap().rect();
        let pos = egui::pos2(menu.left() + 20.0, menu.bottom() - 12.0);
        frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let response = frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(value, pool[1]);
        assert!(
            response.changed(),
            "routing must save and restart when a device is selected"
        );
    }
}
