//! Exercise external actions without opening browsers or Explorer in UI tests.

use super::*;
use egui::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};

#[cfg(windows)]
const SCENES: &[&str] = &["releases", "notes", "setup", "routing", "logs", "recovery"];
#[cfg(not(windows))]
const SCENES: &[&str] = &["releases", "notes", "logs", "recovery"];

fn fixture(scene: &str) -> (App, &'static str, Role, String) {
    let mut app = App::stopped(Settings::default());
    let (label, role, destination) = match scene {
        "releases" => {
            app.page = Page::Soundboard;
            app.updater.set_status_for_test(crate::update::Status::Failed("Release check fixture unavailable".into()));
            app.update_checked_at = Some(Instant::now());
            ("Releases page", Role::Link, crate::update::RELEASES_PAGE.to_owned())
        }
        "notes" => {
            let release = crate::update::Release::sample("v0.11.0");
            let page = release.page.clone();
            app.updater.set_status_for_test(crate::update::Status::Available(release));
            app.show_release_notes = true;
            ("Open the release page", Role::Link, page)
        }
        #[cfg(windows)]
        "setup" | "routing" => {
            app.settings.setup.dismissed = scene == "routing";
            ("Get VB-Cable", Role::Button, routes::VB_CABLE_URL.to_owned())
        }
        "logs" => {
            app.page = Page::Settings;
            ("Open log folder", Role::Button, crate::logfile::dir().display().to_string())
        }
        "recovery" => {
            let path = PathBuf::from("C:/Users/Example/AppData/Roaming/OpenMic/config/settings.json");
            let folder = path.parent().unwrap().display().to_string();
            app.settings.load_error = Some(crate::config::SettingsLoadError {
                path, message: "Parse settings: expected a value at line 12 column 3".into(),
            });
            ("Open settings folder", Role::Button, folder)
        }
        _ => panic!("unknown external action fixture {scene}"),
    };
    // Any unintended update check or download stays inside the test fixture.
    app.updater.hold_next_check_for_test();
    app.updater.hold_next_download_for_test(0);
    app.updater.reject_installs_for_test("External action fixture prevents installation");
    (app, label, role, destination)
}

fn draw(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
    let mut frame = eframe::Frame::_new_kittest();
    ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
    }, |ui| eframe::App::ui(app, ui, &mut frame))
}

fn action(action: Action, target_node: NodeId) -> egui::Event {
    egui::Event::AccessKitActionRequest(ActionRequest { action, target_tree: TreeId::ROOT, target_node, data: None })
}

fn key(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE }
}

fn press(key: egui::Key) -> Vec<egui::Event> {
    let mut events = vec![self::key(key, true)];
    if key == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
    events
}

fn control(output: &egui::FullOutput, role: Role, label: &str) -> NodeId {
    *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
        .find(|(_, node)| node.role() == role && node.label().or(node.value()) == Some(label))
        .map(|(id, _)| id).unwrap_or_else(|| panic!("missing {role:?} {label}"))
}

fn opened(ctx: &egui::Context) -> Vec<String> {
    ctx.data(|data| data.get_temp::<Vec<String>>(egui::Id::new("opened-urls"))).unwrap_or_default()
}

#[test]
fn external_actions_open_once_until_the_activation_key_is_released() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for &scene in SCENES {
                    for activation in ["enter", "space", "accessible", "accessible unfocused", "pointer", "focused pointer"] {
                        let (mut app, label, role, destination) = fixture(scene);
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let target = control(&output, role, label);
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                        assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0 && bounds.x1 <= f64::from(size.x) && bounds.y1 <= f64::from(size.y),
                            "the whole focused external control is visible: {scene}, zoom={zoom}, passes={passes}, {bounds:?}");
                        if matches!(activation, "pointer" | "accessible unfocused") && let Some(owner) = ctx.memory(|memory| memory.focused()) {
                            ctx.memory_mut(|memory| memory.surrender_focus(owner));
                        }
                        let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                        for attempt in 1..=3 {
                            if activation.contains("pointer") {
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                let pos = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
                                assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos),
                                    "external action is reachable: {scene}, {activation}, zoom={zoom}, passes={passes}, {bounds:?}");
                                for pressed in [true, false] {
                                    draw(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                                    }]);
                                }
                            } else {
                                draw(&mut app, &ctx, size, if activation.starts_with("accessible") { vec![action(Action::Click, target)] } else {
                                    let mut events = if attempt == 3 { vec![key(held, false)] } else { Vec::new() };
                                    events.extend(press(held));
                                    events
                                });
                            }
                            assert_eq!(opened(&ctx), vec![destination.clone(); attempt], "one request per activation: {scene}, {activation}, zoom={zoom}, passes={passes}");
                            for _ in 0..3 {
                                draw(&mut app, &ctx, size, if matches!(activation, "enter" | "space") { press(held) } else { Vec::new() });
                                assert_eq!(opened(&ctx), vec![destination.clone(); attempt],
                                    "holding the key must not reopen: {scene}, {activation}, zoom={zoom}, passes={passes}");
                            }
                            if attempt == 1 { draw(&mut app, &ctx, size, vec![key(held, false)]); }
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && !app.quitting && app.update_error.is_none());
                        assert_eq!(app.updater.install_attempts_for_test(), 0);
                        assert_eq!(app.show_release_notes, scene == "notes");
                    }
                }
            }
        }
    }
}

#[test]
fn external_actions_do_not_replay_after_focus_or_navigation_changes() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for &scene in SCENES {
                    for mode in ["focus", "page", "escape"] {
                        if scene == "recovery" && mode == "page" { continue; }
                        for held in [egui::Key::Enter, egui::Key::Space] {
                            let (mut app, label, role, destination) = fixture(scene);
                            let page = app.page;
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let target = control(&output, role, label);
                            let other = if mode == "focus" { Some(control(&output, Role::Button, if scene == "recovery" { "Quit OpenMic" } else { "Mute" })) }
                                else if mode == "page" { Some(control(&output, Role::Button, "Record")) } else { None };
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            draw(&mut app, &ctx, size, press(held));
                            assert_eq!(opened(&ctx).as_slice(), std::slice::from_ref(&destination));
                            let mut events = match mode {
                                "focus" => vec![action(Action::Focus, other.unwrap())],
                                "page" => vec![action(Action::Click, other.unwrap())],
                                _ => vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)],
                            };
                            events.extend(press(held));
                            draw(&mut app, &ctx, size, events);
                            for _ in 0..3 { draw(&mut app, &ctx, size, press(held)); }
                            assert_eq!(opened(&ctx).as_slice(), std::slice::from_ref(&destination), "navigation must not replay the launch: {scene}, {mode}, {held:?}, zoom={zoom}, passes={passes}");
                            assert_eq!(app.page, if mode == "page" { Page::Record } else { page });
                            assert_eq!(app.show_release_notes, scene == "notes" && mode != "escape");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original, "held keys do not mute or change other settings");
                            assert!(!app.quitting && app.dirty_since.is_none(), "held keys do not quit or change the recovered file");
                            if mode == "focus" {
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, other.unwrap(), "the user's new focus stays put");
                            }
                            // A fresh press still works after returning to the same action.
                            draw(&mut app, &ctx, size, vec![key(held, false)]);
                            if mode == "page" {
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let navigation = control(&output, Role::Button, page.title());
                                draw(&mut app, &ctx, size, vec![action(Action::Click, navigation)]);
                            }
                            if scene == "notes" && mode == "escape" {
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let opener = control(&output, Role::Link, "What's new");
                                draw(&mut app, &ctx, size, vec![action(Action::Click, opener)]);
                            }
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            draw(&mut app, &ctx, size, press(held));
                            for _ in 0..3 { draw(&mut app, &ctx, size, press(held)); }
                            assert_eq!(opened(&ctx), vec![destination; 2], "release and a fresh press launch again");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && !app.quitting && app.update_error.is_none());
                            assert_eq!(app.updater.install_attempts_for_test(), 0);
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "writes external action keyboard screenshots; needs Python with Pillow"]
fn snapshot_external_actions_keyboard() {
    struct External { app: App, urls: Vec<String> }
    impl eframe::App for External {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            eframe::App::ui(&mut self.app, ui, frame);
            self.urls = opened(ui.ctx());
        }
    }
    let zoom = 1.5;
    let size = egui::vec2(700.0, 420.0) / zoom;
    for (dark, theme) in [(true, "dark"), (false, "light")] {
        for &scene in SCENES {
            let (mut app, label, role, destination) = fixture(scene);
            let original = serde_json::to_value(&app.settings).unwrap();
            let ctx = egui::Context::default();
            ctx.options_mut(|options| options.zoom_factor = zoom);
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            widgets::configure_text_styles(&ctx);
            ctx.enable_accesskit();
            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
            let output = draw(&mut app, &ctx, size, Vec::new());
            let target = control(&output, role, label);
            let mut external = External { app, urls: Vec::new() };
            let mut events = vec![Vec::new(); 3];
            events.push(vec![action(Action::Focus, target)]);
            events.extend(vec![Vec::new(); 6]);
            let pressed = press(egui::Key::Space);
            events.push(pressed.clone());
            events.extend(vec![Vec::new(); 3]);
            events.extend(vec![pressed; 3]);
            let image = super::snapshot::snapshot_with_events(&mut external,
                &format!("external-action-{scene}-150-{theme}"), size, zoom, dark, events);
            println!("SNAPSHOT {}", image.display());
            assert_eq!(external.urls, [destination], "holding Space launches only once");
            assert_eq!(serde_json::to_value(&external.app.settings).unwrap(), original);
            assert!(external.app.dirty_since.is_none() && !external.app.quitting && external.app.update_error.is_none());
            assert_eq!(external.app.updater.install_attempts_for_test(), 0);
            assert_eq!(external.app.show_release_notes, scene == "notes");
        }
    }
}
