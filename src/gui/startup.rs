//! Keep startup accessible and recover failed settings loads before starting audio.

use eframe::egui::{self, RichText};

use super::{App, Command, DefaultMicSync, RED};
use crate::widgets;

impl App {
    pub(super) fn set_startup_visibility(&mut self, ctx: &egui::Context, requested_hidden: bool) {
        // A collapsed preference is not enough: Windows startup suppresses
        // the strip, so a failed tray needs the main window as its fallback.
        self.hidden = requested_hidden
            && self.settings.load_error.is_none()
            && (self.tray.is_some() || self.strip_visible());
        if requested_hidden && !self.hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
    }

    pub(super) fn draw_settings_recovery(&mut self, ui: &mut egui::Ui) {
        let Some(error) = &self.settings.load_error else { return };
        let (mut retry, mut open_folder, mut quit) = (false, false, false);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading("OpenMic");
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                widgets::card(ui, "SETTINGS NEED ATTENTION", RED, |_| {}, |ui| {
                    ui.label("OpenMic couldn't load your settings. Audio is stopped and your settings file is unchanged.");
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(RichText::new(error.path.display().to_string()).monospace()).wrap());
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new(&error.message).color(RED)).wrap());
                    ui.add_space(8.0);
                    ui.label("Close any program locking the file, or fix or restore it, then retry. To start fresh, move the file to a backup location first.");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        retry = ui.button("Retry loading").clicked();
                        open_folder = ui.button("Open settings folder").clicked();
                        quit = ui.button("Quit OpenMic").clicked();
                    });
                });
            });
        });
        if open_folder && let Some(folder) = error.path.parent() {
            super::open_url(&folder.display().to_string());
        }
        if retry {
            self.retry_settings(ui.ctx());
        }
        if quit { self.run_command(ui.ctx(), Command::Quit); }
    }

    fn retry_settings(&mut self, ctx: &egui::Context) {
        let loaded = self.settings.retry_load();
        if loaded.load_error.is_some() {
            self.settings.load_error = loaded.load_error;
            return;
        }
        self.settings = loaded;
        self.dirty_since = None;
        self.save_error = None;
        self.default_mic_sync = DefaultMicSync::default();
        self.window_placement = Default::default();
        self.overlay_placement = Default::default();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(crate::initial_size(self.settings.window_size).into()));
        self.initialize_settings();
        // A retry is an explicit interaction in the main window; keep it visible.
        self.run_command(ctx, Command::Show);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, SettingsLoadError};
    use eframe::App as _;

    fn failed_app() -> App {
        App::stopped(Settings {
            load_error: Some(SettingsLoadError {
                path: "C:/Users/Example/AppData/Roaming/OpenMic/config/settings.json".into(),
                message: "Parse settings: expected a value at line 12 column 3".into(),
            }),
            ..Default::default()
        })
    }

    fn draw(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    fn label(output: &egui::FullOutput, name: &str) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(text.visual_bounding_rect()),
            _ => None,
        })
    }

    #[test]
    fn a_failed_tray_requires_a_visible_strip_before_starting_hidden() {
        for (collapsed, always_show_strip, suppressed, requested_hidden, hidden) in [
            (false, false, true, true, false),
            (true, false, true, true, false),
            (false, true, true, true, false),
            (true, true, true, true, false),
            (true, false, false, true, true),
            (false, true, false, true, true),
            (false, false, false, true, false),
            (true, true, false, false, false),
        ] {
            let mut app = App::stopped(Settings { collapsed, always_show_strip, ..Settings::default() });
            app.strip_suppressed = suppressed;
            let ctx = egui::Context::default();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| app.set_startup_visibility(ui.ctx(), requested_hidden));
            assert_eq!(app.hidden, hidden, "collapsed={collapsed}, always={always_show_strip}, suppressed={suppressed}");
            let shown = output.viewport_output.values().flat_map(|v| &v.commands)
                .any(|command| *command == egui::ViewportCommand::Visible(true));
            assert_eq!(shown, requested_hidden && !hidden, "show a window that was created hidden");
            assert!(app.dirty_since.is_none(), "the fallback does not change the saved preference");
            assert_eq!(app.settings.collapsed, collapsed);
        }
    }

    #[test]
    fn settings_recovery_stays_visible_even_when_a_strip_would_be_available() {
        let mut app = failed_app();
        app.settings.collapsed = true;
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| app.set_startup_visibility(ui.ctx(), true));
        assert!(!app.hidden);
        assert!(output.viewport_output.values().flat_map(|v| &v.commands)
            .any(|command| *command == egui::ViewportCommand::Visible(true)));
    }

    #[test]
    fn a_load_failure_blocks_controls_autosave_and_collapsing_to_the_strip() {
        let mut app = failed_app();
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let before = serde_json::to_value(&app.settings).unwrap();
        for command in [Command::ToggleMute, Command::ToggleBypass, Command::ToggleCollapse, Command::StopClips, Command::PlayPad("clap.wav".into())] {
            app.commands.0.send(command).unwrap();
        }
        app.dirty_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
        let mut input = egui::RawInput::default();
        input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(true);
        let output = ctx.run_ui(input, |ui| app.logic(ui.ctx(), &mut frame));
        assert_eq!(serde_json::to_value(&app.settings).unwrap(), before);
        assert!(app.engine.is_none() && app.recorder.is_none() && app.dictating.is_none());
        assert!(app.inputs.is_empty() && app.outputs.is_empty() && app.hotkey_bindings().is_empty());
        assert!(app.save_error.is_none(), "autosave must not run while startup is blocked");
        assert!(app.default_mic_sync.attempted.is_none(), "default microphone reconciliation waits for settings");
        assert!(!output.viewport_output.values().flat_map(|v| &v.commands).any(|c| matches!(c, egui::ViewportCommand::Visible(false))));

        app.run_command(&ctx, Command::Show);
        assert!(!app.hidden);
        app.run_command(&ctx, Command::Quit);
        assert!(app.quitting);
        app.on_exit(None);
        assert!(app.save_error.as_ref().unwrap().contains("retry loading before saving"), "shutdown cannot save defaults over the failed source");
        assert!(app.settings.load_error.is_some());
    }

    #[test]
    fn recovery_controls_fit_a_short_window_and_the_quit_button_works() {
        for size in [egui::vec2(700.0, 420.0), egui::vec2(760.0, 880.0)] {
            let mut app = failed_app();
            let ctx = egui::Context::default();
            draw(&mut app, &ctx, size, Vec::new());
            let output = draw(&mut app, &ctx, size, Vec::new());
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            for name in ["OpenMic", "SETTINGS NEED ATTENTION", "Retry loading", "Open settings folder", "Quit OpenMic"] {
                let rect = label(&output, name).unwrap_or_else(|| panic!("{name} missing"));
                assert!(screen.contains_rect(rect), "{name} is off screen at {size:?}: {rect:?}");
            }
            assert!(label(&output, "Start").is_none() && label(&output, "Mute").is_none());
            let pos = label(&output, "Quit OpenMic").unwrap().center();
            for pressed in [true, false] {
                draw(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                }]);
            }
            assert!(app.quitting);
        }
    }

    #[test]
    #[ignore = "writes settings-recovery screenshots; needs Python with Pillow"]
    fn snapshot_settings_recovery() {
        for dark in [true, false] {
            let mut app = failed_app();
            let name = if dark { "settings-recovery-dark" } else { "settings-recovery-light" };
            let path = super::super::snapshot::snapshot(&mut app, name, egui::vec2(700.0, 420.0), 1.25, dark);
            println!("SNAPSHOT {}", path.display());
        }
    }
}
