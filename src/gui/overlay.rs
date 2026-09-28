//! The strip: OpenMic collapsed to a thin always-on-top bar for everyday
//! use. It shows what Discord hears (the microphone: click to mute), the
//! output level, the preset (click for the next one), your starred pads,
//! and a button to expand back to the full window. Drag it to move it and
//! its right edge to resize it; narrower, it drops pads and the preset,
//! then the bar, down to just the microphone.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId,
};

use super::{App, Command, AMBER, GREEN, MUTED, RED};
use crate::config;

pub(super) const HEIGHT: f32 = 34.0;
pub(super) const DEFAULT_WIDTH: f32 = 200.0;
/// Just the microphone.
const MIN_WIDTH: f32 = HEIGHT;
const MAX_WIDTH: f32 = 960.0;
/// Narrower than this, the level shows as a line under the microphone.
const BAR_MIN_WIDTH: f32 = 80.0;
/// Narrower than this, there's no room for the expand button.
const EXPAND_MIN_WIDTH: f32 = 110.0;
/// The level bar's width when the preset and pads share the strip.
const BAR_WIDTH: f32 = 96.0;
const PRESET_WIDTH: f32 = 100.0;
const PAD_WIDTH: f32 = 78.0;
const GAP: f32 = 6.0;
/// The right-edge strip that resizes it.
const GRIP: f32 = 6.0;
/// The bottom of the meter, dBFS.
const FLOOR: f32 = -60.0;
/// Output above this counts as your voice going out.
const SPEAKING_DB: f32 = -45.0;

/// What the meter shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Meter {
    Stopped,
    Muted,
    /// Processing, with the processed output's peak in dBFS.
    Live(f32),
}

/// What the strip shows besides the meter.
#[derive(Clone, Debug, Default)]
pub(super) struct Strip {
    pub preset: String,
    /// Starred pads: board index, name, playing.
    pub pads: Vec<(usize, String, bool)>,
}

/// What the user did on the strip this frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    ToggleMute,
    Expand,
    NextPreset,
    PlayPad(usize),
}

/// A width that fits the meter, the preset and `pads` starred pads.
pub(super) fn fitting_width(pads: usize) -> f32 {
    let pads = pads.min(6) as f32;
    (HEIGHT + BAR_WIDTH + GAP + PRESET_WIDTH + GAP + pads * (PAD_WIDTH + GAP) + 30.0 + GRIP + 4.0).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// Puts the strip where the user left it, once. Re-sending the
/// position every frame fights the user's drag, and across monitors with
/// different scaling it made the window jump between two spots each frame
/// (seen as a doubled meter). Positions are saved in screen pixels so they
/// mean the same on every monitor.
#[derive(Debug, Default)]
pub(super) struct Placement {
    pub(super) placed: bool,
    /// Frames since the window was placed.
    frames: u32,
}

/// Frames to wait after placing the window before trusting its reported
/// position and size (the move takes effect a frame or two later).
const SETTLE_FRAMES: u32 = 3;

impl Placement {
    /// Given this frame's window position (points) and scale, returns where
    /// to move the window (points) if it still needs placing, and whether its
    /// position and size can now be remembered.
    pub(super) fn step(&mut self, saved_px: Option<[i32; 2]>, outer: Option<Rect>, ppp: Option<f32>) -> (Option<Pos2>, bool) {
        let (Some(_), Some(ppp)) = (outer, ppp) else { return (None, false) };
        if !self.placed {
            self.placed = true;
            self.frames = 0;
            return (saved_px.map(|[x, y]| Pos2::new(x as f32 / ppp, y as f32 / ppp)), false);
        }
        self.frames = self.frames.saturating_add(1);
        (None, self.frames > SETTLE_FRAMES)
    }
}

impl App {
    pub(super) fn meter(&self) -> Meter {
        match &self.engine {
            None => Meter::Stopped,
            Some(_) if self.settings.mute => Meter::Muted,
            Some(engine) => Meter::Live(to_db(engine.stats().out_peak)),
        }
    }

    /// The current preset's name, or "Custom".
    pub(super) fn preset_name(&self) -> String {
        let current = self.settings.processing();
        config::builtin_presets()
            .iter()
            .chain(&self.settings.presets)
            .find(|p| p.processing == current)
            .map_or_else(|| "Custom".to_owned(), |p| p.name.clone())
    }

    /// Switch to the next preset (built-in, then saved), wrapping around.
    pub(super) fn next_preset(&mut self) {
        let presets: Vec<config::Preset> =
            config::builtin_presets().into_iter().chain(self.settings.presets.iter().cloned()).collect();
        let current = self.settings.processing();
        let next = presets
            .iter()
            .position(|p| p.processing == current)
            .map_or(0, |i| (i + 1) % presets.len());
        if let Some(preset) = presets.get(next) {
            self.settings.apply_processing(&preset.processing);
            self.apply_live();
        }
    }

    /// Show the strip while OpenMic is collapsed. Called every tick, so it
    /// keeps working with the main window hidden.
    pub(super) fn show_overlay(&mut self, ctx: &egui::Context) {
        if !self.settings.collapsed || self.strip_suppressed {
            self.overlay_placement = Placement::default();
            return;
        }
        if !self.overlay_placement.placed {
            let starred = self.settings.sounds.iter().filter(|p| p.starred).count();
            // Never resized: size it to fit what it shows.
            if self.settings.overlay_width == DEFAULT_WIDTH {
                self.settings.overlay_width = fitting_width(starred);
            }
            self.overlay_spawn_width = self.settings.overlay_width.clamp(MIN_WIDTH, MAX_WIDTH);
        }
        // Constant while shown: egui turns any change into a resize or move.
        let builder = ViewportBuilder::default()
            .with_title("OpenMic level")
            .with_inner_size([self.overlay_spawn_width, HEIGHT])
            .with_min_inner_size([MIN_WIDTH, HEIGHT])
            .with_max_inner_size([MAX_WIDTH, HEIGHT])
            .with_resizable(true)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false);
        let meter = self.meter();
        let strip = Strip {
            preset: self.preset_name(),
            pads: self
                .settings
                .sounds
                .iter()
                .enumerate()
                .filter(|(_, pad)| pad.starred)
                .map(|(i, pad)| (i, super::sound_tab::pad_name(&pad.path), self.playing.contains(&super::clip_key(&pad.path))))
                .collect(),
        };
        let saved = self.settings.overlay_position;
        let (placement, hold) = (&mut self.overlay_placement, &mut self.overlay_hold);
        let (actions, seen) =
            ctx.show_viewport_immediate(ViewportId::from_hash_of("openmic-level-meter"), builder, |ui, _| {
                let info = ui.ctx().input(|i| i.viewport().clone());
                let (place, record) = placement.step(saved, info.outer_rect, info.native_pixels_per_point);
                if let Some(pos) = place {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(pos));
                }
                let actions = draw(ui, meter, &strip, hold);
                let seen = record
                    .then(|| Some((info.outer_rect?.min, info.native_pixels_per_point?, info.inner_rect?.width())))
                    .flatten();
                (actions, seen)
            });

        // Remember where it was dragged to and how wide it was made.
        if let Some((pos, ppp, width)) = seen {
            let px = [(pos.x * ppp).round() as i32, (pos.y * ppp).round() as i32];
            if self.settings.overlay_position != Some(px) || (self.settings.overlay_width - width).abs() > 0.5 {
                self.settings.overlay_position = Some(px);
                self.settings.overlay_width = width;
                self.touch();
            }
        }
        self.overlay_on_top.tick(std::time::Instant::now(), foreground(), raise_meter);
        for action in actions {
            match action {
                Action::ToggleMute => self.run_command(ctx, Command::ToggleMute),
                Action::Expand => self.set_collapsed(ctx, false),
                Action::NextPreset => self.next_preset(),
                Action::PlayPad(index) => self.play_clip(index),
            }
        }
    }
}

/// Keeps the meter above other always-on-top windows. Windows raises the
/// taskbar again whenever another window comes to the front, so a meter
/// placed on the taskbar vanished under it when you clicked elsewhere.
/// Re-assert the meter's place when the foreground window changes, and
/// every couple of seconds in case something else rose above it.
#[derive(Debug, Default)]
pub(super) struct OnTop {
    foreground: Option<isize>,
    raised_at: Option<std::time::Instant>,
}

const RAISE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

impl OnTop {
    /// Calls `raise` when the foreground window changed or it's been a while.
    fn tick(&mut self, now: std::time::Instant, foreground: Option<isize>, raise: impl FnOnce()) {
        let changed = foreground != self.foreground;
        let due = self.raised_at.is_none_or(|at| now.duration_since(at) >= RAISE_EVERY);
        if changed || due {
            self.foreground = foreground;
            self.raised_at = Some(now);
            raise();
        }
    }
}

/// The window in front, as a handle value.
#[cfg(windows)]
fn foreground() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: no arguments; returns a handle or null.
    let window = unsafe { GetForegroundWindow() };
    (!window.is_invalid()).then_some(window.0 as isize)
}

#[cfg(not(windows))]
fn foreground() -> Option<isize> {
    None
}

/// Put the meter back at the top of the always-on-top windows, without
/// taking focus from whatever you're using.
#[cfg(windows)]
fn raise_meter() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetWindowThreadProcessId, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
        SWP_NOSIZE, SetWindowPos,
    };
    use windows::core::w;
    // SAFETY: plain Win32 calls on a window we check belongs to this process.
    unsafe {
        let Ok(meter) = FindWindowW(None, w!("OpenMic level")) else { return };
        let mut pid = 0;
        GetWindowThreadProcessId(meter, Some(&mut pid));
        if pid != std::process::id() {
            return;
        }
        let _ = SetWindowPos(
            meter,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

#[cfg(not(windows))]
fn raise_meter() {}

fn to_db(peak: f32) -> f32 {
    (20.0 * peak.max(1e-6).log10()).clamp(FLOOR, 0.0)
}

/// Draw the strip filling `ui`, and report clicks.
pub(super) fn draw(ui: &mut egui::Ui, meter: Meter, strip: &Strip, hold: &mut f32) -> Vec<Action> {
    let mut actions = Vec::new();
    let rect = ui.max_rect();
    let visuals = ui.visuals().clone();
    ui.painter().rect(
        rect,
        CornerRadius::ZERO,
        visuals.panel_fill,
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );

    // Dragging anywhere moves it; a double-click expands the window.
    let body = ui.interact(rect, ui.id().with("meter"), Sense::click_and_drag());
    if body.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if body.double_clicked() {
        actions.push(Action::Expand);
    }

    let db = match meter {
        Meter::Live(db) => db,
        _ => FLOOR,
    };
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    *hold = if db >= *hold { db } else { (*hold - 20.0 * dt).max(db) }; // falls 20 dB/s

    // The microphone mutes and unmutes; dragging it still moves the strip.
    let compact = rect.width() < BAR_MIN_WIDTH;
    let mic_rect = Rect::from_min_size(rect.min, Vec2::splat(rect.height()));
    let mic = ui.interact(mic_rect, ui.id().with("mic"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    if mic.clicked() {
        actions.push(Action::ToggleMute);
    }
    if mic.hovered() {
        ui.painter().rect_filled(mic_rect.shrink(3.0), CornerRadius::same(5), visuals.widgets.hovered.weak_bg_fill);
    }
    let dot = match meter {
        Meter::Stopped => MUTED.gamma_multiply(0.6),
        Meter::Muted => RED,
        Meter::Live(db) if db > SPEAKING_DB => GREEN,
        Meter::Live(_) => MUTED,
    };
    mic_glyph(ui.painter(), mic_rect.center(), dot);
    if meter == Meter::Muted {
        // A slash across the microphone.
        let r = mic_rect.shrink(8.0);
        ui.painter().line_segment([r.left_top(), r.right_bottom()], Stroke::new(2.0, RED));
    }

    let mut right = rect.right() - GRIP;
    if rect.width() >= EXPAND_MIN_WIDTH {
        let expand = Rect::from_center_size(Pos2::new(right - 14.0, rect.center().y), Vec2::new(24.0, 22.0));
        if ui.put(expand, egui::Button::new("⛶").small()).on_hover_text("Open the full window").clicked() {
            actions.push(Action::Expand);
        }
        right = expand.left() - GAP;
    }

    if compact {
        // Too narrow for the bar: the level runs along the bottom edge.
        if let Meter::Live(db) = meter {
            let line = Rect::from_min_max(
                Pos2::new(rect.left() + 3.0, rect.bottom() - 4.0),
                rect.right_bottom() - Vec2::new(3.0, 1.0),
            );
            level_bar(ui.painter(), line, db, *hold, visuals.extreme_bg_color);
        }
    } else {
        // The bar fills the strip unless there's room for the preset too.
        let room = right - mic_rect.right();
        let with_preset = room >= BAR_WIDTH + GAP + PRESET_WIDTH;
        let bar_right = if with_preset { mic_rect.right() + BAR_WIDTH } else { right.max(mic_rect.right() + 10.0) };
        let bar = Rect::from_min_max(
            Pos2::new(mic_rect.right(), rect.center().y - 5.0),
            Pos2::new(bar_right, rect.center().y + 5.0),
        );
        match meter {
            Meter::Stopped | Meter::Muted => {
                let (text, color) = if meter == Meter::Muted { ("Muted", RED) } else { ("Stopped", MUTED) };
                ui.painter().text(bar.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), color);
            }
            Meter::Live(db) => level_bar(ui.painter(), bar, db, *hold, visuals.extreme_bg_color),
        }
        if with_preset {
            let mut x = bar.right() + GAP;
            let preset = Rect::from_min_size(Pos2::new(x, rect.center().y - 11.0), Vec2::new(PRESET_WIDTH, 22.0));
            let label = egui::RichText::new(truncate(&strip.preset, 12)).small();
            if ui.put(preset, egui::Button::new(label)).on_hover_text("Next preset").clicked() {
                actions.push(Action::NextPreset);
            }
            x = preset.right() + GAP;
            // Starred pads, as many as fit.
            for (index, name, playing) in &strip.pads {
                if x + PAD_WIDTH > right {
                    break;
                }
                let pad = Rect::from_min_size(Pos2::new(x, rect.center().y - 11.0), Vec2::new(PAD_WIDTH, 22.0));
                let mut button = egui::Button::new(egui::RichText::new(truncate(name, 10)).small());
                if *playing {
                    button = button.fill(GREEN.gamma_multiply(0.35));
                }
                if ui.put(pad, button).clicked() {
                    actions.push(Action::PlayPad(*index));
                }
                x = pad.right() + GAP;
            }
        }
    }

    // The right edge resizes, down to just the microphone.
    let grip_rect = Rect::from_min_max(Pos2::new(rect.right() - GRIP, rect.top()), rect.right_bottom());
    let grip = ui.interact(grip_rect, ui.id().with("resize"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
    if grip.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(ResizeDirection::East));
    }
    if grip.hovered() || grip.dragged() {
        let x = grip_rect.center().x;
        let stroke = Stroke::new(1.0, visuals.weak_text_color());
        for dy in [-4.0, 0.0, 4.0] {
            ui.painter().line_segment(
                [Pos2::new(x, rect.center().y + dy - 1.5), Pos2::new(x, rect.center().y + dy + 1.5)],
                stroke,
            );
        }
    }
    actions
}

/// `text` cut to `max` characters with an ellipsis.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/// A small microphone: capsule, stand and base.
fn mic_glyph(painter: &egui::Painter, center: Pos2, color: Color32) {
    let capsule = Rect::from_center_size(center - Vec2::new(0.0, 3.0), Vec2::new(8.0, 13.0));
    painter.rect_filled(capsule, CornerRadius::same(4), color);
    let stroke = Stroke::new(1.6, color);
    let cup = [
        center + Vec2::new(-6.5, -2.0),
        center + Vec2::new(-6.0, 3.0),
        center + Vec2::new(-3.0, 6.5),
        center + Vec2::new(0.0, 7.2),
        center + Vec2::new(3.0, 6.5),
        center + Vec2::new(6.0, 3.0),
        center + Vec2::new(6.5, -2.0),
    ];
    painter.add(egui::Shape::line(cup.to_vec(), stroke));
    painter.line_segment([center + Vec2::new(0.0, 7.2), center + Vec2::new(0.0, 10.5)], stroke);
    painter.line_segment([center + Vec2::new(-4.0, 10.5), center + Vec2::new(4.0, 10.5)], stroke);
}

/// Green up to -18 dB, amber to -6 dB, red above, with a peak-hold tick.
fn level_bar(painter: &egui::Painter, rect: Rect, db: f32, hold: f32, track: Color32) {
    painter.rect_filled(rect, CornerRadius::same(5), track);
    let x = |d: f32| rect.left() + (d - FLOOR) / -FLOOR * rect.width();
    for (from, to, color) in [(FLOOR, -18.0, GREEN), (-18.0, -6.0, AMBER), (-6.0, 0.0, RED)] {
        if db > from {
            let segment = Rect::from_x_y_ranges(x(from)..=x(db.min(to)), rect.y_range());
            painter.rect_filled(segment, CornerRadius::same(5), color);
        }
    }
    if hold > FLOOR + 1.0 {
        let at = x(hold);
        painter.line_segment([Pos2::new(at, rect.top()), Pos2::new(at, rect.bottom())], Stroke::new(2.0, Color32::WHITE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn strip(pads: usize) -> Strip {
        Strip {
            preset: "Balanced".into(),
            pads: (0..pads).map(|i| (i, format!("pad{i}"), false)).collect(),
        }
    }

    /// Draw the strip `width` wide, with the pointer at `pointer` and
    /// optionally a click there; returns the text shown and the actions.
    fn run(meter: Meter, strip: &Strip, width: f32, pointer: Option<Pos2>, click: bool) -> (Vec<String>, Vec<Action>) {
        let ctx = egui::Context::default();
        let mut hold = FLOOR;
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT))),
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut texts = Vec::new();
        let mut frames: Vec<Vec<egui::Event>> = vec![pointer.map(egui::Event::PointerMoved).into_iter().collect()];
        if let (Some(at), true) = (pointer, click) {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frames.push(vec![button(true)]);
            frames.push(vec![button(false)]);
        }
        frames.push(Vec::new());
        for events in frames {
            let output = ctx.run_ui(input(events), |ui| actions.extend(draw(ui, meter, strip, &mut hold)));
            texts = output
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
        }
        (texts, actions)
    }

    #[test]
    fn the_strip_says_when_it_is_stopped_or_muted() {
        let s = strip(0);
        assert!(run(Meter::Stopped, &s, DEFAULT_WIDTH, None, false).0.contains(&"Stopped".to_owned()));
        assert!(run(Meter::Muted, &s, DEFAULT_WIDTH, None, false).0.contains(&"Muted".to_owned()));
        let (live, _) = run(Meter::Live(-20.0), &s, DEFAULT_WIDTH, None, false);
        assert!(!live.iter().any(|t| t == "Stopped" || t == "Muted"), "a live strip shows the bar: {live:?}");
    }

    #[test]
    fn clicking_the_microphone_mutes_and_unmutes() {
        let mic = Pos2::new(HEIGHT / 2.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &strip(0), DEFAULT_WIDTH, Some(mic), true).1, [Action::ToggleMute]);
        assert_eq!(run(Meter::Muted, &strip(0), DEFAULT_WIDTH, Some(mic), true).1, [Action::ToggleMute]);
    }

    #[test]
    fn the_expand_button_opens_the_window_when_there_is_room() {
        let expand = Pos2::new(DEFAULT_WIDTH - GRIP - 14.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &strip(0), DEFAULT_WIDTH, Some(expand), true).1, [Action::Expand]);
        let (texts, _) = run(Meter::Live(-20.0), &strip(0), 90.0, None, false);
        assert!(!texts.contains(&"⛶".to_owned()), "no room on a narrow strip");
    }

    #[test]
    fn a_wide_strip_shows_the_preset_and_starred_pads_that_fit() {
        let s = strip(3);
        let width = fitting_width(3);
        let (texts, _) = run(Meter::Live(-20.0), &s, width, None, false);
        for label in ["Balanced", "pad0", "pad1", "pad2"] {
            assert!(texts.contains(&label.to_owned()), "{label} missing: {texts:?}");
        }
        // Click the preset, then the second pad.
        let preset_x = HEIGHT + BAR_WIDTH + GAP + PRESET_WIDTH / 2.0;
        assert_eq!(run(Meter::Live(-20.0), &s, width, Some(Pos2::new(preset_x, 17.0)), true).1, [Action::NextPreset]);
        let pad1_x = HEIGHT + BAR_WIDTH + GAP + PRESET_WIDTH + GAP + PAD_WIDTH + GAP + PAD_WIDTH / 2.0;
        assert_eq!(run(Meter::Live(-20.0), &s, width, Some(Pos2::new(pad1_x, 17.0)), true).1, [Action::PlayPad(1)]);

        // Narrower: pads drop first, then the preset.
        let (texts, _) = run(Meter::Live(-20.0), &s, fitting_width(1), None, false);
        assert!(texts.contains(&"pad0".to_owned()) && !texts.contains(&"pad2".to_owned()), "{texts:?}");
        let (texts, _) = run(Meter::Live(-20.0), &s, DEFAULT_WIDTH, None, false);
        assert!(!texts.contains(&"Balanced".to_owned()) && !texts.contains(&"pad0".to_owned()), "{texts:?}");
    }

    #[test]
    fn a_strip_resized_to_the_icon_still_mutes_and_shows_no_text() {
        for meter in [Meter::Stopped, Meter::Muted, Meter::Live(-20.0)] {
            let (texts, _) = run(meter, &strip(3), MIN_WIDTH, None, false);
            assert!(texts.is_empty(), "{meter:?} at icon size shows only the icon: {texts:?}");
        }
        let center = Pos2::new(MIN_WIDTH / 2.0 - 3.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &strip(3), MIN_WIDTH, Some(center), true).1, [Action::ToggleMute]);
    }

    /// Press at `from`, move to `to`, release; returns the window commands
    /// sent and the actions reported.
    fn drag(from: Pos2, to: Pos2) -> (Vec<ViewportCommand>, Vec<Action>) {
        let ctx = egui::Context::default();
        let mut hold = FLOOR;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (mut commands, mut actions) = (Vec::new(), Vec::new());
        for events in [
            vec![egui::Event::PointerMoved(from)],
            vec![button(from, true)],
            vec![egui::Event::PointerMoved(to)],
            vec![button(to, false)],
        ] {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                events,
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| actions.extend(draw(ui, Meter::Live(-20.0), &strip(0), &mut hold)));
            commands.extend(output.viewport_output.into_values().flat_map(|v| v.commands));
        }
        (commands, actions)
    }

    #[test]
    fn dragging_moves_even_from_the_microphone_and_the_edge_resizes() {
        let (commands, actions) = drag(Pos2::new(17.0, 17.0), Pos2::new(60.0, 20.0));
        assert!(commands.contains(&ViewportCommand::StartDrag), "{commands:?}");
        assert!(actions.is_empty(), "a drag from the microphone doesn't mute: {actions:?}");

        let edge = Pos2::new(DEFAULT_WIDTH - GRIP / 2.0, 17.0);
        let (commands, _) = drag(edge, edge + Vec2::new(-80.0, 0.0));
        assert!(commands.contains(&ViewportCommand::BeginResize(ResizeDirection::East)), "{commands:?}");
        assert!(!commands.contains(&ViewportCommand::StartDrag), "the edge resizes rather than moves");
    }

    #[test]
    fn the_window_is_placed_once_in_screen_pixels_then_left_alone() {
        let outer = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT)));
        // Saved on the 100% monitor, now drawn at 150%: same pixels.
        let mut placement = Placement::default();
        assert_eq!(placement.step(Some([3900, 150]), outer, Some(1.5)), (Some(Pos2::new(2600.0, 100.0)), false));
        for frame in 1..=SETTLE_FRAMES {
            assert_eq!(placement.step(Some([3900, 150]), outer, Some(1.0)), (None, false), "frame {frame} settles");
        }
        for _ in 0..100 {
            let (place, record) = placement.step(Some([3900, 150]), outer, Some(1.5));
            assert_eq!(place, None, "never re-sent, whatever the scale");
            assert!(record);
        }
        assert_eq!(Placement::default().step(None, outer, Some(1.0)), (None, false));
        assert_eq!(Placement::default().step(Some([1, 1]), None, Some(1.0)), (None, false));
    }

    #[test]
    fn the_strip_is_raised_when_another_window_comes_to_the_front() {
        let mut on_top = OnTop::default();
        let start = std::time::Instant::now();
        let mut raised = 0;
        let mut tick = |at: std::time::Duration, foreground| on_top.tick(start + at, foreground, || raised += 1);
        tick(std::time::Duration::ZERO, Some(1));
        tick(std::time::Duration::from_millis(250), Some(1));
        tick(std::time::Duration::from_millis(500), Some(2)); // clicked another window
        tick(std::time::Duration::from_millis(750), Some(2));
        tick(std::time::Duration::from_millis(2600), Some(2)); // periodic
        assert_eq!(raised, 3);
    }

    #[test]
    fn peaks_map_to_the_meter_range() {
        assert_eq!(to_db(1.0), 0.0);
        assert!((to_db(0.1) + 20.0).abs() < 1e-4);
        assert_eq!(to_db(0.0), FLOOR, "silence sits at the floor");
        assert_eq!(to_db(2.0), 0.0, "clipping stays at the top");
        assert_eq!(truncate("airhorn remix", 7), "airhor…");
        assert_eq!(truncate("bruh", 7), "bruh");
    }

    #[test]
    fn presets_cycle_in_order_and_wrap() {
        let mut app = App::stopped(Settings::default());
        assert_eq!(app.preset_name(), "Balanced");
        let names: Vec<String> = (0..5)
            .map(|_| {
                app.next_preset();
                app.preset_name()
            })
            .collect();
        assert_eq!(names, ["Mechanical keyboard", "Quiet room", "Noisy room", "Low CPU", "Balanced"]);
        app.settings.strength = 0.33;
        assert_eq!(app.preset_name(), "Custom");
        app.next_preset();
        assert_eq!(app.preset_name(), "Balanced", "from custom settings, start at the first preset");
    }

    #[test]
    fn collapsing_swaps_the_window_for_the_strip_and_back() {
        use eframe::App as _;
        let mut app = App::stopped(Settings::default());
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut step = |app: &mut App| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(760.0, 880.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.logic(ui.ctx(), &mut frame);
                app.ui(ui, &mut frame);
            });
            output.viewport_output.into_values().flat_map(|v| v.commands).collect::<Vec<_>>()
        };
        step(&mut app);
        app.commands.0.send(Command::ToggleCollapse).unwrap();
        let commands = step(&mut app);
        assert!(app.settings.collapsed && app.hidden, "collapsed: the window hides");
        assert!(commands.contains(&ViewportCommand::Visible(false)), "{commands:?}");
        assert!(app.dirty_since.is_some(), "and it reopens collapsed");

        app.commands.0.send(Command::Show).unwrap();
        let commands = step(&mut app);
        assert!(!app.settings.collapsed && !app.hidden, "showing the window expands it");
        assert!(commands.contains(&ViewportCommand::Visible(true)), "{commands:?}");
    }

    #[test]
    fn windows_startup_keeps_a_collapsed_openmic_in_the_tray() {
        let mut app = App::stopped(Settings { collapsed: true, ..Settings::default() });
        app.strip_suppressed = true;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| app.show_overlay(ui.ctx()));
        assert!(!app.overlay_placement.placed, "no strip until you open OpenMic");
    }
}

