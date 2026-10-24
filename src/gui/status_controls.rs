//! Main-window processing and mute controls, with device discovery held in a fixture.

use super::*;
use egui::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};

fn fixture(waiting: bool, muted: bool) -> App {
    let mut settings = Settings { auto_start: false, mute: muted, ..Settings::default() };
    settings.setup.dismissed = true;
    let mut app = App::stopped(settings);
    app.devices_for_test = Some((Vec::new(), Vec::new()));
    app.updater.hold_next_check_for_test();
    app.updater.hold_next_download_for_test(0);
    app.updater.reject_installs_for_test("Main control fixture prevents installation");
    // Empty outputs take the real waiting path before any audio device opens.
    if waiting { app.start_engine(); }
    app
}

#[test]
fn main_status_focus_survives_resizing_status_and_page_changes() {
    for dark in [true, false] {
        for passes in [1, 2] {
            for (processing, waiting, muted) in [(true, false, false), (true, true, false), (false, false, false), (false, true, true)] {
                let folder = tempfile::tempdir().unwrap();
                let path = folder.path().join("settings.json");
                let _settings_path = config::use_settings_path_for_test(&path);
                let mut app = fixture(waiting, muted);
                let ctx = context(1.0, dark, passes);
                let large = egui::vec2(760.0, 880.0);
                for _ in 0..3 { draw(&mut app, &ctx, large, Vec::new()); }
                let output = draw(&mut app, &ctx, large, Vec::new());
                let target = control(&output, caption(processing, waiting, muted));
                draw(&mut app, &ctx, large, vec![action(Action::Focus, target)]);
                draw(&mut app, &ctx, large, press(egui::Key::Space));
                let expected_waiting = waiting ^ processing;
                let expected_mute = muted ^ !processing;
                let expected = serde_json::to_value(&app.settings).unwrap();
                for zoom in [1.0, 1.25, 1.5, 1.0] {
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    app.save_error = Some("Settings could not be saved: Permission denied".into());
                    app.status = (short("Waiting for a saved USB microphone with a long device name to connect"), AMBER);
                    for page in Page::ALL {
                        app.page = page;
                        app.updater.set_status_for_test(match page {
                            Page::Soundboard => crate::update::Status::Checking,
                            Page::Record => crate::update::Status::Available(crate::update::Release::sample("v0.11.0")),
                            Page::Dictation => crate::update::Status::Ready(crate::update::Release::sample("v0.11.0")),
                            _ => crate::update::Status::Idle,
                        });
                        let update_state = std::mem::discriminant(app.updater.status());
                        for _ in 0..3 { draw(&mut app, &ctx, size, press(egui::Key::Space)); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        assert_eq!(control(&output, caption(processing, expected_waiting, expected_mute)), target,
                            "the main control keeps its identity across pages, updater states and resizing");
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert_visible(&output, target, size);
                        assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                        assert_eq!(std::mem::discriminant(app.updater.status()), update_state);
                        assert!(!app.manual_update_check && !app.show_release_notes);
                    }
                }
                assert!(app.engine.is_none() && app.hotkeys.is_none() && app.recorder.is_none() && !app.quitting);
            }
        }
    }
}

#[test]
#[ignore = "writes sidebar navigation focus screenshots; needs Python with Pillow"]
fn snapshot_main_status_navigation_keyboard() {
    for (dark, theme) in [(true, "dark"), (false, "light")] {
        let size = egui::vec2(700.0, 420.0) / 1.5;
        for scene in ["navigation", "save-warning"] {
            let mut app = fixture(true, false);
            if scene == "save-warning" { app.save_error = Some("Settings could not be saved: Permission denied".into()); }
            let ctx = context(1.5, dark, 2);
            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
            let output = draw(&mut app, &ctx, size, Vec::new());
            let target = control(&output, "Settings");
            let mut events = vec![Vec::new(); 3];
            events.push(vec![action(Action::Focus, target)]);
            events.extend(vec![Vec::new(); 6]);
            let image = super::snapshot::snapshot_with_events(&mut app,
                &format!("main-status-{scene}-150-{theme}"), size, 1.5, dark, events);
            println!("SNAPSHOT {}", image.display());
            assert!(app.waiting_for_device && app.engine.is_none() && !app.settings.mute && !app.quitting);
        }
    }
}

#[test]
#[ignore = "writes main status keyboard screenshots; needs Python with Pillow"]
fn snapshot_main_status_controls_keyboard() {
    for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (scene, processing, waiting, muted) in [
                ("waiting", true, false, false), ("stopped", true, true, false),
                ("muted", false, false, false), ("unmuted", false, true, true),
            ] {
                let folder = tempfile::tempdir().unwrap();
                let path = folder.path().join("settings.json");
                let _settings_path = config::use_settings_path_for_test(&path);
                let mut app = fixture(waiting, muted);
                let ctx = context(zoom, dark, 2);
                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                let output = draw(&mut app, &ctx, size, Vec::new());
                let target = control(&output, caption(processing, waiting, muted));
                let mut events = vec![Vec::new(); 3];
                events.push(vec![action(Action::Focus, target)]);
                events.extend(vec![Vec::new(); 3]);
                events.push(press(egui::Key::Space));
                events.extend(vec![press(egui::Key::Space); 3]);
                events.extend(vec![Vec::new(); 4]);
                let image = super::snapshot::snapshot_with_events(&mut app,
                    &format!("main-status-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                println!("SNAPSHOT {}", image.display());
                assert_eq!((app.waiting_for_device, app.settings.mute), (waiting ^ processing, muted ^ !processing));
                assert!(app.engine.is_none() && app.hotkeys.is_none() && app.recorder.is_none() && !app.quitting);
            }
        }
    }
}

fn context(zoom: f32, dark: bool, passes: usize) -> egui::Context {
    let ctx = egui::Context::default();
    ctx.options_mut(|options| { options.zoom_factor = zoom; options.max_passes = passes.try_into().unwrap(); });
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    widgets::configure_text_styles(&ctx);
    ctx.enable_accesskit();
    ctx
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

fn control(output: &egui::FullOutput, label: &str) -> NodeId {
    *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
        .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label))
        .map(|(id, _)| id).unwrap_or_else(|| panic!("missing button {label}"))
}

fn caption(processing: bool, waiting: bool, muted: bool) -> &'static str {
    if processing { if waiting { "Stop" } else { "Start" } }
    else if muted { "Unmute" } else { "Mute" }
}

#[test]
fn main_status_controls_toggle_once_until_the_activation_key_is_released() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for page in Page::ALL {
                    for (processing, waiting, muted) in [(true, false, false), (true, true, false), (false, false, false), (false, true, true)] {
                        for held in [egui::Key::Enter, egui::Key::Space] {
                            let folder = tempfile::tempdir().unwrap();
                            let path = folder.path().join("settings.json");
                            let _settings_path = config::use_settings_path_for_test(&path);
                            let mut app = fixture(waiting, muted);
                            app.page = page;
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = context(zoom, dark, passes);
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let target = control(&output, caption(processing, waiting, muted));
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                            for activation in 1..=3 {
                                let mut events = if activation == 3 { vec![key(held, false)] } else { Vec::new() };
                                events.extend(press(held));
                                draw(&mut app, &ctx, size, events);
                                let expected_waiting = waiting ^ (processing && activation % 2 == 1);
                                let expected_mute = muted ^ (!processing && activation % 2 == 1);
                                assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute));
                                for _ in 0..3 {
                                    draw(&mut app, &ctx, size, press(held));
                                    assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute),
                                        "holding a main control must not toggle it again: {processing}, {waiting}, {muted}, {held:?}, {page:?}, zoom={zoom}, passes={passes}");
                                }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                assert_eq!(control(&output, caption(processing, expected_waiting, expected_mute)), target,
                                    "the changing caption keeps the same control");
                                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                                if activation == 1 { draw(&mut app, &ctx, size, vec![key(held, false)]); }
                            }
                            let mut expected = original;
                            expected["mute"] = serde_json::json!(app.settings.mute);
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert!(app.engine.is_none() && app.hotkeys.is_none() && app.recorder.is_none() && !app.quitting);
                            if processing {
                                let saved: Settings = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                                assert_eq!(serde_json::to_value(saved).unwrap(), expected);
                                assert!(app.dirty_since.is_none());
                            } else { assert!(!path.exists() && app.dirty_since.is_some()); }
                        }
                    }
                }
            }
        }
    }
}

fn bounds(output: &egui::FullOutput, target: NodeId) -> egui::accesskit::Rect {
    output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
        .find(|(id, _)| *id == target).unwrap().1.bounds().unwrap()
}

fn assert_visible(output: &egui::FullOutput, target: NodeId, size: egui::Vec2) {
    let rect = bounds(output, target);
    assert!(rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= f64::from(size.x) && rect.y1 <= f64::from(size.y),
        "the whole main control is visible: {rect:?}, viewport={size:?}");
}

#[test]
fn sidebar_navigation_stays_reachable_above_device_and_save_warnings() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for save_warning in [false, true] {
                    for page in Page::ALL {
                        let mut app = fixture(true, false);
                        if save_warning { app.save_error = Some("Settings could not be saved: Permission denied".into()); }
                        let ctx = context(zoom, dark, passes);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let target = control(&output, page.title());
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        assert_visible(&output, target, size);
                        let state_top = output.shapes.iter().find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == "• Waiting for a device" => Some(text.visual_bounding_rect().top()),
                            _ => None,
                        }).unwrap();
                        assert!(bounds(&output, target).y1 <= f64::from(state_top),
                            "navigation does not overlap the state: {page:?}, save_warning={save_warning}, zoom={zoom}, passes={passes}, navigation={:?}, state_top={state_top}", bounds(&output, target));
                        let text_visible = output.shapes.iter().any(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == page.title() && text.visual_bounding_rect().right() < SIDEBAR_WIDTH =>
                                shape.clip_rect.contains_rect(text.visual_bounding_rect()),
                            _ => false,
                        });
                        assert!(text_visible, "focused navigation text is not clipped");
                        draw(&mut app, &ctx, size, vec![action(Action::Click, target)]);
                        assert_eq!(app.page, page);
                        assert!(app.waiting_for_device && app.engine.is_none() && !app.settings.mute && !app.quitting);
                    }
                }
            }
        }
    }
}

#[test]
fn sidebar_navigation_focus_follows_new_status_warnings() {
    let size = egui::vec2(700.0, 420.0) / 1.5;
    for dark in [true, false] {
        for passes in [1, 2] {
            for page in Page::ALL {
                let mut app = fixture(false, false);
                let ctx = context(1.5, dark, passes);
                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                let output = draw(&mut app, &ctx, size, Vec::new());
                let target = control(&output, page.title());
                draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                app.start_engine();
                app.save_error = Some("Settings could not be saved: Permission denied".into());
                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                let output = draw(&mut app, &ctx, size, Vec::new());
                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                let rect = bounds(&output, target);
                let painted = output.shapes.iter().any(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == page.title() && text.visual_bounding_rect().right() < SIDEBAR_WIDTH =>
                        shape.clip_rect.contains_rect(egui::Rect::from_min_max(egui::pos2(rect.x0 as f32, rect.y0 as f32), egui::pos2(rect.x1 as f32, rect.y1 as f32))),
                    _ => false,
                });
                let clips: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == page.title() && text.visual_bounding_rect().right() < SIDEBAR_WIDTH => Some(shape.clip_rect),
                    _ => None,
                }).collect();
                assert!(painted, "new status warnings keep the whole focused navigation button visible: {page:?}, dark={dark}, passes={passes}, {rect:?}, clips={clips:?}");
                assert!(app.waiting_for_device && app.engine.is_none() && !app.quitting);
            }
        }
    }
}

#[test]
fn main_status_controls_support_accessible_and_pointer_activations() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for (processing, waiting, muted) in [(true, false, false), (true, true, false), (false, false, false), (false, true, true)] {
                    for mode in ["accessible", "accessible unfocused", "pointer", "focused pointer"] {
                        let folder = tempfile::tempdir().unwrap();
                        let path = folder.path().join("settings.json");
                        let _settings_path = config::use_settings_path_for_test(&path);
                        let mut app = fixture(waiting, muted);
                        app.page = Page::Soundboard;
                        let ctx = context(zoom, dark, passes);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let target = control(&output, caption(processing, waiting, muted));
                        if matches!(mode, "accessible" | "focused pointer") {
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                        }
                        for activation in 1..=3 {
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            assert_visible(&output, target, size);
                            if mode.contains("pointer") {
                                let rect = bounds(&output, target);
                                let pos = egui::pos2(((rect.x0 + rect.x1) * 0.5) as f32, ((rect.y0 + rect.y1) * 0.5) as f32);
                                for pressed in [true, false] {
                                    draw(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                                        pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                                    }]);
                                }
                            } else { draw(&mut app, &ctx, size, vec![action(Action::Click, target)]); }
                            let expected_waiting = waiting ^ (processing && activation % 2 == 1);
                            let expected_mute = muted ^ (!processing && activation % 2 == 1);
                            assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute),
                                "{processing}, {waiting}, {muted}, {mode}, zoom={zoom}, passes={passes}");
                            for _ in 0..3 {
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute));
                                assert_eq!(control(&output, caption(processing, expected_waiting, expected_mute)), target);
                            }
                        }
                        assert!(app.engine.is_none() && app.hotkeys.is_none() && app.recorder.is_none() && !app.quitting);
                        if processing { assert!(path.exists() && app.dirty_since.is_none()); }
                        else { assert!(!path.exists() && app.dirty_since.is_some()); }
                    }
                }
            }
        }
    }
}

#[test]
fn main_status_controls_do_not_replay_after_focus_or_navigation_changes() {
    for zoom in [1.0, 1.25, 1.5] {
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for (processing, waiting, muted) in [(true, false, false), (true, true, false), (false, false, false), (false, true, true)] {
                    for mode in ["focus", "page", "tab", "escape"] {
                        for held in [egui::Key::Enter, egui::Key::Space] {
                            let folder = tempfile::tempdir().unwrap();
                            let path = folder.path().join("settings.json");
                            let _settings_path = config::use_settings_path_for_test(&path);
                            let mut app = fixture(waiting, muted);
                            let ctx = context(zoom, dark, passes);
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let target = control(&output, caption(processing, waiting, muted));
                            let other = control(&output, caption(!processing, waiting, muted));
                            let page = control(&output, "Soundboard");
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                            draw(&mut app, &ctx, size, press(held));
                            let expected_waiting = waiting ^ processing;
                            let expected_mute = muted ^ !processing;
                            let expected = serde_json::to_value(&app.settings).unwrap();
                            let mut events = match mode {
                                "focus" => vec![action(Action::Focus, other)],
                                "page" => vec![action(Action::Click, page)],
                                "tab" => vec![key(egui::Key::Tab, true), key(egui::Key::Tab, false)],
                                _ => vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)],
                            };
                            events.extend(press(held));
                            draw(&mut app, &ctx, size, events);
                            for _ in 0..3 { draw(&mut app, &ctx, size, press(held)); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            assert_eq!((app.waiting_for_device, app.settings.mute), (expected_waiting, expected_mute),
                                "held main actions do not follow focus: {processing}, {waiting}, {muted}, {mode}, {held:?}, zoom={zoom}, passes={passes}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), expected);
                            assert_eq!(app.page, if mode == "page" { Page::Soundboard } else { Page::Voice });
                            let focus = ctx.memory(|memory| memory.focused());
                            match mode {
                                "focus" => assert_eq!(focus.unwrap().value(), other.0),
                                "page" => assert_eq!(focus.unwrap().value(), target.0),
                                "tab" => assert!(focus.is_some() && focus.unwrap().value() != target.0),
                                _ => assert!(focus.is_none()),
                            }
                            if let Some(focus) = focus { assert_visible(&output, NodeId(focus.value()), size); }
                            draw(&mut app, &ctx, size, vec![key(held, false)]);
                            draw(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                            draw(&mut app, &ctx, size, press(held));
                            assert_eq!((app.waiting_for_device, app.settings.mute), (waiting, muted), "a fresh press still works after returning");
                            assert!(app.engine.is_none() && app.hotkeys.is_none() && app.recorder.is_none() && !app.quitting);
                        }
                    }
                }
            }
        }
    }
}
