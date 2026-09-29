//! The strip: OpenMic collapsed to a thin always-on-top bar for everyday
//! use. The microphone shows what Discord hears (green while your voice goes
//! out, red when muted; click it to mute), then your starred pads, the
//! preset (click for the next one) and a button to expand back to the full
//! window. Buttons are as wide as their names, and pads come first: when the
//! strip is too narrow, the preset goes before any pad does. Drag the strip
//! to move it and its right edge to resize it, down to just the microphone.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId,
};

use super::{App, Command, GREEN, MUTED, RED};
use crate::config;

pub(super) const HEIGHT: f32 = 34.0;
pub(super) const DEFAULT_WIDTH: f32 = 200.0;
/// Just the microphone.
const MIN_WIDTH: f32 = HEIGHT;
const MAX_WIDTH: f32 = 1200.0;
/// The expand button, and the narrowest strip that has room for it.
const EXPAND_WIDTH: f32 = 24.0;
const EXPAND_MIN_WIDTH: f32 = HEIGHT + EXPAND_WIDTH + 10.0;
const GAP: f32 = 5.0;
/// Space either side of a button's text.
const BUTTON_PADDING: f32 = 8.0;
/// Names longer than this are cut short.
const MAX_NAME: usize = 18;
/// The right-edge strip that resizes it.
const GRIP: f32 = 6.0;
/// Output above this counts as your voice going out.
const SPEAKING_DB: f32 = -45.0;

fn label_font() -> FontId {
    FontId::proportional(12.0)
}

/// What the microphone shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Meter {
    Stopped,
    Muted,
    /// Processing, with the processed output's peak in dBFS.
    Live(f32),
}

/// What the strip shows besides the microphone.
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

/// Where the buttons go between `left` and `right`, given each pad's and
/// the preset's width: pads in order while they fit, then the preset only
/// if every pad fit and there's room. Returns each placed pad's (position
/// in `pads`, x) and the preset's x.
fn layout(left: f32, right: f32, pads: &[f32], preset: f32) -> (Vec<(usize, f32)>, Option<f32>) {
    let mut x = left;
    let mut placed = Vec::new();
    for (i, &width) in pads.iter().enumerate() {
        if x + width > right {
            return (placed, None);
        }
        placed.push((i, x));
        x += width + GAP;
    }
    let preset = (x + preset <= right).then_some(x);
    (placed, preset)
}

/// How wide a button for `text` is.
fn button_width(text_width: f32) -> f32 {
    text_width + 2.0 * BUTTON_PADDING
}

/// A strip just wide enough for the microphone, every button and expand.
fn fitting_width(pads: &[f32], preset: f32) -> f32 {
    let buttons: f32 = pads.iter().map(|w| w + GAP).sum::<f32>() + preset;
    (HEIGHT + GAP + buttons + GAP + EXPAND_WIDTH + GRIP + 6.0).clamp(MIN_WIDTH, MAX_WIDTH)
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

    fn strip_contents(&self) -> Strip {
        Strip {
            preset: self.preset_name(),
            pads: self
                .settings
                .sounds
                .iter()
                .enumerate()
                .filter(|(_, pad)| pad.starred)
                .map(|(i, pad)| (i, pad.display_name(), self.playing.contains(&super::clip_key(&pad.path))))
                .collect(),
        }
    }

    pub(super) fn strip_visible(&self) -> bool {
        (self.settings.collapsed || self.settings.always_show_strip) && !self.strip_suppressed
    }

    /// Show the strip while collapsed or always shown. Called every tick, so it
    /// keeps working with the main window hidden.
    pub(super) fn show_overlay(&mut self, ctx: &egui::Context) {
        if !self.strip_visible() {
            self.overlay_placement = Placement::default();
            return;
        }
        let strip = self.strip_contents();
        if !self.overlay_placement.placed {
            // Sized to fit what it shows, unless resized by hand.
            let width = self.settings.strip_width.unwrap_or_else(|| {
                let measure = |text: &str| button_width(text_width(ctx, text));
                let pads: Vec<f32> = strip.pads.iter().map(|(_, name, _)| measure(&truncate(name, MAX_NAME))).collect();
                fitting_width(&pads, measure(&truncate(&strip.preset, MAX_NAME)))
            });
            self.overlay_spawn_width = width.clamp(MIN_WIDTH, MAX_WIDTH);
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
        let saved = self.settings.overlay_position;
        let placement = &mut self.overlay_placement;
        let (actions, seen) =
            ctx.show_viewport_immediate(ViewportId::from_hash_of("openmic-level-meter"), builder, |ui, _| {
                let info = ui.ctx().input(|i| i.viewport().clone());
                let (place, record) = placement.step(saved, info.outer_rect, info.native_pixels_per_point);
                if let Some(pos) = place {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(pos));
                }
                let actions = draw(ui, meter, &strip);
                let seen = record
                    .then(|| Some((info.outer_rect?.min, info.native_pixels_per_point?, info.inner_rect?.width())))
                    .flatten();
                (actions, seen)
            });

        // Remember where it was dragged to, and a width set by hand.
        if let Some((pos, ppp, width)) = seen {
            let px = [(pos.x * ppp).round() as i32, (pos.y * ppp).round() as i32];
            if self.settings.overlay_position != Some(px) {
                self.settings.overlay_position = Some(px);
                self.touch();
            }
            let resized = (width - self.overlay_spawn_width).abs() > 1.0;
            if resized && self.settings.strip_width.is_none_or(|w| (w - width).abs() > 0.5) {
                self.settings.strip_width = Some(width);
                self.touch();
            }
        }
        self.overlay_on_top.tick(std::time::Instant::now(), foreground(), raise_meter);
        for action in actions {
            match action {
                Action::ToggleMute => self.run_command(ctx, Command::ToggleMute),
                Action::Expand => self.set_collapsed(ctx, false),
                Action::NextPreset => self.next_preset(),
                Action::PlayPad(index) => self.toggle_clip(index),
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
    (20.0 * peak.max(1e-6).log10()).clamp(-60.0, 0.0)
}

fn text_width(ctx: &egui::Context, text: &str) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), label_font(), Color32::WHITE).size().x)
}

/// Draw the strip filling `ui`, and report clicks.
pub(super) fn draw(ui: &mut egui::Ui, meter: Meter, strip: &Strip) -> Vec<Action> {
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

    // The microphone: green while your voice goes out, red and slashed when
    // muted, grey when stopped. Click it to mute or unmute; dragging it
    // still moves the strip.
    let mic_rect = Rect::from_min_size(rect.min, Vec2::splat(rect.height()));
    let mic = ui.interact(mic_rect, ui.id().with("mic"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    if mic.clicked() {
        actions.push(Action::ToggleMute);
    }
    if mic.hovered() {
        ui.painter().rect_filled(mic_rect.shrink(3.0), CornerRadius::same(5), visuals.widgets.hovered.weak_bg_fill);
    }
    let (color, speaking) = match meter {
        Meter::Stopped => (MUTED.gamma_multiply(0.6), false),
        Meter::Muted => (RED, false),
        Meter::Live(db) if db > SPEAKING_DB => (GREEN, true),
        Meter::Live(_) => (visuals.text_color(), false),
    };
    if speaking {
        ui.painter().circle_filled(mic_rect.center(), 14.0, GREEN.gamma_multiply(0.18));
    }
    mic_glyph(ui.painter(), mic_rect.center(), color);
    if meter == Meter::Muted {
        let r = mic_rect.shrink(8.0);
        ui.painter().line_segment([r.left_top(), r.right_bottom()], Stroke::new(2.0, RED));
    }

    let mut right = rect.right() - GRIP;
    if rect.width() >= EXPAND_MIN_WIDTH {
        let expand = Rect::from_center_size(Pos2::new(right - EXPAND_WIDTH / 2.0 - 2.0, rect.center().y), Vec2::new(EXPAND_WIDTH, 22.0));
        if ui.put(expand, egui::Button::new("⛶").small()).on_hover_text("Open the full window").clicked() {
            actions.push(Action::Expand);
        }
        right = expand.left() - GAP;
    }

    // Pads first, as wide as their names; the preset after them if there's room.
    let names: Vec<String> = strip.pads.iter().map(|(_, name, _)| truncate(name, MAX_NAME)).collect();
    let preset_name = truncate(&strip.preset, MAX_NAME);
    let measure = |text: &str| button_width(ui.painter().layout_no_wrap(text.to_owned(), label_font(), visuals.text_color()).size().x);
    let widths: Vec<f32> = names.iter().map(|n| measure(n)).collect();
    let preset_width = measure(&preset_name);
    let (placed, preset_x) = layout(mic_rect.right() + GAP, right, &widths, preset_width);
    let button_rect = |x: f32, width: f32| Rect::from_min_size(Pos2::new(x, rect.center().y - 11.0), Vec2::new(width, 22.0));
    for (i, x) in placed {
        let (index, _, playing) = &strip.pads[i];
        let mut button = egui::Button::new(egui::RichText::new(&names[i]).font(label_font()));
        if *playing {
            button = button.fill(GREEN.gamma_multiply(0.35));
        }
        if ui.put(button_rect(x, widths[i]), button).clicked() {
            actions.push(Action::PlayPad(*index));
        }
    }
    if let Some(x) = preset_x {
        let label = egui::RichText::new(&preset_name).font(label_font()).color(visuals.weak_text_color());
        if ui.put(button_rect(x, preset_width), egui::Button::new(label).frame(false)).on_hover_text("Next preset").clicked() {
            actions.push(Action::NextPreset);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    fn strip(names: &[&str]) -> Strip {
        Strip {
            preset: "Balanced".into(),
            pads: names.iter().enumerate().map(|(i, n)| (i, n.to_string(), false)).collect(),
        }
    }

    /// Draw the strip `width` wide, with the pointer at `pointer` and
    /// optionally a click there; returns the text shown (with where) and
    /// the actions.
    fn run(meter: Meter, strip: &Strip, width: f32, pointer: Option<Pos2>, click: bool) -> (Vec<(String, Rect)>, Vec<Action>) {
        let ctx = egui::Context::default();
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
            let output = ctx.run_ui(input(events), |ui| actions.extend(draw(ui, meter, strip)));
            texts = output
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect())),
                    _ => None,
                })
                .collect();
        }
        (texts, actions)
    }

    fn names(texts: &[(String, Rect)]) -> Vec<&str> {
        texts.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn pads_come_first_and_the_preset_only_if_every_pad_fits() {
        // Room for everything.
        let (pads, preset) = layout(0.0, 300.0, &[50.0, 60.0], 70.0);
        assert_eq!(pads, [(0, 0.0), (1, 50.0 + GAP)]);
        assert_eq!(preset, Some(110.0 + 2.0 * GAP));
        // The preset goes first...
        let (pads, preset) = layout(0.0, 140.0, &[50.0, 60.0], 70.0);
        assert_eq!((pads.len(), preset), (2, None));
        // ...then pads that don't fit, in order.
        let (pads, preset) = layout(0.0, 80.0, &[50.0, 60.0], 70.0);
        assert_eq!((pads, preset), (vec![(0, 0.0)], None));
        // No pads: just the preset.
        assert_eq!(layout(10.0, 100.0, &[], 70.0), (vec![], Some(10.0)));
    }

    #[test]
    fn there_is_no_level_meter_or_status_text() {
        for meter in [Meter::Stopped, Meter::Muted, Meter::Live(-20.0), Meter::Live(-80.0)] {
            let (texts, _) = run(meter, &strip(&[]), DEFAULT_WIDTH, None, false);
            let shown = names(&texts);
            assert!(!shown.iter().any(|t| *t == "Stopped" || *t == "Muted"), "{meter:?}: {shown:?}");
        }
    }

    #[test]
    fn pad_buttons_are_as_wide_as_their_names() {
        let s = strip(&["hi", "a much longer name"]);
        let (texts, _) = run(Meter::Live(-20.0), &s, 600.0, None, false);
        let short = texts.iter().find(|(t, _)| t == "hi").unwrap().1;
        let long = texts.iter().find(|(t, _)| t == "a much longer name").unwrap().1;
        assert!(long.width() > 3.0 * short.width());
        // The second pad starts right after the first one's short button.
        assert!(long.left() < HEIGHT + GAP + button_width(short.width()) + GAP + BUTTON_PADDING + 4.0, "{short:?} {long:?}");
        let (texts, _) = run(Meter::Live(-20.0), &strip(&["an extremely long clip name indeed"]), 600.0, None, false);
        assert!(names(&texts).contains(&"an extremely long…"), "long names are cut: {:?}", names(&texts));
    }

    #[test]
    fn a_narrow_strip_keeps_pads_and_drops_the_preset() {
        let s = strip(&["horn", "rim", "bruh"]);
        let (texts, _) = run(Meter::Live(-20.0), &s, 600.0, None, false);
        for label in ["horn", "rim", "bruh", "Balanced"] {
            assert!(names(&texts).contains(&label), "{label}: {:?}", names(&texts));
        }
        // Too narrow for everything: the preset goes before any pad...
        let (texts, _) = run(Meter::Live(-20.0), &s, 220.0, None, false);
        let shown = names(&texts);
        assert!(shown.contains(&"horn") && shown.contains(&"rim") && !shown.contains(&"Balanced"), "{shown:?}");
        // ...then pads drop from the end.
        let (texts, _) = run(Meter::Live(-20.0), &s, 130.0, None, false);
        let shown = names(&texts);
        assert!(shown.contains(&"horn") && !shown.contains(&"bruh"), "{shown:?}");
    }

    #[test]
    fn clicks_mute_play_pads_cycle_presets_and_expand() {
        let s = strip(&["horn", "rim"]);
        let mic = Pos2::new(HEIGHT / 2.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &s, 600.0, Some(mic), true).1, [Action::ToggleMute]);
        assert_eq!(run(Meter::Muted, &s, 600.0, Some(mic), true).1, [Action::ToggleMute]);

        let (texts, _) = run(Meter::Live(-20.0), &s, 600.0, None, false);
        let at = |label: &str| texts.iter().find(|(t, _)| t == label).unwrap().1.center();
        assert_eq!(run(Meter::Live(-20.0), &s, 600.0, Some(at("rim")), true).1, [Action::PlayPad(1)]);
        assert_eq!(run(Meter::Live(-20.0), &s, 600.0, Some(at("Balanced")), true).1, [Action::NextPreset]);
        let expand = Pos2::new(600.0 - GRIP - EXPAND_WIDTH / 2.0 - 2.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &s, 600.0, Some(expand), true).1, [Action::Expand]);
    }

    #[test]
    fn a_strip_resized_to_the_icon_still_mutes_and_shows_no_text() {
        for meter in [Meter::Stopped, Meter::Muted, Meter::Live(-20.0)] {
            let (texts, _) = run(meter, &strip(&["horn"]), MIN_WIDTH, None, false);
            assert!(texts.is_empty(), "{meter:?} at icon size shows only the icon: {texts:?}");
        }
        let center = Pos2::new(MIN_WIDTH / 2.0 - 3.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), &strip(&["horn"]), MIN_WIDTH, Some(center), true).1, [Action::ToggleMute]);
    }

    #[test]
    fn a_fitted_strip_shows_everything_it_holds() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let s = strip(&["airhorn", "rimshot", "bruh"]);
        let measure = |t: &str| button_width(text_width(&ctx, t));
        let pads: Vec<f32> = s.pads.iter().map(|(_, n, _)| measure(n)).collect();
        let width = fitting_width(&pads, measure(&s.preset));
        let (texts, _) = run(Meter::Live(-20.0), &s, width, None, false);
        for label in ["airhorn", "rimshot", "bruh", "Balanced", "⛶"] {
            assert!(names(&texts).contains(&label), "{label} doesn't fit in {width}: {:?}", names(&texts));
        }
    }

    /// Press at `from`, move to `to`, release; returns the window commands
    /// sent and the actions reported.
    fn drag(from: Pos2, to: Pos2) -> (Vec<ViewportCommand>, Vec<Action>) {
        let ctx = egui::Context::default();
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
            let output = ctx.run_ui(input, |ui| actions.extend(draw(ui, Meter::Live(-20.0), &strip(&[]))));
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
        assert_eq!(to_db(0.0), -60.0, "silence sits at the floor");
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
    fn opening_the_full_window_keeps_the_strip_when_enabled() {
        use eframe::App as _;
        for always_show_strip in [true, false] {
            let mut app = App::stopped(Settings { always_show_strip, ..Settings::default() });
            let ctx = egui::Context::default();
            let mut frame = eframe::Frame::_new_kittest();
            let mut step = |app: &mut App| {
                let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(760.0, 880.0));
                let mut input = egui::RawInput { screen_rect: Some(rect), ..Default::default() };
                let info = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
                info.outer_rect = Some(rect);
                info.native_pixels_per_point = Some(1.0);
                let output = ctx.run_ui(input, |ui| {
                    app.logic(ui.ctx(), &mut frame);
                    app.ui(ui, &mut frame);
                });
                output.viewport_output.into_values().flat_map(|v| v.commands).collect::<Vec<_>>()
            };
            step(&mut app);
            assert_eq!(app.overlay_placement.placed, always_show_strip, "strip visibility on launch");
            app.commands.0.send(Command::ToggleCollapse).unwrap();
            let commands = step(&mut app);
            assert!(app.settings.collapsed && app.hidden, "collapsed: the window hides");
            assert!(commands.contains(&ViewportCommand::Visible(false)), "{commands:?}");
            assert!(app.overlay_placement.placed, "collapsing always shows the strip");
            assert!(app.dirty_since.is_some(), "and it reopens collapsed");

            // The strip's expand button restores the main window.
            let placed_frames = app.overlay_placement.frames;
            app.set_collapsed(&ctx, false);
            let commands = step(&mut app);
            assert!(!app.settings.collapsed && !app.hidden, "showing the window expands it");
            assert!(commands.contains(&ViewportCommand::Visible(true)), "{commands:?}");
            assert_eq!(app.overlay_placement.placed, always_show_strip, "strip visibility after expanding");
            if always_show_strip {
                assert!(app.overlay_placement.frames > placed_frames, "the strip stays in place");
            }

            app.settings.always_show_strip = !always_show_strip;
            step(&mut app);
            assert_eq!(app.overlay_placement.placed, !always_show_strip, "changing the setting takes effect immediately");
        }
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

