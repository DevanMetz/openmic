//! Updates in the header: the version number checks for a new release when
//! clicked, and the right of the title row shows what the updater is doing.
//! When there's nothing to report it shows the tagline, so the daily
//! background check never flickers the header.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use super::{open_url, short, App, AMBER, GREEN, MUTED, RED};
use crate::update::{self, Status};
use crate::widgets;

const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// How long a manual check's result ("Up to date", a failure) stays up.
const RESULT_SHOWN: Duration = Duration::from_secs(8);
/// How long "Updated to …" shows after restarting into a new version.
pub(super) const UPDATED_SHOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum UpdateControls { Quiet, Checking, CheckFailed, Available, Downloading, Cancelling, Ready, Blocked, Retry, Error }

impl App {
    pub(super) fn poll_updates(&mut self) {
        self.updater.tick(self.settings.check_for_updates);
        if self.manual_update_check && !matches!(self.updater.status(), Status::Checking) {
            self.manual_update_check = false;
            self.update_checked_at = Some(Instant::now());
        }
    }

    /// The version beside the title; clicking it checks for updates now.
    pub(super) fn draw_version(&mut self, ui: &mut egui::Ui) {
        if !update::updates_supported() {
            ui.label(RichText::new(VERSION).weak());
            return;
        }
        let status = self.updater.status();
        let idle = !matches!(status, Status::Checking | Status::Downloading(_) | Status::Ready(_));
        let button = ui
            .add(egui::Button::new(RichText::new(VERSION).weak()).frame(false))
            .on_hover_text(if idle { "Check for updates" } else { "Update in progress" });
        if button.has_focus() {
            ui.painter().rect_stroke(button.rect, egui::CornerRadius::same(2), ui.visuals().selection.stroke, egui::StrokeKind::Inside);
        }
        ui.data_mut(|data| data.insert_temp(egui::Id::new("update-version-control"), button.id));
        if idle && button.clicked() {
            widgets::finish_keyboard_activation(ui.ctx());
            self.update_error = None;
            self.manual_update_check = true;
            self.update_checked_at = None;
            self.updater.check();
            ui.ctx().request_repaint();
        }
    }

    /// Right side of the title row, in a right-to-left layout: widgets are
    /// added from the right edge inward.
    pub(super) fn draw_update_status(&mut self, ui: &mut egui::Ui) {
        if !update::updates_supported() {
            return;
        }
        let recent_check = self.update_checked_at.is_some_and(|t| t.elapsed() < RESULT_SHOWN);
        let blocker = self.update_blocker();
        let phase = if self.update_error.is_some() { UpdateControls::Error } else {
            match self.updater.status() {
                Status::Ready(_) if blocker.is_some() => UpdateControls::Blocked,
                Status::Ready(_) => UpdateControls::Ready,
                Status::Downloading(_) if self.updater.download_cancelling() => UpdateControls::Cancelling,
                Status::Downloading(_) => UpdateControls::Downloading,
                Status::Available(_) => UpdateControls::Available,
                Status::DownloadFailed { .. } => UpdateControls::Retry,
                Status::Checking => UpdateControls::Checking,
                Status::Failed(_) if recent_check => UpdateControls::CheckFailed,
                _ => UpdateControls::Quiet,
            }
        };
        let keyboard = ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released());
        let focused = ui.memory(|memory| memory.focused());
        let mut next_focus = ui.data(|data| data.get_temp::<(UpdateControls, Option<egui::Id>, Option<egui::Id>, bool)>(
            egui::Id::new("update-controls")))
            .and_then(|(previous, action, link, started)| {
                let owner = focused?;
                (keyboard && (previous != phase || started) && [action, link].contains(&Some(owner))).then_some(owner)
            });
        // Consume the old control's key before drawing its replacement.
        if next_focus.is_some() { widgets::finish_keyboard_activation(ui.ctx()); }
        let mut primary = None;
        let mut secondary = None;
        let mut started = false;
        let mut return_to_version = false;
        ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new(("update-status", phase))), |ui| {
            if let Some(error) = self.update_error.clone() {
                let dismiss = ui.small_button("×").on_hover_text("Dismiss");
                dismiss.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Dismiss update error"));
                primary = Some(dismiss.id);
                if dismiss.clicked() {
                    started = dismiss.has_focus() && keyboard;
                    if !keyboard { dismiss.surrender_focus(); primary = None; }
                    widgets::finish_keyboard_activation(ui.ctx());
                    self.update_error = None;
                    ui.ctx().request_repaint();
                }
                ui.add(egui::Label::new(RichText::new(short(&error)).color(widgets::text_color(ui, RED))).truncate()).on_hover_text(error);
                return;
            }
            match self.updater.status().clone() {
                Status::Ready(release) => {
                    let restart = ui.add_enabled(blocker.is_none(), egui::Button::new("Restart to update"))
                        .on_hover_text("OpenMic closes and opens again as the new version (a few seconds)")
                        .on_disabled_hover_text(blocker.unwrap_or_default());
                    primary = Some(restart.id);
                    if restart.clicked() {
                        started = restart.has_focus() && keyboard;
                        if !keyboard { restart.surrender_focus(); primary = None; }
                        widgets::finish_keyboard_activation(ui.ctx());
                        let ctx = ui.ctx().clone();
                        self.install_update(&ctx);
                        ui.ctx().request_repaint();
                    }
                    ui.add(egui::Label::new(RichText::new(format!("{} ready", release.tag)).color(widgets::text_color(ui, GREEN))).truncate())
                        .on_hover_text("Downloaded and checked against the release's SHA-256 checksum");
                }
                Status::Downloading(release) => {
                    let cancelling = self.updater.download_cancelling();
                    let cancel = ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Cancelling…" } else { "Cancel" }).small());
                    primary = Some(cancel.id);
                    if cancel.clicked() {
                        if cancel.has_focus() && keyboard { next_focus = Some(cancel.id); return_to_version = true; }
                        if !keyboard { cancel.surrender_focus(); primary = None; }
                        widgets::finish_keyboard_activation(ui.ctx());
                        self.updater.cancel_download();
                        ui.ctx().request_repaint();
                    }
                    let (done, total) = self.updater.progress().unwrap_or_default();
                    let fraction = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(fraction).desired_width(ui.available_width().min(140.0))
                        .text(format!("{} / {} MB", done / 1_000_000, total / 1_000_000)))
                        .on_hover_text(format!("Downloading {}", release.tag));
                }
                Status::Available(release) => {
                    let notes = ui.link("What's new");
                    // Selectable text can overwrite the link's accessibility role.
                    notes.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), "What's new"));
                    secondary = Some(notes.id);
                    if notes.clicked() {
                        widgets::finish_keyboard_activation(ui.ctx());
                        ui.data_mut(|data| data.insert_temp(egui::Id::new("release-notes-opener"), (notes.id, keyboard)));
                        if keyboard { notes.request_focus(); } else { notes.surrender_focus(); }
                        self.show_release_notes = true;
                        ui.ctx().request_repaint();
                    }
                    let download = ui.button("Update").on_hover_text("Download and verify the new version");
                    primary = Some(download.id);
                    if download.clicked() {
                        started = download.has_focus() && keyboard;
                        if !keyboard { download.surrender_focus(); primary = None; }
                        widgets::finish_keyboard_activation(ui.ctx());
                        self.updater.download();
                        ui.ctx().request_repaint();
                    }
                    ui.add(egui::Label::new(RichText::new(format!("{} available", release.tag)).color(widgets::text_color(ui, GREEN))).truncate());
                }
                Status::DownloadFailed { release, error } => {
                    let retry = ui.button("Retry update").on_hover_text(format!("Download and verify {} again", release.tag));
                    primary = Some(retry.id);
                    if retry.clicked() {
                        started = retry.has_focus() && keyboard;
                        if !keyboard { retry.surrender_focus(); primary = None; }
                        widgets::finish_keyboard_activation(ui.ctx());
                        self.updater.download();
                        ui.ctx().request_repaint();
                    }
                    ui.add(egui::Label::new(RichText::new("Update download failed").color(widgets::text_color(ui, AMBER))).truncate()).on_hover_text(error);
                }
                Status::Checking => {
                    ui.spinner();
                    ui.add(egui::Label::new(RichText::new("Checking for updates…").weak()).truncate());
                }
                Status::Failed(e) if recent_check => {
                    let releases = ui.link("Releases page");
                    releases.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), "Releases page"));
                    primary = Some(releases.id);
                    if releases.clicked() { open_url(ui.ctx(), update::RELEASES_PAGE); }
                    ui.add(egui::Label::new(RichText::new("Couldn't check for updates").color(widgets::text_color(ui, AMBER))).truncate()).on_hover_text(e);
                }
                Status::UpToDate if recent_check => {
                    ui.add(egui::Label::new(RichText::new(format!("{VERSION} is up to date")).color(widgets::text_color(ui, MUTED))).truncate());
                }
                _ if self.updated_at.is_some_and(|t| t.elapsed() < UPDATED_SHOWN) => {
                    ui.add(egui::Label::new(RichText::new(format!("Updated to {VERSION}")).color(widgets::text_color(ui, GREEN))).truncate());
                }
                _ => { ui.add(egui::Label::new(RichText::new("Clean voice. Instant sounds. Fully local.").weak()).truncate()); }
            }
        });
        ui.data_mut(|data| data.insert_temp(egui::Id::new("update-controls"), (phase, primary, secondary, started)));
        if let Some(owner) = next_focus
            && ui.memory(|memory| memory.focused().is_none_or(|focused| focused == owner))
        {
            let action = primary.filter(|_| !return_to_version).and_then(|id| ui.ctx().read_response(id))
                .filter(|response| response.enabled() && response.sense.is_focusable());
            let target = action.or_else(|| ui.data(|data| data.get_temp::<egui::Id>(egui::Id::new("update-version-control")))
                .and_then(|id| ui.ctx().read_response(id)));
            if let Some(target) = target { target.request_focus(); }
        }
    }

    /// The available release's notes, in a window over the app.
    /// Returns whether keyboard focus should return to the update controls.
    pub(super) fn draw_release_notes(&mut self, ctx: &egui::Context) -> bool {
        if !self.show_release_notes { return false; }
        let id = egui::Id::new("release-notes");
        let layer = egui::LayerId::new(egui::Order::Middle, id);
        let keyboard = ctx.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released());
        let focused = ctx.memory(|memory| memory.focused());
        // egui clears focus before delivering Escape; use the last rendered owner.
        let owner = focused.or_else(|| ctx.data(|data| data.get_temp::<Option<egui::Id>>(egui::Id::new("keyboard-focus"))).flatten());
        let owns_focus = owner.and_then(|owner| ctx.read_response(owner)).is_some_and(|response| response.layer_id == layer);
        let opener = ctx.data_mut(|data| {
            let opener = data.get_temp::<(egui::Id, bool)>(egui::Id::new("release-notes-opener"));
            data.remove::<(egui::Id, bool)>(egui::Id::new("release-notes-opener"));
            opener
        });
        let entering = opener.is_some_and(|(opener, keyboard)| keyboard && focused.is_none_or(|focused| focused == opener));
        let escape = keyboard && (owns_focus || entering || owner.is_none())
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let release = match self.updater.status() {
            Status::Available(release) | Status::Downloading(release) | Status::DownloadFailed { release, .. }
                | Status::Ready(release) => release.clone(),
            _ => {
                self.show_release_notes = false;
                if keyboard && owns_focus { widgets::finish_keyboard_activation(ctx); }
                return keyboard && owns_focus;
            }
        };
        if escape {
            self.show_release_notes = false;
            widgets::finish_keyboard_activation(ctx);
            return true;
        }
        let mut open = true;
        let mut close_clicked = false;
        egui::Window::new(format!("What's new in {}", release.tag))
            .id(id)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0_f32.min(ctx.content_rect().width() - 24.0))
            .max_height(ctx.content_rect().height() * 0.7)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let footer_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0 + 6.0;
                let scroll = egui::ScrollArea::vertical()
                    .id_salt(("notes", &release.tag))
                    .max_height((ui.available_height() - footer_height).max(32.0))
                    .show(ui, |ui| {
                        if release.notes.trim().is_empty() {
                            ui.weak("This release has no notes.");
                        } else {
                            markdown(ui, &release.notes);
                        }
                        if keyboard && owns_focus {
                            let page = ui.clip_rect().height() * 0.85;
                            for (key, delta) in [(egui::Key::PageDown, -page), (egui::Key::PageUp, page)] {
                                if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
                                    ui.scroll_with_delta_animation(egui::vec2(0.0, delta), egui::style::ScrollAnimation::none());
                                }
                            }
                            for (key, edge, align) in [(egui::Key::Home, ui.min_rect().left_top(), egui::Align::Min),
                                (egui::Key::End, ui.min_rect().left_bottom(), egui::Align::Max)] {
                                if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
                                    ui.scroll_to_rect_animation(egui::Rect::from_min_size(edge, egui::Vec2::ZERO), Some(align),
                                        egui::style::ScrollAnimation::none());
                                }
                            }
                        }
                    });
                #[cfg(test)]
                ui.data_mut(|data| data.insert_temp(id.with("scroll"), (scroll.id, scroll.inner_rect, scroll.content_size)));
                #[cfg(not(test))]
                let _ = scroll;
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let link = ui.link("Open the release page");
                    link.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, ui.is_enabled(), "Open the release page"));
                    if link.clicked() {
                        open_url(ctx, &release.page);
                    }
                    let close = ui.button("Close");
                    close_clicked = close.clicked();
                    if entering { close.request_focus(); }
                });
            });
        open &= !close_clicked;
        self.show_release_notes = open;
        if !open {
            widgets::finish_keyboard_activation(ctx);
            if !keyboard && let Some(owner) = ctx.memory(|memory| memory.focused())
                && ctx.read_response(owner).is_some_and(|response| response.layer_id == layer)
            {
                ctx.memory_mut(|memory| memory.surrender_focus(owner));
            }
        }
        !open && keyboard
    }

    pub(super) fn restore_release_notes_focus(&self, ctx: &egui::Context) {
        let notes = ctx.data(|data| data.get_temp::<(UpdateControls, Option<egui::Id>, Option<egui::Id>, bool)>(
            egui::Id::new("update-controls"))).and_then(|(_, _, notes, _)| notes);
        let version = ctx.data(|data| data.get_temp::<egui::Id>(egui::Id::new("update-version-control")));
        if let Some(target) = notes.or(version).and_then(|id| ctx.read_response(id)) { target.request_focus(); }
    }
}

/// Just enough Markdown for release notes: headings, bullets, paragraphs.
fn markdown(ui: &mut egui::Ui, text: &str) {
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(heading) = line.trim_start_matches('#').strip_prefix(' ').filter(|_| line.starts_with('#')) {
            ui.add_space(6.0);
            ui.label(RichText::new(plain(heading)).strong().size(15.0));
        } else if let Some(item) = line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
            ui.horizontal_wrapped(|ui| {
                ui.label("•");
                ui.label(plain(item));
            });
        } else if line.is_empty() {
            ui.add_space(4.0);
        } else {
            ui.label(plain(line));
        }
    }
}

/// Inline Markdown as plain text: `[text](url)` keeps the text, and
/// emphasis and code marks are dropped.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|end| (mid, mid + 2 + end))) {
            Some((mid, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::{Action, ActionRequest, Role, TreeId};

    fn draw(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| eframe::App::ui(app, ui, &mut frame))
    }

    fn action(action: Action, target_node: egui::accesskit::NodeId) -> egui::Event {
        egui::Event::AccessKitActionRequest(ActionRequest { action, target_tree: TreeId::ROOT, target_node, data: None })
    }

    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn held_events(key: egui::Key) -> Vec<egui::Event> {
        let mut events = vec![self::key(key, true)];
        if key == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
        events
    }

    fn control(output: &egui::FullOutput, role: Role, label: &str) -> egui::accesskit::NodeId {
        *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_, node)|
            node.role() == role && node.label().or(node.value()) == Some(label)).map(|(id, _)| id)
            .unwrap_or_else(|| panic!("missing {role:?} {label}; rendered controls: {:?}", output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .filter_map(|(_, node)| node.label().map(|label| (node.role(), label))).collect::<Vec<_>>()))
    }

    fn activate(app: &mut App, ctx: &egui::Context, size: egui::Vec2, target: egui::accesskit::NodeId, activation: &str) {
        if activation.contains("pointer") {
            let output = draw(app, ctx, size, Vec::new());
            let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            let pos = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
            assert!(egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains(pos));
            for pressed in [true, false] {
                draw(app, ctx, size, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                }]);
            }
        } else {
            draw(app, ctx, size, if activation.starts_with("accessible") { vec![action(Action::Click, target)] }
                else { held_events(if activation == "space" { egui::Key::Space } else { egui::Key::Enter }) });
        }
    }

    fn finish_update(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while matches!(app.updater.status(), Status::Checking | Status::Downloading(_)) {
            app.poll_updates();
            assert!(Instant::now() < deadline, "the controlled update worker finishes");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn release_notes_take_keyboard_focus_and_close_without_reopening() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for opening in ["enter", "space", "accessible", "accessible unfocused", "pointer", "focused pointer"] {
                        for closing in ["enter", "space", "accessible", "title", "title enter", "title space", "escape", "pointer"] {
                            let mut app = App::stopped(Default::default());
                            app.page = crate::gui::Page::Soundboard;
                            app.updater.set_status_for_test(Status::Available(update::Release::sample("v0.11.0")));
                            let _check = app.updater.hold_next_check_for_test();
                            let _download = app.updater.hold_next_download_for_test(0);
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let opener = control(&output, Role::Link, "What's new");
                            if opening == "accessible unfocused" {
                                let mute = control(&output, Role::Button, "Mute");
                                draw(&mut app, &ctx, size, vec![action(Action::Focus, mute)]);
                            } else if opening != "pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, opener)]); }
                            activate(&mut app, &ctx, size, opener, opening);
                            let mut output = draw(&mut app, &ctx, size, Vec::new());
                            let close = control(&output, Role::Button, "Close");
                            let pointer = opening.contains("pointer");
                            if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                            else { assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, close,
                                "keyboard opening enters the notes window: {opening}, {closing}, zoom={zoom}, passes={passes}"); }
                            let held = if opening == "space" { egui::Key::Space } else { egui::Key::Enter };
                            for _ in 0..3 {
                                output = draw(&mut app, &ctx, size, if matches!(opening, "enter" | "space") { held_events(held) } else { Vec::new() });
                                assert!(app.show_release_notes, "holding the opener must not close the notes");
                                if !pointer { assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, close); }
                            }
                            draw(&mut app, &ctx, size, vec![key(held, false)]);
                            if pointer && closing != "pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, close)]); }
                            if closing == "escape" {
                                draw(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                            } else if closing.starts_with("title") {
                                let title_close = control(&output, Role::Button, "Close window");
                                draw(&mut app, &ctx, size, vec![action(Action::Focus, title_close)]);
                                if closing == "title" { draw(&mut app, &ctx, size, vec![action(Action::Click, title_close)]); }
                                else { activate(&mut app, &ctx, size, title_close, if closing.ends_with("space") { "space" } else { "enter" }); }
                            } else { activate(&mut app, &ctx, size, close, closing); }
                            assert!(!app.show_release_notes, "the notes close: {opening}, {closing}, zoom={zoom}, passes={passes}");
                            for _ in 0..3 {
                                let output = draw(&mut app, &ctx, size, if closing.ends_with("enter") || closing.ends_with("space") {
                                    held_events(if closing.ends_with("space") { egui::Key::Space } else { egui::Key::Enter })
                                } else { Vec::new() });
                                assert!(!app.show_release_notes, "holding Close must not reopen the notes");
                                if closing == "pointer" { assert!(ctx.memory(|memory| memory.focused()).is_none(), "mouse closing leaves focus alone"); }
                                else { assert_eq!(output.platform_output.accesskit_update.unwrap().focus, opener, "closing returns to What's new"); }
                            }
                            assert!(matches!(app.updater.status(), Status::Available(_)));
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && !app.quitting && app.update_error.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn release_notes_page_link_has_an_accessible_name_and_role() {
        let mut app = App::stopped(Default::default());
        app.updater.set_status_for_test(Status::Available(update::Release::sample("v0.11.0")));
        app.show_release_notes = true;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        for _ in 0..3 { draw(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new()); }
        let output = draw(&mut app, &ctx, egui::vec2(700.0, 420.0), Vec::new());
        let link = control(&output, Role::Link, "Open the release page");
        let node = &output.platform_output.accesskit_update.unwrap().nodes.into_iter().find(|(id, _)| *id == link).unwrap().1;
        assert!(node.supports_action(Action::Focus) && node.supports_action(Action::Click));
    }

    fn long_notes() -> update::Release {
        let mut release = update::Release::sample("v0.11.0");
        release.notes = "## Improvements\n\n".into();
        for item in 1..=40 {
            release.notes.push_str(&format!("- Improvement {item:02}: clearer audio controls and keyboard navigation.\n"));
        }
        release
    }

    fn notes_scroll(ctx: &egui::Context) -> (f32, egui::Rect, egui::Vec2) {
        let (id, rect, content) = ctx.data(|data| data.get_temp::<(egui::Id, egui::Rect, egui::Vec2)>(
            egui::Id::new("release-notes").with("scroll"))).unwrap();
        (egui::scroll_area::State::load(ctx, id).unwrap().offset.y, rect, content)
    }

    #[test]
    fn release_notes_scroll_by_keyboard_and_preserve_focus_elsewhere() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for target in ["Close", "Close window", "Open the release page"] {
                        let mut app = App::stopped(Default::default());
                        app.updater.set_status_for_test(Status::Available(long_notes()));
                        let _check = app.updater.hold_next_check_for_test();
                        let _download = app.updater.hold_next_download_for_test(0);
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let opener = control(&output, Role::Link, "What's new");
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, opener)]);
                        draw(&mut app, &ctx, size, vec![action(Action::Click, opener)]);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        let focus = control(&output, if target == "Open the release page" { Role::Link } else { Role::Button }, target);
                        let mute = control(&output, Role::Button, "Mute");
                        for label in ["Close", "Open the release page", "Close window"] {
                            let role = if label == "Open the release page" { Role::Link } else { Role::Button };
                            let id = control(&output, role, label);
                            let bounds = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(node, _)| *node == id).unwrap().1.bounds().unwrap();
                            assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0 && bounds.x1 <= f64::from(size.x) && bounds.y1 <= f64::from(size.y),
                                "notes controls fit the viewport: {label}, zoom={zoom}, passes={passes}, {bounds:?}");
                        }
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, focus)]);
                        for pressed in [egui::Key::PageDown, egui::Key::PageDown, egui::Key::PageUp, egui::Key::End, egui::Key::Home] {
                            let (before, rect, content) = notes_scroll(&ctx);
                            draw(&mut app, &ctx, size, vec![key(pressed, true), key(pressed, false)]);
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let (after, _, _) = notes_scroll(&ctx);
                            let page = rect.height() * 0.85;
                            let expected = match pressed {
                                egui::Key::PageDown => before + page,
                                egui::Key::PageUp => before - page,
                                egui::Key::End => content.y - rect.height(),
                                _ => 0.0,
                            };
                            assert!((after - expected).abs() < 1.0,
                                "one key scrolls once: {target}, {pressed:?}, zoom={zoom}, passes={passes}, before={before}, after={after}, expected={expected}");
                            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, focus, "scrolling keeps its window control");
                        }
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, mute)]);
                        draw(&mut app, &ctx, size, vec![key(egui::Key::PageDown, true), key(egui::Key::PageDown, false)]);
                        draw(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                        assert!(app.show_release_notes, "Escape outside the notes leaves the window open");
                        assert!(notes_scroll(&ctx).0.abs() < 1.0, "outside keyboard input must not scroll the notes");
                        draw(&mut app, &ctx, size, vec![action(Action::Focus, focus), key(egui::Key::PageDown, true), key(egui::Key::PageDown, false)]);
                        for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                        assert!(notes_scroll(&ctx).0 > 0.0, "a focus request and scrolling in one batch enter the window");
                        draw(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                        assert!(!app.show_release_notes);
                        let output = draw(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, opener);
                        assert!(matches!(app.updater.status(), Status::Available(_)));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.dirty_since.is_none() && !app.quitting);
                    }
                }
            }
        }
    }

    #[test]
    fn release_notes_keep_focus_when_the_update_or_page_changes() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for page in crate::gui::Page::ALL {
                        for outside in [true, false] {
                            for gone in [true, false] {
                                let mut app = App::stopped(Default::default());
                                let release = long_notes();
                                app.updater.set_status_for_test(Status::Available(release.clone()));
                                let _check = app.updater.hold_next_check_for_test();
                                let _download = app.updater.hold_next_download_for_test(0);
                                app.updater.reject_installs_for_test("Release notes fixture prevents installation");
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let opener = control(&output, Role::Link, "What's new");
                                let mute = control(&output, Role::Button, "Mute");
                                let version = control(&output, Role::Button, VERSION);
                                draw(&mut app, &ctx, size, vec![action(Action::Focus, opener)]);
                                draw(&mut app, &ctx, size, vec![action(Action::Click, opener)]);
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let close = control(&output, Role::Button, "Close");
                                if outside { draw(&mut app, &ctx, size, vec![action(Action::Focus, mute)]); }
                                app.page = page;
                                for status in [Status::Downloading(release.clone()), Status::DownloadFailed { release: release.clone(), error: "Fixture interrupted".into() }, Status::Ready(release)] {
                                    app.updater.set_status_for_test(status);
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    assert!(app.show_release_notes);
                                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, if outside { mute } else { close },
                                        "update and page changes preserve the chosen control: {page:?}, zoom={zoom}, passes={passes}");
                                }
                                if gone {
                                    app.updater.set_status_for_test(Status::UpToDate);
                                    draw(&mut app, &ctx, size, if outside { Vec::new() } else { held_events(egui::Key::Space) });
                                } else {
                                    if outside { draw(&mut app, &ctx, size, vec![action(Action::Focus, close)]); }
                                    draw(&mut app, &ctx, size, held_events(egui::Key::Space));
                                }
                                assert!(!app.show_release_notes);
                                for _ in 0..3 {
                                    let output = draw(&mut app, &ctx, size, if gone && outside { Vec::new() } else { held_events(egui::Key::Space) });
                                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, if gone && outside { mute } else { version });
                                    assert!(!app.show_release_notes, "the old window does not reopen");
                                }
                                assert_eq!(app.updater.install_attempts_for_test(), 0);
                                assert!(matches!(app.updater.status(), Status::UpToDate | Status::Ready(_)));
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none() && !app.quitting && app.update_error.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes release notes keyboard screenshots; needs Python with Pillow"]
    fn snapshot_release_notes_keyboard() {
        struct Notes { app: App, scene: &'static str, changed: bool }
        impl eframe::App for Notes {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if self.scene == "version" && !self.changed && ui.ctx().cumulative_frame_nr() >= 18 {
                    self.changed = true;
                    self.app.updater.set_status_for_test(Status::Ready(long_notes()));
                }
                eframe::App::ui(&mut self.app, ui, frame);
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["short", "long", "end", "empty", "closed", "version"] {
                    let mut app = App::stopped(Default::default());
                    app.page = crate::gui::Page::Soundboard;
                    let mut release = if matches!(scene, "short" | "empty") { update::Release::sample("v0.11.0") } else { long_notes() };
                    if scene == "empty" { release.notes.clear(); }
                    app.updater.set_status_for_test(Status::Available(release));
                    let _check = app.updater.hold_next_check_for_test();
                    let _download = app.updater.hold_next_download_for_test(0);
                    app.updater.reject_installs_for_test("Release notes fixture prevents installation");
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                    let output = draw(&mut app, &ctx, size, Vec::new());
                    let opener = control(&output, Role::Link, "What's new");
                    let mut notes = Notes { app, scene, changed: false };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![action(Action::Focus, opener)]);
                    events.extend(vec![Vec::new(); 5]);
                    let press = held_events(egui::Key::Space);
                    events.push(press.clone());
                    events.extend(vec![Vec::new(); 4]);
                    events.extend(vec![press; 3]);
                    events.push(vec![key(egui::Key::Space, false)]);
                    events.push(match scene {
                        "end" => vec![key(egui::Key::End, true), key(egui::Key::End, false)],
                        "closed" | "version" => vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)],
                        _ => Vec::new(),
                    });
                    events.extend(vec![Vec::new(); 4]);
                    let image = crate::gui::snapshot::snapshot_with_events(&mut notes,
                        &format!("release-notes-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(notes.app.show_release_notes, !matches!(scene, "closed" | "version"));
                    assert_eq!(notes.changed, scene == "version");
                    assert_eq!(notes.app.updater.install_attempts_for_test(), 0);
                    assert!(matches!(notes.app.updater.status(), Status::Available(_) | Status::Ready(_)));
                    assert_eq!(serde_json::to_value(&notes.app.settings).unwrap(), original);
                    assert!(notes.app.dirty_since.is_none() && !notes.app.quitting && notes.app.update_error.is_none());
                }
            }
        }
    }

    #[test]
    fn update_downloads_keep_focus_without_replaying_keys() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for retry in [true, false] {
                        for outcome in ["ready", "failed", "cancelled"] {
                            for early in [true, false] {
                                if early && outcome == "cancelled" { continue; }
                                for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                                    let mut app = App::stopped(Default::default());
                                    app.page = crate::gui::Page::Soundboard;
                                    let release = update::Release::sample("v0.11.0");
                                    app.updater.set_status_for_test(if retry { Status::DownloadFailed { release: release.clone(), error: "Earlier failure".into() } }
                                        else { Status::Available(release.clone()) });
                                    app.updater.reject_installs_for_test("Update installation fixture declined");
                                    let completion = app.updater.hold_next_download_for_test(27_000_000);
                                    let _check = app.updater.hold_next_check_for_test();
                                    let original = serde_json::to_value(&app.settings).unwrap();
                                    let ctx = egui::Context::default();
                                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                    widgets::configure_text_styles(&ctx);
                                    ctx.enable_accesskit();
                                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    let download = control(&output, Role::Button, if retry { "Retry update" } else { "Update" });
                                    let version = control(&output, Role::Button, VERSION);
                                    let pointer = activation.contains("pointer");
                                    let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                    if activation != "pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, download)]); }
                                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                    activate(&mut app, &ctx, size, download, activation);
                                    assert_eq!(app.updater.progress(), Some((27_000_000, 54_000_000)));
                                    let _retry = app.updater.hold_next_download_for_test(0);
                                    if !early {
                                        for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                        let output = draw(&mut app, &ctx, size, Vec::new());
                                        let cancel = control(&output, Role::Button, "Cancel");
                                        if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                        else {
                                            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, cancel,
                                                "a download focuses Cancel: retry={retry}, {activation}, zoom={zoom}, passes={passes}");
                                            if matches!(activation, "enter" | "space") {
                                                for _ in 0..3 { draw(&mut app, &ctx, size, held_events(held)); }
                                                assert!(!app.updater.download_cancelling(), "the starting key must not cancel an update");
                                            }
                                        }
                                        if outcome == "cancelled" {
                                            if matches!(activation, "enter" | "space") { draw(&mut app, &ctx, size, vec![key(held, false)]); }
                                            if activation == "focused pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, cancel)]); }
                                            activate(&mut app, &ctx, size, cancel, activation);
                                            for _ in 0..5 { draw(&mut app, &ctx, size, Vec::new()); }
                                            assert!(app.updater.download_cancelling());
                                            if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                            else {
                                                let output = draw(&mut app, &ctx, size, Vec::new());
                                                assert_eq!(output.platform_output.accesskit_update.unwrap().focus, version);
                                            }
                                        }
                                    }
                                    completion.send(if outcome == "failed" { Err(anyhow::anyhow!("Update download fixture interrupted")) } else { Ok(()) }).unwrap();
                                    finish_update(&mut app);
                                    for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    let target = control(&output, Role::Button, match outcome { "ready" => "Restart to update", "failed" => "Retry update", _ => VERSION });
                                    if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                    else {
                                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target,
                                            "completion keeps focus: {outcome}, {activation}, early={early}, zoom={zoom}, passes={passes}");
                                        let bounds = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                        let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                            && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))));
                                        if matches!(activation, "enter" | "space") {
                                            let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                            assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)));
                                            for _ in 0..3 { draw(&mut app, &ctx, size, held_events(held)); }
                                            draw(&mut app, &ctx, size, vec![key(held, false)]);
                                        }
                                    }
                                    assert_eq!(app.updater.install_attempts_for_test(), 0, "a download must not install itself");
                                    assert!(app.update_error.is_none() && !app.show_release_notes && !app.quitting);
                                    assert_eq!(app.updater.status(), &match outcome {
                                        "ready" => Status::Ready(release),
                                        "failed" => Status::DownloadFailed { release, error: "Update download fixture interrupted".into() },
                                        _ => Status::Available(release),
                                    });
                                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                    assert!(app.dirty_since.is_none());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn manual_update_checks_do_not_repeat_when_the_result_arrives() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for outcome in ["current", "available", "failed"] {
                        for early in [true, false] {
                            for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                                let mut app = App::stopped(Default::default());
                                app.updater.reject_installs_for_test("Update installation fixture declined");
                                let completion = app.updater.hold_next_check_for_test();
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let version = control(&output, Role::Button, VERSION);
                                if activation != "pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, version)]); }
                                activate(&mut app, &ctx, size, version, activation);
                                assert_eq!(app.updater.status(), &Status::Checking);
                                assert!(app.manual_update_check && app.update_checked_at.is_none());
                                let again = app.updater.hold_next_check_for_test();
                                if !early { for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); } }
                                let release = update::Release::sample("v0.11.0");
                                completion.send(match outcome {
                                    "current" => Ok(None), "available" => Ok(Some(release.clone())),
                                    _ => Err(anyhow::anyhow!("Update check fixture interrupted")),
                                }).unwrap();
                                finish_update(&mut app);
                                for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                let expected = app.updater.status().clone();
                                if activation != "pointer" {
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    assert_eq!(tree.focus, version);
                                    let bounds = tree.nodes.iter().find(|(id, _)| *id == version).unwrap().1.bounds().unwrap();
                                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32), egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                        && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect == rect && shape.stroke.width > 0.0 && shape.stroke.color.a() > 0)),
                                        "the focused version number has a visible outline: {outcome}, {activation}, zoom={zoom}, passes={passes}");
                                }
                                let shown_at = app.update_checked_at.unwrap();
                                if matches!(activation, "enter" | "space") {
                                    let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                    for _ in 0..3 { draw(&mut app, &ctx, size, held_events(held)); }
                                    assert_eq!(app.updater.status(), &expected, "the held checking key leaves its result visible: {outcome}, {activation}, zoom={zoom}, passes={passes}");
                                    assert_eq!(app.update_checked_at, Some(shown_at));
                                    assert!(!app.manual_update_check);
                                    draw(&mut app, &ctx, size, vec![key(held, false)]);
                                    draw(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                    assert_eq!(app.updater.status(), &Status::Checking, "a fresh Enter starts a new manual check");
                                    again.send(Ok(None)).unwrap();
                                    finish_update(&mut app);
                                }
                                assert_eq!(app.updater.install_attempts_for_test(), 0);
                                assert!(app.update_error.is_none() && !app.show_release_notes && !app.quitting);
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn update_completion_preserves_navigation_and_new_recordings() {
        use crate::gui::Page;
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for (index, page) in Page::ALL.into_iter().enumerate() {
                        for failed in [false, true] {
                            for early in [true, false] {
                                for mode in ["unchanged", "focus moved", "page switched", "result focus queued", "result focus cleared", "recording starts"] {
                                    let mut app = App::stopped(Default::default());
                                    app.page = page;
                                    let release = update::Release::sample("v0.11.0");
                                    app.updater.set_status_for_test(Status::Available(release.clone()));
                                    app.updater.reject_installs_for_test("Update installation fixture declined");
                                    let completion = app.updater.hold_next_download_for_test(27_000_000);
                                    let original = serde_json::to_value(&app.settings).unwrap();
                                    let ctx = egui::Context::default();
                                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                    widgets::configure_text_styles(&ctx);
                                    ctx.enable_accesskit();
                                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    let download = control(&output, Role::Button, "Update");
                                    let version = control(&output, Role::Button, VERSION);
                                    let next_page = Page::ALL[(index + 1) % Page::ALL.len()];
                                    let other = control(&output, Role::Button, if mode == "page switched" { next_page.title() } else { "Mute" });
                                    let move_focus = |app: &mut App| {
                                        let mut events = vec![action(Action::Focus, other)];
                                        if mode == "page switched" { events.push(action(Action::Click, other)); }
                                        draw(app, &ctx, size, events);
                                        for _ in 0..3 { draw(app, &ctx, size, Vec::new()); }
                                    };
                                    draw(&mut app, &ctx, size, vec![action(Action::Focus, download)]);
                                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                    draw(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                    let _retry = app.updater.hold_next_download_for_test(0);
                                    if !early { for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); } }
                                    if matches!(mode, "focus moved" | "page switched") { move_focus(&mut app); }
                                    if mode == "recording starts" {
                                        app.recorder = Some(crate::record::Recorder::from_samples_for_test(&[0.25; 96]));
                                    }
                                    completion.send(if failed { Err(anyhow::anyhow!("Update download fixture interrupted")) } else { Ok(()) }).unwrap();
                                    finish_update(&mut app);
                                    if mode == "result focus queued" { move_focus(&mut app); }
                                    if mode == "result focus cleared" {
                                        draw(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    }
                                    for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    if mode == "result focus cleared" { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                    else {
                                        let target = match mode {
                                            "focus moved" | "page switched" | "result focus queued" => other,
                                            "recording starts" if !failed => version,
                                            _ => control(&output, Role::Button, if failed { "Retry update" } else { "Restart to update" }),
                                        };
                                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target,
                                            "completion preserves navigation: {page:?}, {mode}, failed={failed}, early={early}, zoom={zoom}, passes={passes}");
                                    }
                                    assert_eq!(app.page, if mode == "page switched" { next_page } else { page });
                                    assert_eq!(app.recorder.is_some(), mode == "recording starts");
                                    assert_eq!(app.updater.status(), &if failed {
                                        Status::DownloadFailed { release, error: "Update download fixture interrupted".into() }
                                    } else { Status::Ready(release) });
                                    assert_eq!(app.updater.install_attempts_for_test(), 0);
                                    assert!(app.update_error.is_none() && !app.show_release_notes && !app.quitting);
                                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                    assert!(app.dirty_since.is_none());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn update_header_focus_survives_blocked_restarts_and_expired_results() {
        use crate::gui::Page;
        use crate::record::Recorder;
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for page in Page::ALL {
                        for scene in ["recording", "take", "dictation", "expired"] {
                            for focus in ["header", "mute", "cleared"] {
                                let mut app = App::stopped(Default::default());
                                app.page = page;
                                app.updater.set_status_for_test(if scene == "expired" { Status::Failed("Update check fixture interrupted".into()) }
                                    else { Status::Ready(update::Release::sample("v0.11.0")) });
                                app.updater.reject_installs_for_test("Update installation fixture declined");
                                app.update_checked_at = Some(Instant::now());
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                widgets::configure_text_styles(&ctx);
                                ctx.enable_accesskit();
                                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                let header = control(&output, if scene == "expired" { Role::Link } else { Role::Button },
                                    if scene == "expired" { "Releases page" } else { "Restart to update" });
                                let mute = control(&output, Role::Button, "Mute");
                                let version = control(&output, Role::Button, VERSION);
                                draw(&mut app, &ctx, size, vec![action(Action::Focus, if focus == "mute" { mute } else { header })]);
                                for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                                if focus == "cleared" { draw(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]); }
                                match scene {
                                    "recording" => app.recorder = Some(Recorder::from_samples_for_test(&[0.25; 96])),
                                    "take" => app.take = Recorder::from_samples_for_test(&[0.25; 96]).finish().unwrap().map(std::sync::Arc::new),
                                    "dictation" => app.dictating = Some(crate::gui::DictationRecording {
                                        recorder: Recorder::from_samples_for_test(&[0.25; 96]), model: app.settings.speech_model, hold_to_talk: false,
                                    }),
                                    _ => app.update_checked_at = Some(Instant::now() - Duration::from_secs(60)),
                                }
                                for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                let output = draw(&mut app, &ctx, size, Vec::new());
                                if focus == "cleared" { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                else {
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, if focus == "mute" { mute } else { version },
                                        "unavailable header controls preserve focus: {page:?}, {scene}, {focus}, zoom={zoom}, passes={passes}");
                                }
                                if scene == "expired" {
                                    assert!(!output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().any(|(_, node)| node.label() == Some("Releases page")));
                                } else {
                                    let restart = control(&output, Role::Button, "Restart to update");
                                    assert!(output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(id, _)| *id == restart).unwrap().1.is_disabled());
                                    assert!(app.update_blocker().is_some());
                                    app.recorder = None; app.take = None; app.dictating = None;
                                    for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                                    let output = draw(&mut app, &ctx, size, Vec::new());
                                    if focus == "cleared" { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                                    else { assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, if focus == "mute" { mute } else { version }); }
                                }
                                assert_eq!(app.updater.install_attempts_for_test(), 0);
                                assert!(app.update_error.is_none() && !app.show_release_notes && !app.quitting);
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.dirty_since.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn update_install_errors_keep_keyboard_focus_until_dismissed() {
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for page in crate::gui::Page::ALL {
                        for activation in ["enter", "space", "accessible", "pointer", "focused pointer"] {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            let release = update::Release::sample("v0.11.0");
                            app.updater.set_status_for_test(Status::Ready(release.clone()));
                            app.updater.reject_installs_for_test("Update installation fixture declined");
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let restart = control(&output, Role::Button, "Restart to update");
                            let pointer = activation.contains("pointer");
                            if activation != "pointer" { draw(&mut app, &ctx, size, vec![action(Action::Focus, restart)]); }
                            for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                            activate(&mut app, &ctx, size, restart, activation);
                            for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            let dismiss = control(&output, Role::Button, "Dismiss update error");
                            if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                            else { assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, dismiss); }
                            assert_eq!(app.update_error.as_deref(), Some("Update installation fixture declined"));
                            assert_eq!(app.updater.install_attempts_for_test(), 1);
                            if matches!(activation, "enter" | "space") {
                                let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                for _ in 0..3 { draw(&mut app, &ctx, size, held_events(held)); }
                                assert!(app.update_error.is_some(), "the held restarting key leaves its failure visible");
                                draw(&mut app, &ctx, size, vec![key(held, false)]);
                            }
                            activate(&mut app, &ctx, size, dismiss, activation);
                            for _ in 0..6 { draw(&mut app, &ctx, size, Vec::new()); }
                            let output = draw(&mut app, &ctx, size, Vec::new());
                            if pointer { assert!(ctx.memory(|memory| memory.focused()).is_none()); }
                            else { assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, control(&output, Role::Button, "Restart to update")); }
                            if matches!(activation, "enter" | "space") {
                                let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                for _ in 0..3 { draw(&mut app, &ctx, size, held_events(held)); }
                            }
                            assert_eq!(app.updater.install_attempts_for_test(), 1, "dismissing an error does not repeat installation");
                            assert!(app.update_error.is_none() && !app.show_release_notes && !app.quitting);
                            assert_eq!(app.updater.status(), &Status::Ready(release));
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn header_messages_fit_beside_page_titles_in_a_zoomed_window() {
        use eframe::App as _;
        use egui::accesskit::Role;
        use crate::gui::Page;
        let release = update::Release::sample("v0.11.0");
        let states = [Status::Idle, Status::Checking, Status::UpToDate,
            Status::Available(release.clone()), Status::Downloading(release.clone()),
            Status::DownloadFailed { release: release.clone(), error: "Download failed".into() },
            Status::Ready(release), Status::Failed("Release check failed".into())];
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for page in Page::ALL {
                        for state in 0..states.len() + 2 {
                            let mut app = App::stopped(Default::default());
                            app.page = page;
                            app.update_checked_at = Some(Instant::now());
                            if let Some(status) = states.get(state) { app.updater.set_status_for_test(status.clone()); }
                            else if state == states.len() {
                                app.update_error = Some("The update could not be installed in the current directory. Choose a writable location and retry.".into());
                            } else { app.updated_at = Some(Instant::now()); }
                            let original = serde_json::to_value(&app.settings).unwrap();
                            let ctx = egui::Context::default();
                            ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                            widgets::configure_text_styles(&ctx);
                            ctx.enable_accesskit();
                            let mut frame = eframe::Frame::_new_kittest();
                            let mut draw = || ctx.run_ui(egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                                ..Default::default()
                            }, |ui| app.ui(ui, &mut frame));
                            for _ in 0..3 { draw(); }
                            let output = draw();
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            if state == 3 || state == 7 {
                                let label = if state == 3 { "What's new" } else { "Releases page" };
                                let (_, link) = tree.nodes.iter().find(|(_, node)| node.role() == Role::Link && node.label() == Some(label)).unwrap();
                                assert!(link.supports_action(Action::Focus) && link.supports_action(Action::Click));
                            }
                            let (heading, title) = tree.nodes.iter().find(|(_, node)| node.role() == Role::Label
                                && node.value() == Some(page.title()) && node.bounds().is_some_and(|rect| rect.x0 > 176.0 && rect.y1 < 42.0)).unwrap();
                            let title = title.bounds().unwrap();
                            let mut messages = 0;
                            for (id, node) in &tree.nodes {
                                if id == heading || !matches!(node.role(), Role::Label | Role::Button | Role::Link | Role::ProgressIndicator) { continue; }
                                let Some(rect) = node.bounds().filter(|rect| rect.y1 < 42.0) else { continue };
                                if matches!(node.label().or(node.value()), Some("OpenMic" | "dev build") | Some(VERSION)) { continue; }
                                messages += 1;
                                assert!(rect.x0 >= title.x1 && rect.x1 <= f64::from(size.x),
                                    "header content keeps clear of {page:?}; state={state}, zoom={zoom}, passes={passes}: {rect:?}, title={title:?}");
                            }
                            assert!(messages > 0);
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.dirty_since.is_none() && app.update_error.is_some() == (state == states.len()));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes zoomed update header screenshots; needs Python with Pillow"]
    fn snapshot_zoomed_update_headers() {
        let release = update::Release::sample("v0.11.0");
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (name, status) in [
                ("available", Status::Available(release.clone())), ("ready", Status::Ready(release.clone())),
                ("downloading", Status::Downloading(release.clone())),
                ("failed", Status::DownloadFailed { release: release.clone(), error: "Download failed".into() }),
                ("error", Status::Idle),
            ] {
                let mut app = App::stopped(Default::default());
                app.page = crate::gui::Page::Soundboard;
                app.updater.set_status_for_test(status);
                if name == "error" {
                    app.update_error = Some("The update could not be installed in the current directory. Choose a writable location and retry.".into());
                }
                let image = crate::gui::snapshot::snapshot(&mut app, &format!("update-header-{name}-{theme}"),
                    egui::vec2(700.0, 420.0) / 1.5, 1.5, dark);
                println!("SNAPSHOT {}", image.display());
                assert!(app.dirty_since.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes keyboard update focus screenshots; needs Python with Pillow"]
    fn snapshot_update_keyboard_focus() {
        struct UpdateDownload {
            app: App,
            completion: Option<crossbeam_channel::Sender<anyhow::Result<()>>>,
            scene: &'static str,
            changed: bool,
        }
        impl eframe::App for UpdateDownload {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if ui.ctx().cumulative_frame_nr() >= 18 && !self.changed {
                    self.changed = true;
                    if matches!(self.scene, "ready" | "failed" | "cancelled" | "blocked") {
                        if self.scene == "blocked" { self.app.recorder = Some(crate::record::Recorder::from_samples_for_test(&[0.25; 96])); }
                        self.completion.take().unwrap().send(if self.scene == "failed" { Err(anyhow::anyhow!("Update download fixture interrupted")) }
                            else { Ok(()) }).unwrap();
                        finish_update(&mut self.app);
                        self.completion = Some(self.app.updater.hold_next_download_for_test(0));
                    }
                }
                eframe::App::ui(&mut self.app, ui, frame);
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["downloading", "cancelling", "cancelled", "ready", "failed", "blocked", "error", "dismissed"] {
                    let mut app = App::stopped(Default::default());
                    app.page = crate::gui::Page::Soundboard;
                    let release = update::Release::sample("v0.11.0");
                    let installing = matches!(scene, "error" | "dismissed");
                    app.updater.set_status_for_test(if installing { Status::Ready(release) } else { Status::Available(release) });
                    app.updater.reject_installs_for_test("The update could not be installed in the current directory. Choose a writable location and retry.");
                    let completion = app.updater.hold_next_download_for_test(27_000_000);
                    let _check = app.updater.hold_next_check_for_test();
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { draw(&mut app, &ctx, size, Vec::new()); }
                    let output = draw(&mut app, &ctx, size, Vec::new());
                    let target = control(&output, Role::Button, if installing { "Restart to update" } else { "Update" });
                    let mut download = UpdateDownload { app, completion: Some(completion), scene, changed: false };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![action(Action::Focus, target)]);
                    events.extend(vec![Vec::new(); 6]);
                    let press = held_events(egui::Key::Space);
                    events.push(press.clone());
                    events.extend(vec![Vec::new(); 3]);
                    let second_action = matches!(scene, "cancelling" | "cancelled" | "dismissed");
                    events.push(if second_action { vec![key(egui::Key::Space, false)] } else { Vec::new() });
                    events.push(if second_action { press.clone() } else { Vec::new() });
                    events.extend(vec![Vec::new(); 5]);
                    events.extend(vec![press; 3]);
                    events.extend(vec![Vec::new(); 3]);
                    let image = crate::gui::snapshot::snapshot_with_events(&mut download,
                        &format!("update-focus-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(download.app.updater.install_attempts_for_test(), usize::from(installing));
                    assert_eq!(download.app.update_error.is_some(), scene == "error");
                    assert_eq!(download.app.updater.progress().is_some(), matches!(scene, "downloading" | "cancelling"));
                    assert_eq!(download.app.updater.download_cancelling(), scene == "cancelling");
                    assert_eq!(download.app.recorder.is_some(), scene == "blocked");
                    assert!(!download.app.show_release_notes && !download.app.quitting);
                    assert_eq!(serde_json::to_value(&download.app.settings).unwrap(), original);
                    assert!(download.app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    fn inline_markdown_becomes_plain_text() {
        assert_eq!(plain("Click **Update**, then `Restart`"), "Click Update, then Restart");
        assert_eq!(
            plain("Requires the [Visual C++ Redistributable](https://example.test/vc) on Windows"),
            "Requires the Visual C++ Redistributable on Windows"
        );
        assert_eq!(plain("an [unclosed bracket"), "an [unclosed bracket");
        assert_eq!(plain("[a](x) and [b](y)"), "a and b");
    }
}
