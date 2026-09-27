//! A level meter you can pin to the screen: a small always-on-top window
//! showing what Discord hears, so you can see your voice going out (or
//! being muted) over a game or another app. Drag it to move it, double-click
//! it to open OpenMic; mute and close buttons appear on hover.

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId,
};

use super::{App, Command, AMBER, GREEN, MUTED, RED};

pub(super) const SIZE: Vec2 = Vec2::new(200.0, 34.0);
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
            return;
        }
        let meter = self.meter();
        let mut builder = ViewportBuilder::default()
            .with_title("OpenMic level")
            .with_inner_size(SIZE)
            .with_resizable(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false);
        if let Some([x, y]) = self.settings.overlay_pos {
            builder = builder.with_position([x, y]);
        }
        let hold = &mut self.overlay_hold;
        let (actions, position) =
            ctx.show_viewport_immediate(ViewportId::from_hash_of("openmic-level-meter"), builder, |ui, _| {
                let actions = draw(ui, meter, hold);
                (actions, ui.ctx().input(|i| i.viewport().outer_rect).map(|r| r.min))
            });

        // Remember where it was dragged to.
        if let Some(pos) = position {
            let moved = self
                .settings
                .overlay_pos
                .is_none_or(|[x, y]| (x - pos.x).abs() > 0.5 || (y - pos.y).abs() > 0.5);
            if moved {
                self.settings.overlay_pos = Some([pos.x, pos.y]);
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

    // The whole meter drags the window; a double-click opens OpenMic.
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

    let dot = match meter {
        Meter::Stopped => MUTED.gamma_multiply(0.6),
        Meter::Muted => RED,
        Meter::Live(db) if db > SPEAKING_DB => GREEN,
        Meter::Live(_) => MUTED,
    };
    mic_glyph(ui.painter(), rect.left_center() + Vec2::new(17.0, 0.0), dot);

    // Buttons take the right edge while hovered; the bar gives way.
    let buttons = if hovered { 84.0 } else { 0.0 };
    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + 34.0, rect.center().y - 5.0),
        Pos2::new(rect.right() - 10.0 - buttons, rect.center().y + 5.0),
    );
    match meter {
        Meter::Stopped | Meter::Muted => {
            let (text, color) = if meter == Meter::Muted { ("Muted", RED) } else { ("Stopped", MUTED) };
            ui.painter().text(
                bar.left_center(),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(13.0),
                color,
            );
        }
        Meter::Live(db) => level_bar(ui.painter(), bar, db, *hold, visuals.extreme_bg_color),
    }

    if hovered {
        let close = Rect::from_center_size(Pos2::new(rect.right() - 15.0, rect.center().y), Vec2::new(22.0, 20.0));
        let mute = Rect::from_center_size(Pos2::new(close.left() - 29.0, rect.center().y), Vec2::new(52.0, 20.0));
        let muted = meter == Meter::Muted;
        let mute_button = egui::Button::new(if muted { "Unmute" } else { "Mute" }).small();
        if ui
            .put(mute, mute_button)
            .on_hover_text(if muted { "Unmute mic" } else { "Mute mic" })
            .clicked()
        {
            actions.push(Action::ToggleMute);
        }
        if ui.put(close, egui::Button::new("×").small()).on_hover_text("Unpin the level meter").clicked() {
            actions.push(Action::Hide);
        }
    }
    body.on_hover_text_at_pointer("Drag to move · double-click to open OpenMic");
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

    /// Draw the meter at its real size, with the pointer at `pointer` and
    /// optionally a click there; returns the text shown and the actions.
    fn run(meter: Meter, pointer: Option<Pos2>, click: bool) -> (Vec<String>, Vec<Action>) {
        let ctx = egui::Context::default();
        let mut hold = FLOOR;
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
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
        assert!(run(Meter::Stopped, None, false).0.contains(&"Stopped".to_owned()));
        assert!(run(Meter::Muted, None, false).0.contains(&"Muted".to_owned()));
        let (live, _) = run(Meter::Live(-20.0), None, false);
        assert!(!live.iter().any(|t| t == "Stopped" || t == "Muted"), "a live meter shows the bar: {live:?}");
    }

    #[test]
    fn hover_buttons_mute_and_unpin() {
        let (texts, _) = run(Meter::Live(-20.0), None, false);
        assert!(!texts.contains(&"Mute".to_owned()), "buttons stay hidden until hovered");
        let (texts, _) = run(Meter::Live(-20.0), Some(Pos2::new(100.0, 17.0)), false);
        assert!(texts.contains(&"Mute".to_owned()) && texts.contains(&"×".to_owned()), "{texts:?}");

        // Mute sits left of the close button at the right edge.
        let mute = Pos2::new(SIZE.x - 15.0 - 11.0 - 29.0, 17.0);
        assert_eq!(run(Meter::Live(-20.0), Some(mute), true).1, [Action::ToggleMute]);
        assert!(run(Meter::Muted, Some(mute), false).0.contains(&"Unmute".to_owned()));
        let close = Pos2::new(SIZE.x - 15.0, 17.0);
        assert_eq!(run(Meter::Live(-20.0), Some(close), true).1, [Action::Hide]);
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
