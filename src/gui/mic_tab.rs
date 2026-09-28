//! Microphone tab: first-run checklist, routing (with the headphone
//! monitor), processing (Simple or Advanced, with presets and a mic test),
//! and the live scope with its mixer strip.

use std::time::Duration;

use eframe::egui::{self, ComboBox, RichText};

use super::routes::{cable_input, VB_CABLE_URL};
use super::{open_url, volume_slider, App, AMBER, CYAN, GREEN, MUTED, RED, VIOLET};
use crate::config::{self, Processing};
use crate::denoise::ModelState;
use crate::dsp::Model;
use crate::engine::Engine;
use crate::viz::{self, Focus};
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
        self.draw_processing(ui);
        ui.add_space(8.0);
        self.draw_scope(ui);
        self.update_running_status();
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
                        let (mark, color) = if done { ("✔".to_owned(), GREEN) } else { (format!("{}", i + 1), MUTED) };
                        ui.add_sized([18.0, 18.0], egui::Label::new(RichText::new(mark).strong().color(color)));
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
        let mut reopen = false;
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
                    // The headphone monitor plays on the monitor output.
                    ui.label("");
                    ui.horizontal(|ui| {
                        let toggled = widgets::pill(ui, &mut self.settings.monitor, "Headphone monitor", CYAN)
                            .on_hover_text("Hear the processed voice yourself (use headphones)")
                            .changed();
                        // The monitor device wasn't connected when processing
                        // started; open it now.
                        reopen = toggled
                            && self.settings.monitor
                            && self.engine.as_ref().is_some_and(|e| !e.has_monitor());
                        ui.add_space(8.0);
                        if volume_slider(ui, &mut self.settings.monitor_volume) || toggled {
                            self.apply_live();
                        }
                    });
                    ui.end_row();
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
        if reopen {
            self.restart_engine();
        }
    }

    // ---- processing ----------------------------------------------------------

    fn draw_processing(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        // Which setting the pointer is on, so the scope can highlight it.
        let mut focus = None;
        let mut reset = false;
        let advanced = self.settings.advanced;
        let mut view = advanced;
        widgets::card(
            ui,
            "PROCESSING",
            GREEN,
            |ui| {
                reset = ui
                    .small_button("Reset")
                    .on_hover_text("Restore every processing setting to its default")
                    .clicked();
                widgets::segmented(ui, &mut view, &[(false, "Simple", ""), (true, "Advanced", "")], GREEN);
            },
            |ui| {
                if advanced {
                    ui.horizontal(|ui| {
                        let (picked, hovered) = widgets::segmented(
                            ui,
                            &mut self.settings.model,
                            &[
                                (Model::DeepFilter, "DeepFilterNet 3", "best"),
                                (Model::Rnnoise, "RNNoise", "light"),
                            ],
                            GREEN,
                        );
                        changed |= picked;
                        if hovered {
                            focus = Some(Focus::Model);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            changed |= self.presets_menu(ui);
                        });
                    });
                    ui.add_space(6.0);
                } else {
                    // A starting point and one control; the rest is under Advanced.
                    ui.horizontal(|ui| {
                        changed |= self.presets_menu(ui);
                        ui.add_space(12.0);
                        ui.label("Noise reduction");
                        let r = ui.add(
                            egui::Slider::new(&mut self.settings.strength, 0.0..=1.0)
                                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                        );
                        let notches = viz::wheel_notches(ui, r.id, r.hovered() && !r.dragged());
                        changed |= r.changed() | viz::step_by(&mut self.settings.strength, notches, 0.01, 0.0..=1.0);
                        if r.hovered() || r.dragged() {
                            focus = Some(Focus::Reduction);
                        }
                    });
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    let s = &mut self.settings;
                    let mut pills = vec![];
                    if advanced {
                        pills.extend([
                            (&mut s.voice_gate, "Voice gate", VIOLET, Some(Focus::VoiceGate),
                                "Silence everything that isn't speech, however loud"),
                            (&mut s.highpass, "Rumble filter", AMBER, Some(Focus::Rumble),
                                "Cut low rumble: desk bumps, hum, handling noise"),
                            (&mut s.gate, "Level gate", AMBER, Some(Focus::LevelGate),
                                "Fade out anything quieter than a set level"),
                        ]);
                    }
                    pills.extend([
                        (&mut s.bypass, "Bypass", MUTED, None, "Send your raw microphone, unprocessed"),
                        (&mut s.mute, "Mute mic", RED, None, "Silence your voice; the soundboard still plays"),
                    ]);
                    for (on, text, color, target, tip) in pills {
                        let r = widgets::pill(ui, on, text, color).on_hover_text(tip);
                        if r.hovered() {
                            focus = target.or(focus);
                        }
                        changed |= r.changed();
                    }
                });
                ui.add_space(6.0);
                self.draw_mic_test(ui);
            },
        );
        if view != self.settings.advanced {
            self.settings.advanced = view;
            self.touch();
        }
        if reset {
            let s = &mut self.settings;
            s.apply_processing(&Processing::default());
            s.bypass = false;
            s.mute = false;
            s.monitor_volume = 1.0;
            changed = true;
        }
        if changed {
            self.apply_live();
        }
        self.focus = focus;
    }

    // ---- scope ---------------------------------------------------------------

    fn draw_scope(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.engine.as_ref().map(Engine::stats);
        let mut pinned = self.settings.overlay;
        let advanced = self.settings.advanced;
        widgets::card(
            ui,
            "LIVE SCOPE",
            CYAN,
            |ui| {
                ui.checkbox(&mut pinned, "Pin meter to screen").on_hover_text(
                    "A small level meter that stays on top of other windows, even with OpenMic in the tray",
                );
            },
            |ui| {
                let frames = self.engine.as_ref().map(Engine::scope).unwrap_or_default();
                // A slider under the pointer highlights what it changes.
                let focus = self.focus.or(self.slider_focus);
                let mut changed = false;
                ui.horizontal_top(|ui| {
                    let gap = 12.0;
                    let scope_width = (ui.available_width() - viz::SLIDERS_WIDTH - gap).max(320.0);
                    ui.vertical(|ui| {
                        ui.set_width(scope_width);
                        changed |= viz::scope(ui, &frames, &mut self.settings, focus, advanced);
                    });
                    ui.add_space(gap - ui.spacing().item_spacing.x);
                    ui.vertical(|ui| {
                        let levels = [snapshot.map(|s| s.in_peak), snapshot.map(|s| s.out_peak)];
                        let (slid, pointed) =
                            viz::gain_sliders(ui, &mut self.settings, levels, &mut self.hold, advanced);
                        changed |= slid;
                        self.slider_focus = pointed;
                    });
                });
                if changed {
                    self.apply_live();
                }
            },
        );
        if pinned != self.settings.overlay {
            self.settings.overlay = pinned;
            self.touch();
        }
    }

    /// The footer's "Running · voice 42%", unless a warning is showing.
    fn update_running_status(&mut self) {
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

    /// The preset picker. Returns whether a preset was applied.
    fn presets_menu(&mut self, ui: &mut egui::Ui) -> bool {
        let current = self.settings.processing();
        let builtins = config::builtin_presets();
        let label = builtins
            .iter()
            .chain(&self.settings.presets)
            .find(|p| p.processing == current)
            .map_or("Custom", |p| p.name.as_str())
            .to_owned();
        let mut apply = None;
        let mut delete = None;
        let mut save = false;
        ui.menu_button(format!("Preset: {label}"), |ui| {
            for preset in &builtins {
                if ui.button(&preset.name).clicked() {
                    apply = Some(preset.processing.clone());
                }
            }
            if !self.settings.presets.is_empty() {
                ui.separator();
                ui.weak("Saved");
                for (i, preset) in self.settings.presets.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.button(&preset.name).clicked() {
                            apply = Some(preset.processing.clone());
                        }
                        if ui.small_button("×").on_hover_text("Delete this preset").clicked() {
                            delete = Some(i);
                        }
                    });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.preset_name)
                        .hint_text("Preset name")
                        .desired_width(140.0),
                );
                let named = !self.preset_name.trim().is_empty();
                save = ui.add_enabled(named, egui::Button::new("Save current")).clicked()
                    || (named && field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            });
        })
        .response
        .on_hover_text("Apply a starting point, or save your own settings");

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
            ui.close();
            self.touch();
        }
        match apply {
            Some(processing) => {
                self.settings.apply_processing(&processing);
                true
            }
            None => false,
        }
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
