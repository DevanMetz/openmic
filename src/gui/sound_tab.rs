//! Soundboard tab: recording clips, the pads (with per-pad volume and
//! hotkeys), and playback.

use std::path::Path;

use eframe::egui::{self, Color32, ComboBox, RichText, ScrollArea};

use super::{
    clip_key, clock_time, file_name, short, volume_slider, App, Binding, AMBER, CYAN, GREEN,
    MUTED, RED, VIOLET,
};
use crate::config::Pad;
use crate::record::{self, Recorder, Source, PEAKS_PER_SECOND};
use crate::widgets;

impl App {
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
        let key = clip_key(&removed.path);
        if let Some(engine) = &self.engine {
            engine.stop_sound(key);
        }
        self.playing.retain(|&k| k != key);
        self.clip_cache.remove(&removed.path);
        if self.capturing == Some(Binding::Pad(removed.path.clone())) {
            self.capturing = None;
        }
        self.sound_status = (format!("Removed {}", file_name(&removed.path)), MUTED);
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
                        ui.label(RichText::new("●").color(RED.gamma_multiply(pulse)));
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
                            .add(egui::Button::new(RichText::new("●  Record").color(Color32::BLACK)).fill(RED))
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
        ui.label(RichText::new(pad_name(&pad.path)).strong());
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
        ui.separator();
        if ui.button("Remove from soundboard").clicked() {
            *remove = Some(index);
            ui.close();
        }
    }

    pub(super) fn draw_sound_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Your soundboard").size(22.0).strong());
        ui.weak("Click a pad to play it, or record a new clip below.");
        if self.engine.is_none() && !self.settings.sounds.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(
                AMBER,
                if self.waiting_for_device {
                    "Waiting for your audio device before clips can play."
                } else {
                    "Start OpenMic below before playing clips."
                },
            );
        }
        if let Some(Binding::Pad(path)) = &self.capturing {
            ui.add_space(6.0);
            ui.colored_label(
                CYAN,
                format!("Press the hotkey for {}, or Esc to cancel. {}", pad_name(path), self.hotkey_status.0),
            );
        }
        ui.add_space(12.0);

        self.draw_recording(ui);
        ui.add_space(10.0);

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
                    ui.weak("Click a pad to play or restart it. Right-click for its volume and hotkey.");
                    ui.add_space(8.0);
                    ScrollArea::vertical()
                        .id_salt("sound_pads")
                        .max_height(258.0)
                        .show(ui, |ui| {
                            let gap = 8.0;
                            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                            let width = ((ui.available_width() - 2.0 * gap) / 3.0).max(120.0);
                            for (row, pads) in sounds.chunks(3).enumerate() {
                                ui.horizontal(|ui| {
                                    for (column, pad) in pads.iter().enumerate() {
                                        let i = row * 3 + column;
                                        let state = PadState {
                                            playing: self.playing.contains(&clip_key(&pad.path)),
                                            hotkey: pad.hotkey.as_deref(),
                                            capturing: self.capturing
                                                == Some(Binding::Pad(pad.path.clone())),
                                        };
                                        let (play, remove) = clip_pad(ui, i, &pad.path, width, state);
                                        if play.clicked() {
                                            self.play_clip(i);
                                        }
                                        if remove.clicked() {
                                            remove_index = Some(i);
                                        }
                                        play.context_menu(|ui| self.pad_menu(ui, i, &mut remove_index));
                                    }
                                });
                            }
                        });
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
            .map(|pad| pad_name(&pad.path))
            .collect();
        let engine_running = self.engine.is_some();
        let waiting_for_device = self.waiting_for_device;
        let title = match playing.len() {
            0 if self.settings.sounds.is_empty() => "Add a clip to get started".to_owned(),
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
                            "Click a pad to play or restart it."
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_enabled(!playing.is_empty(), egui::Button::new("Stop clips")).clicked() {
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

/// A pad's display name: its file name without the extension.
fn pad_name(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name(path))
}

#[derive(Clone, Copy, Default)]
struct PadState<'a> {
    playing: bool,
    hotkey: Option<&'a str>,
    /// Waiting for the user to press this pad's new hotkey.
    capturing: bool,
}

fn clip_pad(
    ui: &mut egui::Ui,
    index: usize,
    path: &Path,
    width: f32,
    state: PadState,
) -> (egui::Response, egui::Response) {
    let playing = state.playing;
    let name = pad_name(path);
    let format = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_else(|| "AUDIO".into());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 78.0), egui::Sense::hover());
    let play_rect = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.right() - 36.0, rect.bottom()),
    );
    let remove_rect = egui::Rect::from_min_max(
        egui::pos2(rect.right() - 36.0, rect.top()),
        rect.max,
    );
    let play_response = ui.interact(
        play_rect,
        ui.id().with(("sound_play", index)),
        egui::Sense::click(),
    );
    let remove_response = ui.interact(
        remove_rect,
        ui.id().with(("sound_remove", index)),
        egui::Sense::click(),
    );
    play_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Play {name}"),
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
    let accent = if playing { GREEN } else { CYAN };
    let fill = if playing {
        GREEN.gamma_multiply(0.13)
    } else if play_response.hovered() {
        CYAN.gamma_multiply(0.06)
    } else {
        visuals.faint_bg_color
    };
    let border = if playing || play_response.hovered() || state.capturing {
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

    let center = egui::pos2(rect.left() + 28.0, rect.center().y);
    painter.circle_filled(center, 16.0, accent);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - 3.0, center.y - 6.0),
            egui::pos2(center.x + 7.0, center.y),
            egui::pos2(center.x - 3.0, center.y + 6.0),
        ],
        Color32::from_gray(18),
        egui::Stroke::NONE,
    ));

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
        egui::pos2(rect.right() - 43.0, rect.bottom() - 8.0),
    );
    let text_painter = painter.with_clip_rect(text_area);
    text_painter.text(
        egui::pos2(rect.left() + 52.0, rect.top() + 30.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(14.0),
        visuals.text_color(),
    );
    let (detail, detail_color) = if state.capturing {
        ("Press a hotkey…".to_owned(), CYAN)
    } else if playing {
        (format!("{format} · Playing"), GREEN)
    } else if let Some(hotkey) = state.hotkey {
        (format!("{format} · {hotkey}"), visuals.weak_text_color())
    } else {
        (format!("{format} · Play now"), visuals.weak_text_color())
    };
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
            egui::pos2(remove_rect.left(), rect.bottom() - 8.0),
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
    let play_response = play_response.on_hover_text(format!(
        "Click to play, right-click for volume and hotkey\n{}",
        path.display()
    ));
    let remove_response = remove_response.on_hover_text(format!(
        "Remove {} from the soundboard. The file stays on disk.",
        file_name(path)
    ));
    (play_response, remove_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sound_pad_play_and_remove_have_separate_hit_targets() {
        let ctx = egui::Context::default();
        let path = PathBuf::from("airhorn.mp3");
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
                    responses = Some(clip_pad(ui, 0, &path, 240.0, PadState::default()));
                },
            );
            responses.unwrap()
        };
        let (play, remove) = frame(Vec::new());
        assert!(play.rect.right() <= remove.rect.left());
        for (pos, expect_play) in [(play.rect.center(), true), (remove.rect.center(), false)] {
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            let (play, remove) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert_eq!(play.clicked(), expect_play);
            assert_eq!(remove.clicked(), !expect_play);
        }
    }
}
