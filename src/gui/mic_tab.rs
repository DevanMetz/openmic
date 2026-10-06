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
                if ui.small_button("Get VB-Cable").on_hover_text("Run the installer as administrator").clicked() {
                    open_url(VB_CABLE_URL);
                }
                ui.weak("If installed, enable CABLE Input in Windows");
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
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("VB-Cable input isn't available.")
                                    .color(widgets::text_color(ui, AMBER)),
                            );
                            if ui.button("Get VB-Cable").clicked() {
                                open_url(VB_CABLE_URL);
                            }
                        });
                        ui.weak("If installed, enable CABLE Input in Windows. Otherwise, run VB-Cable's setup as administrator.");
                    }
                    Some(cable) if self.settings.output != cable => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("A different processed output is selected.").color(widgets::text_color(ui, AMBER)));
                            if ui.button("Use VB-Cable").clicked() {
                                self.settings.output = cable;
                                self.touch();
                                if self.running() {
                                    self.restart_engine();
                                }
                            }
                        });
                        ui.weak("For another virtual output, choose its matching input in Discord.");
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
                    widgets::badge(ui, "VB-Cable unavailable", AMBER);
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
                .on_hover_text("Skip noise reduction and both gates. Mic level, rumble filter, output level and mute still apply.")
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
                ui.weak("drag or scroll · Tab, then arrows · double-click to reset");
            },
            |ui| {
                if bypassed {
                    ui.colored_label(widgets::text_color(ui, AMBER), "Bypass is on: noise reduction and both gates are skipped.")
                        .on_hover_text("Mic level, rumble filter, output level and mute still apply.");
                }
                // Keep native switch and link IDs stable when other page sections change.
                changed = ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("voice-settings")), |ui| {
                    viz::voice_rows(ui, &frames, &mut self.settings, &mut self.strength_before_off)
                }).inner;
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
        painter.text(center, egui::Align2::CENTER_CENTER, number.to_string(), egui::FontId::proportional(11.0), widgets::text_color(ui, MUTED));
    }
}

fn step_title(step: Step, default_mic: bool) -> &'static str {
    match step {
        Step::Cable => "Set up VB-Cable",
        Step::Microphone => "Choose your microphone",
        Step::Discord if default_mic => "In Discord, set Voice & Video > Input Device to Default",
        Step::Discord => "In Discord, set Voice & Video > Input Device to CABLE Output",
        Step::Test => "Test your mic: hear yourself raw and cleaned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw_voice(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    #[test]
    fn assistive_focus_and_keyboard_activation_change_only_the_requested_control() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for (old_role, old_label, new_role, new_label, key, field) in [
                    (Role::Button, "Soundboard", Role::CheckBox, "Remove noise", egui::Key::Space, "noise"),
                    (Role::CheckBox, "Remove rumble", Role::CheckBox, "Remove noise", egui::Key::Space, "noise"),
                    (Role::CheckBox, "Remove noise", Role::CheckBox, "Remove rumble", egui::Key::Space, "rumble"),
                    (Role::CheckBox, "Remove rumble", Role::Link, "DeepFilterNet", egui::Key::Enter, "model"),
                    (Role::Link, "DeepFilterNet", Role::CheckBox, "Only my voice", egui::Key::Enter, "voice"),
                    (Role::CheckBox, "Only my voice", Role::Link, "DeepFilterNet", egui::Key::Space, "model"),
                ] {
                    let mut app = App::stopped(config::Settings { strength: 0.74321, highpass_hz: 137.25, monitor_volume: 0.43561, ..Default::default() });
                    app.settings.setup.dismissed = true;
                    let page = app.page;
                    let original = app.settings.processing();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    let size = egui::vec2(760.0, 880.0);
                    for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let [old, new] = [(old_role, old_label), (new_role, new_label)].map(|(role, label)| *tree.nodes.iter()
                        .find(|(_, node)| node.role() == role && node.label() == Some(label)).map(|(id, _)| id).unwrap());
                    let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                    });
                    draw_voice(&mut app, &ctx, size, vec![focus(old)]);
                    for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, old);
                    assert!(app.dirty_since.is_none());
                    draw_voice(&mut app, &ctx, size, vec![focus(new), egui::Event::Key {
                        key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }]);
                    for _ in 0..7 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                    let output = draw_voice(&mut app, &ctx, size, Vec::new());
                    let mut expected = original;
                    match field {
                        "noise" => expected.strength = 0.0,
                        "rumble" => expected.highpass = !expected.highpass,
                        "model" => expected.model = crate::dsp::Model::Rnnoise,
                        "voice" => expected.voice_gate = !expected.voice_gate,
                        _ => unreachable!(),
                    }
                    assert_eq!(app.page, page, "the key must not activate the old {old_label} sidebar button");
                    assert_eq!(app.settings.processing(), expected, "{key:?} belongs only to the requested {new_label}");
                    assert_eq!(app.settings.monitor_volume, 0.43561);
                    assert!(!app.settings.monitor);
                    assert!(app.dirty_since.is_some());
                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, new);
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
                let mut app = App::stopped(config::Settings { strength: 0.74321, ..Default::default() });
                app.settings.setup.dismissed = true;
                let page = app.page;
                let original = app.settings.processing();
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let size = egui::vec2(760.0, 880.0);
                for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let [old, new] = [(role, label), (Role::CheckBox, "Remove noise")].map(|(role, label)| *tree.nodes.iter()
                    .find(|(_, node)| node.role() == role && node.label() == Some(label)).map(|(id, _)| id).unwrap());
                let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                });
                let key = |key| egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                };
                draw_voice(&mut app, &ctx, size, vec![focus(old)]);
                for _ in 0..2 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                let movement = modifiers.map_or_else(|| focus(new), |modifiers| egui::Event::Key {
                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
                });
                draw_voice(&mut app, &ctx, size, vec![movement, key(egui::Key::Space)]);
                assert_eq!(app.settings.processing(), original, "the command waits for focus");
                draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Escape)]);
                for _ in 0..4 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                assert_eq!(app.settings.processing(), original, "Escape discards the unseen activation");
                assert_eq!(app.page, page);
                assert!(app.dirty_since.is_none());
                draw_voice(&mut app, &ctx, size, vec![focus(new), key(egui::Key::Space)]);
                for _ in 0..5 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                let mut expected = original;
                expected.strength = 0.0;
                assert_eq!(app.settings.processing(), expected, "a fresh command still works after cancellation");
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
                        (Role::Link, "DeepFilterNet", Role::CheckBox, "Only my voice", false, "voice"),
                        (Role::CheckBox, "Only my voice", Role::Link, "DeepFilterNet", true, "model"),
                        (Role::Slider, "Remove rumble", Role::CheckBox, "Remove noise", false, "noise"),
                        (Role::Slider, "Only my voice", Role::CheckBox, "Quiet cut-off", false, "gate"),
                        (Role::Slider, "Quiet cut-off", Role::CheckBox, "Quiet cut-off", true, "gate"),
                        (Role::CheckBox, "Remove rumble", Role::Slider, "Your mic", true, "none"),
                        (Role::Link, "DeepFilterNet", Role::CheckBox, "Only my voice", false, "both"),
                        (Role::CheckBox, "Only my voice", Role::Link, "DeepFilterNet", true, "both"),
                    ] {
                        let mut app = App::stopped(config::Settings {
                            strength: 0.74321, highpass_hz: 137.25, voice_threshold: 0.61347, monitor_volume: 0.43561,
                            ..Default::default()
                        });
                        app.settings.setup.dismissed = true;
                        let page = app.page;
                        let original = app.settings.processing();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        let size = egui::vec2(760.0, 880.0);
                        for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let [old, new] = [(old_role, old_label), (new_role, new_label)].map(|(role, label)| *tree.nodes.iter()
                            .find(|(_, node)| node.role() == role && node.label() == Some(label)).map(|(id, _)| id).unwrap());
                        draw_voice(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: old, data: None,
                        })]);
                        for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                        let mut clicked = Vec::new();
                        let command = |pressed| egui::Event::Key {
                            key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                        };
                        let mut events = if field == "both" { vec![command(true), command(false)] } else { Vec::new() };
                        events.extend([
                            egui::Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false,
                                modifiers: if backwards { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE } },
                            command(true),
                        ]);
                        for _ in 0..8 {
                            let output = draw_voice(&mut app, &ctx, size, std::mem::take(&mut events));
                            clicked.extend(output.platform_output.events.iter().filter_map(|event| match event {
                                egui::output::OutputEvent::Clicked(info) => info.label.clone(),
                                _ => None,
                            }));
                        }
                        let mut expected = original;
                        match field {
                            "voice" => expected.voice_gate = !expected.voice_gate,
                            "model" => expected.model = crate::dsp::Model::Rnnoise,
                            "noise" => expected.strength = 0.0,
                            "gate" => expected.gate = !expected.gate,
                            "both" => { expected.model = crate::dsp::Model::Rnnoise; expected.voice_gate = !expected.voice_gate; }
                            "none" => {},
                            _ => unreachable!(),
                        }
                        assert_eq!(app.settings.processing(), expected,
                            "{key:?} after Tab belongs to {new_label}; backwards={backwards}, passes={passes}");
                        let expected_clicks = if field == "both" { vec![old_label, new_label] } else { vec![new_label] };
                        assert_eq!(clicked, expected_clicks, "each control reports its activation once, in input order");
                        assert_eq!(app.page, page);
                        assert_eq!(app.settings.monitor_volume, 0.43561);
                        assert!(!app.settings.monitor);
                        assert_eq!(app.dirty_since.is_some(), field != "none");
                        let output = draw_voice(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, new);
                    }
                }
            }
        }
    }

    #[test]
    fn voice_keyboard_controls_scroll_into_view_and_apply_batched_input() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let controls = [
            (Role::Slider, "Your mic"), (Role::Slider, "Remove rumble"), (Role::Slider, "Remove noise"),
            (Role::Slider, "Only my voice"), (Role::Slider, "Quiet cut-off"), (Role::Slider, "To Discord"),
            (Role::Slider, "Hear yourself"), (Role::CheckBox, "Remove rumble"), (Role::CheckBox, "Remove noise"),
            (Role::CheckBox, "Only my voice"), (Role::CheckBox, "Quiet cut-off"), (Role::CheckBox, "Hear yourself"),
            (Role::Link, "DeepFilterNet"),
        ];
        for dark in [true, false] {
            let mut app = App::stopped(config::Settings::default());
            app.settings.setup.dismissed = true;
            app.settings.input_gain_db = 3.37;
            app.settings.highpass_hz = 137.25;
            app.settings.monitor_volume = 0.43561;
            let original = app.settings.processing();
            let ctx = egui::Context::default();
            ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            let size = egui::vec2(700.0, 420.0);
            for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
            let mut visited = std::collections::HashSet::new();
            let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
            for _ in 0..80 {
                let output = draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Tab, egui::Modifiers::NONE)]);
                if ctx.memory(|memory| memory.focused()).is_none() { continue; }
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let node = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
                let role = node.role();
                if !node.label().is_some_and(|label| controls.contains(&(role, label))) { continue; }
                let name = node.label().unwrap().to_owned();
                for _ in 0..6 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                let node = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
                assert_eq!(node.label(), Some(name.as_str()));
                assert_eq!(node.role(), role);
                let bounds = node.bounds().unwrap();
                let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect) && match &shape.shape {
                    egui::Shape::Rect(outline) if role != Role::Link => outline.rect == rect && outline.stroke.width == 2.0,
                    egui::Shape::Text(link) if role == Role::Link => link.galley.text() == name && link.underline.width > 0.0,
                    _ => false,
                }), "the focused {role:?} {name} and its focus indicator are fully visible");
                visited.insert((role, name));
                if visited.len() == controls.len() { break; }
            }
            assert_eq!(visited, controls.into_iter().map(|(role, label)| (role, label.to_owned())).collect());
            assert_eq!(app.settings.processing(), original);
            assert_eq!(app.settings.monitor_volume, 0.43561);
            assert!(app.dirty_since.is_none(), "Tab preserves saved values");
            for _ in 0..2 {
                draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Tab, egui::Modifiers::SHIFT)]);
                for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
            }
            let output = draw_voice(&mut app, &ctx, size, Vec::new());
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            assert_eq!(tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1.label(), Some("To Discord"));
            let rumble_switch = *tree.nodes.iter().find(|(_, node)| node.role() == Role::CheckBox && node.label() == Some("Remove rumble"))
                .map(|(id, _)| id).unwrap();
            draw_voice(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::Focus, target_tree: TreeId::ROOT, target_node: rumble_switch, data: None,
            })]);
            for _ in 0..2 { draw_voice(&mut app, &ctx, size, Vec::new()); }
            draw_voice(&mut app, &ctx, size, vec![key(egui::Key::Tab, egui::Modifiers::NONE), key(egui::Key::ArrowUp, egui::Modifiers::NONE)]);
            for _ in 0..6 { draw_voice(&mut app, &ctx, size, Vec::new()); }
            let mut expected = original;
            expected.highpass_hz = 138.0;
            assert_eq!(app.settings.processing(), expected, "a batched Tab and arrow edits the new control");
            assert_eq!(app.settings.monitor_volume, 0.43561);
            assert!(app.dirty_since.is_some(), "keyboard edits enter the normal settings save path");
        }
    }

    #[test]
    #[ignore]
    fn snapshot_voice_keyboard_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (role, label, name, size, action) in [
            (Role::Slider, "Your mic", "voice-keyboard", egui::vec2(760.0, 880.0), Action::Focus),
            (Role::Slider, "Hear yourself", "voice-keyboard-small", egui::vec2(700.0, 420.0), Action::Focus),
            (Role::CheckBox, "Remove rumble", "voice-switch", egui::vec2(760.0, 880.0), Action::Focus),
            (Role::CheckBox, "Hear yourself", "voice-switch-small", egui::vec2(700.0, 420.0), Action::Focus),
            (Role::Link, "DeepFilterNet", "voice-model-small", egui::vec2(700.0, 420.0), Action::Focus),
            (Role::CheckBox, "Remove noise", "voice-noise-off", egui::vec2(760.0, 880.0), Action::Click),
        ] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                let mut app = App::stopped(config::Settings::default());
                app.settings.setup.dismissed = true;
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                for _ in 0..3 { draw_voice(&mut app, &ctx, size, Vec::new()); }
                let output = draw_voice(&mut app, &ctx, size, Vec::new());
                let target_node = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| node.role() == role && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                let mut events = vec![vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action, target_tree: TreeId::ROOT, target_node, data: None,
                })]];
                events.extend(vec![Vec::new(); 6]);
                let path = super::super::snapshot::snapshot_with_events(&mut app, &format!("{name}-{theme}"), size, 1.5, dark, events);
                if action == Action::Click { assert_eq!(app.settings.strength, 0.0); }
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
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let mut frame = eframe::Frame::_new_kittest();
                let mut draw = |app: &mut App, events| ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 880.0))),
                    events, ..Default::default()
                }, |ui| app.ui(ui, &mut frame));
                for _ in 0..3 { draw(&mut app, Vec::new()); }
                let output = draw(&mut app, Vec::new());
                let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "+" => Some(text.visual_bounding_rect().center()),
                    _ => None,
                }).unwrap();
                for pressed in [true, false] {
                    draw(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                    }]);
                }
                for _ in 0..3 { draw(&mut app, Vec::new()); }
                let output = draw(&mut app, Vec::new());
                let save = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| node.label() == Some("Save")).map(|(id, _)| id).unwrap();
                draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: save, data: None,
                })]);
                for _ in 0..2 { draw(&mut app, Vec::new()); }
                let key = |key, modifiers| egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers,
                };
                draw(&mut app, vec![
                    key(egui::Key::Tab, egui::Modifiers::SHIFT),
                    key(egui::Key::A, egui::Modifiers::COMMAND),
                    egui::Event::Text("Studio".into()),
                    key(egui::Key::ArrowLeft, egui::Modifiers::NONE),
                    egui::Event::Text("X".into()),
                    key(egui::Key::Enter, egui::Modifiers::NONE),
                ]);
                for _ in 0..4 { draw(&mut app, Vec::new()); }
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
