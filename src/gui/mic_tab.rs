//! Voice page: first-run checklist, routing (which devices your voice
//! comes from and goes to), presets and the mic test, and each setting as a
//! row that shows what it does.

use std::time::Duration;

use eframe::egui::{self, ComboBox, RichText};

use super::routes::{cable_input, VB_CABLE_URL};
use super::{open_url, App, AMBER, CYAN, GREEN, MUTED, VIOLET};
use crate::config;
use crate::denoise::ModelState;
use crate::engine::Engine;
use crate::viz;
use crate::widgets;

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
                hide = ui.small_button("Hide").on_hover_text("Put the checklist away").clicked();
                widgets::badge(ui, &format!("{done} of {}", steps.len()), if done == steps.len() { GREEN } else { VIOLET });
            },
            |ui| {
                // The first step not yet done is the one to do now.
                let current = steps.iter().position(|(_, done)| !done);
                for (i, (step, done)) in steps.into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        step_mark(ui, i + 1, done);
                        let active = current == Some(i);
                        let title = RichText::new(step_title(step, self.settings.default_mic));
                        ui.label(if done { title.weak() } else if active { title.strong() } else { title });
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
                if ui.small_button("Get VB-Cable").clicked() {
                    open_url(VB_CABLE_URL);
                }
                ui.weak("run its setup as administrator");
            }
            Step::Microphone => {
                ui.weak("pick it under Routing below");
            }
            Step::Discord => {
                if ui.small_button("Done").on_hover_text("Discord > User Settings > Voice & Video").clicked() {
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

    // ---- routing -------------------------------------------------------------

    /// Discord hears OpenMic through VB-Cable. The checklist covers a missing
    /// VB-Cable; this covers a wrong output and the default-mic setting.
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
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("VB-Cable isn't installed: Discord needs it to hear OpenMic.")
                                    .color(AMBER),
                            );
                            if ui.button("Get VB-Cable").clicked() {
                                open_url(VB_CABLE_URL);
                            }
                        });
                        ui.weak("Run its setup as administrator; OpenMic switches to it automatically.");
                    }
                    Some(cable) if self.settings.output != cable => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Discord can't hear this output.").color(AMBER));
                            if ui.button("Use VB-Cable").clicked() {
                                self.settings.output = cable;
                                self.touch();
                                if self.running() {
                                    self.restart_engine();
                                }
                            }
                        });
                    }
                    Some(_) => {
                        let changed = ui
                            .checkbox(
                                &mut self.settings.default_mic,
                                "Make CABLE Output my Windows default mic while running",
                            )
                            .on_hover_text(
                                "Apps set to the default microphone (Discord's default) hear your \
                                 cleaned voice. Your previous default comes back when OpenMic stops.",
                            )
                            .changed();
                        if changed {
                            self.touch();
                        }
                        ui.weak(if self.settings.default_mic {
                            "In Discord: leave Voice & Video > Input Device on Default"
                        } else {
                            "In Discord: Voice & Video > Input Device > CABLE Output"
                        });
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
                    widgets::badge(ui, "VB-Cable connected", GREEN);
                } else {
                    widgets::badge(ui, "VB-Cable not installed", AMBER);
                }
            },
            |ui| {
                let mut changed = false;
                egui::Grid::new("routing").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    for (label, id, field, pool) in [
                        ("Microphone", 0, &mut self.settings.microphone, &self.inputs),
                        ("Processed output", 1, &mut self.settings.output, &self.outputs),
                        ("Monitor output", 2, &mut self.settings.monitor_output, &self.outputs),
                    ] {
                        ui.label(RichText::new(label).weak());
                        changed |= combo(ui, id, field, pool).changed();
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
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Preset").weak());
            for preset in &builtins {
                if ui.selectable_label(preset.processing == current, &preset.name).clicked() {
                    apply = Some(preset.processing.clone());
                }
            }
            for (i, preset) in self.settings.presets.iter().enumerate() {
                let chip = ui.selectable_label(preset.processing == current, &preset.name);
                if chip.clicked() {
                    apply = Some(preset.processing.clone());
                }
                chip.context_menu(|ui| {
                    if ui.button("Delete preset").clicked() {
                        delete = Some(i);
                        ui.close();
                    }
                });
            }
            ui.menu_button("+", |ui| {
                ui.label("Save your settings as a preset");
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.preset_name).hint_text("Preset name").desired_width(160.0),
                );
                let named = !self.preset_name.trim().is_empty();
                save = ui.add_enabled(named, egui::Button::new("Save")).clicked()
                    || (named && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if save {
                    ui.close();
                }
            })
            .response
            .on_hover_text("Save your current settings as a preset (right-click a saved one to delete it)");
        });
        if let Some(i) = delete {
            self.settings.presets.remove(i);
            self.touch();
        }
        if save {
            let name = self.preset_name.trim().to_owned();
            let preset = config::Preset { name: name.clone(), processing: current };
            match self.settings.presets.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
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
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if widgets::pill(ui, &mut self.settings.bypass, "Bypass", MUTED)
                .on_hover_text("Send your raw microphone, with none of the settings below")
                .changed()
            {
                self.apply_live();
            }
            self.draw_mic_test(ui);
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
                ui.weak("drag in a picture · scroll to fine-tune · double-click to reset");
            },
            |ui| {
                if bypassed {
                    ui.colored_label(AMBER, "Bypass is on: Discord hears your raw microphone.");
                }
                changed = viz::voice_rows(ui, &frames, &mut self.settings, &mut self.strength_before_off);
            },
        );
        if changed.settings {
            self.apply_live();
        }
        // The monitor device wasn't connected when processing started: open it now.
        if changed.monitor && self.settings.monitor && self.engine.as_ref().is_some_and(|e| !e.has_monitor()) {
            self.restart_engine();
        }
    }

    /// The footer's "Running · voice 42%", unless a warning is showing.
    pub(super) fn update_running_status(&mut self) {
        let Some(stats) = self.engine.as_ref().map(Engine::stats) else { return };
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
            ModelState::DeepFilterFailed => (format!("DeepFilterNet unavailable, using RNNoise · {voice}"), AMBER),
            ModelState::DeepFilter | ModelState::Rnnoise => (format!("Running · {voice}"), GREEN),
        };
    }
}

pub(super) fn combo(ui: &mut egui::Ui, id: usize, value: &mut String, pool: &[String]) -> egui::Response {
    let mut changed = false;
    let mut response = ComboBox::from_id_salt(id)
        .width(360.0)
        .selected_text(value.as_str())
        .show_ui(ui, |ui| {
            for name in pool {
                changed |= ui.selectable_value(value, name.clone(), name).changed();
            }
        })
        .response;
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
        let tick = [center + egui::vec2(-4.0, 0.0), center + egui::vec2(-1.0, 3.0), center + egui::vec2(4.5, -3.5)];
        painter.add(egui::Shape::line(tick.to_vec(), egui::Stroke::new(2.0, egui::Color32::from_gray(18))));
    } else {
        painter.circle_stroke(center, 8.0, egui::Stroke::new(1.0, MUTED));
        painter.text(center, egui::Align2::CENTER_CENTER, number.to_string(), egui::FontId::proportional(11.0), MUTED);
    }
}

fn step_title(step: Step, default_mic: bool) -> &'static str {
    match step {
        Step::Cable => "Install VB-Cable",
        Step::Microphone => "Choose your microphone",
        Step::Discord if default_mic => "In Discord, set Voice & Video > Input Device to Default",
        Step::Discord => "In Discord, set Voice & Video > Input Device to CABLE Output",
        Step::Test => "Test your mic: hear yourself raw and cleaned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(response.changed(), "routing must save and restart when a device is selected");
    }
}
