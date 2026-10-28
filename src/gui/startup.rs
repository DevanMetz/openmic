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
        widgets::consume_held_activation(ui.ctx());
        let Some(error) = &self.settings.load_error else {
            return;
        };
        let (mut retry, mut open_folder, mut quit) = (false, false, false);
        let mut retry_owner = None;
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
                    ui.add(egui::Label::new(RichText::new(&error.message).color(widgets::text_color(ui, RED))).wrap());
                    ui.add_space(8.0);
                    ui.label("Close any program locking the file, or fix or restore it, then retry. To start fresh, move the file to a backup location first.");
                    ui.add_space(10.0);
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("settings-recovery-actions")), |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let retry_button = ui.button("Retry loading");
                            self.restore_settings_retry_focus(&retry_button, false);
                            widgets::scroll_on_focus(&retry_button);
                            retry = retry_button.clicked();
                            if retry {
                                widgets::finish_keyboard_activation(ui.ctx());
                                if ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released()) {
                                    retry_button.request_focus();
                                    retry_owner = Some(retry_button.id);
                                } else { retry_button.surrender_focus(); }
                            }
                            let folder = ui.button("Open settings folder");
                            widgets::scroll_on_focus(&folder);
                            open_folder = folder.clicked();
                            let close = ui.button("Quit OpenMic");
                            widgets::scroll_on_focus(&close);
                            quit = close.clicked();
                        });
                    });
                });
            });
        });
        if open_folder && let Some(folder) = error.path.parent() {
            super::open_url(ui.ctx(), &folder.display().to_string());
        }
        if retry {
            let recovered = self.retry_settings(ui.ctx());
            if let Some(owner) = retry_owner {
                ui.data_mut(|data| {
                    data.insert_temp(egui::Id::new("settings-retry-focus"), (owner, recovered))
                });
                ui.ctx().request_repaint();
            }
        }
        if quit {
            self.run_command(ui.ctx(), Command::Quit);
        }
    }

    pub(super) fn restore_settings_retry_focus(&self, target: &egui::Response, recovered: bool) {
        let id = egui::Id::new("settings-retry-focus");
        let Some((owner, completed)) = target
            .ctx
            .data(|data| data.get_temp::<(egui::Id, bool)>(id))
        else {
            return;
        };
        if completed != recovered {
            return;
        }
        target
            .ctx
            .data_mut(|data| data.remove::<(egui::Id, bool)>(id));
        if target
            .ctx
            .memory(|memory| memory.focused().is_none_or(|focused| focused == owner))
            && target.ctx.input(|input| {
                input.focused
                    && !input.pointer.any_pressed()
                    && !input.pointer.any_down()
                    && !input.pointer.any_released()
                    && !input.key_pressed(egui::Key::Escape)
            })
        {
            target.request_focus();
            target.scroll_to_me(Some(egui::Align::Center));
        }
    }

    fn retry_settings(&mut self, ctx: &egui::Context) -> bool {
        #[cfg(test)]
        ctx.data_mut(|data| {
            *data.get_temp_mut_or_default::<usize>(egui::Id::new("settings-retry-attempts")) += 1
        });
        let loaded = self.settings.retry_load();
        if loaded.load_error.is_some() {
            self.settings.load_error = loaded.load_error;
            return false;
        }
        self.settings = loaded;
        self.dirty_since = None;
        self.save_error = None;
        self.default_mic_sync = DefaultMicSync::default();
        self.window_placement = Default::default();
        self.overlay_placement = Default::default();
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
            crate::initial_size(self.settings.window_size).into(),
        ));
        #[cfg(not(test))]
        self.initialize_settings();
        #[cfg(test)]
        ctx.data_mut(|data| {
            *data.get_temp_mut_or_default::<usize>(egui::Id::new(
                "settings-retry-initializations",
            )) += 1
        });
        // A retry is an explicit interaction in the main window; keep it visible.
        self.run_command(ctx, Command::Show);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Settings, SettingsLoadError};
    use eframe::App as _;
    use egui::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};

    fn failed_app() -> App {
        App::stopped(Settings {
            load_error: Some(SettingsLoadError {
                path: "C:/Users/Example/AppData/Roaming/OpenMic/config/settings.json".into(),
                message: "Parse settings: expected a value at line 12 column 3".into(),
            }),
            ..Default::default()
        })
    }

    fn draw(
        app: &mut App,
        ctx: &egui::Context,
        size: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
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

    fn label(output: &egui::FullOutput, name: &str) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => {
                Some(text.visual_bounding_rect())
            }
            _ => None,
        })
    }

    fn action(action: Action, target_node: NodeId) -> egui::Event {
        egui::Event::AccessKitActionRequest(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        })
    }

    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn press(key: egui::Key) -> Vec<egui::Event> {
        let mut events = vec![self::key(key, true)];
        if key == egui::Key::Space {
            events.push(egui::Event::Text(" ".into()));
        }
        events
    }

    fn control(output: &egui::FullOutput, label: &str) -> NodeId {
        *output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("missing button {label}"))
    }

    fn attempts(ctx: &egui::Context, initialized: bool) -> usize {
        ctx.data(|data| {
            data.get_temp::<usize>(egui::Id::new(if initialized {
                "settings-retry-initializations"
            } else {
                "settings-retry-attempts"
            }))
        })
        .unwrap_or_default()
    }

    fn context(zoom: f32, dark: bool, passes: usize) -> egui::Context {
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
        ctx
    }

    fn activate(app: &mut App, ctx: &egui::Context, size: egui::Vec2, target: NodeId, mode: &str) {
        if mode.contains("pointer") {
            let output = draw(app, ctx, size, Vec::new());
            let bounds = output
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
            let pos = egui::pos2(
                ((bounds.x0 + bounds.x1) * 0.5) as f32,
                ((bounds.y0 + bounds.y1) * 0.5) as f32,
            );
            assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos));
            for pressed in [true, false] {
                draw(
                    app,
                    ctx,
                    size,
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
        } else {
            draw(
                app,
                ctx,
                size,
                if mode.starts_with("accessible") {
                    vec![action(Action::Click, target)]
                } else {
                    press(if mode == "space" {
                        egui::Key::Space
                    } else {
                        egui::Key::Enter
                    })
                },
            );
        }
    }

    fn long_parse_error() -> Vec<u8> {
        let mut settings = serde_json::to_value(Settings::default()).unwrap();
        settings["model"] = serde_json::json!("An unsupported saved noise model ".repeat(70));
        serde_json::to_vec_pretty(&settings).unwrap()
    }

    fn bounds(output: &egui::FullOutput, target: NodeId) -> egui::accesskit::Rect {
        output
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
            .unwrap()
    }

    #[test]
    fn settings_retry_results_preserve_files_preferences_and_keyboard_focus() {
        #[cfg(windows)]
        use std::os::windows::fs::OpenOptionsExt;
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for outcome in ["parse", "long error", "read", "recovered", "fresh"] {
                        for activation in [
                            "enter",
                            "space",
                            "accessible",
                            "accessible unfocused",
                            "pointer",
                            "focused pointer",
                        ] {
                            let folder = tempfile::tempdir().unwrap();
                            let path = folder.path().join("settings.json");
                            let _settings_path = crate::config::use_settings_path_for_test(&path);
                            std::fs::write(&path, b"{").unwrap();
                            let mut app = App::stopped(Settings::load());
                            let before = serde_json::to_value(&app.settings).unwrap();
                            let ctx = context(zoom, dark, passes);
                            for _ in 0..3 {
                                draw(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let retry = control(&output, "Retry loading");
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, retry)]);
                            for _ in 0..3 {
                                draw(&mut app, &ctx, size, Vec::new());
                            }
                            if matches!(activation, "pointer" | "accessible unfocused")
                                && let Some(owner) = ctx.memory(|memory| memory.focused())
                            {
                                ctx.memory_mut(|memory| memory.surrender_focus(owner));
                            }
                            let mut recovered = Settings {
                                auto_start: false,
                                mute: true,
                                highpass_hz: 123.0,
                                window_size: Some([720.0, 500.0]),
                                ..Settings::default()
                            };
                            recovered.setup.dismissed = true;
                            let bytes = match outcome {
                                "parse" => b"\xff\xfe\x00".to_vec(),
                                "long error" => long_parse_error(),
                                _ => serde_json::to_vec_pretty(&recovered).unwrap(),
                            };
                            if outcome == "fresh" {
                                std::fs::remove_file(&path).unwrap();
                            } else {
                                std::fs::write(&path, &bytes).unwrap();
                            }
                            #[cfg(windows)]
                            let lock = (outcome == "read").then(|| {
                                std::fs::OpenOptions::new()
                                    .read(true)
                                    .share_mode(0)
                                    .open(&path)
                                    .unwrap()
                            });
                            #[cfg(not(windows))]
                            let unreadable = (outcome == "read").then(|| {
                                use std::os::unix::fs::PermissionsExt;
                                let permissions = std::fs::metadata(&path).unwrap().permissions();
                                std::fs::set_permissions(
                                    &path,
                                    std::fs::Permissions::from_mode(0o000),
                                )
                                .unwrap();
                                permissions
                            });
                            activate(&mut app, &ctx, size, retry, activation);
                            let success = matches!(outcome, "recovered" | "fresh");
                            assert_eq!(
                                app.settings.load_error.is_none(),
                                success,
                                "{outcome}, {activation}, zoom={zoom}, passes={passes}"
                            );
                            if outcome == "long error" {
                                assert!(
                                    app.settings.load_error.as_ref().unwrap().message.len() > 1500
                                );
                            }
                            assert_eq!(attempts(&ctx, false), 1);
                            assert_eq!(attempts(&ctx, true), usize::from(success));
                            // Simulate auto-start waiting for hardware without opening a device.
                            if outcome == "fresh" {
                                app.waiting_for_device = true;
                            }
                            let held = if activation == "space" {
                                egui::Key::Space
                            } else {
                                egui::Key::Enter
                            };
                            for _ in 0..3 {
                                draw(
                                    &mut app,
                                    &ctx,
                                    size,
                                    if matches!(activation, "enter" | "space") {
                                        press(held)
                                    } else {
                                        Vec::new()
                                    },
                                );
                            }
                            #[cfg(windows)]
                            drop(lock);
                            #[cfg(not(windows))]
                            if let Some(permissions) = unreadable {
                                std::fs::set_permissions(&path, permissions).unwrap();
                            }
                            for _ in 0..3 {
                                draw(
                                    &mut app,
                                    &ctx,
                                    size,
                                    if matches!(activation, "enter" | "space") {
                                        press(held)
                                    } else {
                                        Vec::new()
                                    },
                                );
                            }
                            assert_eq!(
                                attempts(&ctx, false),
                                1,
                                "releasing a file lock is not another retry"
                            );
                            assert_eq!(attempts(&ctx, true), usize::from(success));
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let pointer = activation.contains("pointer");
                            if pointer {
                                assert!(
                                    ctx.memory(|memory| memory.focused()).is_none(),
                                    "mouse retries preserve focus"
                                );
                            } else {
                                let target = if outcome == "fresh" {
                                    control(&output, "Stop")
                                } else if success {
                                    control(&output, "Start")
                                } else {
                                    retry
                                };
                                assert_eq!(
                                    output
                                        .platform_output
                                        .accesskit_update
                                        .as_ref()
                                        .unwrap()
                                        .focus,
                                    target,
                                    "retry focus follows its result: {outcome}, {activation}, zoom={zoom}, passes={passes}"
                                );
                                let rect = bounds(&output, target);
                                assert!(
                                    rect.x0 >= 0.0
                                        && rect.y0 >= 0.0
                                        && rect.x1 <= f64::from(size.x)
                                        && rect.y1 <= f64::from(size.y),
                                    "the focused result stays visible: {outcome}, {activation}, zoom={zoom}, passes={passes}, {rect:?}"
                                );
                            }
                            let expected = if outcome == "fresh" {
                                serde_json::to_value(Settings::default()).unwrap()
                            } else if success {
                                serde_json::to_value(recovered).unwrap()
                            } else {
                                before
                            };
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert_eq!(
                                app.waiting_for_device,
                                outcome == "fresh",
                                "held keys do not stop a pending startup"
                            );
                            if outcome == "fresh" {
                                assert!(
                                    !path.exists(),
                                    "starting fresh does not write a file during loading"
                                );
                            } else {
                                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                            }
                            assert!(
                                app.engine.is_none()
                                    && app.hotkeys.is_none()
                                    && app.recorder.is_none()
                                    && app.dirty_since.is_none()
                                    && !app.quitting
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn settings_retry_reflow_respects_new_focus_escape_and_manual_scrolling() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for mode in ["retained", "moved", "escape", "pointer"] {
                        for held in [egui::Key::Enter, egui::Key::Space] {
                            let folder = tempfile::tempdir().unwrap();
                            let path = folder.path().join("settings.json");
                            let _settings_path = crate::config::use_settings_path_for_test(&path);
                            std::fs::write(&path, b"{").unwrap();
                            let mut app = App::stopped(Settings::load());
                            let before = serde_json::to_value(&app.settings).unwrap();
                            let ctx = context(zoom, dark, passes);
                            for _ in 0..3 {
                                draw(&mut app, &ctx, size, Vec::new());
                            }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let retry = control(&output, "Retry loading");
                            let next = control(&output, "Open settings folder");
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, retry)]);
                            for _ in 0..3 {
                                draw(&mut app, &ctx, size, Vec::new());
                            }
                            let bytes = long_parse_error();
                            std::fs::write(&path, &bytes).unwrap();
                            activate(
                                &mut app,
                                &ctx,
                                size,
                                retry,
                                if mode == "pointer" {
                                    "pointer"
                                } else if held == egui::Key::Space {
                                    "space"
                                } else {
                                    "enter"
                                },
                            );
                            assert!(app.settings.load_error.as_ref().unwrap().message.len() > 1500);
                            draw(
                                &mut app,
                                &ctx,
                                size,
                                match mode {
                                    "moved" => vec![action(Action::Focus, next)],
                                    "escape" => vec![
                                        key(egui::Key::Escape, true),
                                        key(egui::Key::Escape, false),
                                    ],
                                    _ => Vec::new(),
                                },
                            );
                            for _ in 0..3 {
                                draw(
                                    &mut app,
                                    &ctx,
                                    size,
                                    if mode == "pointer" {
                                        Vec::new()
                                    } else {
                                        press(held)
                                    },
                                );
                            }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            if matches!(mode, "escape" | "pointer") {
                                assert!(ctx.memory(|memory| memory.focused()).is_none());
                            } else {
                                let target = if mode == "moved" { next } else { retry };
                                assert_eq!(
                                    output
                                        .platform_output
                                        .accesskit_update
                                        .as_ref()
                                        .unwrap()
                                        .focus,
                                    target,
                                    "focus survives error reflow: {mode}, zoom={zoom}, passes={passes}"
                                );
                                let rect = bounds(&output, target);
                                assert!(
                                    rect.y0 >= 0.0 && rect.y1 <= f64::from(size.y),
                                    "the reflowed control is visible: {rect:?}"
                                );
                                draw(
                                    &mut app,
                                    &ctx,
                                    size,
                                    vec![
                                        egui::Event::PointerMoved(egui::pos2(
                                            size.x * 0.5,
                                            size.y * 0.5,
                                        )),
                                        egui::Event::MouseWheel {
                                            unit: egui::MouseWheelUnit::Point,
                                            delta: egui::vec2(0.0, 360.0),
                                            modifiers: egui::Modifiers::NONE,
                                            phase: egui::TouchPhase::Move,
                                        },
                                    ],
                                );
                                for _ in 0..3 {
                                    draw(&mut app, &ctx, size, Vec::new());
                                }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                assert!(
                                    bounds(&output, target).y0 > rect.y0 + 50.0,
                                    "manual scrolling can leave the control to read the error: {mode}, zoom={zoom}, passes={passes}, before={rect:?}, after={:?}",
                                    bounds(&output, target)
                                );
                                assert_eq!(
                                    output.platform_output.accesskit_update.unwrap().focus,
                                    target,
                                    "scrolling keeps the user's focus choice"
                                );
                            }
                            assert_eq!(attempts(&ctx, false), 1);
                            assert_eq!(attempts(&ctx, true), 0);
                            assert_eq!(std::fs::read(&path).unwrap(), bytes);
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), before);
                            assert!(
                                app.dirty_since.is_none() && app.engine.is_none() && !app.quitting
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes settings retry keyboard screenshots; needs Python with Pillow"]
    fn snapshot_settings_retry_keyboard() {
        struct Retry {
            app: App,
            path: std::path::PathBuf,
            scene: &'static str,
            changed: bool,
            attempts: usize,
            initialized: usize,
            waiting_simulated: bool,
        }
        impl eframe::App for Retry {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if !self.changed && ui.ctx().cumulative_frame_nr() >= 10 {
                    self.changed = true;
                    match self.scene {
                        "failed" => std::fs::write(&self.path, b"\xff\xfe\x00").unwrap(),
                        "long" => std::fs::write(&self.path, long_parse_error()).unwrap(),
                        "recovered" => std::fs::write(
                            &self.path,
                            serde_json::to_vec_pretty(&Settings {
                                auto_start: false,
                                highpass_hz: 123.0,
                                ..Settings::default()
                            })
                            .unwrap(),
                        )
                        .unwrap(),
                        _ => std::fs::remove_file(&self.path).unwrap(),
                    }
                }
                eframe::App::ui(&mut self.app, ui, frame);
                self.attempts = attempts(ui.ctx(), false);
                self.initialized = attempts(ui.ctx(), true);
                if self.scene == "fresh" && self.initialized == 1 && !self.waiting_simulated {
                    self.waiting_simulated = true;
                    self.app.waiting_for_device = true;
                }
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["failed", "long", "recovered", "fresh"] {
                    let folder = tempfile::tempdir().unwrap();
                    let path = folder.path().join("settings.json");
                    let _settings_path = crate::config::use_settings_path_for_test(&path);
                    std::fs::write(&path, b"{").unwrap();
                    let mut app = App::stopped(Settings::load());
                    let ctx = context(zoom, dark, 2);
                    for _ in 0..3 {
                        draw(&mut app, &ctx, size, Vec::new());
                    }
                    let output = draw(&mut app, &ctx, size, Vec::new());
                    let retry = control(&output, "Retry loading");
                    let mut fixture = Retry {
                        app,
                        path: path.clone(),
                        scene,
                        changed: false,
                        attempts: 0,
                        initialized: 0,
                        waiting_simulated: false,
                    };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![action(Action::Focus, retry)]);
                    events.extend(vec![Vec::new(); 6]);
                    let pressed = press(egui::Key::Space);
                    events.push(pressed.clone());
                    events.extend(vec![Vec::new(); 3]);
                    events.extend(vec![pressed; 3]);
                    events.extend(vec![Vec::new(); 4]);
                    let image = super::super::snapshot::snapshot_with_events(
                        &mut fixture,
                        &format!("settings-retry-{scene}-{scale}-{theme}"),
                        size,
                        zoom,
                        dark,
                        events,
                    );
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(fixture.attempts, 1);
                    assert_eq!(
                        fixture.initialized,
                        usize::from(matches!(scene, "recovered" | "fresh"))
                    );
                    assert_eq!(
                        fixture.app.settings.load_error.is_some(),
                        matches!(scene, "failed" | "long")
                    );
                    assert_eq!(fixture.app.waiting_for_device, scene == "fresh");
                    if scene == "failed" {
                        assert_eq!(std::fs::read(&path).unwrap(), b"\xff\xfe\x00");
                    }
                    if scene == "long" {
                        assert_eq!(std::fs::read(&path).unwrap(), long_parse_error());
                    }
                    if scene == "fresh" {
                        assert!(!path.exists());
                    }
                    assert!(
                        fixture.app.engine.is_none()
                            && fixture.app.hotkeys.is_none()
                            && fixture.app.dirty_since.is_none()
                            && !fixture.app.quitting
                    );
                }
            }
        }
    }

    #[test]
    fn retrying_settings_waits_for_a_fresh_key_after_the_file_is_repaired() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for held in [egui::Key::Enter, egui::Key::Space] {
                        let folder = tempfile::tempdir().unwrap();
                        let path = folder.path().join("settings.json");
                        let _settings_path = crate::config::use_settings_path_for_test(&path);
                        let bad = b"{\"sounds\":[";
                        std::fs::write(&path, bad).unwrap();
                        let mut app = App::stopped(Settings::load());
                        assert!(app.settings.load_error.is_some());
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
                            draw(&mut app, &ctx, size, Vec::new());
                        }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let retry = control(&output, "Retry loading");
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, retry)]);
                        draw(&mut app, &ctx, size, press(held));
                        assert_eq!(attempts(&ctx, false), 1);
                        assert!(app.settings.load_error.is_some() && attempts(&ctx, true) == 0);
                        assert_eq!(std::fs::read(&path).unwrap(), bad);
                        let repaired = Settings {
                            auto_start: false,
                            highpass_hz: 123.0,
                            ..Settings::default()
                        };
                        let bytes = serde_json::to_vec_pretty(&repaired).unwrap();
                        std::fs::write(&path, &bytes).unwrap();
                        for _ in 0..3 {
                            draw(&mut app, &ctx, size, press(held));
                            assert!(
                                app.settings.load_error.is_some(),
                                "holding Retry does not reload a newly repaired file: {held:?}, zoom={zoom}, passes={passes}"
                            );
                            assert_eq!(attempts(&ctx, false), 1);
                            assert_eq!(attempts(&ctx, true), 0);
                        }
                        draw(&mut app, &ctx, size, vec![key(held, false)]);
                        draw(&mut app, &ctx, size, press(held));
                        assert!(app.settings.load_error.is_none());
                        assert_eq!(attempts(&ctx, false), 2);
                        assert_eq!(attempts(&ctx, true), 1);
                        for _ in 0..3 {
                            draw(&mut app, &ctx, size, press(held));
                        }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let start = control(&output, "Start");
                        assert_eq!(
                            output.platform_output.accesskit_update.unwrap().focus,
                            start,
                            "a successful keyboard retry reaches Start"
                        );
                        assert_eq!(std::fs::read(&path).unwrap(), bytes);
                        assert_eq!(
                            serde_json::to_value(&app.settings).unwrap(),
                            serde_json::to_value(repaired).unwrap()
                        );
                        assert!(
                            app.engine.is_none()
                                && app.recorder.is_none()
                                && app.hotkeys.is_none()
                                && app.dirty_since.is_none()
                                && !app.quitting
                        );
                    }
                }
            }
        }
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
            let mut app = App::stopped(Settings {
                collapsed,
                always_show_strip,
                ..Settings::default()
            });
            app.strip_suppressed = suppressed;
            let ctx = egui::Context::default();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.set_startup_visibility(ui.ctx(), requested_hidden)
            });
            assert_eq!(
                app.hidden, hidden,
                "collapsed={collapsed}, always={always_show_strip}, suppressed={suppressed}"
            );
            let shown = output
                .viewport_output
                .values()
                .flat_map(|v| &v.commands)
                .any(|command| *command == egui::ViewportCommand::Visible(true));
            assert_eq!(
                shown,
                requested_hidden && !hidden,
                "show a window that was created hidden"
            );
            assert!(
                app.dirty_since.is_none(),
                "the fallback does not change the saved preference"
            );
            assert_eq!(app.settings.collapsed, collapsed);
        }
    }

    #[test]
    fn settings_recovery_stays_visible_even_when_a_strip_would_be_available() {
        let mut app = failed_app();
        app.settings.collapsed = true;
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.set_startup_visibility(ui.ctx(), true)
        });
        assert!(!app.hidden);
        assert!(
            output
                .viewport_output
                .values()
                .flat_map(|v| &v.commands)
                .any(|command| *command == egui::ViewportCommand::Visible(true))
        );
    }

    #[test]
    fn a_load_failure_blocks_controls_autosave_and_collapsing_to_the_strip() {
        let mut app = failed_app();
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let before = serde_json::to_value(&app.settings).unwrap();
        for command in [
            Command::ToggleMute,
            Command::ToggleBypass,
            Command::ToggleCollapse,
            Command::StopClips,
            Command::PlayPad("clap.wav".into()),
        ] {
            app.commands.0.send(command).unwrap();
        }
        app.dirty_since = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .minimized = Some(true);
        let output = ctx.run_ui(input, |ui| app.logic(ui.ctx(), &mut frame));
        assert_eq!(serde_json::to_value(&app.settings).unwrap(), before);
        assert!(app.engine.is_none() && app.recorder.is_none() && app.dictating.is_none());
        assert!(
            app.inputs.is_empty() && app.outputs.is_empty() && app.hotkey_bindings().is_empty()
        );
        assert!(
            app.save_error.is_none(),
            "autosave must not run while startup is blocked"
        );
        assert!(
            app.default_mic_sync.attempted.is_none(),
            "default microphone reconciliation waits for settings"
        );
        assert!(
            !output
                .viewport_output
                .values()
                .flat_map(|v| &v.commands)
                .any(|c| matches!(c, egui::ViewportCommand::Visible(false)))
        );

        app.run_command(&ctx, Command::Show);
        assert!(!app.hidden);
        app.run_command(&ctx, Command::Quit);
        assert!(app.quitting);
        app.on_exit(None);
        assert!(
            app.save_error
                .as_ref()
                .unwrap()
                .contains("retry loading before saving"),
            "shutdown cannot save defaults over the failed source"
        );
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
            for name in [
                "OpenMic",
                "SETTINGS NEED ATTENTION",
                "Retry loading",
                "Open settings folder",
                "Quit OpenMic",
            ] {
                let rect = label(&output, name).unwrap_or_else(|| panic!("{name} missing"));
                assert!(
                    screen.contains_rect(rect),
                    "{name} is off screen at {size:?}: {rect:?}"
                );
            }
            assert!(label(&output, "Start").is_none() && label(&output, "Mute").is_none());
            let pos = label(&output, "Quit OpenMic").unwrap().center();
            for pressed in [true, false] {
                draw(
                    &mut app,
                    &ctx,
                    size,
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
            assert!(app.quitting);
        }
    }

    #[test]
    #[ignore = "writes settings-recovery screenshots; needs Python with Pillow"]
    fn snapshot_settings_recovery() {
        for dark in [true, false] {
            let mut app = failed_app();
            let name = if dark {
                "settings-recovery-dark"
            } else {
                "settings-recovery-light"
            };
            let path = super::super::snapshot::snapshot(
                &mut app,
                name,
                egui::vec2(700.0, 420.0),
                1.25,
                dark,
            );
            println!("SNAPSHOT {}", path.display());
        }
    }
}
