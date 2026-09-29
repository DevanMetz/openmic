//! Soundboard tab: recording clips, the pads (with per-pad volume and
//! hotkeys), and playback.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, ComboBox, RichText, ScrollArea};

use super::{
    clip_key, clock_time, file_name, short, volume_slider, App, Binding, AMBER, CYAN, GREEN,
    MUTED, RED, VIOLET,
};
use crate::config::Pad;
use crate::record::{self, Recorder, Source, PEAKS_PER_SECOND};
use crate::widgets;

pub(super) struct PadRename {
    path: PathBuf,
    text: String,
    focus: bool,
}

impl App {
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
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            response.surrender_focus();
            self.finish_pad_rename(false);
        } else if !first_frame && (response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter))) {
            response.surrender_focus();
            self.finish_pad_rename(true);
        }
    }

    fn add_sounds(&mut self) {
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
        let mut invalid = Vec::new();
        let mut added = 0;
        for path in paths {
            if self.settings.sounds.iter().any(|p| p.path == path) {
                continue;
            }
            // Decoding up front checks the file and warms the cache.
            match self.clip_samples(&path) {
                Ok(_) => {
                    self.settings.sounds.push(Pad::new(path));
                    added += 1;
                }
                Err(_) => invalid.push(file_name(&path)),
            }
        }
        if !invalid.is_empty() {
            self.sound_status = (format!("Could not read: {}", invalid.join(", ")), RED);
        } else if added > 0 {
            self.sound_status = (
                format!("Added {added} clip{}", if added == 1 { "" } else { "s" }),
                GREEN,
            );
        }
        if added > 0 {
            self.touch();
        }
    }

    fn remove_clip(&mut self, index: usize) {
        if index >= self.settings.sounds.len() {
            return;
        }
        let removed = self.settings.sounds.remove(index);
        if self.renaming_pad.as_ref().is_some_and(|edit| edit.path == removed.path) {
            self.renaming_pad = None;
        }
        let key = clip_key(&removed.path);
        if let Some(engine) = &self.engine {
            engine.stop_sound(key);
        }
        self.playing.retain(|&k| k != key);
        self.pad_progress.remove(&key);
        self.clip_cache.remove(&removed.path);
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
            Err(err) => self.record_status = (short(&format!("{err:#}")), RED),
        }
    }

    fn stop_recording(&mut self) {
        if let Some(recorder) = self.recorder.take() {
            self.record_status = match recorder.finish() {
                Ok(Some(take)) => {
                    let duration = clock_time(take.duration());
                    let status = match take.warning() {
                        Some(warning) => (format!("Partial take ready · {duration} · {}", short(warning)), AMBER),
                        None => (format!("Take ready · {duration}"), GREEN),
                    };
                    self.take = Some(take);
                    status
                }
                Ok(None) => ("No audio was captured. Try again while sound is playing.".into(), AMBER),
                Err(err) => (short(&format!("{err:#}")), RED),
            };
        }
    }

    pub(super) fn poll_recording(&mut self) {
        let error = self.recorder.as_ref().and_then(Recorder::take_error);
        if let Some(error) = error {
            crate::logfile::error(format_args!("recording stopped: {error}"));
            self.stop_recording();
            self.record_status = (short(&format!("Recording stopped: {error}")), RED);
        } else if self.recorder.as_ref().is_some_and(Recorder::is_full) {
            self.stop_recording();
            self.record_status = ("Reached the 4 GB WAV size limit; your take is ready".into(), AMBER);
        } else if self.recorder.as_ref().is_some_and(Recorder::take_skipped) {
            self.record_status = ("The disk fell behind, so a moment of audio was skipped".into(), AMBER);
        }
    }

    fn save_recording(&mut self) {
        let Some(take) = &self.take else { return };
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
        match take.save_wav(&path) {
            Ok(()) => {
                self.take = None;
                self.record_status = (format!("Saved {}", file_name(&path)), GREEN);
            }
            Err(err) => self.record_status = (short(&format!("{err:#}")), RED),
        }
    }

    fn add_recording(&mut self) {
        let Some(take) = &self.take else { return };
        match take.add_to_library(&self.record_name) {
            Ok(path) => {
                self.record_status = (format!("Added {} to your pads", file_name(&path)), GREEN);
                self.settings.sounds.push(Pad::new(path));
                self.take = None;
                self.touch();
            }
            Err(err) => self.record_status = (short(&format!("{err:#}")), RED),
        }
    }

    fn draw_recording(&mut self, ui: &mut egui::Ui) {
        let active = self.recorder.is_some();
        let take_ready = self.take.is_some();
        let outputs = self.outputs.clone();
        let mut start = false;
        let mut stop = false;
        let mut save = false;
        let mut add = false;
        let mut discard = false;
        widgets::card(
            ui,
            "RECORD A CLIP",
            if active { RED } else { VIOLET },
            |ui| {
                widgets::badge(
                    ui,
                    if active {
                        "Recording"
                    } else if take_ready {
                        "Take ready"
                    } else {
                        "Local only"
                    },
                    if active { RED } else { VIOLET },
                );
            },
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Source").weak());
                    ui.add_enabled_ui(!active && !take_ready, |ui| {
                        ui.selectable_value(&mut self.record_source, Source::Microphone, "Microphone");
                        ui.selectable_value(&mut self.record_source, Source::Computer, "Computer audio");
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Device").weak());
                    match self.record_source {
                        Source::Microphone => {
                            let name = if self.settings.microphone.is_empty() {
                                "Windows default microphone"
                            } else {
                                &self.settings.microphone
                            };
                            ui.add_sized(
                                [ui.available_width().min(520.0), 18.0],
                                egui::Label::new(name).truncate(),
                            );
                        }
                        Source::Computer => {
                            let label = if self.record_output.is_empty() {
                                "Windows default output".to_owned()
                            } else {
                                self.record_output.clone()
                            };
                            ui.add_enabled_ui(!active && !take_ready, |ui| {
                                ComboBox::from_id_salt("record-output")
                                    .width(360.0)
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.record_output,
                                            String::new(),
                                            "Windows default output",
                                        );
                                        for output in &outputs {
                                            ui.selectable_value(
                                                &mut self.record_output,
                                                output.clone(),
                                                output,
                                            );
                                        }
                                    });
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
                    ui.horizontal(|ui| {
                        stop = ui
                            .add(egui::Button::new(RichText::new("Stop recording").color(Color32::BLACK)).fill(RED))
                            .clicked();
                        let pulse = 0.55 + 0.45 * (ui.input(|i| i.time) * 4.0).sin() as f32;
                        ui.label(RichText::new("⏺").color(RED.gamma_multiply(pulse)));
                        ui.label(RichText::new(clock_time(elapsed)).color(RED).monospace());
                        ui.weak(format!("· {}", file_size(recorder.bytes())));
                    });
                    ui.add_space(6.0);
                    // The last six seconds, scrolling in from the right.
                    widgets::waveform(ui, &peaks, Some(window), 64.0, RED);
                    ui.add_space(6.0);
                    let level = peaks.iter().rev().take(5).fold(0.0f32, |m, &p| m.max(p));
                    let width = ui.available_width();
                    widgets::level_meter(ui, "Level", Some(level), &mut self.record_hold, width);
                    if peaks.len() >= 2 * PEAKS_PER_SECOND && is_silent(&peaks[peaks.len() - 2 * PEAKS_PER_SECOND..]) {
                        ui.colored_label(AMBER, match self.record_source {
                            Source::Microphone => "No sound from the microphone yet.",
                            Source::Computer => "Nothing is playing through this output yet.",
                        });
                    }
                } else if let Some(take) = &self.take {
                    widgets::waveform(ui, take.peaks(), None, 56.0, VIOLET);
                    if is_silent(take.peaks()) {
                        ui.colored_label(AMBER, "This take is silent.");
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut self.record_name).desired_width(260.0));
                        ui.weak(clock_time(take.duration()));
                    });
                    ui.horizontal(|ui| {
                        add = ui.button("Add to soundboard").clicked();
                        save = ui.button("Save WAV…").clicked();
                        discard = ui.small_button("Discard take").clicked();
                    });
                } else {
                    ui.horizontal(|ui| {
                        start = ui
                            .add(egui::Button::new(RichText::new("⏺  Record").color(Color32::BLACK)).fill(RED))
                            .clicked();
                        ui.weak("Recording stays on this computer.");
                    });
                }
                ui.add_space(4.0);
                ui.label(RichText::new(&self.record_status.0).color(self.record_status.1));
            },
        );
        if start {
            self.start_recording();
        }
        if stop {
            self.stop_recording();
        }
        if add {
            self.add_recording();
        }
        if save {
            self.save_recording();
        }
        if discard {
            self.take = None;
            self.record_status = ("Take discarded".into(), MUTED);
        }
    }

    /// Right-click menu for a pad: its volume, hotkey and removal.
    fn pad_menu(&mut self, ui: &mut egui::Ui, index: usize, remove: &mut Option<usize>) {
        let Some(pad) = self.settings.sounds.get(index).cloned() else { return };
        ui.label(RichText::new(pad.display_name()).strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Volume");
            let mut volume = pad.volume;
            if volume_slider(ui, &mut volume) {
                self.set_pad_volume(index, volume);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Hotkey");
            ui.monospace(pad.hotkey.as_deref().unwrap_or("none"));
            if ui.button("Set…").on_hover_text("Plays this pad from any app").clicked() {
                self.start_capture(Binding::Pad(pad.path.clone()));
                ui.close();
            }
            if pad.hotkey.is_some() && ui.button("Clear").clicked() {
                self.assign_hotkey(&Binding::Pad(pad.path.clone()), None);
            }
        });
        let mut starred = pad.starred;
        if ui.checkbox(&mut starred, "Show on the strip").changed() {
            self.toggle_star(index);
        }
        ui.separator();
        self.trim_editor(ui, index);
        ui.separator();
        if ui.button("Remove from soundboard").clicked() {
            *remove = Some(index);
            ui.close();
        }
    }

    /// A clip's full length in seconds: from its header, or from the
    /// decoded samples when the header doesn't say.
    fn pad_length(&mut self, path: &Path) -> Option<f32> {
        let from_header = *self.pad_durations.entry(path.to_owned()).or_insert_with(|| crate::decode::duration(path));
        from_header.or_else(|| self.clip_cache.get(path).map(|s| s.len() as f32 / crate::dsp::SR as f32))
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
        ui.label(RichText::new("Trim").strong());
        let samples = match self.clip_samples(&pad.path) {
            Ok(samples) => samples,
            Err(e) => {
                ui.colored_label(RED, short(&format!("{e:#}")));
                return;
            }
        };
        let length = samples.len() as f32 / crate::dsp::SR as f32;
        if self.trim_peaks.as_ref().is_none_or(|(path, _)| *path != pad.path) {
            self.trim_peaks = Some((pad.path.clone(), peaks(&samples, 240)));
        }
        let peaks = self.trim_peaks.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        ui.set_min_width(300.0);
        let (mut start, mut end) = (pad.start.clamp(0.0, length), pad.end.unwrap_or(length).clamp(0.0, length));

        // Drag near either edge of the kept part to move it.
        let wave = widgets::waveform(ui, &peaks, None, 44.0, CYAN);
        let rect = wave.rect.shrink2(egui::vec2(4.0, 0.0));
        let drag = ui.interact(wave.rect, ui.id().with(("trim", index)), egui::Sense::drag());
        let time_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * length;
        let x_of = |t: f32| rect.left() + t / length.max(1e-3) * rect.width();
        if let Some(pos) = drag.interact_pointer_pos() {
            let t = time_at(pos.x);
            let grab_start = ui.memory_mut(|m| {
                let id = ui.id().with(("trim grab", index));
                if drag.drag_started() {
                    m.data.insert_temp(id, (pos.x - x_of(start)).abs() <= (pos.x - x_of(end)).abs());
                }
                m.data.get_temp::<bool>(id).unwrap_or(true)
            });
            if grab_start {
                start = t.min(end - MIN_TRIM);
            } else {
                end = t.max(start + MIN_TRIM);
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

        ui.horizontal(|ui| {
            ui.label("Start");
            ui.add(egui::DragValue::new(&mut start).range(0.0..=(end - MIN_TRIM).max(0.0)).speed(0.01).suffix(" s").max_decimals(2));
            ui.label("End");
            ui.add(egui::DragValue::new(&mut end).range((start + MIN_TRIM).min(length)..=length).speed(0.01).suffix(" s").max_decimals(2));
            if (start > 0.0 || end < length) && ui.small_button("Reset").clicked() {
                (start, end) = (0.0, length);
            }
        });
        ui.weak(format!("Plays {} of {}", clock_seconds(end - start), clock_seconds(length)));

        let new_end = (end < length - 0.005).then_some(end);
        let new_start = if start < 0.005 { 0.0 } else { start };
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
        ui.weak("Click a pad to play, pause or resume. Use Add to strip to pin it.");
        if self.engine.is_none() && !self.settings.sounds.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(
                AMBER,
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
                CYAN,
                format!("Press the hotkey for {name}, or Esc to cancel. {}", self.hotkey_status.0),
            );
        }
        ui.add_space(12.0);

        let sounds = self.settings.sounds.clone();
        let mut add_header = false;
        let mut add_empty = false;
        let mut remove_index = None;
        widgets::card(
            ui,
            "SOUND PADS",
            CYAN,
            |ui| {
                if !sounds.is_empty() {
                    add_header = ui
                        .add(
                            egui::Button::new(RichText::new("Add clips").color(Color32::BLACK))
                                .fill(CYAN),
                        )
                        .clicked();
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
                                ui.weak("Add a few sounds, then click a pad to play one.");
                                ui.add_space(10.0);
                                add_empty = ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Add your first clips").color(Color32::BLACK),
                                        )
                                        .fill(CYAN),
                                    )
                                    .clicked();
                                ui.add_space(6.0);
                                ui.weak("WAV, FLAC, OGG, MP3, and AIFF");
                            });
                            ui.add_space(22.0);
                        });
                } else {
                    ui.horizontal(|ui| {
                        ui.weak("Click a name to rename; drag to reorder.");
                        // A search box once the board gets long.
                        if sounds.len() > SEARCH_FROM {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.pad_filter)
                                        .hint_text("Search clips")
                                        .desired_width(140.0),
                                );
                            });
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
                    ScrollArea::vertical()
                        .id_salt("sound_pads")
                        .max_height(258.0)
                        .show(ui, |ui| {
                            let gap = 8.0;
                            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                            let width = ((ui.available_width() - 2.0 * gap) / 3.0).max(120.0);
                            for row in shown.chunks(3) {
                                ui.horizontal(|ui| {
                                    for &i in row {
                                        let pad = &sounds[i];
                                        let key = clip_key(&pad.path);
                                        let length = self.pad_length(&pad.path);
                                        let state = PadState {
                                            playing: self.playing.contains(&key),
                                            progress: self.pad_progress.get(&key).copied(),
                                            length: length.map(|full| trimmed_length(full, pad.start, pad.end)),
                                            hotkey: pad.hotkey.as_deref(),
                                            starred: pad.starred,
                                            capturing: self.capturing
                                                == Some(Binding::Pad(pad.path.clone())),
                                            renaming: self.renaming_pad.as_ref().is_some_and(|edit| edit.path == pad.path),
                                        };
                                        let (play, remove, star, name) = clip_pad(ui, i, pad, width, state);
                                        if name.clicked() {
                                            self.begin_pad_rename(pad);
                                        } else if star.clicked() {
                                            self.toggle_star(i);
                                        } else if play.clicked() {
                                            self.toggle_clip(i);
                                        }
                                        if remove.clicked() {
                                            remove_index = Some(i);
                                        }
                                        self.draw_pad_rename(ui, &pad.path, name.rect);
                                        // Drag a pad onto another to move it there.
                                        play.dnd_set_drag_payload(i);
                                        if let Some(from) = play.dnd_release_payload::<usize>() {
                                            moved = Some((*from, i));
                                        }
                                        play.context_menu(|ui| self.pad_menu(ui, i, &mut remove_index));
                                    }
                                });
                            }
                        });
                    if let Some((from, to)) = moved {
                        self.move_pad(from, to);
                    }
                }
            },
        );
        if add_header || add_empty {
            self.add_sounds();
        }
        if let Some(index) = remove_index {
            self.remove_clip(index);
        }

        ui.add_space(10.0);
        let playing: Vec<String> = self
            .settings
            .sounds
            .iter()
            .filter(|pad| self.playing.contains(&clip_key(&pad.path)))
            .map(Pad::display_name)
            .collect();
        let engine_running = self.engine.is_some();
        let waiting_for_device = self.waiting_for_device;
        let paused = self.pad_progress.len().saturating_sub(playing.len());
        let title = match playing.len() {
            0 if self.settings.sounds.is_empty() => "Add a clip to get started".to_owned(),
            0 if paused == 1 => "1 clip paused".to_owned(),
            0 if paused > 1 => format!("{paused} clips paused"),
            0 => "Choose a pad above".to_owned(),
            1 => playing[0].clone(),
            n => format!("{n} clips playing"),
        };
        widgets::card(
            ui,
            "PLAYBACK",
            VIOLET,
            |ui| {
                if !playing.is_empty() {
                    widgets::badge(ui, "Playing", GREEN);
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
                ui.horizontal(|ui| {
                    let title_width = (ui.available_width() - 115.0).max(140.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(title_width);
                        ui.add(
                            egui::Label::new(RichText::new(title).size(15.0).strong()).truncate(),
                        );
                        ui.weak(if self.settings.sounds.is_empty() {
                            "WAV, FLAC, OGG, MP3, and AIFF supported"
                        } else {
                            "Click a pad to play, pause or resume."
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!self.pad_progress.is_empty(), egui::Button::new("Stop clips")).clicked() {
                            self.stop_clips();
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Clip volume");
                    if volume_slider(ui, &mut self.settings.sound_volume) {
                        self.apply_live();
                    }
                    ui.weak("Clips only");
                });
                if ui
                    .checkbox(&mut self.settings.overlap_clips, "Let clips overlap")
                    .on_hover_text("Play pads over each other instead of replacing the current clip")
                    .changed()
                {
                    self.touch();
                }
                if self.sound_status.0 != "Ready" && !self.sound_status.0.starts_with("Playing ·") {
                    ui.add_space(5.0);
                    ui.label(RichText::new(&self.sound_status.0).color(self.sound_status.1));
                }
            },
        );
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

/// "0:04" or "0:04.5" for short clips.
fn clock_seconds(seconds: f32) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{}:{:02}", (seconds / 60.0) as u32, seconds as u32 % 60)
    }
}

/// How long a pad plays with its trim points.
fn trimmed_length(full: f32, start: f32, end: Option<f32>) -> f32 {
    (end.unwrap_or(full).min(full) - start.max(0.0)).max(0.0)
}

/// Peak level in `buckets` even slices of `samples`, for a waveform.
fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    let per = samples.len().div_ceil(buckets.max(1)).max(1);
    samples.chunks(per).map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs()))).collect()
}

#[derive(Clone, Copy, Default)]
struct PadState<'a> {
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

fn clip_pad(
    ui: &mut egui::Ui,
    index: usize,
    pad: &Pad,
    width: f32,
    state: PadState,
) -> (egui::Response, egui::Response, egui::Response, egui::Response) {
    let playing = state.playing;
    let paused = !playing && state.progress.is_some();
    let action = if playing { "Pause" } else if paused { "Resume" } else { "Play" };
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
        ui.id().with(("sound_play", index)),
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
    let star_response = ui.interact(star_rect, ui.id().with(("sound_star", index)), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    star_response.widget_info(|| {
        let label = if state.starred { format!("Remove {name} from the strip") } else { format!("Add {name} to the strip") };
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), state.starred, label)
    });
    // Another pad is being dragged over this one: it will land here.
    let drop_here = play_response.dnd_hover_payload::<usize>().is_some_and(|from| *from != index);
    let remove_response = ui.interact(
        remove_rect,
        ui.id().with(("sound_remove", index)),
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

    let visuals = ui.visuals();
    let accent = if playing { GREEN } else if paused { AMBER } else { CYAN };
    let fill = if playing || paused {
        accent.gamma_multiply(0.13)
    } else if play_response.hovered() {
        CYAN.gamma_multiply(0.06)
    } else {
        visuals.faint_bg_color
    };
    let border = if drop_here {
        AMBER
    } else if playing || paused || play_response.hovered() || state.capturing {
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
    if playing {
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

    let max_chars = ((width - 106.0) / 7.0).max(7.0) as usize;
    let title = if name.chars().count() > max_chars {
        format!(
            "{}…",
            name.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        name.clone()
    };
    let text_area = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 52.0, rect.top() + 9.0),
        egui::pos2(rect.right() - 43.0, body.bottom() - 8.0),
    );
    let text_painter = painter.with_clip_rect(text_area);
    if !state.renaming {
        if name_response.hovered() || name_response.has_focus() {
            painter.rect_filled(name_rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
        }
        text_painter.text(
            egui::pos2(rect.left() + 52.0, rect.top() + 30.0),
            egui::Align2::LEFT_CENTER,
            title,
            egui::FontId::proportional(14.0),
            if name_response.hovered() || name_response.has_focus() { CYAN } else { visuals.text_color() },
        );
    }
    let format = match state.length {
        Some(length) => format!("{format} · {}", clock_seconds(length)),
        None => format,
    };
    let (detail, detail_color) = if state.capturing {
        ("Press a hotkey…".to_owned(), CYAN)
    } else if drop_here {
        ("Move here".to_owned(), AMBER)
    } else if playing {
        (format!("{format} · Playing"), GREEN)
    } else if paused {
        (format!("Paused · {format}"), AMBER)
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
    text_painter.text(
        egui::pos2(rect.left() + 52.0, rect.top() + 55.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::proportional(11.0),
        detail_color,
    );
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
            RED
        } else {
            visuals.weak_text_color()
        },
    );
    // A separate, labeled strip toggle below the playback hit target.
    let star_visuals = ui.style().interact(&star_response);
    let star_color = if state.starred { AMBER } else { star_visuals.text_color() };
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
    let play_response = play_response.on_hover_text(format!(
        "Click to {}, drag to reorder, right-click for volume, hotkey and trim\n{}",
        action.to_lowercase(),
        path.display()
    ));
    let remove_response = remove_response.on_hover_text(format!(
        "Remove {} from the soundboard. The file stays on disk.",
        file_name(path)
    ));
    let name_response = name_response.on_hover_text(format!("{name}\nClick to rename"));
    (play_response, remove_response, star_response, name_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

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
                    responses = Some(clip_pad(ui, 0, &pad, 240.0, PadState::default()));
                },
            );
            responses.unwrap()
        };
        let (play, remove, star, name) = frame(Vec::new());
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
            let (play, remove, star, name) = frame(vec![egui::Event::PointerButton {
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
    fn trim_points_pick_the_part_a_pad_plays() {
        let samples = Arc::new((0..48_000).map(|i| i as f32).collect::<Vec<_>>());
        assert!(Arc::ptr_eq(&crate::gui::trimmed(&samples, 0.0, None), &samples), "untrimmed pads share the clip");
        let part = crate::gui::trimmed(&samples, 0.25, Some(0.5));
        assert_eq!(part.len(), 12_000);
        assert_eq!(part[0], 12_000.0);
        assert!(crate::gui::trimmed(&samples, 2.0, None).is_empty(), "a start past the end plays nothing");
        assert_eq!(crate::gui::trimmed(&samples, 0.5, Some(0.25)).len(), 0, "an end before the start plays nothing");

        assert_eq!(trimmed_length(4.0, 1.0, Some(3.0)), 2.0);
        assert_eq!(trimmed_length(4.0, 0.0, None), 4.0);
        assert_eq!(clock_seconds(4.25), "4.2 s");
        assert_eq!(clock_seconds(75.0), "1:15");
        assert_eq!(peaks(&[0.1, -0.5, 0.2, 0.9], 2), [0.5, 0.9]);
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
        assert_eq!(mixer.progress(), [(7, 0.25, false)]);
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
        app.pad_progress.insert(key, 0.25);
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
        ] {
            let name = app.settings.sounds[0].display_name();
            let pos = pad_title_position(&mut app, &ctx, &name);
            click(&mut app, pos);
            assert!(app.renaming_pad.is_some(), "clicking the title starts an inline edit");
            board(&mut app, &ctx, vec![egui::Event::Text(text.into())]);
            if let Some(key) = finish {
                board(&mut app, &ctx, [true, false].into_iter().map(|pressed| egui::Event::Key {
                    key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                }).collect());
            } else {
                click(&mut app, egui::pos2(730.0, 1500.0));
            }
            assert!(app.renaming_pad.is_none());
            assert_eq!(app.settings.sounds[0].display_name(), expected);
            assert_eq!(app.sound_status.0, "Ready", "renaming never triggers play/pause");
            assert_eq!(app.playing, [key]);
            assert_eq!(app.pad_progress.get(&key), Some(&0.25));
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

