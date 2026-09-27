//! A level meter you can pin to the screen: a small always-on-top window
//! showing what Discord hears, so you can see your voice going out (or
//! being muted) over a game or another app. Click the microphone to mute,
//! drag the meter to move it, drag its right edge to resize it (down to just
//! the microphone), and double-click it to open OpenMic.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId,
};

use super::{App, Command, AMBER, GREEN, MUTED, RED};

pub(super) const HEIGHT: f32 = 34.0;
pub(super) const DEFAULT_WIDTH: f32 = 200.0;
/// Just the microphone.
const MIN_WIDTH: f32 = HEIGHT;
const MAX_WIDTH: f32 = 480.0;
/// Narrower than this, the level shows as a strip under the microphone.
const BAR_MIN_WIDTH: f32 = 80.0;
/// Narrower than this, there's no room for the unpin button.
const CLOSE_MIN_WIDTH: f32 = 110.0;
/// The right-edge strip that resizes the meter.
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

/// What the user did on the meter this frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    ToggleMute,
    Hide,
    OpenApp,
}

/// Puts the pinned window where the user left it, once. Re-sending the
/// position every frame fights the user's drag, and across monitors with
/// different scaling it made the window jump between two spots each frame
/// (seen as a doubled meter). Positions are saved in screen pixels so they
/// mean the same on every monitor.
#[derive(Debug, Default)]
pub(super) struct Placement {
    placed: bool,
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
    fn step(&mut self, saved_px: Option<[i32; 2]>, outer: Option<Rect>, ppp: Option<f32>) -> (Option<Pos2>, bool) {
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

    /// Show the pinned meter, if it's switched on. Called every tick, so it
    /// keeps working while the main window is hidden in the tray.
    pub(super) fn show_overlay(&mut self, ctx: &egui::Context) {
        if !self.settings.overlay {
            self.overlay_placement = Placement::default();
            return;
        }
        if !self.overlay_placement.placed {
            self.overlay_spawn_width = self.settings.overlay_width.clamp(MIN_WIDTH, MAX_WIDTH);
        }
        // Constant while pinned: egui turns any change into a resize or move.
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
        let (placement, hold) = (&mut self.overlay_placement, &mut self.overlay_hold);
        let (actions, seen) =
            ctx.show_viewport_immediate(ViewportId::from_hash_of("openmic-level-meter"), builder, |ui, _| {
                let info = ui.ctx().input(|i| i.viewport().clone());
                let (place, record) = placement.step(saved, info.outer_rect, info.native_pixels_per_point);
                if let Some(pos) = place {
                    ui.ctx().send_viewport_cmd(ViewportCommand::OuterPosition(pos));
                }
                let actions = draw(ui, meter, hold);
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
        for action in actions {
            match action {
                Action::ToggleMute => self.run_command(ctx, Command::ToggleMute),
                Action::Hide => self.run_command(ctx, Command::ToggleOverlay),
                Action::OpenApp => self.run_command(ctx, Command::Show),
            }
        }
    }
}

fn to_db(peak: f32) -> f32 {
    (20.0 * peak.max(1e-6).log10()).clamp(FLOOR, 0.0)
}

/// Draw the meter filling `ui`, and report clicks.
pub(super) fn draw(ui: &mut egui::Ui, meter: Meter, hold: &mut f32) -> Vec<Action> {
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

    // Dragging anywhere moves the window; a double-click opens OpenMic.
    let body = ui.interact(rect, ui.id().with("meter"), Sense::click_and_drag());
    if body.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if body.double_clicked() {
        actions.push(Action::OpenApp);
    }
    let hovered = ui.rect_contains_pointer(rect);

    let db = match meter {
        Meter::Live(db) => db,
        _ => FLOOR,
    };
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    *hold = if db >= *hold { db } else { (*hold - 20.0 * dt).max(db) }; // falls 20 dB/s

    // The microphone mutes and unmutes; dragging it still moves the window.
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

    let show_close = hovered && rect.width() >= CLOSE_MIN_WIDTH;
    if compact {
        // Too narrow for the bar: the level runs along the bottom edge.
        if let Meter::Live(db) = meter {
            let strip =
                Rect::from_min_max(Pos2::new(rect.left() + 3.0, rect.bottom() - 4.0), rect.right_bottom() - Vec2::new(3.0, 1.0));
            level_bar(ui.painter(), strip, db, *hold, visuals.extreme_bg_color);
        }
    } else {
        let right = rect.right() - GRIP - 4.0 - if show_close { 28.0 } else { 0.0 };
        let bar = Rect::from_min_max(
            Pos2::new(mic_rect.right(), rect.center().y - 5.0),
            Pos2::new(right.max(mic_rect.right() + 10.0), rect.center().y + 5.0),
        );
        match meter {
            Meter::Stopped | Meter::Muted => {
                let (text, color) = if meter == Meter::Muted { ("Muted", RED) } else { ("Stopped", MUTED) };
                ui.painter().text(bar.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(13.0), color);
            }
            Meter::Live(db) => level_bar(ui.painter(), bar, db, *hold, visuals.extreme_bg_color),
        }
    }
    if show_close {
        let close = Rect::from_center_size(Pos2::new(rect.right() - GRIP - 15.0, rect.center().y), Vec2::new(22.0, 20.0));
        if ui.put(close, egui::Button::new("×").small()).clicked() {
            actions.push(Action::Hide);
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

    /// Draw the meter `width` wide, with the pointer at `pointer` and
    /// optionally a click there; returns the text shown and the actions.
    fn run(meter: Meter, width: f32, pointer: Option<Pos2>, click: bool) -> (Vec<String>, Vec<Action>) {
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
            let output = ctx.run_ui(input(events), |ui| actions.extend(draw(ui, meter, &mut hold)));
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
    fn the_meter_says_when_it_is_stopped_or_muted() {
        let wide = DEFAULT_WIDTH;
        assert!(run(Meter::Stopped, wide, None, false).0.contains(&"Stopped".to_owned()));
        assert!(run(Meter::Muted, wide, None, false).0.contains(&"Muted".to_owned()));
        let (live, _) = run(Meter::Live(-20.0), wide, None, false);
        assert!(!live.iter().any(|t| t == "Stopped" || t == "Muted"), "a live meter shows the bar: {live:?}");
    }

    #[test]
    fn clicking_the_microphone_mutes_and_there_is_no_mute_button() {
        let mic = Pos2::new(HEIGHT / 2.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), DEFAULT_WIDTH, Some(mic), true).1, [Action::ToggleMute]);
        assert_eq!(run(Meter::Muted, DEFAULT_WIDTH, Some(mic), true).1, [Action::ToggleMute], "and unmutes");
        let (texts, _) = run(Meter::Live(-20.0), DEFAULT_WIDTH, Some(Pos2::new(100.0, 17.0)), false);
        assert!(!texts.iter().any(|t| t.contains("Mute")), "{texts:?}");
    }

    #[test]
    fn the_unpin_button_shows_on_hover_when_there_is_room() {
        let (texts, _) = run(Meter::Live(-20.0), DEFAULT_WIDTH, None, false);
        assert!(!texts.contains(&"×".to_owned()), "hidden until hovered");
        let close = Pos2::new(DEFAULT_WIDTH - GRIP - 15.0, 17.0);
        assert_eq!(run(Meter::Live(-20.0), DEFAULT_WIDTH, Some(close), true).1, [Action::Hide]);
        let (texts, _) = run(Meter::Live(-20.0), 90.0, Some(Pos2::new(60.0, 17.0)), false);
        assert!(!texts.contains(&"×".to_owned()), "no room on a narrow meter");
    }

    #[test]
    fn a_meter_resized_to_the_icon_still_mutes_and_shows_no_text() {
        for meter in [Meter::Stopped, Meter::Muted, Meter::Live(-20.0)] {
            let (texts, _) = run(meter, MIN_WIDTH, None, false);
            assert!(texts.is_empty(), "{meter:?} at icon size shows only the icon: {texts:?}");
        }
        let center = Pos2::new(MIN_WIDTH / 2.0 - 3.0, HEIGHT / 2.0);
        assert_eq!(run(Meter::Live(-20.0), MIN_WIDTH, Some(center), true).1, [Action::ToggleMute]);
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
            let output = ctx.run_ui(input, |ui| actions.extend(draw(ui, Meter::Live(-20.0), &mut hold)));
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
        // Nothing saved yet: leave the window where Windows put it.
        assert_eq!(Placement::default().step(None, outer, Some(1.0)), (None, false));
        // No window yet: wait.
        assert_eq!(Placement::default().step(Some([1, 1]), None, Some(1.0)), (None, false));
    }

    #[test]
    fn peaks_map_to_the_meter_range() {
        assert_eq!(to_db(1.0), 0.0);
        assert!((to_db(0.1) + 20.0).abs() < 1e-4);
        assert_eq!(to_db(0.0), FLOOR, "silence sits at the floor");
        assert_eq!(to_db(2.0), 0.0, "clipping stays at the top");
    }

    #[test]
    fn the_app_pins_the_meter_from_its_tray_command_and_remembers_it() {
        use eframe::App as _;
        let mut app = App::stopped(Settings::default());
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut frame_texts = |app: &mut App| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(760.0, 880.0))),
                ..Default::default()
            };
            let output = ctx.run_ui(input, |ui| {
                app.logic(ui.ctx(), &mut frame);
                app.ui(ui, &mut frame);
            });
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
        };
        // The footer says "Stopped"; a pinned meter says it too.
        let stopped = |texts: Vec<String>| texts.iter().filter(|t| *t == "Stopped").count();
        assert_eq!(stopped(frame_texts(&mut app)), 1);

        app.commands.0.send(Command::ToggleOverlay).unwrap();
        frame_texts(&mut app);
        assert!(app.settings.overlay);
        assert!(app.dirty_since.is_some(), "pinning is saved");
        // Tests embed extra windows in the main one.
        assert_eq!(stopped(frame_texts(&mut app)), 2, "the meter is drawn");

        app.commands.0.send(Command::ToggleOverlay).unwrap();
        frame_texts(&mut app);
        assert!(!app.settings.overlay);
        assert_eq!(stopped(frame_texts(&mut app)), 1, "unpinned");
    }
}
