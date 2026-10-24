//! Soundboard tab: recording clips, the pads (with per-pad volume and
//! hotkeys), and playback.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui::{self, Color32, ComboBox, RichText, ScrollArea};

use super::{
    clip_key, clock_time, file_name, number_field, number_text_edit, short, volume_slider, App, Binding, CachedClip, AMBER, CYAN, GREEN,
    MUTED, RED, VIOLET,
};
use crate::config::Pad;
use crate::record::{self, Recorder, Source, PEAKS_PER_SECOND};
use crate::widgets;
use super::routes::{default_microphone_label, default_output_label};

#[cfg(windows)]
fn computer_audio_available(_inputs: &[String]) -> bool {
    true
}

#[cfg(not(windows))]
fn computer_audio_available(inputs: &[String]) -> bool {
    inputs.iter().any(|name| record::is_monitor_source(name))
}

pub(super) struct PadRename {
    path: PathBuf,
    text: String,
    focus: bool,
}

#[derive(Default)]
pub(super) struct ClipImport {
    /// The first path belongs to the current job unless it was cancelled.
    files: VecDeque<PathBuf>,
    job: Option<Receiver<anyhow::Result<CachedClip>>>,
    cancel: Arc<AtomicBool>,
    total: usize,
    added: usize,
    invalid: Vec<String>,
}

impl ClipImport {
    pub(super) fn has_pending_files(&self) -> bool { !self.files.is_empty() }
}

impl Drop for ClipImport {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(super) struct RecordingSave {
    job: Receiver<anyhow::Result<PathBuf>>,
    add_to_board: bool,
}

enum RecordingDestination {
    Wav(PathBuf),
    Soundboard(String),
}

pub(super) fn focus_soundboard_control(ctx: &egui::Context, target: egui::Id) {
    widgets::finish_keyboard_activation(ctx);
    ctx.memory_mut(|memory| memory.request_focus(target));
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("soundboard-focus-reveal"), target));
    ctx.request_repaint();
}

fn add_clips_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("soundboard-add")), |ui| {
        ui.add(egui::Button::new(RichText::new(label).color(Color32::BLACK)).fill(CYAN))
    }).inner
}

impl App {
    pub(super) fn reveal_pad(&mut self, index: usize, ctx: &egui::Context) {
        if self.quitting { return; }
        let Some(path) = self.settings.sounds.get(index).map(|pad| pad.path.clone()) else { return };
        self.finish_pad_rename(true);
        self.capturing = None;
        self.close_trim_editor();
        egui::Popup::close_all(ctx);
        self.pad_filter.clear();
        self.pad_to_reveal = Some(path);
        self.page = super::Page::Soundboard;
        self.set_collapsed(ctx, false);
    }

    pub(super) fn begin_pad_rename(&mut self, pad: &Pad) {
        self.finish_pad_rename(true);
        self.capturing = None;
        self.renaming_pad = Some(PadRename { path: pad.path.clone(), text: pad.display_name(), focus: true });
    }

    pub(super) fn finish_pad_rename(&mut self, save: bool) {
        let Some(edit) = self.renaming_pad.take() else { return };
        if !save {
            return;
        }
        let Some(pad) = self.settings.sounds.iter_mut().find(|pad| pad.path == edit.path) else { return };
        let name = edit.text.trim();
        if name == pad.display_name() {
            return;
        }
        let name = (!name.is_empty()).then(|| name.to_owned());
        if pad.name != name {
            pad.name = name;
            if pad.starred && self.settings.strip_width.is_none() {
                // Fit the new label when the strip hasn't been resized by hand.
                self.overlay_placement = Default::default();
            }
            self.touch();
        }
    }

    fn draw_pad_rename(&mut self, ui: &mut egui::Ui, path: &Path, rect: egui::Rect) {
        let Some(edit) = self.renaming_pad.as_mut().filter(|edit| edit.path == path) else { return };
        let first_frame = std::mem::take(&mut edit.focus);
        let pending = if first_frame {
            let name_id = ui.id().with(("sound_name", path));
            ui.input_mut(|input| {
                let opening = input.events.iter().position(|event| match event {
                    egui::Event::Key { key: egui::Key::Enter | egui::Key::Space, pressed: true, .. } => true,
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => rect.contains(*pos),
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click, target_tree: egui::accesskit::TreeId::ROOT, target_node, ..
                    }) => *target_node == name_id.accesskit_id(),
                    _ => false,
                });
                opening.map_or_else(Vec::new, |opening| {
                    let space = matches!(input.events[opening], egui::Event::Key { key: egui::Key::Space, .. });
                    let mut pending = input.events.split_off(opening + 1);
                    // Space opens the editor; its native text event is not a new name.
                    if space && matches!(pending.first(), Some(egui::Event::Text(text)) if text == " ") {
                        pending.remove(0);
                    }
                    pending
                })
            })
        } else { Vec::new() };
        // Preserve deferred typing before consuming the action opening the editor.
        if first_frame { widgets::finish_keyboard_activation(ui.ctx()); }
        let response = ui.place(
            rect,
            egui::TextEdit::singleline(&mut edit.text)
                .id(ui.id().with(("rename_pad", path)))
                .font(egui::FontId::proportional(14.0))
                .desired_width(rect.width()),
        ).on_hover_text("Enter to save, Esc to cancel. Leave blank to use the filename.");
        if first_frame {
            response.request_focus();
            if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), response.id) {
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(edit.text.chars().count()),
                )));
                state.store(ui.ctx(), response.id);
            }
            // Focus and select the existing name before applying input that
            // arrived in the same batch as the action opening this editor.
            super::defer_keyboard_input(ui, Some(response.id), pending);
        }
        let cancel = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
        if cancel || (!first_frame && (response.lost_focus() || enter)) {
            let restore = ui.input(|input| input.focused && (cancel || enter)
                && !input.pointer.any_down() && !input.pointer.any_released())
                && (response.has_focus() || (response.lost_focus() && ui.memory(|memory| memory.focused()).is_none()));
            response.surrender_focus();
            self.finish_pad_rename(!cancel);
            if restore {
                let filter = self.pad_filter.trim().to_lowercase();
                let hidden = self.settings.sounds.len() > SEARCH_FROM && self.settings.sounds.iter()
                    .find(|pad| pad.path == path).is_some_and(|pad| !pad.display_name().to_lowercase().contains(&filter));
                let target = if hidden { egui::Id::new("soundboard-search") } else { ui.id().with(("sound_name", path)) };
                focus_soundboard_control(ui.ctx(), target);
            }
        }
    }

    fn add_sounds(&mut self, ctx: &egui::Context) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Audio files",
                &["wav", "flac", "ogg", "mp3", "aiff", "aif"],
            )
            .add_filter("All files", &["*"])
            .pick_files()
        else {
            return;
        };
        self.import_sounds(paths, ctx);
    }

    /// Both the file picker and native file drops validate and cache clips
    /// before adding pads. Existing pads keep their names, trims and hotkeys.
    fn import_sounds(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let import = self.clip_import.get_or_insert_with(ClipImport::default);
        for path in paths {
            if self.settings.sounds.iter().any(|p| p.path == path) || import.files.contains(&path) {
                continue;
            }
            import.files.push_back(path);
            import.total += 1;
        }
        self.poll_clip_import(ctx);
    }

    pub(super) fn poll_clip_import(&mut self, ctx: &egui::Context) {
        let Some(mut import) = self.clip_import.take() else { return };
        let result = import.job.as_ref().and_then(|job| match job.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("clip decoder stopped unexpectedly"))),
        });
        if let Some(result) = result {
            import.job = None;
            if import.cancel.load(Ordering::Relaxed) {
                // New files can queue while cancellation finishes. The old result
                // must not consume one of them or overwrite the cancellation status.
                import.cancel = Arc::default();
                if import.files.is_empty() { return; }
            } else {
                let path = import.files.pop_front().expect("an import job has a path");
                let result = result.and_then(|clip| {
                    clip.validate_source(&path)?;
                    Ok(clip)
                });
                match result {
                    Ok(clip) => {
                        self.cache_clip(path.clone(), clip);
                        if !self.settings.sounds.iter().any(|pad| pad.path == path) {
                            self.settings.sounds.push(Pad::new(path));
                            if !ctx.memory(|memory| memory.has_focus(egui::Id::new("soundboard-search"))) {
                                self.pad_filter.clear();
                            }
                            self.touch();
                            import.added += 1;
                        }
                    }
                    Err(error) => {
                        crate::logfile::error(format_args!("could not import {}: {error:#}", path.display()));
                        import.invalid.push(file_name(&path));
                    }
                }
            }
        }

        while import.job.is_none() && let Some(path) = import.files.front().cloned() {
            let (done, job) = crossbeam_channel::bounded(1);
            let cancel = Arc::clone(&import.cancel);
            let wake = ctx.clone();
            let file = path.clone();
            let spawned = std::thread::Builder::new().name("openmic-clip-import".into()).spawn(move || {
                let result = CachedClip::read(&file, &cancel);
                let _ = done.send(result);
                wake.request_repaint();
            });
            match spawned {
                Ok(_) => import.job = Some(job),
                Err(error) => {
                    crate::logfile::error(format_args!("could not start clip import: {error}"));
                    import.files.pop_front();
                    import.invalid.push(file_name(&path));
                }
            }
        }
        if import.job.is_some() {
            self.clip_import = Some(import);
            return;
        }
        let added_message = format!("Added {} clip{}", import.added, if import.added == 1 { "" } else { "s" });
        self.sound_status = match (import.added, import.invalid.is_empty()) {
            (0, false) => (format!("Could not read: {}", import.invalid.join(", ")), RED),
            (_, false) => (format!("{added_message} · Could not read: {}", import.invalid.join(", ")), AMBER),
            (0, true) => ("These clips are already on the soundboard".into(), MUTED),
            (_, true) => (added_message, GREEN),
        };
    }

    fn cancel_clip_import(&mut self) {
        let Some(import) = &mut self.clip_import else { return };
        import.cancel.store(true, Ordering::Relaxed);
        import.files.clear();
        import.total = 0;
        import.added = 0;
        import.invalid.clear();
        if import.job.is_none() { self.clip_import = None; }
        self.sound_status = ("Import cancelled · clips already added are kept".into(), AMBER);
    }

    pub(super) fn import_dropped_sounds(&mut self, ctx: &egui::Context) {
        let paths: Vec<_> = ctx.input_mut(|input| std::mem::take(&mut input.raw.dropped_files))
            .into_iter()
            .map(|file| file.path().to_owned())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.finish_pad_rename(true);
        self.capturing = None;
        self.page = super::Page::Soundboard;
        self.pad_filter.clear();
        self.import_sounds(paths, ctx);
    }

    pub(super) fn remove_clip(&mut self, index: usize) {
        if index >= self.settings.sounds.len() {
            return;
        }
        let removed = self.settings.sounds.remove(index);
        self.cancel_clip_loading(&removed.path);
        if self.renaming_pad.as_ref().is_some_and(|edit| edit.path == removed.path) {
            self.renaming_pad = None;
        }
        let key = clip_key(&removed.path);
        if let Some(engine) = &self.engine {
            engine.stop_sound(key);
        }
        self.playing.retain(|&k| k != key);
        self.pad_progress.remove(&key);
        self.forget_clip(&removed.path);
        if self.capturing == Some(Binding::Pad(removed.path.clone())) {
            self.capturing = None;
        }
        self.sound_status = (format!("Removed {}", removed.display_name()), MUTED);
        self.touch();
    }

    fn set_pad_volume(&mut self, index: usize, volume: f32) {
        let Some(pad) = self.settings.sounds.get_mut(index) else { return };
        pad.volume = volume;
        if let Some(engine) = &self.engine {
            engine.set_sound_gain(clip_key(&pad.path), volume);
        }
        self.touch();
    }

    fn start_recording(&mut self) {
        if self.recorder.is_some() || self.take.is_some() {
            return;
        }
        let device = match self.record_source {
            Source::Microphone => self.settings.microphone.as_str(),
            Source::Computer => self.record_output.as_str(),
        };
        match Recorder::start(self.record_source, device) {
            Ok(recorder) => {
                self.recorder = Some(recorder);
                self.record_name = match self.record_source {
                    Source::Microphone => "Mic recording",
                    Source::Computer => "Computer recording",
                }
                .into();
                self.record_status = ("Recording…".into(), RED);
            }
            Err(err) => self.record_status = (format!("{err:#}"), RED),
        }
    }

    fn stop_recording(&mut self) {
        if let Some(recorder) = &mut self.recorder {
            recorder.stop_capture();
            self.record_status = ("Finishing recording…".into(), CYAN);
            self.poll_recording();
        }
    }

    fn finish_recording(&mut self) {
        if let Some(recorder) = self.recorder.take() {
            let full = recorder.is_full();
            self.record_status = match recorder.finish() {
                Ok(Some(take)) => {
                    let duration = clock_time(take.duration());
                    let status = match take.warning() {
                        Some(warning) => (format!("Partial take ready · {duration} · {warning}"), AMBER),
                        None if full => ("Reached the 4 GB WAV size limit; your take is ready".into(), AMBER),
                        None => (format!("Take ready · {duration}"), GREEN),
                    };
                    self.take = Some(Arc::new(take));
                    status
                }
                Ok(None) => ("No audio was captured. Try again while sound is playing.".into(), AMBER),
                Err(err) => {
                    self.confirm_quit = false;
                    (format!("{err:#}"), RED)
                }
            };
        }
    }

    pub(super) fn poll_recording(&mut self) {
        if self.recorder.as_ref().is_some_and(Recorder::is_stopping) {
            if self.recorder.as_ref().is_some_and(Recorder::ready_to_finish) {
                self.finish_recording();
            }
            return;
        }
        let error = self.recorder.as_ref().and_then(Recorder::error);
        if let Some(error) = error {
            crate::logfile::error(format_args!("recording stopped: {error}"));
            self.stop_recording();
            if self.recorder.is_some() {
                self.record_status = (format!("Finishing partial take · {error}"), AMBER);
            }
        } else if self.recorder.as_ref().is_some_and(Recorder::is_full) {
            self.stop_recording();
            if self.recorder.is_some() {
                self.record_status = ("WAV size limit reached · finishing your take…".into(), AMBER);
            }
        } else if self.recorder.as_mut().is_some_and(Recorder::take_skipped) {
            self.record_status = ("The disk fell behind, so a moment of audio was skipped".into(), AMBER);
        }
    }

    fn save_recording(&mut self, ctx: &egui::Context) {
        if self.take.is_none() || self.recording_save.is_some() {
            return;
        }
        let default_name = format!("{}.wav", record::safe_stem(&self.record_name));
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV audio", &["wav"])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        {
            self.record_status = ("Choose a .wav filename".into(), AMBER);
            return;
        }
        self.start_recording_save(RecordingDestination::Wav(path), ctx);
    }

    fn start_recording_save(&mut self, destination: RecordingDestination, ctx: &egui::Context) {
        if self.recording_save.is_some() {
            return;
        }
        let Some(take) = self.take.clone() else { return };
        let add_to_board = matches!(destination, RecordingDestination::Soundboard(_));
        let (done, job) = crossbeam_channel::bounded(1);
        let wake = ctx.clone();
        let spawned = std::thread::Builder::new().name("openmic-save-recording".into()).spawn(move || {
            let result = match destination {
                RecordingDestination::Wav(path) => take.save_wav(&path).map(|()| path),
                RecordingDestination::Soundboard(name) => take.add_to_library(&name),
            };
            let _ = done.send(result);
            wake.request_repaint();
        });
        match spawned {
            Ok(_) => {
                self.recording_save = Some(RecordingSave { job, add_to_board });
                self.record_status = ("Saving recording…".into(), CYAN);
                if self.page == super::Page::Record
                    && ctx.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released())
                    && let Some(owner) = ctx.memory(|memory| memory.focused())
                    && let Some((controls, status)) = ctx.data(|data|
                        data.get_temp::<(Vec<egui::Id>, egui::Id)>(egui::Id::new("recording-controls")))
                    && controls.contains(&owner)
                {
                    ctx.data_mut(|data| {
                        data.insert_temp(egui::Id::new("recording-save-return"), owner);
                        data.insert_temp(egui::Id::new("recording-next-focus"), (owner, Some(status)));
                    });
                    ctx.request_repaint();
                }
            }
            Err(error) => self.record_status = (format!("Could not start saving: {error}"), RED),
        }
    }

    pub(super) fn poll_recording_save(&mut self, ctx: &egui::Context) {
        let Some(save) = &self.recording_save else { return };
        let result = match save.job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("Recording save stopped unexpectedly; try again")),
        };
        let add_to_board = save.add_to_board;
        let controls = ctx.data(|data| data.get_temp::<(Vec<egui::Id>, egui::Id)>(egui::Id::new("recording-controls")));
        let (return_focus, pending_focus) = ctx.data_mut(|data| {
            let return_focus = data.get_temp::<egui::Id>(egui::Id::new("recording-save-return"));
            let pending_focus = data.get_temp::<(egui::Id, Option<egui::Id>)>(egui::Id::new("recording-next-focus"));
            data.remove::<egui::Id>(egui::Id::new("recording-save-return"));
            data.remove::<(egui::Id, Option<egui::Id>)>(egui::Id::new("recording-next-focus"));
            (return_focus, pending_focus)
        });
        let focused = ctx.memory(|memory| memory.focused());
        // A fast save may finish before the pending move to its status is drawn.
        let focus_owner = controls.as_ref().and_then(|(_, status)| {
            if focused == Some(*status) { Some(*status) }
            else { pending_focus.filter(|(owner, target)| focused == Some(*owner) && *target == Some(*status)).map(|(owner, _)| owner) }
        }).filter(|_| self.page == super::Page::Record);
        let succeeded = result.is_ok();
        self.recording_save = None;
        match result {
            Ok(path) => {
                if add_to_board {
                    self.record_status = (format!("Added {} to your pads", file_name(&path)), GREEN);
                    if !self.settings.sounds.iter().any(|pad| pad.path == path) {
                        self.settings.sounds.push(Pad::new(path));
                        self.touch();
                    }
                    if !ctx.memory(|memory| memory.has_focus(egui::Id::new("soundboard-search"))) {
                        self.pad_filter.clear();
                    }
                } else {
                    self.record_status = (format!("Saved {}", file_name(&path)), GREEN);
                }
                self.take = None;
            }
            Err(error) => {
                self.confirm_quit = false;
                crate::logfile::error(format_args!("recording was not saved: {error:#}"));
                self.record_status = (format!("{error:#}"), RED);
            }
        }
        if let Some(owner) = focus_owner {
            let target = if succeeded { None } else { return_focus.or_else(|| controls.as_ref().and_then(|(controls, _)| controls.first().copied())) };
            ctx.data_mut(|data| data.insert_temp(egui::Id::new("recording-next-focus"), (owner, target)));
            ctx.request_repaint();
        }
    }

    fn draw_recording(&mut self, ui: &mut egui::Ui) {
        let active = self.recorder.is_some();
        let finishing = self.recorder.as_ref().is_some_and(Recorder::is_stopping);
        let take_ready = self.take.is_some();
        let saving = self.recording_save.is_some();
        let outputs = self.outputs.clone();
        let computer_audio = computer_audio_available(&self.inputs);
        let mut start = false;
        let mut stop = false;
        let mut save = false;
        let mut add = false;
        let mut discard = false;
        let mut discard_focus = None;
        let mut record_button = None;
        let mut stop_control = None;
        let mut take_controls = Vec::new();
        let capture_transition = ui.data(|data|
            data.get_temp::<(bool, bool, Option<egui::Id>)>(egui::Id::new("recording-capture-focus")))
            .filter(|(was_active, was_finishing, _)| *was_active && (!active || finishing != *was_finishing))
            .and_then(|(_, was_finishing, stop)| {
                let status = ui.data(|data| data.get_temp::<(Vec<egui::Id>, egui::Id)>(egui::Id::new("recording-controls")))
                    .map(|(_, status)| status);
                let owner = if was_finishing { status } else { stop }?;
                let target = if finishing { status } else if take_ready { Some(egui::Id::new("recording-name")) } else { None };
                Some((owner, target))
            });
        let next_focus = ui.data_mut(|data| {
            let id = egui::Id::new("recording-next-focus");
            let focus = data.get_temp::<(egui::Id, Option<egui::Id>)>(id);
            data.remove::<(egui::Id, Option<egui::Id>)>(id);
            focus
        }).or(capture_transition).filter(|(owner, _)| ui.memory(|memory| memory.has_focus(*owner))
            && ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released()));
        widgets::card(
            ui,
            "RECORD A CLIP",
            if finishing { CYAN } else if active { RED } else { VIOLET },
            |ui| {
                widgets::badge(
                    ui,
                    if finishing {
                        "Finishing"
                    } else if active {
                        "Recording"
                    } else if saving {
                        "Saving"
                    } else if take_ready {
                        "Take ready"
                    } else {
                        "Local only"
                    },
                    if finishing { CYAN } else if active { RED } else { VIOLET },
                );
            },
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Source").weak());
                    ui.add_enabled_ui(!active && !take_ready, |ui| {
                        let microphone = ui.selectable_value(&mut self.record_source, Source::Microphone, "Microphone");
                        widgets::scroll_on_focus(&microphone);
                        if computer_audio {
                            let computer = ui.selectable_value(&mut self.record_source, Source::Computer, "Computer audio");
                            widgets::scroll_on_focus(&computer);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    let device = ui.label(RichText::new("Device").weak());
                    let recorded_device = self.recorder.as_ref().map(Recorder::device_name)
                        .or_else(|| self.take.as_ref().map(|take| take.device_name()));
                    match (self.record_source, recorded_device) {
                        (_, Some(name)) => {
                            ui.add_sized(
                                [ui.available_width().min(520.0), 18.0],
                                egui::Label::new(name).truncate(),
                            ).on_hover_text(name);
                        }
                        (Source::Microphone, None) => {
                            let name = if self.settings.microphone.is_empty() {
                                default_microphone_label()
                            } else {
                                &self.settings.microphone
                            };
                            ui.add_sized(
                                [ui.available_width().min(520.0), 18.0],
                                egui::Label::new(name).truncate(),
                            );
                        }
                        (Source::Computer, None) => {
                            let label = if self.record_output.is_empty() {
                                default_output_label().to_owned()
                            } else {
                                self.record_output.clone()
                            };
                            ui.add_enabled_ui(!active && !take_ready, |ui| {
                                let mut selected = false;
                                let output = ComboBox::from_id_salt("record-output")
                                    .width(ui.available_width().min(360.0))
                                    .selected_text(label.as_str())
                                    .truncate()
                                    .show_ui(ui, |ui| {
                                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                        for (value, name) in std::iter::once(("", default_output_label()))
                                            .chain(outputs.iter().map(|name| (name.as_str(), name.as_str())))
                                        {
                                            let choice = ui.selectable_value(&mut self.record_output, value.to_owned(), name)
                                                .on_hover_text(name);
                                            widgets::scroll_on_focus(&choice);
                                            if choice.clicked() {
                                                selected = true;
                                                ui.close();
                                            }
                                        }
                                    }).response.on_hover_text(label).labelled_by(device.id);
                                if selected { output.request_focus(); }
                                widgets::scroll_on_focus(&output);
                            });
                        }
                    }
                });
                ui.weak(match self.record_source {
                    Source::Microphone => "Records the raw mic, before OpenMic processing.",
                    Source::Computer => "Records everything playing through the selected output.",
                });
                ui.add_space(8.0);
                if let Some(recorder) = &self.recorder {
                    let elapsed = recorder.elapsed();
                    let window = 6 * PEAKS_PER_SECOND;
                    let peaks = recorder.recent_peaks(window);
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("recording-capture")), |ui| ui.horizontal_wrapped(|ui| {
                        if finishing {
                            ui.spinner();
                            ui.colored_label(widgets::text_color(ui, CYAN), "Finishing your take…");
                        } else {
                            let stop_button = ui
                                .add(egui::Button::new(RichText::new("Stop recording").color(Color32::BLACK)).fill(RED));
                            widgets::scroll_on_focus(&stop_button);
                            stop = stop_button.clicked();
                            stop_control = Some(stop_button.id);
                            let pulse = 0.55 + 0.45 * (ui.input(|i| i.time) * 4.0).sin() as f32;
                            ui.label(RichText::new("⏺").color(RED.gamma_multiply(pulse)));
                            ui.label(RichText::new(clock_time(elapsed)).color(widgets::text_color(ui, RED)).monospace());
                        }
                        ui.weak(format!("· {}", file_size(recorder.bytes())));
                    }));
                    ui.add_space(6.0);
                    // The last six seconds, scrolling in from the right.
                    widgets::waveform(ui, &peaks, Some(window), 64.0, if finishing { CYAN } else { RED });
                    ui.add_space(6.0);
                    let level = peaks.iter().rev().take(5).fold(0.0f32, |m, &p| m.max(p));
                    let width = ui.available_width();
                    widgets::level_meter(ui, "Level", Some(level), &mut self.record_hold, width);
                    if !finishing && peaks.len() >= 2 * PEAKS_PER_SECOND && is_silent(&peaks[peaks.len() - 2 * PEAKS_PER_SECOND..]) {
                        ui.colored_label(widgets::text_color(ui, AMBER), match self.record_source {
                            Source::Microphone => "No sound from the microphone yet.",
                            Source::Computer => "Nothing is playing through this output yet.",
                        });
                    }
                } else if let Some(take) = &self.take {
                    widgets::waveform(ui, take.peaks(), None, 56.0, VIOLET);
                    if is_silent(take.peaks()) {
                        ui.colored_label(widgets::text_color(ui, AMBER), "This take is silent.");
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("Name");
                        let name = ui.add_enabled(!saving, egui::TextEdit::singleline(&mut self.record_name)
                            .id(egui::Id::new("recording-name")).desired_width(260.0))
                            .labelled_by(label.id);
                        take_controls.push(name.id);
                        widgets::scroll_on_focus(&name);
                        ui.weak(clock_time(take.duration()));
                    });
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("recording-actions")), |ui| ui.horizontal_wrapped(|ui| {
                        ui.add_enabled_ui(!saving, |ui| {
                            let add_button = ui.button("Add to soundboard");
                            widgets::scroll_on_focus(&add_button);
                            add = add_button.clicked();
                            let save_button = ui.button("Save WAV…");
                            widgets::scroll_on_focus(&save_button);
                            save = save_button.clicked();
                            let discard_button = ui.small_button("Discard take");
                            widgets::scroll_on_focus(&discard_button);
                            discard = discard_button.clicked();
                            if discard && discard_button.has_focus() { discard_focus = Some(discard_button.id); }
                            take_controls.extend([add_button.id, save_button.id, discard_button.id]);
                        });
                        if saving {
                            ui.spinner();
                        }
                    }));
                } else {
                    ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("recording-start")), |ui| ui.horizontal_wrapped(|ui| {
                        let record = ui.add(egui::Button::new(RichText::new("⏺  Record").color(Color32::BLACK)).fill(RED));
                        widgets::scroll_on_focus(&record);
                        start = record.clicked();
                        record_button = Some(record);
                        ui.weak("Recording stays on this computer.");
                    }));
                }
                ui.add_space(4.0);
                let mut label = egui::Label::new(RichText::new(&self.record_status.0).color(widgets::text_color(ui, self.record_status.1)))
                    .truncate().show_tooltip_when_elided(false);
                if saving || finishing { label = label.sense(egui::Sense::focusable_noninteractive()).selectable(false); }
                let status = ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("recording-status")), |ui| ui.add(label))
                    .inner.on_hover_text(&self.record_status.0);
                widgets::scroll_on_focus(&status);
                ui.data_mut(|data| {
                    data.insert_temp(egui::Id::new("recording-controls"), (std::mem::take(&mut take_controls), status.id));
                    // Track the rendered phase even if the writer finishes between frames.
                    data.insert_temp(egui::Id::new("recording-capture-focus"), (active, finishing, stop_control));
                });
            },
        );
        if let Some((owner, target)) = next_focus
            && ui.memory(|memory| memory.focused().is_none_or(|focused| focused == owner))
            && let Some(response) = target.and_then(|target| ui.ctx().read_response(target))
                .or_else(|| if target.is_none() { record_button } else { None })
            && response.enabled() && response.sense.is_focusable()
        {
            widgets::finish_keyboard_activation(ui.ctx());
            response.request_focus();
            response.scroll_to_me(Some(egui::Align::Center));
        }
        if start {
            self.start_recording();
        }
        if stop {
            widgets::finish_keyboard_activation(ui.ctx());
            self.stop_recording();
        }
        if add || save { widgets::finish_keyboard_activation(ui.ctx()); }
        if add {
            self.start_recording_save(RecordingDestination::Soundboard(self.record_name.clone()), ui.ctx());
        }
        if save {
            self.save_recording(ui.ctx());
        }
        if discard {
            if let Some(owner) = discard_focus
                && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
            {
                widgets::finish_keyboard_activation(ui.ctx());
                ui.data_mut(|data| data.insert_temp(egui::Id::new("recording-next-focus"), (owner, None::<egui::Id>)));
                ui.ctx().request_repaint();
            }
            self.take = None;
            self.record_status = ("Take discarded".into(), MUTED);
        }
    }

    /// A pad's volume, hotkey, strip visibility, trim and removal.
    fn pad_menu(&mut self, ui: &mut egui::Ui, index: usize, remove: &mut Option<(usize, bool)>, focus_volume: bool) {
        ui.set_width(320.0);
        let max_height = ui.ctx().content_rect().height() - egui::Frame::popup(ui.style()).total_margin().sum().y;
        // Areas remember their previous height. Allow newly loaded trim controls
        // to grow the menu, while auto-shrink keeps short menus compact.
        ScrollArea::vertical().id_salt("pad-menu-scroll").min_scrolled_height(max_height).max_height(max_height)
            .show(ui, |ui| {
                self.pad_menu_contents(ui, index, remove, focus_volume);
                let layout_id = ui.make_persistent_id("pad-menu-layout");
                let layout = (ui.min_size().round(), max_height.round());
                let reflowed = ui.data_mut(|data| {
                    let previous = data.get_temp::<(egui::Vec2, f32)>(layout_id);
                    data.insert_temp(layout_id, layout);
                    previous != Some(layout)
                });
                if let Some(focused) = ui.memory(|memory| memory.focused()).and_then(|id| ui.ctx().read_response(id))
                    && focused.layer_id == ui.layer_id()
                    && (focused.gained_focus() || (focused.has_focus() && reflowed))
                    && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
                {
                    focused.scroll_to_me(Some(egui::Align::Center));
                }
            });
    }

    fn pad_menu_contents(&mut self, ui: &mut egui::Ui, index: usize, remove: &mut Option<(usize, bool)>, focus_volume: bool) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        ui.add(egui::Label::new(RichText::new(pad.display_name()).strong()).truncate());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = ui.label("Volume");
            let mut volume = pad.volume;
            let slider = volume_slider(ui, &mut volume).labelled_by(label.id);
            if focus_volume { slider.request_focus(); }
            if volume != pad.volume {
                self.set_pad_volume(index, volume);
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Hotkey");
            ui.monospace(pad.hotkey.as_deref().unwrap_or("none"));
            let set = ui.add_enabled(self.hotkeys.is_some(), egui::Button::new("Set…"))
                .on_hover_text("Plays this pad from any app")
                .on_disabled_hover_text("Global shortcuts are unavailable; restart OpenMic to retry");
            if set.clicked() {
                widgets::finish_keyboard_activation(ui.ctx());
                self.start_capture(Binding::Pad(pad.path.clone()));
                ui.close();
            } else { super::shortcut_capture_opener(&set); }
        });
        if pad.hotkey.is_some() {
            let clear = menu_control(ui, "clear-shortcut", egui::Button::new("Clear"));
            if clear.clicked() {
                self.assign_hotkey(&Binding::Pad(pad.path.clone()), None);
                if clear.has_focus() {
                    let keyboard = ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released());
                    clear.surrender_focus();
                    if keyboard {
                        widgets::finish_keyboard_activation(ui.ctx());
                        ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::Next));
                    }
                }
            }
        }
        if let Some(hotkeys) = &self.hotkeys {
            if let Some(shortcut) = &pad.hotkey
                && hotkeys.was_refused(shortcut, &super::Command::PlayPad(pad.path.clone()))
            {
                ui.colored_label(widgets::text_color(ui, AMBER), "This shortcut is unavailable. Choose another combination.");
            }
        } else {
            ui.colored_label(widgets::text_color(ui, AMBER), "Global shortcuts are unavailable. Restart OpenMic to retry.");
        }
        let mut starred = pad.starred;
        if menu_control(ui, "strip", egui::Checkbox::new(&mut starred, "Show on the strip")).changed() {
            self.toggle_star(index);
        }
        ui.separator();
        ui.scope_builder(egui::UiBuilder::new().id(ui.make_persistent_id("pad-trim")), |ui| self.trim_editor(ui, index));
        ui.separator();
        let remove_button = menu_control(ui, "remove", egui::Button::new("Remove from soundboard"));
        if remove_button.clicked() {
            let keyboard = remove_button.has_focus() && ui.input(|input|
                input.focused && !input.pointer.any_down() && !input.pointer.any_released());
            *remove = Some((index, keyboard));
            ui.close();
        }
    }

    /// A clip's full length in seconds, using decoded audio when available
    /// because container headers can be approximate.
    fn pad_length(&mut self, path: &Path) -> Option<f32> {
        if let Some(clip) = self.clip_cache.get(path) {
            return Some(clip.samples.len() as f32 / crate::dsp::SR as f32);
        }
        self.pad_durations.get(path)
    }

    fn toggle_star(&mut self, index: usize) {
        if let Some(pad) = self.settings.sounds.get_mut(index) {
            pad.starred = !pad.starred;
            self.touch();
        }
    }

    /// Move the pad at `from` to `to` (both board positions).
    fn move_pad(&mut self, from: usize, to: usize) {
        let pads = &mut self.settings.sounds;
        if from == to || from >= pads.len() || to >= pads.len() {
            return;
        }
        let pad = pads.remove(from);
        pads.insert(to, pad);
        self.touch();
    }

    /// Trim a pad: drag the start and end on its waveform, or type them.
    fn trim_editor(&mut self, ui: &mut egui::Ui, index: usize) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        ui.set_min_width(300.0);
        ui.label(RichText::new("Trim").strong());
        let Some((length, peaks)) = self.trim_waveform(&pad.path, ui.ctx()) else {
            if let Some(error) = self.clip_errors.get(&pad.path) {
                ui.colored_label(widgets::text_color(ui, RED), short(error)).on_hover_text(error);
                if menu_control(ui, "retry-waveform", egui::Button::new("Retry waveform")).clicked() {
                    self.clip_errors.remove(&pad.path);
                    ui.ctx().request_repaint();
                }
            } else {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Loading waveform…"); });
            }
            return;
        };
        let (mut start, mut end) = (pad.start, pad.end.unwrap_or(length));
        let (mut start_changed, mut end_changed) = (false, false);
        let min_trim = MIN_TRIM.min(length);

        // Drag near either edge of the kept part to move it.
        let wave = widgets::waveform(ui, &peaks, None, 44.0, CYAN);
        let rect = wave.rect.shrink2(egui::vec2(4.0, 0.0));
        // Start and End provide keyboard editing; the waveform uses the pointer.
        let drag = ui.interact(wave.rect, ui.id().with(("trim", index)), egui::Sense::CLICK | egui::Sense::DRAG);
        let time_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * length;
        let x_of = |t: f32| rect.left() + t.clamp(0.0, length) / length.max(f32::EPSILON) * rect.width();
        if (drag.dragged_by(egui::PointerButton::Primary) || drag.drag_stopped_by(egui::PointerButton::Primary))
            && let Some(pos) = drag.interact_pointer_pos()
        {
            let t = time_at(pos.x);
            let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
            let grab_start = ui.memory_mut(|m| {
                let id = ui.id().with(("trim grab", index));
                if drag.drag_started() {
                    m.data.insert_temp(id, (origin.x - x_of(start)).abs() <= (origin.x - x_of(end)).abs());
                }
                m.data.get_temp::<bool>(id).unwrap_or(true)
            });
            if grab_start {
                start = t.min((end.min(length) - min_trim).max(0.0));
                start_changed = true;
            } else {
                end = t.max((start + min_trim).min(length));
                end_changed = true;
            }
        }
        if drag.hovered() || drag.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        // Shade what's cut, and mark the edges.
        let painter = ui.painter_at(wave.rect);
        let shade = ui.visuals().extreme_bg_color.gamma_multiply(0.85);
        painter.rect_filled(egui::Rect::from_x_y_ranges(wave.rect.left()..=x_of(start), wave.rect.y_range()), 0.0, shade);
        painter.rect_filled(egui::Rect::from_x_y_ranges(x_of(end)..=wave.rect.right(), wave.rect.y_range()), 0.0, shade);
        for x in [x_of(start), x_of(end)] {
            painter.line_segment([egui::pos2(x, wave.rect.top()), egui::pos2(x, wave.rect.bottom())], egui::Stroke::new(2.0, AMBER));
        }

        // DragValue also parses its displayed (rounded) number on blur. Ignore
        // that alone, but accept typed input and explicit accessibility edits
        // even when focus has just moved away from the field.
        let entered_text = number_text_edit(ui);
        let edited = |response: egui::Response, focus_before: Option<egui::Id>| {
            use egui::accesskit::Action;
            response.changed() && (!response.lost_focus() || response.has_focus() || response.dragged()
                || response.ctx.input(|input| (entered_text && focus_before == Some(response.id)
                    && input.key_pressed(egui::Key::Enter) && !input.key_pressed(egui::Key::Tab))
                    || [Action::SetValue, Action::Increment, Action::Decrement].into_iter()
                        .any(|action| input.has_accesskit_action_request(response.id, action))))
        };
        ui.horizontal(|ui| {
            let start_label = ui.label("Start");
            // A shorter source or a previously saved narrow trim must not be
            // rewritten just because the controls are drawn.
            let focus_before = ui.memory(|memory| memory.focused());
            start_changed |= edited(number_field(ui, egui::DragValue::new(&mut start).range(0.0..=(end.min(length) - min_trim).max(0.0))
                .clamp_existing_to_range(false).speed(0.01).suffix(" s").max_decimals(3)).labelled_by(start_label.id), focus_before);
            let end_label = ui.label("End");
            let focus_before = ui.memory(|memory| memory.focused());
            end_changed |= edited(number_field(ui, egui::DragValue::new(&mut end).range((start + min_trim).min(length)..=length)
                .clamp_existing_to_range(false).speed(0.01).suffix(" s").max_decimals(3)).labelled_by(end_label.id), focus_before);
            if (start > 0.0 || pad.end.is_some()) && menu_control(ui, "reset-trim", egui::Button::new("Reset").small()).clicked() {
                (start, end) = (0.0, length);
                (start_changed, end_changed) = (true, true);
            }
        });
        start = if start_changed { start.clamp(0.0, (end.min(length) - min_trim).max(0.0)) } else { pad.start };
        end = if end_changed { end.clamp((start + min_trim).min(length), length) } else { pad.end.unwrap_or(length) };
        if start > length || end > length {
            ui.add(egui::Label::new(RichText::new("Saved trim extends past this clip. Adjust it or Reset.").color(widgets::text_color(ui, AMBER))).wrap());
        }
        ui.weak(format!("Plays {} of {}", clock_seconds((end.min(length) - start.min(length)).max(0.0)), clock_seconds(length)));

        let new_end = if end_changed { (end < length - 0.005).then_some(end) } else { pad.end };
        let new_start = if start_changed { if start < 0.005 { 0.0 } else { start } } else { pad.start };
        if (new_start - pad.start).abs() > 1e-4 || new_end != pad.end {
            let pad = &mut self.settings.sounds[index];
            pad.start = new_start;
            pad.end = new_end;
            self.touch();
        }
    }

    /// Record a clip from the microphone or computer audio.
    pub(super) fn draw_record_page(&mut self, ui: &mut egui::Ui) {
        ui.weak("Record your microphone or what's playing on your computer, then save it or add it to your pads.");
        ui.add_space(8.0);
        self.draw_recording(ui);
    }

    pub(super) fn draw_sound_tab(&mut self, ui: &mut egui::Ui) {
        let focus_to_reveal = ui.data_mut(|data| {
            let id = egui::Id::new("soundboard-focus-reveal");
            let target = data.get_temp::<egui::Id>(id);
            data.remove::<egui::Id>(id);
            target
        }).filter(|target| ui.memory(|memory| memory.has_focus(*target))
            && ui.input(|input| input.focused && !input.pointer.any_down() && !input.pointer.any_released()
                && !input.key_pressed(egui::Key::Tab)));
        let reveal = self.pad_to_reveal.take();
        ui.weak("Drop audio files to add clips. Click a pad to play, pause or resume.");
        if let Some(import) = &self.clip_import {
            let mut cancel = false;
            let cancelling = import.cancel.load(Ordering::Relaxed);
            ui.horizontal_wrapped(|ui| {
                ui.spinner();
                ui.label(if cancelling {
                    "Finishing cancellation…".into()
                } else {
                    format!("Adding clips · {} of {}", import.total - import.files.len(), import.total)
                });
                let response = ui.add_enabled(import.has_pending_files(), egui::Button::new("Cancel import"));
                widgets::scroll_on_focus(&response);
                cancel = response.clicked();
            });
            if let Some(path) = import.files.front() {
                let name = if cancelling { format!("Next: {}", file_name(path)) } else { file_name(path) };
                ui.add(egui::Label::new(RichText::new(&name).weak()).truncate()).on_hover_text(name);
            }
            if cancel {
                self.cancel_clip_import();
            }
        }
        if self.engine.is_none() && !self.settings.sounds.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(
                widgets::text_color(ui, AMBER),
                if self.waiting_for_device {
                    "Waiting for your audio device before clips can play."
                } else {
                    "Start OpenMic (bottom left) to play clips."
                },
            );
        }
        if let Some(Binding::Pad(path)) = &self.capturing {
            let name = self.settings.sounds.iter().find(|pad| &pad.path == path)
                .map_or_else(|| file_name(path), Pad::display_name);
            ui.add_space(6.0);
            ui.colored_label(
                widgets::text_color(ui, CYAN),
                format!("Press the hotkey for {name}, or Esc to cancel. {}", self.hotkey_status.0),
            );
        }
        ui.add_space(12.0);

        let sounds = self.settings.sounds.clone();
        let mut add_header = None;
        let mut add_empty = None;
        let mut remove_index = None;
        let mut pad_scope = None;
        let mut trim_open = false;
        widgets::card(
            ui,
            "SOUND PADS",
            CYAN,
            |ui| {
                if !sounds.is_empty() {
                    let response = add_clips_button(ui, "Add clips");
                    widgets::scroll_on_focus(&response);
                    if focus_to_reveal == Some(response.id) { response.scroll_to_me(Some(egui::Align::Center)); }
                    add_header = Some(response);
                    widgets::badge(ui, &format!("{} clips", sounds.len()), CYAN);
                }
            },
            |ui| {
                if sounds.is_empty() {
                    egui::Frame::new()
                        .fill(CYAN.gamma_multiply(0.08))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::same(18))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add_space(22.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("No clips yet").size(17.0).strong());
                                ui.weak("Drop audio files here, or choose a few below.");
                                ui.add_space(10.0);
                                let response = add_clips_button(ui, "Add your first clips");
                                widgets::scroll_on_focus(&response);
                                if focus_to_reveal == Some(response.id) { response.scroll_to_me(Some(egui::Align::Center)); }
                                add_empty = Some(response);
                                ui.add_space(6.0);
                                ui.weak("WAV, FLAC, OGG, MP3, and AIFF");
                            });
                            ui.add_space(22.0);
                        });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("Click a name to rename; drag to reorder.");
                        // A search box once the board gets long.
                        if sounds.len() > SEARCH_FROM {
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.pad_filter)
                                    .id(egui::Id::new("soundboard-search"))
                                    .hint_text("Search clips")
                                    .desired_width(ui.available_width().min(140.0)),
                            );
                            widgets::scroll_on_focus(&response);
                            if focus_to_reveal == Some(response.id) { response.scroll_to_me(Some(egui::Align::Center)); }
                        }
                    });
                    ui.add_space(8.0);
                    let filter = if sounds.len() > SEARCH_FROM { self.pad_filter.trim().to_lowercase() } else { String::new() };
                    let shown: Vec<usize> = (0..sounds.len())
                        .filter(|&i| filter.is_empty() || sounds[i].display_name().to_lowercase().contains(&filter))
                        .collect();
                    if shown.is_empty() {
                        ui.weak(format!("No clips match \"{}\"", self.pad_filter.trim()));
                    }
                    let mut moved = None;
                    let mut reveal_grid = false;
                    // Leave room for one full pad and its keyboard focus outline.
                    let pads = ScrollArea::vertical()
                        .id_salt("sound_pads")
                        .min_scrolled_height(124.0)
                        .max_height((ui.clip_rect().height() - 16.0).clamp(124.0, 258.0))
                        .show(ui, |ui| {
                            let gap = 8.0;
                            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                            // Keep a readable name beside the play and remove controls.
                            let columns = ((ui.available_width() + gap) / (150.0 + gap)).floor().clamp(1.0, 3.0) as usize;
                            let width = (ui.available_width() - (columns - 1) as f32 * gap) / columns as f32;
                            for (row_number, row) in shown.chunks(columns).enumerate() {
                                ui.horizontal(|ui| {
                                    pad_scope = Some(ui.id());
                                    for (column, &i) in row.iter().enumerate() {
                                        let pad = &sounds[i];
                                        let key = clip_key(&pad.path);
                                        let loaded = self.pad_progress.get(&key).copied();
                                        let length = loaded.map(|clip| clip.duration).or_else(|| {
                                            self.pad_length(&pad.path).map(|full| trimmed_length(full, pad.start, pad.end))
                                        });
                                        let state = PadState {
                                            loading: self.clip_loading(&pad.path),
                                            error: self.clip_errors.get(&pad.path).map(String::as_str),
                                            playing: self.playing.contains(&key),
                                            progress: loaded.map(|clip| clip.fraction),
                                            length,
                                            hotkey: pad.hotkey.as_deref(),
                                            starred: pad.starred,
                                            capturing: self.capturing
                                                == Some(Binding::Pad(pad.path.clone())),
                                            renaming: self.renaming_pad.as_ref().is_some_and(|edit| edit.path == pad.path),
                                        };
                                        let (play, remove, star, name, reveal_page) = clip_pad(ui, i, pad, width, state, (row_number, column));
                                        reveal_grid |= reveal_page;
                                        // A removed pad shifts the rows. Reveal the new grid
                                        // position rather than the destination's previous rect.
                                        reveal_grid |= focus_to_reveal.is_some_and(|target|
                                            [&play, &remove, &star, &name].into_iter().any(|response| response.id == target));
                                        if reveal.as_deref() == Some(pad.path.as_path()) {
                                            play.scroll_to_me(Some(egui::Align::Center));
                                            play.request_focus();
                                        }
                                        if name.clicked() {
                                            self.begin_pad_rename(pad);
                                        } else if star.clicked() {
                                            self.toggle_star(i);
                                        } else if play.clicked() {
                                            self.toggle_clip(i, ui.ctx());
                                        }
                                        if remove.clicked() {
                                            let keyboard = remove.has_focus() && ui.input(|input|
                                                input.focused && !input.pointer.any_down() && !input.pointer.any_released());
                                            remove_index = Some((i, keyboard));
                                        }
                                        self.draw_pad_rename(ui, &pad.path, name.rect);
                                        // Drag a pad onto another to move it there.
                                        play.dnd_set_drag_payload(i);
                                        if let Some(from) = play.dnd_release_payload::<usize>() {
                                            moved = Some((*from, i));
                                        }
                                        let menu_id = egui::Popup::default_response_id(&play);
                                        let origin_id = menu_id.with("keyboard-origin");
                                        let keyboard_owner = [&play, &remove, &star, &name].into_iter()
                                            .find(|response| response.has_focus())
                                            .filter(|_| ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F10)));
                                        if let Some(owner) = keyboard_owner {
                                            // A fixed anchor keeps the menu beside the pad even
                                            // when the keyboard user has no pointer position.
                                            egui::Popup::open_id(ui.ctx(), menu_id);
                                            ui.data_mut(|data| data.insert_temp(origin_id, (owner.id, play.interact_rect.left_bottom())));
                                        } else if play.secondary_clicked() {
                                            ui.data_mut(|data| data.remove::<(egui::Id, egui::Pos2)>(origin_id));
                                        }
                                        let mut popup = egui::Popup::context_menu(&play)
                                            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                                        if let Some((_, position)) = ui.data(|data| data.get_temp::<(egui::Id, egui::Pos2)>(origin_id)) {
                                            popup = popup.at_position(position);
                                        }
                                        let menu = popup.show(|ui| {
                                            trim_open = true;
                                            self.pad_menu(ui, i, &mut remove_index, keyboard_owner.is_some());
                                        });
                                        if !egui::Popup::is_id_open(ui.ctx(), menu_id) {
                                            let owner = ui.data_mut(|data| {
                                                let origin = data.get_temp::<(egui::Id, egui::Pos2)>(origin_id);
                                                data.remove::<(egui::Id, egui::Pos2)>(origin_id);
                                                origin.map(|(owner, _)| owner)
                                            });
                                            if let Some(owner) = owner
                                                && menu.is_some()
                                            {
                                                if self.capturing.as_ref() == Some(&Binding::Pad(pad.path.clone())) {
                                                    focus_soundboard_control(ui.ctx(), owner);
                                                } else if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                                                    ui.memory_mut(|memory| memory.request_focus(owner));
                                                }
                                            }
                                        }
                                    }
                                });
                            }
                        });
                    if reveal_grid {
                        // The inner scroll area consumes the pad's scroll target.
                        // Reveal its viewport in the page as well, so the whole pad fits.
                        ui.scroll_to_rect(pads.inner_rect, None);
                    }
                    if let Some((from, to)) = moved {
                        self.move_pad(from, to);
                    }
                }
            },
        );
        let add = add_header.as_ref().or(add_empty.as_ref());
        if add.is_some_and(|response| response.clicked()) {
            self.add_sounds(ui.ctx());
        }
        if let Some((index, keyboard)) = remove_index {
            self.remove_clip(index);
            if keyboard && let Some(scope) = pad_scope {
                let filter = if self.settings.sounds.len() > SEARCH_FROM { self.pad_filter.trim().to_lowercase() } else { String::new() };
                let mut remaining = self.settings.sounds.iter().enumerate()
                    .filter(|(_, pad)| filter.is_empty() || pad.display_name().to_lowercase().contains(&filter));
                let next = remaining.clone().find(|(i, _)| *i >= index).or_else(|| remaining.next_back());
                let target = next.map(|(_, pad)| scope.with(("sound_play", &pad.path))).or_else(|| {
                    if self.settings.sounds.is_empty() { add.map(|response| response.id) } else { Some(egui::Id::new("soundboard-search")) }
                });
                if let Some(target) = target { focus_soundboard_control(ui.ctx(), target); }
            }
        }
        if !trim_open { self.close_trim_editor(); }

        ui.add_space(10.0);
        let playing: Vec<String> = self
            .settings
            .sounds
            .iter()
            .filter(|pad| self.playing.contains(&clip_key(&pad.path)))
            .map(Pad::display_name)
            .collect();
        let engine_running = self.engine.is_some();
        let loading = self.has_pending_playback();
        let waiting_for_device = self.waiting_for_device;
        let paused = self.pad_progress.len().saturating_sub(playing.len());
        let title = match playing.len() {
            0 if self.settings.sounds.is_empty() => "Add a clip to get started".to_owned(),
            0 if loading => "Loading audio…".to_owned(),
            0 if paused == 1 => "1 clip paused".to_owned(),
            0 if paused > 1 => format!("{paused} clips paused"),
            0 => "Choose a pad above".to_owned(),
            1 => playing[0].clone(),
            n => format!("{n} clips playing"),
        };
        // Header status changes must not replace focused playback controls.
        ui.scope_builder(egui::UiBuilder::new().id(egui::Id::new("soundboard-playback")), |ui| {
            let width_id = ui.id().with("width");
            let width = ui.available_width();
            let reflowed = ui.data_mut(|data| {
                let previous = data.get_temp::<f32>(width_id);
                data.insert_temp(width_id, width);
                previous.is_some_and(|previous| previous != width)
            });
            widgets::card(
                ui,
                "PLAYBACK",
                VIOLET,
                |ui| {
                    if !playing.is_empty() {
                        widgets::badge(ui, "Playing", GREEN);
                    } else if loading {
                        widgets::badge(ui, "Loading", CYAN);
                    } else if paused > 0 {
                        widgets::badge(ui, "Paused", AMBER);
                    } else if engine_running {
                        widgets::badge(ui, "Ready", GREEN);
                    } else if waiting_for_device {
                        widgets::badge(ui, "Waiting for device", AMBER);
                    } else {
                        widgets::badge(ui, "OpenMic stopped", AMBER);
                    }
                },
                |ui| {
                    let stop = ui.horizontal_wrapped(|ui| {
                        let title_width = (ui.available_width() - 115.0).max(140.0);
                        ui.vertical(|ui| {
                            ui.set_max_width(title_width);
                            ui.add(
                                egui::Label::new(RichText::new(title).size(15.0).strong()).truncate(),
                            );
                            ui.weak(if self.settings.sounds.is_empty() {
                                "WAV, FLAC, OGG, MP3, and AIFF supported"
                            } else if loading {
                                "Click a loading pad to cancel."
                            } else {
                                "Click a pad to play, pause or resume."
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let response = ui.add_enabled(!self.pad_progress.is_empty() || loading, egui::Button::new("Stop clips"));
                            if response.clicked() {
                                self.stop_clips();
                            }
                            response
                        }).inner
                    }).inner;
                    ui.add_space(8.0);
                    let volume = ui.horizontal_wrapped(|ui| {
                        let label = ui.label("Clip volume");
                        let original = self.settings.sound_volume;
                        let response = volume_slider(ui, &mut self.settings.sound_volume).labelled_by(label.id);
                        if self.settings.sound_volume != original {
                            self.apply_live();
                        }
                        ui.weak("Clips only");
                        response
                    }).inner;
                    let overlap = ui
                        .checkbox(&mut self.settings.overlap_clips, "Let clips overlap")
                        .on_hover_text("Play pads over each other instead of replacing the current clip");
                    if overlap.changed() {
                        self.touch();
                    }
                    for response in [&stop, &volume, &overlap] {
                        if reflowed && response.has_focus()
                            && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
                        {
                            response.scroll_to_me(Some(egui::Align::Center));
                        } else {
                            widgets::scroll_on_focus(response);
                        }
                    }
                    if self.sound_status.0 != "Ready" && !self.sound_status.0.starts_with("Playing ·") {
                        ui.add_space(5.0);
                        ui.label(RichText::new(&self.sound_status.0).color(widgets::text_color(ui, self.sound_status.1)));
                    }
                },
            );
        });
    }
}

/// "12.4 MB" for a byte count.
fn file_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / f64::from(1u32 << 30)),
        b => format!("{:.1} MB", b as f64 / f64::from(1u32 << 20)),
    }
}

/// Whether every block is below -60 dBFS.
fn is_silent(peaks: &[f32]) -> bool {
    peaks.iter().all(|&p| p < 1e-3)
}

/// Show a search box above the pads once there are more than this many.
const SEARCH_FROM: usize = 9;
/// The shortest a trimmed clip can be, seconds.
const MIN_TRIM: f32 = 0.05;

/// Show milliseconds for very short clips, seconds below ten, then minutes.
fn clock_seconds(seconds: f32) -> String {
    let seconds = seconds.max(0.0);
    if seconds > 0.0 && seconds < 0.001 {
        "<1 ms".into()
    } else if (0.001..0.1).contains(&seconds) {
        format!("{:.0} ms", seconds * 1000.0)
    } else if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{}:{:02}", (seconds / 60.0) as u32, seconds as u32 % 60)
    }
}

/// How long a pad plays with its trim points.
fn trimmed_length(full: f32, start: f32, end: Option<f32>) -> f32 {
    (end.unwrap_or(full).min(full) - start.max(0.0)).max(0.0)
}

#[derive(Clone, Copy, Default)]
struct PadState<'a> {
    loading: bool,
    error: Option<&'a str>,
    playing: bool,
    /// How far through the clip playback is, including while paused.
    progress: Option<f32>,
    /// How long the pad plays, seconds (after trimming).
    length: Option<f32>,
    hotkey: Option<&'a str>,
    /// Shown on the collapsed strip.
    starred: bool,
    /// Waiting for the user to press this pad's new hotkey.
    capturing: bool,
    renaming: bool,
}

/// Menu actions keep their IDs when preceding controls appear or disappear.
fn menu_control(ui: &mut egui::Ui, name: &str, widget: impl egui::Widget) -> egui::Response {
    ui.scope_builder(egui::UiBuilder::new().id(ui.make_persistent_id(("pad-menu", name))), |ui| ui.add(widget)).inner
}

fn clip_pad(
    ui: &mut egui::Ui,
    index: usize,
    pad: &Pad,
    width: f32,
    state: PadState,
    position: (usize, usize),
) -> (egui::Response, egui::Response, egui::Response, egui::Response, bool) {
    let playing = state.playing;
    let paused = !playing && state.progress.is_some();
    let failed = state.error.is_some() && !state.loading && !playing && !paused;
    let action = if state.loading { "Cancel loading" } else if playing { "Pause" } else if paused { "Resume" } else if failed { "Retry" } else { "Play" };
    let path = &pad.path;
    let name = pad.display_name();
    let format = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_else(|| "AUDIO".into());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 108.0), egui::Sense::hover());
    let body = egui::Rect::from_min_size(rect.min, egui::vec2(width, 78.0));
    let play_rect = egui::Rect::from_min_max(
        body.min,
        egui::pos2(body.right() - 36.0, body.bottom()),
    );
    let remove_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 36.0, rect.top()),
        body.max,
    );
    let play_response = ui.interact(
        play_rect,
        ui.id().with(("sound_play", path)),
        egui::Sense::click_and_drag(),
    );
    let name_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 50.0, rect.top() + 18.0),
        egui::pos2(play_rect.right() - 4.0, rect.top() + 42.0),
    );
    let name_response = ui.interact(
        name_rect,
        ui.id().with(("sound_name", &pad.path)),
        if state.renaming { egui::Sense::hover() } else { egui::Sense::click() },
    ).on_hover_cursor(egui::CursorIcon::Text);
    name_response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("Rename {name}"))
    });
    let star_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 8.0, body.bottom() + 2.0),
        egui::pos2(rect.right() - 8.0, rect.bottom() - 6.0),
    );
    let star_response = ui.interact(star_rect, ui.id().with(("sound_star", path)), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    star_response.widget_info(|| {
        let label = if state.starred { format!("Remove {name} from the strip") } else { format!("Add {name} to the strip") };
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), state.starred, label)
    });
    // Another pad is being dragged over this one: it will land here.
    let drop_here = play_response.dnd_hover_payload::<usize>().is_some_and(|from| *from != index);
    let remove_response = ui.interact(
        remove_rect,
        ui.id().with(("sound_remove", path)),
        egui::Sense::click(),
    );
    play_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("{action} {name}"),
        )
    });
    remove_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Remove {name}"),
        )
    });

    let rename_response = state.renaming.then(|| ui.ctx().read_response(ui.id().with(("rename_pad", path)))).flatten();
    let mut reveal_page = false;
    if let Some(response) = [&play_response, &remove_response, &star_response, &name_response]
        .into_iter().chain(rename_response.iter()).find(|response| response.has_focus())
    {
        // Track the grid position and viewport height, so wheel scrolling
        // can move away from a focused pad without snapping back.
        let position_id = ui.make_persistent_id("focused-sound-pad-position");
        let position = (response.id, position.0, position.1, egui::vec2(width, ui.ctx().content_rect().height()));
        let moved = ui.data_mut(|data| {
            let previous = data.get_temp::<(egui::Id, usize, usize, egui::Vec2)>(position_id);
            data.insert_temp(position_id, position);
            previous != Some(position)
        });
        if (response.gained_focus() || moved) && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released()) {
            reveal_page = !ui.clip_rect().contains_rect(rect);
            ui.scroll_to_rect(rect, None);
        }
    }

    let visuals = ui.visuals();
    let accent = if state.loading { CYAN } else if playing { GREEN } else if paused { AMBER } else if failed { RED } else { CYAN };
    let fill = if playing || paused || state.loading || failed {
        accent.gamma_multiply(0.13)
    } else if play_response.hovered() {
        CYAN.gamma_multiply(0.06)
    } else {
        visuals.faint_bg_color
    };
    let border = if drop_here {
        AMBER
    } else if playing || paused || state.loading || failed || play_response.hovered() || state.capturing {
        accent
    } else {
        visuals.widgets.noninteractive.bg_stroke.color
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        9.0,
        fill,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );
    if play_response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            11.0,
            visuals.selection.stroke,
            egui::StrokeKind::Outside,
        );
    }

    let center = egui::pos2(rect.left() + 28.0, body.center().y);
    painter.circle_filled(center, 16.0, accent);
    if state.loading {
        painter.rect_filled(egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0)), 1.0, Color32::from_gray(18));
    } else if playing {
        for dx in [-4.0, 4.0] {
            let bar = egui::Rect::from_center_size(center + egui::vec2(dx, 0.0), egui::vec2(3.0, 12.0));
            painter.rect_filled(bar, 0.0, Color32::from_gray(18));
        }
    } else {
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(center.x - 3.0, center.y - 6.0),
                egui::pos2(center.x + 7.0, center.y),
                egui::pos2(center.x - 3.0, center.y + 6.0),
            ],
            Color32::from_gray(18),
            egui::Stroke::NONE,
        ));
    }

    let text_area = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 52.0, rect.top() + 9.0),
        egui::pos2(rect.right() - 43.0, body.bottom() - 8.0),
    );
    let text_painter = painter.with_clip_rect(text_area);
    let paint_text = |text: String, y: f32, size: f32, color| {
        let mut job = egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(size), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_area.width());
        let galley = text_painter.layout_job(job);
        text_painter.galley(egui::pos2(text_area.left(), rect.top() + y - galley.size().y / 2.0), galley, color);
    };
    if !state.renaming {
        if name_response.hovered() || name_response.has_focus() {
            painter.rect_filled(name_rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
        }
        paint_text(
            name.clone(), 30.0, 14.0,
            if name_response.hovered() || name_response.has_focus() { widgets::text_color(ui, CYAN) } else { visuals.text_color() },
        );
    }
    let format = match state.length {
        Some(length) => format!("{format} · {}", clock_seconds(length)),
        None => format,
    };
    let (detail, detail_color) = if state.capturing {
        ("Press a hotkey…".to_owned(), CYAN)
    } else if state.loading {
        ("Loading…".to_owned(), CYAN)
    } else if drop_here {
        ("Move here".to_owned(), AMBER)
    } else if playing {
        (format!("{format} · Playing"), GREEN)
    } else if paused {
        (format!("{format} · Paused"), AMBER)
    } else if failed {
        ("Failed".to_owned(), RED)
    } else if let Some(hotkey) = state.hotkey {
        (format!("{format} · {hotkey}"), visuals.weak_text_color())
    } else {
        (format, visuals.weak_text_color())
    };
    // Playback progress along the bottom of the pad.
    if let Some(progress) = state.progress {
        let track = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 10.0, body.bottom() - 6.0),
            egui::pos2(remove_rect.left() - 8.0, body.bottom() - 3.0),
        );
        painter.rect_filled(track, 1.5, accent.gamma_multiply(0.2));
        let done = egui::Rect::from_min_max(track.min, egui::pos2(track.left() + track.width() * progress.clamp(0.0, 1.0), track.bottom()));
        painter.rect_filled(done, 1.5, accent);
    }
    paint_text(detail, 55.0, 11.0, widgets::text_color(ui, detail_color));
    painter.line_segment(
        [
            egui::pos2(remove_rect.left(), rect.top() + 8.0),
            egui::pos2(remove_rect.left(), body.bottom() - 8.0),
        ],
        egui::Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
    if remove_response.hovered() {
        painter.rect_filled(remove_rect.shrink(4.0), 6.0, RED.gamma_multiply(0.14));
    }
    if remove_response.has_focus() {
        painter.rect_stroke(
            remove_rect.shrink(3.0),
            6.0,
            visuals.selection.stroke,
            egui::StrokeKind::Outside,
        );
    }
    painter.text(
        remove_rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        egui::FontId::proportional(18.0),
        if remove_response.hovered() {
            widgets::text_color(ui, RED)
        } else {
            visuals.weak_text_color()
        },
    );
    // A separate, labeled strip toggle below the playback hit target.
    let star_visuals = ui.style().interact(&star_response);
    let star_color = if state.starred { widgets::text_color(ui, AMBER) } else { star_visuals.text_color() };
    painter.rect(
        star_rect,
        4.0,
        if state.starred { AMBER.gamma_multiply(0.13) } else { star_visuals.weak_bg_fill },
        if state.starred { egui::Stroke::new(1.0, AMBER) } else { star_visuals.bg_stroke },
        egui::StrokeKind::Inside,
    );
    if star_response.has_focus() {
        painter.rect_stroke(star_rect.expand(2.0), 6.0, visuals.selection.stroke, egui::StrokeKind::Outside);
    }
    painter.text(
        star_rect.center(),
        egui::Align2::CENTER_CENTER,
        if state.starred { "★ On strip" } else { "☆ Add to strip" },
        egui::FontId::proportional(11.0),
        star_color,
    );
    let star_response = star_response.on_hover_text(if state.starred {
        "Remove this pad from the strip"
    } else {
        "Add this pad to the strip"
    });
    let mut hint = format!(
        "Click to {}, drag to reorder, right-click or press Shift+F10 for volume, hotkey and trim\n{}",
        action.to_lowercase(),
        path.display()
    );
    if let Some(error) = state.error { hint.push_str(&format!("\n\nCould not load this clip: {error}")); }
    let play_response = play_response.on_hover_text(hint);
    let remove_response = remove_response.on_hover_text(format!(
        "Remove {} from the soundboard. The file stays on disk.",
        file_name(path)
    ));
    let name_response = name_response.on_hover_text(format!("{name}\nClick to rename"));
    (play_response, remove_response, star_response, name_response, reveal_page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn recording_and_take_labels_keep_the_device_that_was_opened() {
        for source in [Source::Microphone, Source::Computer] {
            let mut app = App::stopped(Default::default());
            app.page = super::super::Page::Record;
            app.record_source = source;
            let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
            let recorded_device = recorder.device_name().to_owned();
            app.recorder = Some(recorder);
            app.settings.microphone = "Next microphone".into();
            app.record_output = "Next playback device".into();
            let next_device = match source {
                Source::Microphone => app.settings.microphone.clone(),
                Source::Computer => app.record_output.clone(),
            };
            let ctx = egui::Context::default();

            for finished in [false, true] {
                if finished {
                    app.stop_recording();
                    assert_eq!(app.take.as_ref().unwrap().device_name(), recorded_device);
                }
                let texts = board(&mut app, &ctx, Vec::new());
                assert!(texts.contains(&recorded_device), "show the captured device while recording and after stopping");
                assert!(!texts.contains(&next_device), "a new routing choice belongs to the next take");
            }
            app.take = None;
            let texts = board(&mut app, &ctx, Vec::new());
            assert!(texts.contains(&next_device), "return to the selected route once the take is discarded");
            assert!(!texts.contains(&recorded_device));
        }
    }

    #[test]
    fn stopping_after_an_audio_gap_keeps_the_take_and_its_warning() {
        let mut app = App::stopped(Default::default());
        let recorder = Recorder::from_samples_for_test(&[0.25; 480]);
        recorder.report_skipped_for_test();
        app.recorder = Some(recorder);
        app.poll_recording();
        assert!(app.recorder.is_some(), "an overrun warns without ending capture");
        assert!(app.record_status.0.contains("skipped"));
        app.stop_recording();
        assert!(app.recorder.is_none());
        assert!(app.record_status.0.starts_with("Partial take ready"));
        assert!(app.record_status.0.contains("audio was skipped"));
        assert_eq!(app.record_status.1, AMBER);
        assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), 240);
    }

    fn finish_recording(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.recorder.is_some() {
            app.poll_recording();
            assert!(std::time::Instant::now() < deadline, "recording finalization did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn stopping_a_slow_writer_keeps_controls_responsive_and_protects_the_take() {
        let (recorder, release) = Recorder::delayed_writer_for_test(&[0.25; 480]);
        let device = recorder.device_name().to_owned();
        let mut app = App::stopped(Default::default());
        app.page = super::super::Page::Record;
        app.recorder = Some(recorder);
        app.stop_recording();
        for _ in 0..10 { app.poll_recording(); }
        assert!(app.recorder.as_ref().unwrap().is_stopping());
        assert!(app.take.is_none(), "a take cannot be saved before its WAV is complete");
        assert!(app.record_status.0.contains("Finishing"));
        assert!(app.update_blocker().unwrap().contains("finish"));
        let ctx = egui::Context::default();
        let texts = board(&mut app, &ctx, Vec::new());
        assert!(texts.iter().any(|text| text == "Finishing"));
        assert!(texts.contains(&device));
        assert!(!texts.iter().any(|text| text == "Stop recording" || text == "Save WAV…"));
        app.run_command(&ctx, super::super::Command::ToggleMute);
        assert!(app.settings.mute, "the microphone controls still work while the writer drains");
        app.start_recording();
        assert!(app.recorder.as_ref().unwrap().is_stopping(), "recording again cannot replace an unfinished take");
        release.send(()).unwrap();
        finish_recording(&mut app);
        let take = app.take.as_ref().unwrap();
        assert_eq!(take.samples().unwrap().len(), 240);
        assert_eq!(take.device_name(), device);
        assert!(take.warning().is_none(), "normal writer completion must not be reported as a crash");
        assert!(app.record_status.0.starts_with("Take ready"));
    }

    #[test]
    fn automatic_stops_keep_errors_and_size_limits_while_the_writer_finishes() {
        for reason in ["capture", "size", "skipped"] {
            let (recorder, release) = Recorder::delayed_writer_for_test(&[0.25; 480]);
            match reason {
                "capture" => recorder.report_error_for_test("Microphone disconnected"),
                "size" => recorder.report_full_for_test(),
                _ => recorder.report_skipped_for_test(),
            }
            let mut app = App::stopped(Default::default());
            app.recorder = Some(recorder);
            if reason == "skipped" { app.stop_recording(); } else { app.poll_recording(); }
            assert!(app.recorder.as_ref().unwrap().is_stopping());
            assert!(app.take.is_none());
            if reason == "capture" { assert!(app.record_status.0.contains("Microphone disconnected")); }
            if reason == "size" { assert!(app.record_status.0.contains("size limit")); }
            app.poll_recording();
            release.send(()).unwrap();
            finish_recording(&mut app);
            assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), 240);
            assert_eq!(app.record_status.1, AMBER);
            match reason {
                "capture" => {
                    assert!(app.record_status.0.starts_with("Partial take ready"));
                    assert!(app.record_status.0.contains("Microphone disconnected"));
                }
                "size" => assert!(app.record_status.0.contains("4 GB WAV size limit; your take is ready")),
                _ => assert!(app.record_status.0.contains("audio was skipped")),
            }
        }
    }

    #[test]
    fn quitting_during_finalization_keeps_the_recording_available() {
        let (recorder, release) = Recorder::delayed_writer_for_test(&[0.25; 480]);
        let mut app = App::stopped(Default::default());
        app.page = super::super::Page::Record;
        app.recorder = Some(recorder);
        app.stop_recording();
        let ctx = egui::Context::default();
        app.run_command(&ctx, super::super::Command::Quit);
        assert!(app.confirm_quit && !app.quitting);
        board(&mut app, &ctx, Vec::new());
        let texts = board(&mut app, &ctx, Vec::new());
        assert!(texts.iter().any(|text| text.contains("finishing your recording")), "{texts:?}");
        assert!(texts.iter().any(|text| text == "Keep OpenMic open"));
        assert!(!texts.iter().any(|text| text == "Keep recording"));
        release.send(()).unwrap();
        finish_recording(&mut app);
        assert!(app.confirm_quit && !app.quitting, "completion must leave the unsaved take protected");
        assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), 240);
    }

    #[test]
    fn failed_finalization_cancels_a_pending_quit_and_keeps_the_error_visible() {
        for failed in [false, true] {
            let (recorder, release) = Recorder::delayed_writer_for_test(&[]);
            if failed { recorder.report_error_for_test("Microphone disconnected"); }
            let mut app = App::stopped(Default::default());
            app.page = super::super::Page::Record;
            app.recorder = Some(recorder);
            app.stop_recording();
            let ctx = egui::Context::default();
            app.run_command(&ctx, super::super::Command::Quit);
            assert!(app.confirm_quit && !app.quitting);
            release.send(()).unwrap();
            finish_recording(&mut app);
            assert!(app.recorder.is_none() && app.take.is_none());
            board(&mut app, &ctx, Vec::new());
            assert_eq!(app.quitting, !failed, "a capture error must not disappear behind automatic quitting");
            if failed {
                assert!(!app.confirm_quit);
                assert_eq!(app.record_status.1, RED);
                assert!(app.record_status.0.contains("Microphone disconnected"));
            }
        }
    }

    #[test]
    #[ignore = "writes recording-finalization screenshots; needs Python with Pillow"]
    fn snapshot_recording_finalization() {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let (recorder, release) = Recorder::delayed_writer_for_test(&vec![0.25; 96_000]);
            let mut app = App::stopped(Default::default());
            app.page = super::super::Page::Record;
            app.recorder = Some(recorder);
            app.stop_recording();
            super::super::snapshot::snapshot(&mut app, &format!("record-finishing-{theme}"), egui::vec2(700.0, 420.0), 1.0, dark);
            app.run_command(&egui::Context::default(), super::super::Command::Quit);
            super::super::snapshot::snapshot(&mut app, &format!("record-finishing-quit-{theme}"), egui::vec2(700.0, 420.0), 1.0, dark);
            release.send(()).unwrap();
            finish_recording(&mut app);
        }
    }

    #[test]
    fn reaching_the_wav_limit_preserves_partial_and_empty_take_results() {
        for (count, skipped) in [(480, false), (480, true), (0, false)] {
            let mut app = App::stopped(Default::default());
            let recorder = Recorder::from_samples_for_test(&vec![0.25; count]);
            recorder.report_full_for_test();
            if skipped {
                recorder.report_skipped_for_test();
            }
            app.recorder = Some(recorder);
            app.poll_recording();
            assert!(app.recorder.is_none(), "the size limit must stop recording");
            if count == 0 {
                assert!(app.take.is_none());
                assert!(app.record_status.0.contains("No audio was captured"));
            } else {
                assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), count / 2);
                if skipped {
                    assert!(app.record_status.0.starts_with("Partial take ready"));
                    assert!(app.record_status.0.contains("audio was skipped"));
                } else {
                    assert!(app.record_status.0.contains("4 GB WAV size limit; your take is ready"));
                }
            }
            assert_eq!(app.record_status.1, AMBER);
        }
    }

    #[test]
    fn sound_pad_play_remove_and_strip_button_have_separate_hit_targets() {
        let ctx = egui::Context::default();
        let pad = Pad::new(PathBuf::from("airhorn.mp3"));
        let frame = |events| {
            let mut responses = None;
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
                    responses = Some(clip_pad(ui, 0, &pad, 240.0, PadState::default(), (0, 0)));
                },
            );
            responses.unwrap()
        };
        let (play, remove, star, name, _) = frame(Vec::new());
        assert!(play.rect.right() <= remove.rect.left());
        assert!(star.rect.top() >= play.rect.bottom() && star.rect.top() >= remove.rect.bottom());
        for (pos, expected) in [
            (egui::pos2(play.rect.left() + 28.0, play.rect.center().y), (true, false, false, false)),
            (remove.rect.center(), (false, true, false, false)),
            (star.rect.center(), (false, false, true, false)),
            (name.rect.center(), (false, false, false, true)),
        ] {
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            let (play, remove, star, name, _) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert_eq!((play.clicked(), remove.clicked(), star.clicked(), name.clicked()), expected);
        }
    }

    /// A mono 16-bit WAV of `seconds` of a quiet tone.
    fn wav(dir: &Path, name: &str, seconds: f32) -> PathBuf {
        let rate = 48_000u32;
        let frames = (seconds * rate as f32) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            let s = ((i as f32 * 0.05).sin() * 3000.0) as i16;
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn trimmed_lengths_and_duration_labels_describe_the_kept_audio() {
        assert_eq!(trimmed_length(4.0, 1.0, Some(3.0)), 2.0);
        assert_eq!(trimmed_length(4.0, 0.0, None), 4.0);
        assert_eq!(clock_seconds(4.25), "4.2 s");
        assert_eq!(clock_seconds(75.0), "1:15");
    }

    #[test]
    fn older_pads_load_untrimmed_and_trim_round_trips() {
        let pad: crate::config::Pad = serde_json::from_str(r#"{"path": "a.wav", "volume": 0.5}"#).unwrap();
        assert_eq!((pad.start, pad.end), (0.0, None));
        let trimmed = crate::config::Pad { start: 0.5, end: Some(2.0), ..pad };
        let again: crate::config::Pad = serde_json::from_str(&serde_json::to_string(&trimmed).unwrap()).unwrap();
        assert_eq!(again, trimmed);
    }

    #[test]
    fn the_mixer_reports_how_far_each_clip_has_played() {
        let mut mixer = crate::dsp::Mixer::default();
        mixer.play(crate::dsp::Clip::new(7, Arc::new(vec![0.1; 1000]), 1.0), false);
        mixer.mix(&mut [0.0; 250], 1.0);
        assert_eq!(mixer.progress(), [crate::dsp::ClipProgress { key: 7, fraction: 0.25, paused: false, duration: 1000.0 / crate::dsp::SR as f32 }]);
    }

    /// Draw the whole app on the Soundboard tab, with `events` this frame.
    fn board_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 1600.0))),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.ui(ui, &mut frame))
    }

    fn board(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<String> {
        let output = board_frame(app, ctx, events);
        let mut texts = Vec::new();
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        output.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        texts
    }

    fn pad_title_position(app: &mut App, ctx: &egui::Context, name: &str) -> egui::Pos2 {
        board_frame(app, ctx, Vec::new()).shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text() == name => Some(t.visual_bounding_rect().center()),
            _ => None,
        }).unwrap_or_else(|| panic!("{name} not drawn"))
    }

    fn finish_import(app: &mut App, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.clip_import.is_some() {
            app.poll_clip_import(ctx);
            assert!(std::time::Instant::now() < deadline, "clip import did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn board_keyboard_actions_follow_the_file_after_reordering_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        for action in ["Play", "Remove", "Add to strip", "Rename"] {
            let sounds = (0..7).map(|i| {
                let mut pad = Pad::new(wav(dir.path(), &format!("clip-{i}.wav"), 0.1));
                pad.name = Some(if i == 1 { "target".into() } else { format!("clip-{i}") });
                pad
            }).collect();
            let mut app = App::stopped(crate::config::Settings { sounds, ..Default::default() });
            app.page = super::super::Page::Soundboard;
            app.engine = Some(crate::engine::Engine::for_test());
            for pad in app.settings.sounds.clone() {
                app.cache_clip(pad.path.clone(), super::super::CachedClip::read(&pad.path, &std::sync::atomic::AtomicBool::new(false)).unwrap());
            }
            let path = app.settings.sounds[1].path.clone();
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = board_frame(&mut app, &ctx, Vec::new());
            let label = if action == "Add to strip" { "Add target to the strip".into() } else { format!("{action} target") };
            let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .find(|(_, node)| node.label() == Some(label.as_str())).map(|(id, _)| id).unwrap();
            let key = |key| vec![egui::Event::Key {
                key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
            }];
            let mut focused = false;
            for _ in 0..80 {
                let output = board_frame(&mut app, &ctx, key(egui::Key::Tab));
                if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
            }
            assert!(focused, "keyboard navigation reaches {action}");
            app.move_pad(1, 6);
            app.remove_clip(0);
            app.settings.sounds[0].name = Some("same".into());
            app.settings.sounds.last_mut().unwrap().name = Some("same".into());
            let output = board_frame(&mut app, &ctx, Vec::new());
            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target, "{action} keeps its focused file");
            assert!(app.playing.is_empty());
            assert_eq!(app.settings.sounds.len(), 6);
            assert!(app.settings.sounds.iter().all(|pad| !pad.starred));
            board_frame(&mut app, &ctx, key(egui::Key::Enter));
            match action {
                "Play" => {
                    assert_eq!(app.playing, [clip_key(&path)], "Enter plays the focused file, despite duplicate names");
                    assert_eq!(app.engine.as_ref().unwrap().clip_progress()[0].key, clip_key(&path));
                }
                "Remove" => {
                    assert_eq!(app.settings.sounds.len(), 5);
                    assert!(!app.settings.sounds.iter().any(|pad| pad.path == path), "Enter removes the focused file");
                    board_frame(&mut app, &ctx, key(egui::Key::Enter));
                    assert_eq!(app.settings.sounds.len(), 5, "removing a pad does not transfer Remove focus to its neighbor");
                }
                "Add to strip" => {
                    assert_eq!(app.settings.sounds.iter().filter(|pad| pad.starred).map(|pad| &pad.path).collect::<Vec<_>>(), [&path]);
                }
                _ => assert_eq!(app.renaming_pad.as_ref().unwrap().path, path, "Enter renames the same file"),
            }
        }
    }

    fn finish_recording_save(app: &mut App) {
        finish_recording_save_with_context(app, &egui::Context::default());
    }

    fn finish_recording_save_with_context(app: &mut App, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.recording_save.is_some() {
            app.poll_recording_save(ctx);
            assert!(std::time::Instant::now() < deadline, "recording save did not finish");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn recording_failure(stage: &str) -> (App, Option<crossbeam_channel::Sender<()>>, String) {
        let mut app = App::stopped(Default::default());
        app.page = super::super::Page::Record;
        let error = anyhow::anyhow!("The disk is full; free some space before trying again")
            .context("write C:\\Recordings\\Sessions\\Long recording names can hide the useful part of an error message.wav");
        let message = format!("{error:#}");
        let mut release = None;
        if stage == "save" {
            app.take = Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().map(Arc::new);
            let (done, job) = crossbeam_channel::bounded(1);
            app.recording_save = Some(RecordingSave { job, add_to_board: false });
            done.send(Err(error)).unwrap();
            app.poll_recording_save(&egui::Context::default());
        } else {
            let samples: &[f32] = if stage == "empty" { &[] } else { &[0.25; 480] };
            let recorder = if stage == "finishing" {
                let (recorder, finish) = Recorder::delayed_writer_for_test(samples);
                release = Some(finish);
                recorder
            } else {
                Recorder::from_samples_for_test(samples)
            };
            recorder.report_error_for_test(&message);
            app.recorder = Some(recorder);
            app.poll_recording();
        }
        (app, release, message)
    }

    fn short_record_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        record_frame_at_size(app, ctx, egui::vec2(700.0, 420.0), events)
    }

    fn record_frame_at_size(app: &mut App, ctx: &egui::Context, size: egui::Vec2, events: Vec<egui::Event>) -> egui::FullOutput {
        use eframe::App as _;
        let mut frame = eframe::Frame::_new_kittest();
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
        }, |ui| app.ui(ui, &mut frame))
    }

    #[test]
    fn recording_output_selector_fits_a_zoomed_window() {
        use egui::accesskit::Role;
        let zoom = 1.5;
        let size = egui::vec2(700.0, 420.0) / zoom;
        for dark in [true, false] {
            for passes in [1, 2] {
                for long_name in [false, true] {
                    let mut app = App::stopped(Default::default());
                    app.page = super::super::Page::Record;
                    app.record_source = Source::Computer;
                    if long_name { app.record_output = "USB speakers with a long manufacturer and device description".repeat(3); }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let node = &tree.nodes.iter()
                        .find(|(_, node)| node.role() == Role::ComboBox).unwrap().1;
                    let bounds = node.bounds().unwrap();
                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                        && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))),
                        "the output selector fits the zoomed window; long_name={long_name}, passes={passes}: {rect:?}");
                    let label = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Label && node.value() == Some("Device"))
                        .map(|(id, _)| id).unwrap();
                    assert!(node.labelled_by().contains(&label), "the recording output has an accessible device label");
                    assert!(app.recorder.is_none() && app.take.is_none() && app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    fn recording_output_menu_fits_and_keyboard_selection_only_changes_the_next_device() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for default in [false, true] {
                        let mut app = App::stopped(Default::default());
                        app.page = super::super::Page::Record;
                        app.record_source = Source::Computer;
                        app.outputs = ["USB speakers", "Headphones"].map(|name|
                            format!("{name}{}", " with a long manufacturer and device description".repeat(3))).into();
                        app.record_output = app.outputs[0].clone();
                        let before = app.record_output.clone();
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                        let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                            action, target_tree: TreeId::ROOT, target_node, data: None,
                        });
                        let key = |key| egui::Event::Key {
                            key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        };
                        record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, target)]);
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let popup_id = ctx.memory(|memory| memory.focused()).unwrap().with("popup");
                        record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(egui::Popup::is_any_open(&ctx));
                        let menu = egui::AreaState::load(&ctx, popup_id).unwrap().rect();
                        assert!(menu.left() >= 0.0 && menu.right() <= size.x,
                            "the output menu fits with long device names; zoom={zoom}, passes={passes}: {menu:?}");
                        record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Escape)]);
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(app.record_output, before);
                        record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, target), key(egui::Key::Enter)]);
                        for _ in 0..6 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let name = if default { default_output_label() } else { &app.outputs[1] };
                        let choice = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(name)).map(|(id, _)| id).unwrap();
                        if passes == 1 {
                            let mut reached = false;
                            for _ in 0..8 {
                                let output = record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Tab)]);
                                if output.platform_output.accesskit_update.unwrap().focus == choice { reached = true; break; }
                            }
                            assert!(reached, "Tab reaches the requested output inside the menu");
                            record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        } else { record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Click, choice)]); }
                        for _ in 0..6 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        assert_eq!(app.record_output, if default { "" } else { &app.outputs[1] });
                        assert!(app.record_source == Source::Computer);
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.recorder.is_none() && app.take.is_none() && app.recording_save.is_none() && app.dirty_since.is_none());
                        assert!(!egui::Popup::is_any_open(&ctx));
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                        record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Enter)]);
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Click, choice)]);
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        assert!(!egui::Popup::is_any_open(&ctx));
                        assert_eq!(app.record_output, if default { "" } else { &app.outputs[1] });
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.recorder.is_none() && app.take.is_none() && app.dirty_since.is_none());
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                    }
                }
            }
        }
    }

    #[test]
    fn navigation_reveals_recording_take_controls_in_a_zoomed_window() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let zoom = 1.5;
        let size = egui::vec2(700.0, 420.0) / zoom;
        let bounds = |output: &egui::FullOutput, target| {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32))
        };
        let visible = |output: &egui::FullOutput, target| {
            let rect = bounds(output, target);
            output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect)))
        };
        let source_position = |output: &egui::FullOutput| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Source" => Some(text.visual_bounding_rect().center()), _ => None,
        }).unwrap();
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    for label in ["Mic recording", "Add to soundboard", "Save WAV…", "Discard take"] {
                        let mut app = App::stopped(Default::default());
                        app.page = super::super::Page::Record;
                        let take = Arc::new(Recorder::from_samples_for_test(&[0.0; 480]).finish().unwrap().unwrap());
                        app.take = Some(Arc::clone(&take));
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let source_top = source_position(&output);
                        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_, node)|
                            (node.role() == Role::Button && node.label() == Some(label))
                                || (node.role() == Role::TextInput && node.value() == Some(label))).map(|(id, _)| id).unwrap();
                        if label == "Discard take" { assert!(!visible(&output, target), "this case starts below the viewport"); }
                        if assistive {
                            record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                                action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                            })]);
                        } else {
                            let mut reached = false;
                            for _ in 0..64 {
                                let output = record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::Key {
                                    key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                                }]);
                                if output.platform_output.accesskit_update.unwrap().focus == target { reached = true; break; }
                            }
                            assert!(reached, "Tab reaches {label}");
                        }
                        for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert!(visible(&output, target), "{label} is visible when focused; dark={dark}, passes={passes}, assistive={assistive}");
                        for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::NONE] {
                            record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::Key {
                                key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers,
                            }]);
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                        assert!(visible(&output, target));
                        for delta in [1000.0, -1000.0] {
                            record_frame_at_size(&mut app, &ctx, size, vec![
                                egui::Event::PointerMoved(egui::pos2(size.x - 14.0, size.y - 60.0)),
                                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                    modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move },
                            ]);
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, target);
                            if delta > 0.0 { assert_eq!(source_position(&output), source_top, "focus allows manually scrolling back to the page top"); }
                            else { assert!(visible(&output, target)); }
                        }
                        assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        assert!(app.recorder.is_none() && app.recording_save.is_none() && app.dirty_since.is_none());
                        if label == "Discard take" {
                            record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::Key {
                                key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                            }]);
                            for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            assert!(app.take.is_none() && app.recorder.is_none() && app.recording_save.is_none());
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes Record keyboard focus screenshots; needs Python with Pillow"]
    fn snapshot_recording_keyboard_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (zoom, size_label) in [(1.0, "standard"), (1.5, "zoomed")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                let mut app = App::stopped(Default::default());
                app.page = super::super::Page::Record;
                app.record_source = Source::Computer;
                app.record_output = "USB speakers with a long manufacturer and device description".repeat(3);
                app.outputs = vec![app.record_output.clone(), "Headphones with a long manufacturer and device description".repeat(3)];
                let image = super::super::snapshot::snapshot(&mut app, &format!("record-output-{size_label}-{theme}"), size, zoom, dark);
                println!("SNAPSHOT {}", image.display());
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.zoom_factor = zoom);
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                widgets::configure_text_styles(&ctx);
                ctx.enable_accesskit();
                for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                    .find(|(_, node)| node.role() == Role::ComboBox).map(|(id, _)| id).unwrap();
                let mut events = vec![Vec::new(); 3];
                events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                })]);
                events.extend(vec![Vec::new(); 8]);
                events.push(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None,
                    pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
                let image = super::super::snapshot::snapshot_with_events(&mut app,
                    &format!("record-output-menu-{size_label}-{theme}"), size, zoom, dark, events);
                println!("SNAPSHOT {}", image.display());
                assert!(app.recorder.is_none() && app.take.is_none() && app.dirty_since.is_none());
                for (control, label) in [("name", "Mic recording"), ("discard", "Discard take")] {
                    let mut app = App::stopped(Default::default());
                    app.page = super::super::Page::Record;
                    app.take = Some(Arc::new(Recorder::from_samples_for_test(&[0.0; 480]).finish().unwrap().unwrap()));
                    app.record_status = ("Take ready".into(), VIOLET);
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        (node.role() == Role::Button && node.label() == Some(label))
                            || (node.role() == Role::TextInput && node.value() == Some(label))).map(|(id, _)| id).unwrap();
                    let mut events = vec![Vec::new(); 3];
                    let mut reached = false;
                    for _ in 0..64 {
                        let tab = vec![egui::Event::Key {
                            key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                        }];
                        let output = record_frame_at_size(&mut app, &ctx, size, tab.clone());
                        events.push(tab);
                        if output.platform_output.accesskit_update.unwrap().focus == target { reached = true; break; }
                    }
                    assert!(reached);
                    events.extend(vec![Vec::new(); 8]);
                    let image = super::super::snapshot::snapshot_with_events(&mut app,
                        &format!("record-keyboard-{control}-{size_label}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert!(app.take.is_some() && app.recording_save.is_none() && app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    fn recording_name_has_an_accessible_label_and_preserves_the_take_while_editing() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                for saving in [false, true] {
                    let mut app = App::stopped(Default::default());
                    app.page = super::super::Page::Record;
                    let take = Arc::new(Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().unwrap());
                    app.take = Some(Arc::clone(&take));
                    let (_done, job) = crossbeam_channel::bounded(1);
                    if saving { app.recording_save = Some(RecordingSave { job, add_to_board: false }); }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { short_record_frame(&mut app, &ctx, Vec::new()); }
                    let output = short_record_frame(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let (field, node) = tree.nodes.iter().find(|(_, node)| node.role() == Role::TextInput
                        && node.value() == Some("Mic recording")).unwrap();
                    let field = *field;
                    let label = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Label && node.value() == Some("Name"))
                        .map(|(id, _)| id).unwrap();
                    assert!(node.labelled_by().contains(&label), "screen readers identify the recording name field");
                    assert_eq!(node.is_disabled(), saving);
                    if !saving {
                        short_record_frame(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: field, data: None,
                        })]);
                        for _ in 0..3 { short_record_frame(&mut app, &ctx, Vec::new()); }
                        short_record_frame(&mut app, &ctx, vec![egui::Event::Key {
                            key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND,
                        }, egui::Event::Text("Studio take café".into())]);
                        let output = short_record_frame(&mut app, &ctx, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let node = &tree.nodes.iter().find(|(id, _)| *id == field).unwrap().1;
                        assert_eq!(tree.focus, field);
                        assert_eq!(node.value(), Some("Studio take café"));
                        assert!(node.labelled_by().contains(&label), "the name stays labelled during editing");
                        assert_eq!(app.record_name, "Studio take café");
                    }
                    assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
                    assert!(app.recorder.is_none() && app.settings.sounds.is_empty() && app.dirty_since.is_none());
                    assert_eq!(app.recording_save.is_some(), saving);
                }
            }
        }
    }

    #[test]
    fn stopping_a_recording_keeps_focus_through_finishing_and_the_result() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for source in [Source::Microphone, Source::Computer] {
                        for outcome in ["normal", "empty", "capture error", "empty error", "size limit", "empty size limit", "audio gap"] {
                            for delayed in [false, true] {
                                for activation in ["enter", "space", "accessible", "programmatic"] {
                                    let samples: &[f32] = if outcome.starts_with("empty") { &[] } else { &[0.25; 480] };
                                    let (recorder, release) = if delayed {
                                        let (recorder, release) = Recorder::delayed_writer_for_test(samples);
                                        (recorder, Some(release))
                                    } else { (Recorder::from_samples_for_test(samples), None) };
                                    if outcome.contains("error") { recorder.report_error_for_test("Focus fixture microphone disconnected"); }
                                    if outcome.contains("size limit") { recorder.report_full_for_test(); }
                                    if outcome == "audio gap" { recorder.report_skipped_for_test(); }
                                    let mut app = App::stopped(Default::default());
                                    app.page = super::super::Page::Record;
                                    app.record_source = source;
                                    app.record_name = "Studio take café".into();
                                    app.settings.microphone = "Missing stop-focus test microphone".into();
                                    app.record_output = "Missing stop-focus test output".into();
                                    app.recorder = Some(recorder);
                                    let original = serde_json::to_value(&app.settings).unwrap();
                                    let ctx = egui::Context::default();
                                    ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                    widgets::configure_text_styles(&ctx);
                                    ctx.enable_accesskit();
                                    let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                        action, target_tree: TreeId::ROOT, target_node, data: None,
                                    });
                                    let key = |key, pressed, modifiers| egui::Event::Key {
                                        key, physical_key: Some(key), pressed, repeat: false, modifiers,
                                    };
                                    let visible = |output: &egui::FullOutput, target| {
                                        let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                            .find(|(id, _)| *id == target).unwrap().1;
                                        let bounds = node.bounds().unwrap();
                                        let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                        let caption = node.label().or(node.value()).unwrap();
                                        assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)),
                                            "focused recording control is visible: {caption}, {rect:?}");
                                        assert!(output.shapes.iter().any(|shape|
                                            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == caption
                                                && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                                            "focused recording text is visible: {caption}, zoom={zoom}, passes={passes}, {outcome}, {activation}");
                                    };
                                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                    let stop = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                                        node.role() == Role::Button && node.label() == Some("Stop recording")).map(|(id, _)| id).unwrap();
                                    record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, stop)]);
                                    for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                                    let held_events = || {
                                        let mut events = vec![key(held, true, egui::Modifiers::NONE)];
                                        if held == egui::Key::Space { events.push(egui::Event::Text(" ".into())); }
                                        events
                                    };
                                    if activation == "programmatic" {
                                        if outcome.contains("error") || outcome.contains("size limit") { app.poll_recording(); }
                                        else { app.stop_recording(); }
                                    } else {
                                        let events = if activation == "accessible" { vec![action(Action::Click, stop)] } else { held_events() };
                                        record_frame_at_size(&mut app, &ctx, size, events);
                                    }
                                    for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    if delayed {
                                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                        let (status, node) = tree.nodes.iter().find(|(_, node)|
                                            node.role() == Role::Label && node.value() == Some(app.record_status.0.as_str())).unwrap();
                                        assert_eq!(tree.focus, *status, "finishing keeps focus: {activation}, {outcome}, zoom={zoom}, passes={passes}");
                                        assert!(!node.is_disabled() && node.supports_action(Action::Focus));
                                        visible(&output, *status);
                                        assert!(app.recorder.as_ref().unwrap().is_stopping() && app.take.is_none());
                                        if matches!(activation, "enter" | "space") {
                                            for _ in 0..2 { record_frame_at_size(&mut app, &ctx, size, held_events()); }
                                        }
                                        release.unwrap().send(()).unwrap();
                                        finish_recording(&mut app);
                                    }
                                    for _ in 0..5 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    let empty = samples.is_empty();
                                    let target = *tree.nodes.iter().find(|(_, node)| if empty {
                                        node.role() == Role::Button && node.label() == Some("⏺  Record")
                                    } else { node.role() == Role::TextInput && node.value() == Some("Studio take café") }).map(|(id, _)| id).unwrap();
                                    assert_eq!(tree.focus, target, "finished result keeps focus: {activation}, {outcome}, delayed={delayed}, zoom={zoom}, passes={passes}");
                                    visible(&output, target);
                                    let status = app.record_status.clone();
                                    if matches!(activation, "enter" | "space") {
                                        let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                        assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)), "guard the held key before replaying it");
                                        for _ in 0..3 {
                                            record_frame_at_size(&mut app, &ctx, size, held_events());
                                            assert_eq!(app.record_status, status);
                                            assert_eq!(app.record_name, "Studio take café", "the stopping key must not edit the ready name");
                                            assert!(app.recorder.is_none() && app.recording_save.is_none());
                                        }
                                        record_frame_at_size(&mut app, &ctx, size, vec![key(held, false, egui::Modifiers::NONE)]);
                                    }
                                    if empty {
                                        assert!(app.take.is_none());
                                        assert_eq!(app.record_status.1, if outcome.contains("error") { RED } else { AMBER });
                                    } else {
                                        let take = Arc::clone(app.take.as_ref().unwrap());
                                        let samples = take.samples().unwrap();
                                        assert_eq!(samples.len(), 240);
                                        assert!(samples.iter().all(|sample| (*sample - 0.25).abs() < 0.001));
                                        assert_eq!(app.record_status.1, if outcome == "normal" { GREEN } else { AMBER });
                                        let modifiers = egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE };
                                        record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::A, true, modifiers),
                                            key(egui::Key::A, false, modifiers), egui::Event::Text("Kept studio take".into())]);
                                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                        assert_eq!(app.record_name, "Kept studio take", "the finished take can be named immediately");
                                        assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
                                    }
                                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                    assert!(app.recorder.is_none() && app.recording_save.is_none() && app.dirty_since.is_none());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn recording_finalization_preserves_navigation_and_pointer_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for source in [Source::Microphone, Source::Computer] {
                        for empty in [false, true] {
                            for mode in ["capture focus moved", "capture page switched", "finishing focus moved", "finishing page switched",
                                "result focus queued", "result page queued", "result focus cleared", "pointer stop"] {
                                let samples: &[f32] = if empty { &[] } else { &[0.25; 480] };
                                let (recorder, release) = Recorder::delayed_writer_for_test(samples);
                                let mut app = App::stopped(Default::default());
                                app.page = super::super::Page::Record;
                                app.record_source = source;
                                app.recorder = Some(recorder);
                                let original = serde_json::to_value(&app.settings).unwrap();
                                let ctx = egui::Context::default();
                                ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                                ctx.enable_accesskit();
                                let action = |action, target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                                    action, target_tree: TreeId::ROOT, target_node, data: None,
                                });
                                let key = |key, pressed| egui::Event::Key {
                                    key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                                };
                                let button = |output: &egui::FullOutput, label: &str| *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                                for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                let stop = button(&output, "Stop recording");
                                let switched = mode.contains("page");
                                let other = button(&output, if switched { "Soundboard" } else { "Mute" });
                                let move_focus = |app: &mut App| {
                                    let mut events = vec![action(Action::Focus, other)];
                                    if switched { events.push(action(Action::Click, other)); }
                                    record_frame_at_size(app, &ctx, size, events);
                                    for _ in 0..3 { record_frame_at_size(app, &ctx, size, Vec::new()); }
                                };
                                if mode != "pointer stop" {
                                    record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, stop)]);
                                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                }
                                if mode.starts_with("capture") {
                                    move_focus(&mut app);
                                    app.stop_recording();
                                } else if mode == "pointer stop" {
                                    let bounds = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                                        .find(|(id, _)| *id == stop).unwrap().1.bounds().unwrap();
                                    let pos = egui::pos2((bounds.x0 + bounds.x1) as f32 / 2.0, (bounds.y0 + bounds.y1) as f32 / 2.0);
                                    for pressed in [true, false] {
                                        record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos),
                                            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
                                    }
                                } else {
                                    record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                                }
                                for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                assert!(app.recorder.as_ref().unwrap().is_stopping());
                                if mode.starts_with("finishing") { move_focus(&mut app); }
                                release.send(()).unwrap();
                                finish_recording(&mut app);
                                if mode.starts_with("result") {
                                    if mode == "result focus cleared" {
                                        record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Escape, true), key(egui::Key::Escape, false)]);
                                    } else { move_focus(&mut app); }
                                }
                                for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                if matches!(mode, "pointer stop" | "result focus cleared") {
                                    assert!(ctx.memory(|memory| memory.focused()).is_none(), "completion must not force focus: {mode}");
                                } else {
                                    assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, other,
                                        "completion preserves navigation: {mode}, empty={empty}, zoom={zoom}, passes={passes}");
                                }
                                if switched {
                                    assert_eq!(app.page, super::super::Page::Soundboard);
                                    let record = button(&output, "Record");
                                    record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, record), action(Action::Click, record)]);
                                    for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                    assert_eq!(app.page, super::super::Page::Record);
                                    assert_eq!(output.platform_output.accesskit_update.unwrap().focus, record,
                                        "returning to the page preserves its navigation button rather than the stale capture control");
                                }
                                assert_eq!(app.take.is_none(), empty);
                                if let Some(take) = &app.take { assert_eq!(take.samples().unwrap().len(), 240); }
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                                assert!(app.recorder.is_none() && app.recording_save.is_none() && app.dirty_since.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "writes recording stop/finalization focus screenshots; needs Python with Pillow"]
    fn snapshot_recording_stop_focus() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        struct StoppedTake {
            app: App,
            release: Option<crossbeam_channel::Sender<()>>,
            complete: bool,
        }
        impl eframe::App for StoppedTake {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if self.complete && ui.ctx().cumulative_frame_nr() >= 16 && let Some(release) = self.release.take() {
                    release.send(()).unwrap();
                    finish_recording(&mut self.app);
                }
                eframe::App::ui(&mut self.app, ui, frame);
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["finishing", "ready", "partial", "empty", "failed"] {
                    let samples = if matches!(scene, "empty" | "failed") { Vec::new() } else { vec![0.25; 96_000] };
                    let (recorder, release) = Recorder::delayed_writer_for_test(&samples);
                    if matches!(scene, "partial" | "failed") { recorder.report_error_for_test("The recording device disconnected"); }
                    let mut app = App::stopped(Default::default());
                    app.page = super::super::Page::Record;
                    app.record_source = Source::Computer;
                    app.record_output = "Headphones with a long manufacturer and device description".repeat(3);
                    app.outputs = vec![app.record_output.clone()];
                    app.record_name = "Studio take café".into();
                    app.recorder = Some(recorder);
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                    let stop = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some("Stop recording")).map(|(id, _)| id).unwrap();
                    let mut stopped = StoppedTake { app, release: Some(release), complete: scene != "finishing" };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: stop, data: None,
                    })]);
                    events.extend(vec![Vec::new(); 6]);
                    let press = vec![egui::Event::Key { key: egui::Key::Space, physical_key: Some(egui::Key::Space),
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }, egui::Event::Text(" ".into())];
                    events.push(press.clone());
                    events.extend(vec![Vec::new(); 5]);
                    events.extend(vec![press; 3]);
                    events.extend(vec![Vec::new(); 3]);
                    let image = super::super::snapshot::snapshot_with_events(&mut stopped,
                        &format!("record-stop-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(stopped.app.record_name, "Studio take café");
                    assert!(stopped.app.recording_save.is_none() && stopped.app.dirty_since.is_none());
                    if scene == "finishing" {
                        assert!(stopped.app.recorder.as_ref().unwrap().is_stopping());
                        stopped.release.take().unwrap().send(()).unwrap();
                        finish_recording(&mut stopped.app);
                    } else {
                        assert!(stopped.app.recorder.is_none());
                        assert_eq!(stopped.app.take.is_none(), samples.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn discarding_a_take_restores_record_focus_without_repeating_the_action() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for source in [Source::Microphone, Source::Computer] {
                        for activation in ["enter", "space", "accessible", "pointer"] {
                            let mut app = App::stopped(Default::default());
                            app.page = super::super::Page::Record;
                            app.inputs = vec!["Monitor of Test output".into()];
                            app.record_source = source;
                            app.settings.microphone = "Missing recording-focus test microphone".into();
                            app.record_output = "Missing recording-focus test output".into();
                            app.take = Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().map(Arc::new);
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
                            for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            let discard = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Discard take"))
                                .map(|(id, _)| id).unwrap();
                            record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, discard)]);
                            for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let held = if activation == "space" { egui::Key::Space } else { egui::Key::Enter };
                            if activation == "pointer" {
                                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter()
                                    .find(|(id, _)| *id == discard).unwrap().1.bounds().unwrap();
                                let pos = egui::pos2((bounds.x0 + bounds.x1) as f32 / 2.0, (bounds.y0 + bounds.y1) as f32 / 2.0);
                                for pressed in [true, false] {
                                    record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pos),
                                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
                                }
                            } else {
                                let events = if activation == "accessible" { vec![action(Action::Click, discard)] }
                                    else { vec![key(held, true)] };
                                record_frame_at_size(&mut app, &ctx, size, events);
                            }
                            for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                            let (record, node) = tree.nodes.iter().find(|(_, node)|
                                node.role() == Role::Button && node.label() == Some("⏺  Record")).unwrap();
                            let record = *record;
                            if activation == "pointer" { assert_ne!(tree.focus, record, "pointer discards do not request keyboard focus"); }
                            else { assert_eq!(tree.focus, record, "discard returns to Record: {activation}, zoom={zoom}, passes={passes}"); }
                            let bounds = node.bounds().unwrap();
                            let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                            if activation != "pointer" {
                                assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                    && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))));
                            }
                            if matches!(activation, "enter" | "space") {
                                for _ in 0..3 {
                                    record_frame_at_size(&mut app, &ctx, size, vec![key(held, true)]);
                                    assert_eq!(app.record_status.0, "Take discarded", "held activation must not start a new recording");
                                }
                                record_frame_at_size(&mut app, &ctx, size, vec![key(held, false)]);
                            }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            let next = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                                .find(|(_, node)| node.role() == Role::Button && node.label() == Some(
                                    if source == Source::Microphone { "Computer audio" } else { "Microphone" }))
                                .map(|(id, _)| id).unwrap();
                            record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, next)]);
                            record_frame_at_size(&mut app, &ctx, size, vec![key(egui::Key::Enter, true), key(egui::Key::Enter, false)]);
                            assert!(app.record_source != source, "a fresh activation remains usable");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert!(app.take.is_none() && app.recorder.is_none() && app.recording_save.is_none() && app.dirty_since.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn recording_saves_keep_focus_through_progress_success_and_failure() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for zoom in [1.0, 1.25, 1.5] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for dark in [true, false] {
                for passes in [1, 2] {
                    for source in [Source::Microphone, Source::Computer] {
                        for scene in ["wav", "failed", "add-enter", "add-space", "add-accessible"] {
                            for timing in ["before progress", "after progress", "focus moved", "page switched", "focus queued", "page queued"] {
                                let dir = tempfile::tempdir().unwrap();
                                let saved = dir.path().join("saved.wav");
                                if scene == "failed" { std::fs::create_dir(&saved).unwrap(); }
                                let mut app = App::stopped(Default::default());
                                app.page = super::super::Page::Record;
                                app.record_source = source;
                                app.settings.microphone = "Missing recording-focus test microphone".into();
                                app.record_output = "Missing recording-focus test output".into();
                                app.record_name = "Recording keyboard focus fixture".into();
                                let take = Arc::new(Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().unwrap());
                                app.take = Some(Arc::clone(&take));
                                let original = app.settings.clone();
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
                                for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                let origin = button(&output, if scene.starts_with("add") { "Add to soundboard" } else { "Save WAV…" });
                                record_frame_at_size(&mut app, &ctx, size, vec![action(Action::Focus, origin)]);
                                for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                let held = if scene == "add-space" { egui::Key::Space } else { egui::Key::Enter };
                                if scene.starts_with("add") {
                                    let events = if scene == "add-accessible" { vec![action(Action::Click, origin)] } else { vec![key(held, true)] };
                                    record_frame_at_size(&mut app, &ctx, size, events);
                                } else { app.start_recording_save(RecordingDestination::Wav(saved.clone()), &ctx); }
                                assert!(app.recording_save.is_some());
                                let mut held_active = matches!(scene, "add-enter" | "add-space");
                                let mut other_focus = None;
                                if timing != "before progress" {
                                    for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                    let (progress, node) = tree.nodes.iter().find(|(_, node)|
                                        node.role() == Role::Label && node.value() == Some("Saving recording…")).unwrap();
                                    assert_eq!(tree.focus, *progress, "saving status owns focus: {scene}, {timing}, zoom={zoom}, passes={passes}");
                                    assert!(!node.is_disabled() && node.supports_action(Action::Focus));
                                    for label in ["Add to soundboard", "Save WAV…", "Discard take"] {
                                        let id = button(&output, label);
                                        assert!(tree.nodes.iter().find(|(node, _)| *node == id).unwrap().1.is_disabled());
                                    }
                                    if timing == "after progress" && !held_active {
                                        record_frame_at_size(&mut app, &ctx, size, vec![key(held, true)]);
                                        held_active = true;
                                    }
                                    if matches!(timing, "focus moved" | "page switched") {
                                        let target = button(&output, if timing == "focus moved" { "Mute" } else { "Soundboard" });
                                        let mut events = vec![action(Action::Focus, target)];
                                        if timing == "page switched" { events.push(action(Action::Click, target)); }
                                        record_frame_at_size(&mut app, &ctx, size, events);
                                        for _ in 0..4 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                        other_focus = Some(target);
                                    }
                                }
                                finish_recording_save_with_context(&mut app, &ctx);
                                if matches!(timing, "focus queued" | "page queued") {
                                    let target = button(&output, if timing == "focus queued" { "Mute" } else { "Soundboard" });
                                    let mut events = vec![action(Action::Focus, target)];
                                    if timing == "page queued" { events.push(action(Action::Click, target)); }
                                    record_frame_at_size(&mut app, &ctx, size, events);
                                    other_focus = Some(target);
                                }
                                for _ in 0..5 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                                let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                                let target = other_focus.unwrap_or_else(|| if scene == "failed" { origin } else { button(&output, "⏺  Record") });
                                assert_eq!(tree.focus, target, "completion keeps the intended focus: {scene}, {timing}, zoom={zoom}, passes={passes}");
                                if other_focus.is_none() {
                                    let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                                    let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
                                    assert!(output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                                        && matches!(&shape.shape, egui::Shape::Rect(shape) if shape.rect.intersects(rect))));
                                }
                                assert_eq!(app.page, if matches!(timing, "page switched" | "page queued") { super::super::Page::Soundboard } else { super::super::Page::Record });
                                let status = app.record_status.clone();
                                if held_active {
                                    let flags = ctx.data(|data| data.get_temp::<(bool, bool)>(egui::Id::new(("held-activation", ctx.viewport_id()))));
                                    assert_eq!(flags, Some((held == egui::Key::Enter, held == egui::Key::Space)), "guard the held key before replaying it");
                                    for _ in 0..3 {
                                        let output = record_frame_at_size(&mut app, &ctx, size, vec![key(held, true)]);
                                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
                                        assert_eq!(app.record_status, status);
                                        assert!(app.recorder.is_none() && app.recording_save.is_none());
                                    }
                                    record_frame_at_size(&mut app, &ctx, size, vec![key(held, false)]);
                                }
                                let mut expected = original;
                                if scene == "failed" {
                                    assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
                                    assert!(take.samples().unwrap().iter().all(|sample| (*sample - 0.25).abs() < 0.001));
                                    assert!(saved.is_dir() && app.record_status.1 == RED);
                                } else {
                                    assert!(app.take.is_none() && take.samples().is_err());
                                    let path = if scene.starts_with("add") {
                                        assert_eq!(app.settings.sounds.len(), 1);
                                        let path = app.settings.sounds[0].path.clone();
                                        expected.sounds.push(Pad::new(path.clone()));
                                        path
                                    } else { saved.clone() };
                                    let samples = crate::decode::load_clip(&path).unwrap();
                                    assert_eq!(samples.len(), 240);
                                    assert!(samples.iter().all(|sample| (*sample - 0.25).abs() < 0.001));
                                    if scene.starts_with("add") { std::fs::remove_file(path).unwrap(); }
                                    assert_eq!(app.record_status.1, GREEN);
                                }
                                assert_eq!(serde_json::to_value(&app.settings).unwrap(), serde_json::to_value(&expected).unwrap());
                                assert_eq!(app.dirty_since.is_some(), scene.starts_with("add"));
                                assert_eq!(app.record_name, "Recording keyboard focus fixture");
                                assert!(app.recorder.is_none() && app.clip_loads.is_none() && app.clip_import.is_none());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn disconnected_recording_saves_return_focus_to_the_name_for_retry() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for passes in [1, 2] {
                let mut app = App::stopped(Default::default());
                app.page = super::super::Page::Record;
                let take = Arc::new(Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().unwrap());
                app.take = Some(Arc::clone(&take));
                let (done, job) = crossbeam_channel::bounded(1);
                app.recording_save = Some(RecordingSave { job, add_to_board: false });
                app.record_status = ("Saving recording…".into(), CYAN);
                let ctx = egui::Context::default();
                ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                for _ in 0..3 { short_record_frame(&mut app, &ctx, Vec::new()); }
                let output = short_record_frame(&mut app, &ctx, Vec::new());
                let status = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                    node.role() == Role::Label && node.value() == Some("Saving recording…")).map(|(id, _)| id).unwrap();
                short_record_frame(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: status, data: None,
                })]);
                drop(done);
                app.poll_recording_save(&ctx);
                for _ in 0..4 { short_record_frame(&mut app, &ctx, Vec::new()); }
                let output = short_record_frame(&mut app, &ctx, Vec::new());
                let tree = output.platform_output.accesskit_update.unwrap();
                let name = *tree.nodes.iter().find(|(_, node)| node.role() == Role::TextInput && node.value() == Some("Mic recording"))
                    .map(|(id, _)| id).unwrap();
                assert_eq!(tree.focus, name);
                assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
                assert!(app.record_status.0.contains("stopped unexpectedly"));
                assert!(app.recording_save.is_none() && app.recorder.is_none() && app.dirty_since.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes recording focus-transition screenshots; needs Python with Pillow"]
    fn snapshot_recording_focus_transitions() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        struct Transition {
            app: App,
            start: Option<RecordingDestination>,
            complete: bool,
        }
        impl eframe::App for Transition {
            fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
                if ui.ctx().cumulative_frame_nr() >= 10 && let Some(destination) = self.start.take() {
                    self.app.start_recording_save(destination, ui.ctx());
                }
                if self.complete && ui.ctx().cumulative_frame_nr() >= 16 && self.app.recording_save.is_some() {
                    finish_recording_save_with_context(&mut self.app, ui.ctx());
                }
                eframe::App::ui(&mut self.app, ui, frame);
            }
        }
        for (zoom, scale) in [(1.0, "100"), (1.25, "125"), (1.5, "150")] {
            let size = egui::vec2(700.0, 420.0) / zoom;
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["discarded", "saving", "saved", "failed"] {
                    let dir = tempfile::tempdir().unwrap();
                    let blocked = dir.path().join("blocked.wav");
                    if scene == "failed" { std::fs::create_dir(&blocked).unwrap(); }
                    let mut app = App::stopped(Default::default());
                    app.page = super::super::Page::Record;
                    app.record_source = Source::Computer;
                    app.record_output = "Headphones with a long manufacturer and device description".repeat(3);
                    app.outputs = vec![app.record_output.clone()];
                    app.record_name = "Keyboard focused take".into();
                    app.take = Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().map(Arc::new);
                    app.record_status = ("Take ready".into(), GREEN);
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                    let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                    let label = match scene { "discarded" => "Discard take", "failed" => "Save WAV…", _ => "Add to soundboard" };
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                        node.role() == Role::Button && node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let mut transition = Transition { app,
                        start: (scene == "failed").then_some(RecordingDestination::Wav(blocked)),
                        complete: matches!(scene, "saved" | "failed"),
                    };
                    let mut events = vec![Vec::new(); 3];
                    events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]);
                    events.extend(vec![Vec::new(); 6]);
                    let press = egui::Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter),
                        pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
                    events.push(vec![press.clone()]);
                    events.extend(vec![Vec::new(); 5]);
                    events.extend(vec![vec![press]; 3]);
                    events.extend(vec![Vec::new(); 3]);
                    let image = super::super::snapshot::snapshot_with_events(&mut transition,
                        &format!("record-transition-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert!(transition.app.recorder.is_none());
                    match scene {
                        "discarded" => assert!(transition.app.take.is_none() && transition.app.record_status.0 == "Take discarded"),
                        "saving" => {
                            assert!(transition.app.take.is_some() && transition.app.recording_save.is_some());
                            finish_recording_save(&mut transition.app);
                        }
                        "saved" => assert!(transition.app.take.is_none() && transition.app.record_status.1 == GREEN),
                        _ => assert!(transition.app.take.is_some() && transition.app.recording_save.is_none() && transition.app.record_status.1 == RED),
                    }
                    for pad in &transition.app.settings.sounds { std::fs::remove_file(&pad.path).unwrap(); }
                }
            }
        }
    }

    #[test]
    fn recording_failures_keep_full_details_and_recovery_controls_in_a_short_window() {
        for stage in ["save", "partial", "empty", "finishing"] {
            for dark in [true, false] {
                let (mut app, release, message) = recording_failure(stage);
                assert!(app.record_status.0.contains(&message), "{stage} must preserve the cause beyond a long filename");
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let draw = |app: &mut App, events| short_record_frame(app, &ctx, events);
                let first = draw(&mut app, Vec::new());
                assert!(first.platform_output.accesskit_update.unwrap().nodes.iter()
                    .any(|(_, node)| node.value() == Some(app.record_status.0.as_str())), "the full details are available to screen readers");
                let output = draw(&mut app, Vec::new());
                let rect = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == app.record_status.0 => Some(text.visual_bounding_rect()),
                    _ => None,
                }).expect("the recording status is drawn");
                assert!(rect.right() <= 700.0 && rect.bottom() <= 420.0, "the status fits the minimum window");
                let control = if stage == "empty" { "⏺  Record" } else if stage == "finishing" { "Finishing your take…" } else { "Save WAV…" };
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.text() == control && text.visual_bounding_rect().bottom() <= 420.0)), "{stage} recovery controls stay visible");
                draw(&mut app, vec![egui::Event::PointerMoved(rect.center())]);
                let mut tooltip = false;
                for _ in 0..6 {
                    let output = draw(&mut app, Vec::new());
                    let details = output.shapes.iter().filter(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.text() == app.record_status.0 && text.galley.rows.len() > 1)).count();
                    assert!(details <= 1, "the status shows one tooltip");
                    tooltip |= details == 1;
                }
                assert!(tooltip, "{stage} exposes the full details on hover");
                if let Some(release) = release {
                    release.send(()).unwrap();
                    finish_recording(&mut app);
                    assert!(app.take.as_ref().unwrap().warning().unwrap().contains(&message));
                }
                if stage == "save" {
                    assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), 240, "the take survives the failed save");
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("retried.wav");
                    app.start_recording_save(RecordingDestination::Wav(path.clone()), &ctx);
                    finish_recording_save(&mut app);
                    assert!(app.take.is_none());
                    assert_eq!(crate::decode::load_clip(&path).unwrap().len(), 240);
                    assert_eq!(app.record_status, ("Saved retried.wav".into(), GREEN));
                }
            }
        }
    }

    #[test]
    #[ignore = "writes recording-error screenshots; needs Python with Pillow"]
    fn snapshot_recording_error_details() {
        for stage in ["save", "partial"] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                let (mut app, _, _) = recording_failure(stage);
                let image = super::super::snapshot::snapshot(&mut app, &format!("record-{stage}-error-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark);
                println!("SNAPSHOT {}", image.display());
                let ctx = egui::Context::default();
                short_record_frame(&mut app, &ctx, Vec::new());
                let output = short_record_frame(&mut app, &ctx, Vec::new());
                let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == app.record_status.0 => Some(text.visual_bounding_rect().center()),
                    _ => None,
                }).unwrap();
                let image = super::super::snapshot::snapshot_with_events(
                    &mut app, &format!("record-{stage}-error-details-{theme}"), egui::vec2(700.0, 420.0), 1.25, dark,
                    vec![vec![egui::Event::PointerMoved(pos)]],
                );
                println!("SNAPSHOT {}", image.display());
            }
        }
    }

    #[test]
    fn failed_background_recording_saves_keep_the_take_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked.wav");
        std::fs::create_dir(&blocked).unwrap();
        let mut app = App::stopped(crate::config::Settings::default());
        let take = Arc::new(Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().unwrap());
        app.take = Some(Arc::clone(&take));
        let ctx = egui::Context::default();
        app.start_recording_save(RecordingDestination::Wav(blocked.clone()), &ctx);
        app.run_command(&ctx, super::super::Command::Quit);
        assert!(app.confirm_quit && !app.quitting);
        finish_recording_save(&mut app);
        assert!(!app.confirm_quit && !app.quitting, "a failed save cancels the pending quit");
        assert!(Arc::ptr_eq(app.take.as_ref().unwrap(), &take));
        assert_eq!(take.samples().unwrap().len(), 240);
        assert!(blocked.is_dir());
        assert_eq!(app.record_status.1, RED);

        let saved = dir.path().join("saved.wav");
        let duplicate = dir.path().join("duplicate.wav");
        app.start_recording_save(RecordingDestination::Wav(saved.clone()), &ctx);
        app.start_recording_save(RecordingDestination::Wav(duplicate.clone()), &ctx);
        assert!(app.recording_save.is_some());
        app.run_command(&ctx, super::super::Command::ToggleMute);
        assert!(app.settings.mute, "microphone controls remain available while saving");
        finish_recording_save(&mut app);
        assert!(app.take.is_none() && app.settings.sounds.is_empty());
        assert_eq!(crate::decode::load_clip(&saved).unwrap().len(), 240);
        assert!(!duplicate.exists(), "one save per take at a time");
        assert_eq!(app.record_status, ("Saved saved.wav".into(), GREEN));
    }

    #[test]
    fn recording_pads_are_added_only_after_the_background_save_succeeds() {
        let mut app = App::stopped(crate::config::Settings::default());
        app.take = Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().map(Arc::new);
        app.pad_filter = "old search".into();
        app.start_recording_save(RecordingDestination::Soundboard("Background save test".into()), &egui::Context::default());
        assert!(app.recording_save.is_some() && app.settings.sounds.is_empty());
        finish_recording_save(&mut app);
        assert!(app.take.is_none());
        assert_eq!(app.settings.sounds.len(), 1);
        let path = &app.settings.sounds[0].path;
        assert_eq!(crate::decode::load_clip(path).unwrap().len(), 240);
        assert!(app.pad_filter.is_empty() && app.dirty_since.is_some());
        assert!(app.record_status.0.starts_with("Added "));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pending_recording_saves_protect_the_take_and_wait_before_quitting() {
        for outcome in ["saved", "failed", "disconnected", "keep open"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("saved.wav");
            let mut app = App::stopped(crate::config::Settings::default());
            app.page = super::super::Page::Record;
            let take = Arc::new(Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().unwrap());
            app.take = Some(Arc::clone(&take));
            let (done, job) = crossbeam_channel::bounded(1);
            app.recording_save = Some(RecordingSave { job, add_to_board: false });
            let ctx = egui::Context::default();
            let click = |app: &mut App, label| {
                board(app, &ctx, Vec::new());
                let pos = pad_title_position(app, &ctx, label);
                for pressed in [true, false] {
                    board(app, &ctx, vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                    ]);
                }
            };
            click(&mut app, "Discard take");
            assert!(app.take.is_some(), "discard is disabled while the worker uses the take");
            app.poll_recording_save(&ctx);
            assert!(app.recording_save.is_some(), "polling never waits for disk I/O");
            assert!(app.update_blocker().unwrap().contains("saving"));
            app.run_command(&ctx, super::super::Command::Quit);
            board(&mut app, &ctx, Vec::new());
            let texts = board(&mut app, &ctx, Vec::new());
            assert!(texts.iter().any(|t| t == "Saving before quitting…"));
            assert!(!texts.iter().any(|t| t == "Discard and quit"));
            assert!(!app.quitting);
            if outcome == "keep open" {
                click(&mut app, "Keep OpenMic open");
                assert!(!app.confirm_quit);
            }
            match outcome {
                "saved" | "keep open" => {
                    take.save_wav(&path).unwrap();
                    done.send(Ok(path)).unwrap();
                }
                "failed" => done.send(Err(anyhow::anyhow!("drive disconnected"))).unwrap(),
                _ => drop(done),
            }
            app.poll_recording_save(&ctx);
            board(&mut app, &ctx, Vec::new());
            assert!(app.recording_save.is_none());
            assert_eq!(app.quitting, outcome == "saved");
            if matches!(outcome, "failed" | "disconnected") {
                assert_eq!(app.record_status.1, RED);
                assert!(!app.confirm_quit);
                assert_eq!(app.take.as_ref().unwrap().samples().unwrap().len(), 240);
            } else {
                assert!(app.take.is_none());
            }
        }
    }

    #[test]
    #[ignore = "writes background-save screenshots; needs Python with Pillow"]
    fn snapshot_recording_save() {
        let mut app = App::stopped(crate::config::Settings::default());
        app.page = super::super::Page::Record;
        app.take = Recorder::from_samples_for_test(&vec![0.25; 96_000]).finish().unwrap().map(Arc::new);
        let (_done, job) = crossbeam_channel::bounded(1);
        app.recording_save = Some(RecordingSave { job, add_to_board: false });
        app.record_status = ("Saving recording…".into(), CYAN);
        for quitting in [false, true] {
            if quitting {
                app.run_command(&egui::Context::default(), super::super::Command::Quit);
            }
            let name = if quitting { "record-save-before-quit" } else { "record-saving" };
            let path = super::super::snapshot::snapshot(&mut app, name, egui::vec2(700.0, 420.0), 1.25, true);
            println!("SNAPSHOT {}", path.display());
        }
    }

    #[test]
    fn dropped_files_add_valid_pads_once_and_report_unreadable_files() {
        use eframe::App as _;

        #[derive(Debug)]
        struct Dropped(PathBuf);
        impl egui::DroppedFile for Dropped {
            fn path(&self) -> &Path { &self.0 }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                std::fs::read(&self.0).map_err(|error| error.to_string())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let existing = wav(dir.path(), "existing.wav", 0.1);
        let first = wav(dir.path(), "first.wav", 0.2);
        let second = wav(dir.path(), "second.wav", 0.3);
        let invalid = dir.path().join("unreadable.wav");
        std::fs::write(&invalid, b"not an audio file").unwrap();
        let mut pad = Pad::new(existing.clone());
        pad.starred = true;
        pad.volume = 0.5;
        pad.hotkey = Some("Ctrl+Alt+1".into());
        let mut app = App::stopped(crate::config::Settings { sounds: vec![pad.clone()], ..Default::default() });
        app.page = super::super::Page::Settings;
        app.pad_filter = "old search".into();
        app.begin_pad_rename(&pad);
        app.renaming_pad.as_mut().unwrap().text = "Keep this name".into();

        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 880.0))),
                dropped_files: [&first, &invalid, &existing, &first, &second].into_iter()
                    .map(|path| Arc::new(Dropped(path.clone())) as egui::DroppedFileHandle)
                    .collect(),
                ..Default::default()
            },
            |ui| app.ui(ui, &mut frame),
        );

        finish_import(&mut app, &ctx);
        pad.name = Some("Keep this name".into());
        assert_eq!(app.settings.sounds, [pad, Pad::new(first.clone()), Pad::new(second.clone())]);
        assert!(app.page == super::super::Page::Soundboard);
        assert!(app.pad_filter.is_empty() && app.renaming_pad.is_none());
        assert!(app.dirty_since.is_some());
        assert_eq!(app.sound_status, ("Added 2 clips · Could not read: unreadable.wav".into(), AMBER));
        assert!(ctx.input(|input| input.raw.dropped_files.is_empty()), "consume drops once, even if egui repeats a layout pass");
        let texts = board(&mut app, &ctx, Vec::new());
        assert!(texts.iter().any(|text| text == "first") && texts.iter().any(|text| text == "second"));
        assert_eq!(app.settings.sounds.len(), 3);
        assert!(existing.exists() && first.exists() && second.exists() && invalid.exists());
    }

    #[test]
    fn duplicate_or_unreadable_imports_leave_the_board_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "existing.wav", 0.1);
        let pad = Pad::new(path.clone());
        let mut app = App::stopped(crate::config::Settings { sounds: vec![pad.clone()], ..Default::default() });
        let ctx = egui::Context::default();

        app.import_sounds(vec![path.clone(), path], &ctx);
        assert_eq!(app.settings.sounds.as_slice(), std::slice::from_ref(&pad));
        assert!(app.dirty_since.is_none());
        assert_eq!(app.sound_status, ("These clips are already on the soundboard".into(), MUTED));

        app.import_sounds(vec![dir.path().join("missing.wav")], &ctx);
        finish_import(&mut app, &ctx);
        assert_eq!(app.settings.sounds, [pad]);
        assert!(app.dirty_since.is_none());
        assert_eq!(app.sound_status, ("Could not read: missing.wav".into(), RED));
    }

    #[test]
    fn changed_or_removed_files_are_not_added_from_stale_import_results() {
        for removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = wav(dir.path(), "edited.wav", 0.1);
            let stale = CachedClip::read(&path, &AtomicBool::new(false)).unwrap();
            let mut app = App::stopped(Default::default());
            let (done, job) = crossbeam_channel::bounded(1);
            app.clip_import = Some(ClipImport {
                files: [path.clone()].into(), job: Some(job), total: 1,
                added: 0, invalid: Vec::new(), cancel: Arc::default(),
            });
            if removed {
                std::fs::remove_file(&path).unwrap();
            } else {
                std::fs::write(&path, b"changed while importing").unwrap();
            }
            assert!(done.send(Ok(stale)).is_ok());
            let ctx = egui::Context::default();
            app.poll_clip_import(&ctx);
            assert!(app.settings.sounds.is_empty() && app.dirty_since.is_none());
            assert!(app.clip_import.is_none() && !app.clip_cache.contains_key(&path));
            assert_eq!(app.sound_status, ("Could not read: edited.wav".into(), RED));

            wav(dir.path(), "edited.wav", 0.6);
            app.import_sounds(vec![path.clone()], &ctx);
            finish_import(&mut app, &ctx);
            assert_eq!(app.settings.sounds, [Pad::new(path.clone())]);
            assert_eq!(app.cached_clip(&path).unwrap().samples.len(), 28_800, "retry imports the replacement duration");
            assert_eq!(app.sound_status, ("Added 1 clip".into(), GREEN));
        }
    }

    #[test]
    fn pending_imports_keep_controls_responsive_and_accept_more_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = wav(dir.path(), "first.wav", 0.1);
        let second = wav(dir.path(), "second.wav", 0.2);
        let third = wav(dir.path(), "third.wav", 0.3);
        let mut app = App::stopped(crate::config::Settings::default());
        let ctx = egui::Context::default();
        let (done, job) = crossbeam_channel::bounded(1);
        app.clip_import = Some(ClipImport {
            files: [first.clone(), second.clone()].into(), job: Some(job), total: 2,
            added: 0, invalid: Vec::new(), cancel: Arc::default(),
        });

        app.import_sounds(vec![first.clone(), second.clone(), third.clone(), third.clone()], &ctx);
        assert!(app.settings.sounds.is_empty(), "a pending decoder must not be waited on by the UI");
        assert_eq!(app.clip_import.as_ref().unwrap().total, 3, "deduplicate both current and queued files");
        app.run_command(&ctx, super::super::Command::ToggleMute);
        assert!(app.settings.mute, "global microphone controls keep working during an import");

        let clip = CachedClip::read(&first, &AtomicBool::new(false)).unwrap();
        let samples = Arc::clone(&clip.samples);
        assert!(done.send(Ok(clip)).is_ok());
        app.poll_clip_import(&ctx);
        assert_eq!(app.settings.sounds, [Pad::new(first.clone())], "completed clips become available before the whole batch ends");
        assert!(Arc::ptr_eq(&app.cached_clip(&first).unwrap().samples, &samples), "completed imports warm the playback cache");
        finish_import(&mut app, &ctx);
        assert_eq!(app.settings.sounds, [first, second, third].map(Pad::new));
        assert_eq!(app.sound_status, ("Added 3 clips".into(), GREEN));
    }

    #[test]
    fn soundboard_controls_fit_and_keep_focus_when_text_size_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..12).map(|index| wav(dir.path(), &format!("responsive-{index}.wav"), 0.1)).collect();
        let controls = [
            (Role::Button, "Cancel import"), (Role::TextInput, "Search clips"),
            (Role::Button, "Play target"), (Role::Button, "Rename target"),
            (Role::CheckBox, "Add target to the strip"), (Role::Button, "Remove target"),
            (Role::Button, "Stop clips"), (Role::Slider, "volume"),
            (Role::SpinButton, "volume"), (Role::CheckBox, "Let clips overlap"),
        ];
        let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
        });
        let tab = || egui::Event::Key { key: egui::Key::Tab, physical_key: None,
            pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let visible = |output: &egui::FullOutput, target, label: &str, zoom, size: egui::Vec2| {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            assert_eq!(tree.focus, target, "{label} keeps focus at {zoom}");
            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            let rect = egui::Rect::from_min_max(egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32));
            assert!(rect.right() <= size.x && output.shapes.iter().any(|shape| shape.clip_rect.contains_rect(rect)
                && match &shape.shape {
                    egui::Shape::Rect(shape) => shape.rect.intersects(rect),
                    egui::Shape::Text(shape) => shape.visual_bounding_rect().intersects(rect),
                    _ => false,
                }), "{label} is fully visible at {zoom}: {rect:?}");
            if label.ends_with("target") || label == "Add target to the strip" {
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Rect(pad) if (pad.rect.height() - 108.0).abs() < 0.01
                        && pad.rect.contains(rect.center()) && shape.clip_rect.expand(0.5 / zoom).contains_rect(pad.rect))),
                    "the whole focused {label} pad fits at {zoom}: {rect:?}; pad/clip={:?}",
                    output.shapes.iter().filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(pad) if (pad.rect.height() - 108.0).abs() < 0.01
                            && pad.rect.contains(rect.center()) => Some((pad.rect, shape.clip_rect)), _ => None,
                    }).collect::<Vec<_>>());
                let title = output.shapes.iter().find(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.text() == "target")).expect("the focused pad's title is drawn");
                for shape in &output.shapes {
                    if shape.clip_rect == title.clip_rect && let egui::Shape::Text(text) = &shape.shape {
                        assert!(shape.clip_rect.expand(0.5 / zoom).contains_rect(text.visual_bounding_rect()),
                            "pad names and details fit instead of cutting off glyphs at {zoom}: {}", text.galley.text());
                    }
                }
            }
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for assistive in [false, true] {
                    let mut app = App::stopped(crate::config::Settings {
                        sounds: paths.iter().cloned().map(Pad::new).collect(), sound_volume: 0.61347,
                        overlap_clips: false, ..Default::default()
                    });
                    app.page = super::super::Page::Soundboard;
                    app.pad_filter = "r".into();
                    app.engine = Some(crate::engine::Engine::for_test());
                    app.settings.sounds[11].name = Some("target".into());
                    for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    app.play_clip(0, &ctx);
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let playing = app.playing.clone();
                    let progress = app.pad_progress.clone();
                    let status = app.sound_status.clone();
                    let (_done, job) = crossbeam_channel::bounded(1);
                    app.clip_import = Some(ClipImport {
                        files: [dir.path().join("Field recording - extended audience applause.wav")].into(),
                        job: Some(job), total: 2, added: 1, invalid: Vec::new(), cancel: Arc::default(),
                    });
                    let mut ids = Vec::new();
                    let mut size = egui::vec2(700.0, 420.0);
                    for (transition, zoom) in [1.0, 1.25, 1.0, 1.5, 1.0].into_iter().enumerate() {
                        let keep = (transition > 0).then(|| [3, 7, 1, 8][transition - 1]);
                        if let Some(index) = keep {
                            record_frame_at_size(&mut app, &ctx, size, vec![focus(ids[index])]);
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        }
                        ctx.options_mut(|options| options.zoom_factor = zoom);
                        size = egui::vec2(700.0, 420.0) / zoom;
                        for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        if let Some(index) = keep { visible(&output, ids[index], controls[index].1, zoom, size); }
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let top = tree.nodes.iter().find(|(_, node)| node.label() == Some("Pause responsive-0"))
                            .unwrap().1.bounds().unwrap().y0;
                        let columns = tree.nodes.iter().filter(|(_, node)| node.role() == Role::Button
                            && node.label().is_some_and(|label| label == "Pause responsive-0" || label.starts_with("Play responsive-"))
                            && node.bounds().is_some_and(|bounds| bounds.y0 == top)).count();
                        assert_eq!(columns, if zoom == 1.0 { 3 } else if zoom == 1.25 { 2 } else { 1 }, "the grid reflows at {zoom}");
                        for (index, (role, label)) in controls.into_iter().enumerate() {
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)| {
                                node.role() == role && if label == "volume" { node.numeric_value() == Some(f64::from(0.61347f32)) }
                                    else if role == Role::TextInput { node.placeholder() == Some(label) } else { node.label() == Some(label) }
                            }).map(|(id, _)| id).unwrap_or_else(|| panic!("missing {role:?} {label} at {zoom}"));
                            if ids.len() == index { ids.push(target); } else { assert_eq!(ids[index], target, "{role:?} {label} keeps its identity at {zoom}"); }
                            if assistive {
                                record_frame_at_size(&mut app, &ctx, size, vec![focus(target)]);
                            } else {
                                let mut reached = false;
                                for _ in 0..100 {
                                    let output = record_frame_at_size(&mut app, &ctx, size, vec![tab()]);
                                    if output.platform_output.accesskit_update.unwrap().focus == target { reached = true; break; }
                                }
                                assert!(reached, "Tab reaches {role:?} {label} at {zoom}; passes={passes}");
                            }
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            visible(&record_frame_at_size(&mut app, &ctx, size, Vec::new()), target, label, zoom, size);
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original, "navigation preserves settings precision");
                            assert_eq!(app.playing, playing);
                            assert_eq!(app.pad_progress, progress);
                            assert_eq!(app.sound_status, status);
                            assert_eq!(app.pad_filter, "r", "resizing preserves the search query");
                            assert!(app.dirty_since.is_none() && app.clip_loads.is_none() && app.renaming_pad.is_none());
                            assert!(!app.clip_import.as_ref().unwrap().cancel.load(Ordering::Relaxed));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn focused_soundboard_pads_fit_after_height_only_resizing() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..12).map(|index| wav(dir.path(), &format!("responsive-{index}.wav"), 0.1)).collect();
        let controls = [(Role::Button, "Play target"), (Role::Button, "Rename target"),
            (Role::CheckBox, "Add target to the strip"), (Role::Button, "Remove target"), (Role::TextInput, "target")];
        let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
        });
        let pad_visible = |output: &egui::FullOutput, target, zoom| {
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            assert_eq!(tree.focus, target);
            let bounds = tree.nodes.iter().find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
            let center = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
            output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(pad)
                if (pad.rect.height() - 108.0).abs() < 0.01 && pad.rect.contains(center)
                    && shape.clip_rect.expand(0.5 / zoom).contains_rect(pad.rect)))
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for zoom in [1.0, 1.25, 1.5] {
                    for (role, label) in controls {
                        let mut app = App::stopped(crate::config::Settings {
                            sounds: paths.iter().cloned().map(Pad::new).collect(), sound_volume: 0.61347,
                            overlap_clips: false, ..Default::default()
                        });
                        app.page = super::super::Page::Soundboard;
                        app.settings.sounds[11].name = Some("target".into());
                        app.pad_filter = "r".into();
                        app.engine = Some(crate::engine::Engine::for_test());
                        for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| { options.max_passes = passes.try_into().unwrap(); options.zoom_factor = zoom; });
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        widgets::configure_text_styles(&ctx);
                        ctx.enable_accesskit();
                        app.play_clip(0, &ctx);
                        let (_done, job) = crossbeam_channel::bounded(1);
                        app.clip_import = Some(ClipImport {
                            files: [dir.path().join("Field recording - extended audience applause.wav")].into(),
                            job: Some(job), total: 2, added: 1, invalid: Vec::new(), cancel: Arc::default(),
                        });
                        let original = serde_json::to_value(&app.settings).unwrap();
                        let playing = app.playing.clone();
                        let progress = app.pad_progress.clone();
                        let status = app.sound_status.clone();
                        let mut size = egui::vec2(700.0, 880.0) / zoom;
                        for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let opening = if role == Role::TextInput { "Rename target" } else { label };
                        let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(_, node)| node.label() == Some(opening)).map(|(id, _)| id).unwrap();
                        record_frame_at_size(&mut app, &ctx, size, vec![focus(target)]);
                        for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        if role == Role::TextInput {
                            record_frame_at_size(&mut app, &ctx, size, [true, false].into_iter().map(|pressed| egui::Event::Key {
                                key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                            }).collect());
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                        }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let target = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                            node.role() == role && if role == Role::TextInput { node.value() == Some(label) } else { node.label() == Some(label) }
                        ).map(|(id, _)| id).unwrap();
                        for height in [420.0, 880.0, 420.0] {
                            size.y = height / zoom;
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            assert!(pad_visible(&output, target, zoom), "the whole focused {label} pad fits after resizing: height={height}, zoom={zoom}, passes={passes}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert_eq!(app.playing, playing);
                            assert_eq!(app.pad_progress, progress);
                            assert_eq!(app.sound_status, status);
                            assert_eq!(app.pad_filter, "r");
                            assert_eq!(app.renaming_pad.as_ref().map(|edit| (&edit.path, edit.text.as_str())),
                                (role == Role::TextInput).then_some((&paths[11], "target")));
                            assert!(app.dirty_since.is_none() && app.clip_loads.is_none());
                            assert!(!app.clip_import.as_ref().unwrap().cancel.load(Ordering::Relaxed));
                        }
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(id, _)| *id == target).unwrap().1.bounds().unwrap();
                        let pointer = egui::pos2(((bounds.x0 + bounds.x1) * 0.5) as f32, ((bounds.y0 + bounds.y1) * 0.5) as f32);
                        for (delta, visible) in [(1000.0, false), (-1000.0, true)] {
                            record_frame_at_size(&mut app, &ctx, size, vec![egui::Event::PointerMoved(pointer), egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, delta),
                                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                            }]);
                            for _ in 0..8 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                            let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                            assert_eq!(pad_visible(&output, target, zoom), visible,
                                "manual scrolling can leave and return to the focused {label} pad without snapping back; zoom={zoom}, delta={delta}");
                            assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                            assert_eq!(app.playing, playing);
                            assert_eq!(app.pad_progress, progress);
                            assert_eq!(app.sound_status, status);
                            assert_eq!(app.pad_filter, "r");
                            assert_eq!(app.renaming_pad.as_ref().map(|edit| (&edit.path, edit.text.as_str())),
                                (role == Role::TextInput).then_some((&paths[11], "target")));
                            assert!(app.dirty_since.is_none() && app.clip_loads.is_none());
                            assert!(!app.clip_import.as_ref().unwrap().cancel.load(Ordering::Relaxed));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn search_keeps_focus_and_query_when_background_clips_and_engine_status_change() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..10).map(|index| wav(dir.path(), &format!("search-{index}.wav"), 0.1)).collect();
        let imported = wav(dir.path(), "target-new.wav", 0.1);
        for dark in [true, false] {
            for passes in [1, 2] {
                for scene in ["engine starts", "engine stops", "import fails", "import completes", "inactive search",
                    "recording completes", "recording inactive search"] {
                    let mut app = App::stopped(crate::config::Settings {
                        sounds: paths.iter().cloned().map(Pad::new).collect(), ..Default::default()
                    });
                    app.page = super::super::Page::Soundboard;
                    app.settings.sounds[0].name = Some("target".into());
                    app.pad_filter = "tar".into();
                    if scene != "engine starts" { app.engine = Some(crate::engine::Engine::for_test()); }
                    let (done, job) = crossbeam_channel::bounded(1);
                    let (save_done, save_job) = crossbeam_channel::bounded(1);
                    if scene.starts_with("import") || scene == "inactive search" {
                        app.clip_import = Some(ClipImport {
                            files: [imported.clone()].into(), job: Some(job), total: 1,
                            added: 0, invalid: Vec::new(), cancel: Arc::default(),
                        });
                    } else if scene.starts_with("recording") {
                        app.take = Recorder::from_samples_for_test(&[0.25; 480]).finish().unwrap().map(Arc::new);
                        app.recording_save = Some(RecordingSave { job: save_job, add_to_board: true });
                    }
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    for _ in 0..3 { board_frame(&mut app, &ctx, Vec::new()); }
                    let output = board_frame(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let search = *tree.nodes.iter().find(|(_, node)|
                        node.role() == Role::TextInput && node.placeholder() == Some("Search clips")).map(|(id, _)| id).unwrap();
                    let focus = if scene.ends_with("inactive search") {
                        *tree.nodes.iter().find(|(_, node)| node.role() == Role::Button && node.label() == Some("Soundboard"))
                            .map(|(id, _)| id).unwrap()
                    } else { search };
                    board_frame(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: focus, data: None,
                    })]);
                    for _ in 0..3 { board_frame(&mut app, &ctx, Vec::new()); }
                    let mut expected = app.settings.sounds.clone();
                    match scene {
                        "engine starts" => app.engine = Some(crate::engine::Engine::for_test()),
                        "engine stops" => app.engine = None,
                        "recording completes" | "recording inactive search" => {
                            save_done.send(Ok(imported.clone())).unwrap();
                            app.poll_recording_save(&ctx);
                            assert!(app.recording_save.is_none() && app.take.is_none());
                            expected.push(Pad::new(imported.clone()));
                        }
                        _ => {
                            let result = if scene == "import fails" { Err(anyhow::anyhow!("unreadable fixture")) }
                                else { Ok(CachedClip::read(&imported, &AtomicBool::new(false)).unwrap()) };
                            done.send(result).unwrap();
                            app.poll_clip_import(&ctx);
                            assert!(app.clip_import.is_none());
                            if scene != "import fails" { expected.push(Pad::new(imported.clone())); }
                        }
                    }
                    let output = board_frame(&mut app, &ctx, Vec::new());
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let current = *tree.nodes.iter().find(|(_, node)|
                        node.role() == Role::TextInput && node.placeholder() == Some("Search clips")).map(|(id, _)| id).unwrap();
                    assert_eq!(current, search, "search keeps its identity when {scene}; passes={passes}");
                    assert_eq!(tree.focus, focus, "the status change leaves focus in place");
                    if scene.ends_with("inactive search") {
                        assert!(app.pad_filter.is_empty(), "an inactive filter clears so the imported pad is shown");
                    } else {
                        assert_eq!(app.pad_filter, "tar", "an active query survives {scene}");
                        board_frame(&mut app, &ctx, vec![egui::Event::Text("get".into())]);
                        assert_eq!(app.pad_filter, "target", "typing continues in the search field");
                    }
                    assert_eq!(app.settings.sounds, expected);
                    assert!(app.playing.is_empty() && app.clip_loads.is_none() && app.renaming_pad.is_none());
                }
            }
        }
    }

    #[test]
    fn playback_controls_keep_focus_and_numeric_edits_during_status_changes() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "playback-focus.wav", 0.1);
        let imported = wav(dir.path(), "imported.wav", 0.1);
        let key = |key, modifiers| egui::Event::Key {
            key, physical_key: None, pressed: true, repeat: false, modifiers,
        };
        for dark in [true, false] {
            for passes in [1, 2] {
                for scene in ["engine starts", "engine stops", "import starts", "import fails", "import completes"] {
                    for control in ["slider", "number fraction", "number caret", "overlap"] {
                        let mut app = App::stopped(crate::config::Settings {
                            sounds: vec![Pad::new(path.clone())], sound_volume: 0.61347,
                            overlap_clips: false, ..Default::default()
                        });
                        app.page = super::super::Page::Soundboard;
                        if scene != "engine starts" { app.engine = Some(crate::engine::Engine::for_test()); }
                        let (done, job) = crossbeam_channel::bounded(1);
                        let import = ClipImport {
                            files: [imported.clone()].into(), job: Some(job), total: 1,
                            added: 0, invalid: Vec::new(), cancel: Arc::default(),
                        };
                        if matches!(scene, "import fails" | "import completes") { app.clip_import = Some(import); }
                        let ctx = egui::Context::default();
                        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
                        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                        ctx.enable_accesskit();
                        for _ in 0..3 { board_frame(&mut app, &ctx, Vec::new()); }
                        let output = board_frame(&mut app, &ctx, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let slider = *tree.nodes.iter().find(|(_, node)| node.role() == Role::Slider
                            && node.numeric_value() == Some(f64::from(0.61347f32))).map(|(id, _)| id).unwrap();
                        let number = *tree.nodes.iter().find(|(_, node)| node.role() == Role::SpinButton
                            && node.numeric_value() == Some(f64::from(0.61347f32))).map(|(id, _)| id).unwrap();
                        let overlap = *tree.nodes.iter().find(|(_, node)| node.role() == Role::CheckBox
                            && node.label() == Some("Let clips overlap")).map(|(id, _)| id).unwrap();
                        let focus = match control { "slider" => slider, "overlap" => overlap, _ => number };
                        board_frame(&mut app, &ctx, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node: focus, data: None,
                        })]);
                        for _ in 0..3 { board_frame(&mut app, &ctx, Vec::new()); }
                        assert_eq!(app.settings.sound_volume, 0.61347, "focusing preserves saved precision");
                        assert!(app.dirty_since.is_none());
                        if control.starts_with("number") {
                            board_frame(&mut app, &ctx, vec![key(egui::Key::A, egui::Modifiers::COMMAND)]);
                            let text = if control == "number fraction" { "40." } else { "5%" };
                            let mut events = vec![egui::Event::Text(text.into())];
                            if control == "number caret" { events.push(key(egui::Key::ArrowLeft, egui::Modifiers::NONE)); }
                            board_frame(&mut app, &ctx, events);
                            board_frame(&mut app, &ctx, Vec::new());
                            assert_eq!(app.settings.sound_volume, if control == "number fraction" { 0.4 } else { 0.05 });
                        }
                        let mut expected = app.settings.clone();
                        let before = app.settings.sound_volume;
                        match scene {
                            "engine starts" => app.engine = Some(crate::engine::Engine::for_test()),
                            "engine stops" => app.engine = None,
                            "import starts" => app.clip_import = Some(ClipImport {
                                files: [imported.clone()].into(), job: None, total: 1,
                                added: 0, invalid: Vec::new(), cancel: Arc::default(),
                            }),
                            _ => {
                                let result = if scene == "import fails" { Err(anyhow::anyhow!("unreadable fixture")) }
                                    else { Ok(CachedClip::read(&imported, &AtomicBool::new(false)).unwrap()) };
                                done.send(result).unwrap();
                                app.poll_clip_import(&ctx);
                                assert!(app.clip_import.is_none());
                                if scene == "import completes" { expected.sounds.push(Pad::new(imported.clone())); }
                            }
                        }
                        let output = board_frame(&mut app, &ctx, Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        for id in [slider, number, overlap] {
                            assert!(tree.nodes.iter().any(|(current, _)| *current == id),
                                "playback controls keep their identity when {scene}; control={control}, passes={passes}");
                        }
                        assert_eq!(tree.focus, focus, "{scene} keeps {control} focused; passes={passes}");
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), serde_json::to_value(&expected).unwrap());
                        let events = match control {
                            "slider" => vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
                            "overlap" => vec![key(egui::Key::Space, egui::Modifiers::NONE)],
                            _ => vec![egui::Event::Text(if control == "number fraction" { "5" } else { "0" }.into()),
                                key(egui::Key::Enter, egui::Modifiers::NONE)],
                        };
                        board_frame(&mut app, &ctx, events);
                        for _ in 0..3 { board_frame(&mut app, &ctx, Vec::new()); }
                        match control {
                            "slider" => {
                                assert!(app.settings.sound_volume > before, "the slider still accepts arrow keys");
                                expected.sound_volume = app.settings.sound_volume;
                            }
                            "overlap" => expected.overlap_clips = true,
                            "number fraction" => expected.sound_volume = 0.405,
                            _ => expected.sound_volume = 0.5,
                        }
                        assert_eq!(serde_json::to_value(&app.settings).unwrap(), serde_json::to_value(&expected).unwrap(),
                            "editing continues at the saved caret after {scene}; control={control}, passes={passes}");
                        assert_eq!(app.params().sound_gain, expected.sound_volume);
                        assert!(app.dirty_since.is_some());
                        assert!(app.playing.is_empty() && app.clip_loads.is_none() && app.renaming_pad.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn cancelling_import_keeps_finished_pads_and_discards_late_results() {
        let dir = tempfile::tempdir().unwrap();
        let saved = wav(dir.path(), "saved.wav", 0.1);
        let pending = wav(dir.path(), "pending.wav", 0.2);
        let pad = Pad::new(saved);
        let mut app = App::stopped(crate::config::Settings { sounds: vec![pad.clone()], ..Default::default() });
        app.page = super::super::Page::Soundboard;
        let ctx = egui::Context::default();
        let (done, job) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        app.clip_import = Some(ClipImport {
            files: [pending.clone()].into(), job: Some(job), total: 2, added: 1,
            cancel: Arc::clone(&cancel), invalid: Vec::new(),
        });
        assert!(app.update_blocker().is_some());
        board(&mut app, &ctx, Vec::new());
        let pos = pad_title_position(&mut app, &ctx, "Cancel import");
        for pressed in [true, false] {
            board(&mut app, &ctx, vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
            ]);
        }

        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.clip_import.is_some() && app.update_blocker().is_none());
        assert!(board(&mut app, &ctx, Vec::new()).iter().any(|text| text == "Finishing cancellation…"));
        assert_eq!(app.settings.sounds, [pad]);
        assert!(done.send(Err(anyhow::anyhow!("late result"))).is_ok());
        app.poll_clip_import(&ctx);
        assert!(app.clip_import.is_none());
        assert!(app.sound_status.0.starts_with("Import cancelled"));

        app.import_sounds(vec![pending], &ctx);
        finish_import(&mut app, &ctx);
        assert_eq!(app.settings.sounds.len(), 2, "a cancelled file can be imported again");
    }

    #[test]
    fn a_new_import_waits_for_cancelled_work_and_keeps_its_own_results() {
        let dir = tempfile::tempdir().unwrap();
        let old = wav(dir.path(), "old.wav", 0.1);
        let next = wav(dir.path(), "next.wav", 0.2);
        let ctx = egui::Context::default();
        for disconnected in [false, true] {
            let mut app = App::stopped(Default::default());
            let (done, job) = crossbeam_channel::bounded(1);
            let cancel = Arc::default();
            app.clip_import = Some(ClipImport {
                files: [old.clone()].into(), job: Some(job), cancel: Arc::clone(&cancel),
                total: 3, added: 1, invalid: vec!["earlier failure.wav".into()],
            });
            for _ in 0..3 {
                app.cancel_clip_import();
                app.import_sounds(vec![next.clone(), next.clone()], &ctx);
                let import = app.clip_import.as_ref().unwrap();
                assert!(Arc::ptr_eq(&cancel, &import.cancel) && cancel.load(Ordering::Relaxed));
                assert_eq!((import.files.len(), import.files.front()), (1, Some(&next)));
                assert_eq!((import.total, import.added), (1, 0));
                assert!(import.invalid.is_empty());
                assert!(app.settings.sounds.is_empty() && app.update_blocker().is_some());
            }
            if !disconnected {
                assert!(done.send(CachedClip::read(&old, &AtomicBool::new(false))).is_ok());
            }
            drop(done);
            finish_import(&mut app, &ctx);
            assert_eq!(app.settings.sounds, [Pad::new(next.clone())]);
            assert!(!app.clip_cache.contains_key(&old), "ignore a cancelled import's late success");
            assert!(app.clip_cache.contains_key(&next));
            assert_eq!(app.sound_status, ("Added 1 clip".into(), GREEN));
            assert!(app.update_blocker().is_none());
        }
    }

    #[test]
    fn a_failed_import_worker_does_not_stall_the_rest_of_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let good = wav(dir.path(), "good.wav", 0.1);
        let mut app = App::stopped(crate::config::Settings::default());
        let ctx = egui::Context::default();
        let (done, job) = crossbeam_channel::bounded(1);
        app.clip_import = Some(ClipImport {
            files: [dir.path().join("failed.wav"), good.clone()].into(), job: Some(job), total: 2,
            added: 0, invalid: Vec::new(), cancel: Arc::default(),
        });
        drop(done);
        finish_import(&mut app, &ctx);
        assert_eq!(app.settings.sounds, [Pad::new(good)]);
        assert_eq!(app.sound_status, ("Added 1 clip · Could not read: failed.wav".into(), AMBER));
        assert!(app.update_blocker().is_none());
    }

    #[test]
    #[ignore]
    fn snapshot_responsive_soundboard_controls() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..12).map(|index| wav(dir.path(), &format!("responsive-{index}.wav"), 0.1)).collect();
        for (zoom, scale) in [(1.0, "standard"), (1.25, "medium"), (1.5, "zoomed")] {
            for (dark, theme) in [(true, "dark"), (false, "light")] {
                for scene in ["import", "search", "pad", "playback", "overview", "empty"] {
                    if scene == "empty" && zoom != 1.5 { continue; }
                    let mut app = App::stopped(crate::config::Settings {
                        sounds: if scene == "empty" { Vec::new() } else { paths.iter().cloned().map(Pad::new).collect() },
                        sound_volume: 0.61347, overlap_clips: false, ..Default::default()
                    });
                    app.page = super::super::Page::Soundboard;
                    if scene != "empty" {
                        app.settings.sounds[0].name = Some("Audience applause - extended field recording".into());
                        app.settings.sounds[11].name = Some("target".into());
                        app.engine = Some(crate::engine::Engine::for_test());
                        for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
                        app.play_clip(0, &egui::Context::default());
                        app.clip_import = Some(ClipImport {
                            files: [dir.path().join("Field recording - extended audience applause.wav")].into(),
                            job: None, total: 2, added: 1, invalid: Vec::new(), cancel: Arc::default(),
                        });
                    }
                    if scene == "search" { app.pad_filter = "r".into(); }
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let size = egui::vec2(700.0, if scene == "overview" { 1000.0 } else { 420.0 }) / zoom;
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    widgets::configure_text_styles(&ctx);
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, size, Vec::new()); }
                    let mut events = vec![Vec::new(); 3];
                    if !matches!(scene, "overview" | "empty") {
                        let output = record_frame_at_size(&mut app, &ctx, size, Vec::new());
                        let (role, label) = match scene {
                            "import" => (Role::Button, "Cancel import"), "search" => (Role::TextInput, "Search clips"),
                            "pad" => (Role::Button, "Play target"), _ => (Role::SpinButton, "volume"),
                        };
                        let target_node = *output.platform_output.accesskit_update.unwrap().nodes.iter().find(|(_, node)|
                            node.role() == role && if role == Role::SpinButton { node.numeric_value() == Some(f64::from(0.61347f32)) }
                                else if role == Role::TextInput { node.placeholder() == Some(label) } else { node.label() == Some(label) }
                        ).map(|(id, _)| id).unwrap();
                        events.push(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                        })]);
                        events.extend(vec![Vec::new(); 8]);
                    }
                    let image = super::super::snapshot::snapshot_with_events(&mut app,
                        &format!("board-responsive-{scene}-{scale}-{theme}"), size, zoom, dark, events);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert!(app.dirty_since.is_none());
                }
            }
        }
    }

    #[test]
    #[ignore = "writes focused Soundboard pads after height-only resizing; needs Python with Pillow"]
    fn snapshot_soundboard_pads_after_resizing() {
        use egui::accesskit::{Action, ActionRequest, TreeId};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..12).map(|index| wav(dir.path(), &format!("responsive-{index}.wav"), 0.1)).collect();
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for zoom in [1.0, 1.25, 1.5] {
                for (label, scene) in [("Play target", "play"), ("Rename target", "rename"),
                    ("Add target to the strip", "star"), ("Remove target", "remove"), ("Rename target", "edit")]
                {
                    let mut app = App::stopped(crate::config::Settings {
                        sounds: paths.iter().cloned().map(Pad::new).collect(), sound_volume: 0.61347,
                        overlap_clips: false, ..Default::default()
                    });
                    app.page = super::super::Page::Soundboard;
                    app.settings.sounds[11].name = Some("target".into());
                    app.pad_filter = "r".into();
                    app.engine = Some(crate::engine::Engine::for_test());
                    for path in &paths { app.cache_clip(path.clone(), CachedClip::read(path, &AtomicBool::new(false)).unwrap()); }
                    app.play_clip(0, &egui::Context::default());
                    app.clip_import = Some(ClipImport {
                        files: [dir.path().join("Field recording - extended audience applause.wav")].into(),
                        job: None, total: 2, added: 1, invalid: Vec::new(), cancel: Arc::default(),
                    });
                    let original = serde_json::to_value(&app.settings).unwrap();
                    let playing = app.playing.clone();
                    let progress = app.pad_progress.clone();
                    let status = app.sound_status.clone();
                    let initial = egui::vec2(700.0, 880.0) / zoom;
                    let size = egui::vec2(700.0, 420.0) / zoom;
                    let ctx = egui::Context::default();
                    ctx.options_mut(|options| options.zoom_factor = zoom);
                    ctx.enable_accesskit();
                    for _ in 0..3 { record_frame_at_size(&mut app, &ctx, initial, Vec::new()); }
                    let output = record_frame_at_size(&mut app, &ctx, initial, Vec::new());
                    let target = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                        .find(|(_, node)| node.label() == Some(label)).map(|(id, _)| id).unwrap();
                    let input = |size, events| egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default()
                    };
                    let mut inputs: Vec<_> = (0..3).map(|_| input(initial, Vec::new())).collect();
                    inputs.push(input(initial, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                        action: Action::Focus, target_tree: TreeId::ROOT, target_node: target, data: None,
                    })]));
                    inputs.extend((0..8).map(|_| input(initial, Vec::new())));
                    if scene == "edit" {
                        inputs.push(input(initial, [true, false].into_iter().map(|pressed| egui::Event::Key {
                            key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                        }).collect()));
                        inputs.extend((0..8).map(|_| input(initial, Vec::new())));
                    }
                    inputs.extend((0..8).map(|_| input(size, Vec::new())));
                    let image = super::super::snapshot::snapshot_with_inputs(&mut app,
                        &format!("board-resize-{scene}-{}-{theme}", (zoom * 100.0) as u32), size, zoom, dark, inputs);
                    println!("SNAPSHOT {}", image.display());
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                    assert_eq!(app.playing, playing);
                    assert_eq!(app.pad_progress, progress);
                    assert_eq!(app.sound_status, status);
                    assert_eq!(app.pad_filter, "r");
                    assert_eq!(app.renaming_pad.as_ref().map(|edit| (&edit.path, edit.text.as_str())),
                        (scene == "edit").then_some((&paths[11], "target")));
                    assert!(app.dirty_since.is_none() && app.clip_loads.is_none());
                    assert!(!app.clip_import.as_ref().unwrap().cancel.load(Ordering::Relaxed));
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn snapshot_clip_import() {
        let mut app = App::stopped(crate::config::Settings::default());
        app.page = super::super::Page::Soundboard;
        app.clip_import = Some(ClipImport {
            files: [PathBuf::from("Field recording - extended audience applause.wav"), PathBuf::from("airhorn.wav")].into(),
            total: 2, job: None, added: 0, invalid: Vec::new(), cancel: Arc::default(),
        });
        for (height, name) in [(720.0, "soundboard-import"), (420.0, "soundboard-import-short")] {
            let path = super::super::snapshot::snapshot(&mut app, name, egui::vec2(700.0, height), 1.25, true);
            println!("SNAPSHOT {}", path.display());
        }
    }

    #[test]
    fn edited_clip_files_refresh_audio_duration_and_trim_waveform() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "editable.wav", 0.2);
        let mut app = App::stopped(crate::config::Settings { sounds: vec![Pad::new(path.clone())], ..Default::default() });
        let ctx = egui::Context::default();
        let load = |app: &mut App| {
            app.trim_waveform(&path, &ctx);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.clip_loads.is_some() {
                app.poll_clip_loads(&ctx);
                assert!(std::time::Instant::now() < deadline, "waveform load did not finish");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            app.cached_clip(&path).map(|clip| (Arc::clone(&clip.samples), Arc::clone(&clip.peaks)))
        };
        let (original, original_peaks) = load(&mut app).unwrap();
        assert!(Arc::ptr_eq(&original, &load(&mut app).unwrap().0), "unchanged files reuse decoded audio");
        assert_eq!(app.pad_length(&path), Some(0.2));

        wav(dir.path(), "editable.wav", 1.0);
        let (longer, longer_peaks) = load(&mut app).unwrap();
        assert_eq!(longer.len(), 48_000);
        assert_eq!(original.len(), 9_600, "audio already handed to a playing clip stays intact");
        assert_eq!(app.pad_length(&path), Some(1.0));
        assert!(!Arc::ptr_eq(&original_peaks, &longer_peaks), "regenerate the old waveform");

        // An edit can keep the length while changing all its samples.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut silent = std::fs::read(&path).unwrap();
        silent[44..].fill(0);
        std::fs::write(&path, silent).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(
            std::fs::FileTimes::new().set_modified(modified + std::time::Duration::from_secs(2)),
        ).unwrap();
        let (edited, edited_peaks) = load(&mut app).unwrap();
        assert!(edited.iter().all(|&sample| sample == 0.0));
        assert!(edited_peaks.iter().all(|&peak| peak == 0.0), "same-length edits also refresh the waveform");
        assert!(!Arc::ptr_eq(&longer, &edited));

        std::fs::remove_file(&path).unwrap();
        assert!(load(&mut app).is_none(), "a removed source must not play stale audio");
        assert!(app.clip_errors.contains_key(&path));
        assert_eq!(app.pad_length(&path), None);
    }

    #[test]
    fn clicking_a_name_edits_it_without_changing_playback() {
        let dir = tempfile::tempdir().unwrap();
        let path = wav(dir.path(), "a.wav", 0.5);
        let mut pad = Pad::new(path.clone());
        pad.starred = true;
        let mut app = App::stopped(crate::config::Settings { sounds: vec![pad], ..Default::default() });
        app.page = super::super::Page::Soundboard;
        let key = clip_key(&path);
        app.playing.push(key);
        let progress = crate::dsp::ClipProgress { key, fraction: 0.25, paused: false, duration: 0.5 };
        app.pad_progress.insert(key, progress);
        let ctx = egui::Context::default();
        board(&mut app, &ctx, Vec::new());
        let click = |app: &mut App, pos| {
            for pressed in [true, false] {
                board(app, &ctx, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
                ]);
            }
        };
        for (text, finish, expected) in [
            (" Beep ", Some(egui::Key::Enter), "Beep"),
            ("Cancel", Some(egui::Key::Escape), "Beep"),
            ("Chime", None, "Chime"),
            ("   ", Some(egui::Key::Enter), "a"),
            ("Before Tab", Some(egui::Key::Tab), "Before Tab"),
        ] {
            let name = app.settings.sounds[0].display_name();
            let pos = pad_title_position(&mut app, &ctx, &name);
            click(&mut app, pos);
            assert!(app.renaming_pad.is_some(), "clicking the title starts an inline edit");
            if finish == Some(egui::Key::Tab) {
                board(&mut app, &ctx, vec![
                    egui::Event::Text(text.into()),
                    egui::Event::Key {
                        key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    },
                ]);
                for _ in 0..3 { board(&mut app, &ctx, Vec::new()); }
            } else {
                board(&mut app, &ctx, vec![egui::Event::Text(text.into())]);
            }
            if let Some(key) = finish.filter(|key| *key != egui::Key::Tab) {
                board(&mut app, &ctx, [true, false].into_iter().map(|pressed| egui::Event::Key {
                    key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                }).collect());
            } else if finish.is_none() {
                click(&mut app, egui::pos2(730.0, 1500.0));
            }
            assert!(app.renaming_pad.is_none());
            assert_eq!(app.settings.sounds[0].display_name(), expected);
            assert_eq!(app.sound_status.0, "Ready", "renaming never triggers play/pause");
            assert_eq!(app.playing, [key]);
            assert_eq!(app.pad_progress.get(&key), Some(&progress));
            assert!(app.settings.sounds[0].starred);
            assert_eq!(app.settings.sounds[0].path, path);
            assert!(path.exists(), "the file is not renamed");
        }
        assert!(app.dirty_since.is_some(), "the saved label is persisted");
    }

    #[test]
    fn pads_show_their_length_and_search_narrows_a_long_board() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::config::Settings {
            sounds: (0..12)
                .map(|i| crate::config::Pad::new(wav(dir.path(), &format!("clip{i:02}.wav"), 0.5 + i as f32)))
                .collect(),
            ..Default::default()
        };
        settings.sounds[3].start = 1.0; // 3.5 s trimmed to 2.5 s
        let mut app = App::stopped(settings);
        app.page = super::super::Page::Soundboard;
        let ctx = egui::Context::default();
        board(&mut app, &ctx, Vec::new());
        app.pad_durations.finish(&ctx);
        let texts = board(&mut app, &ctx, Vec::new());
        assert!(texts.iter().any(|t| t == "WAV · 0.5 s"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "WAV · 2.5 s"), "the trimmed length");
        assert!(texts.iter().any(|t| t.starts_with("clip11")));

        app.pad_filter = "clip1".into();
        let texts = board(&mut app, &ctx, Vec::new());
        let names: Vec<&String> = texts.iter().filter(|t| t.starts_with("clip") && *t != "clip1").collect(); // not the search box
        assert_eq!(names, ["clip10", "clip11"].map(String::from).iter().collect::<Vec<_>>(), "only matches");
        app.settings.sounds[0].name = Some("Beep".into());
        app.pad_filter = "BEEP".into();
        let texts = board(&mut app, &ctx, Vec::new());
        assert!(texts.iter().any(|t| t == "Beep"), "search finds the saved name");
        assert!(!texts.iter().any(|t| t.starts_with("clip")), "the old filenames do not match");
    }

    #[test]
    fn dragging_a_pad_onto_another_moves_it_there() {
        let dir = tempfile::tempdir().unwrap();
        let settings = crate::config::Settings {
            sounds: ["a.wav", "b.wav", "c.wav"].map(|n| crate::config::Pad::new(wav(dir.path(), n, 0.2))).to_vec(),
            ..Default::default()
        };
        let mut app = App::stopped(settings);
        app.page = super::super::Page::Soundboard;
        let ctx = egui::Context::default();
        board(&mut app, &ctx, Vec::new());
        let from = pad_title_position(&mut app, &ctx, "a");
        let to = pad_title_position(&mut app, &ctx, "c");
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for events in [
            vec![egui::Event::PointerMoved(from)],
            vec![button(from, true)],
            vec![egui::Event::PointerMoved(from + egui::vec2(20.0, 0.0))],
            vec![egui::Event::PointerMoved(to)],
            vec![egui::Event::PointerMoved(to)],
            vec![button(to, false)],
            Vec::new(),
        ] {
            board(&mut app, &ctx, events);
        }
        let order: Vec<String> = app.settings.sounds.iter().map(Pad::display_name).collect();
        assert_eq!(order, ["b", "c", "a"]);
        assert!(app.dirty_since.is_some(), "the new order is saved");
    }
}

