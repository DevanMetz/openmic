//! The Voice page's settings, one row each, in the order your voice passes
//! through them. Each row says in a few words what the setting does and
//! shows it happening live, and the picture is also the control: drag
//! inside it to change the setting, scroll for fine steps, double-click to
//! reset. A switch turns a setting on or off; rows that are off are dimmed.

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

use crate::config::Settings;
use crate::dsp::{self, Model, HIGHPASS_HZ, HIGHPASS_RANGE, VOICE_THRESHOLD};
use crate::engine::{ScopeFrame, SCOPE_FRAMES};
use crate::gui::{AMBER, CYAN, GREEN, RED, VIOLET};

const INPUT_GAIN: std::ops::RangeInclusive<f32> = -12.0..=24.0;
const OUTPUT_GAIN: std::ops::RangeInclusive<f32> = -24.0..=12.0;
const GATE_THRESHOLD: std::ops::RangeInclusive<f32> = -80.0..=-20.0;
const VOICE_RANGE: std::ops::RangeInclusive<f32> = 0.05..=0.95;
/// The bottom of the level pictures, dBFS.
const FLOOR_DB: f32 = -70.0;
const PICTURE_HEIGHT: f32 = 34.0;
const SWITCH_WIDTH: f32 = 30.0;
const TITLE_WIDTH: f32 = 176.0;
const VALUE_WIDTH: f32 = 92.0;
/// Rumble picture's frequency span, Hz.
const RUMBLE_SPAN: (f32, f32) = (20.0, 500.0);

/// What changed in the rows this frame.
#[derive(Debug, Default, PartialEq)]
pub struct Changed {
    /// A processing setting: apply it live.
    pub settings: bool,
    /// The headphone monitor was switched on or off.
    pub monitor: bool,
    /// Where each row's picture is (for tests and highlighting).
    pub pictures: Vec<(String, Rect)>,
}

/// One setting's row. The switch (if any), then the name and what it does,
/// then its picture (which is also its control), then its value.
struct Row<'a> {
    id: &'a str,
    on: Option<bool>,
    title: &'a str,
    explain: &'a str,
    value: String,
    /// A second, clickable line under the value (the noise model).
    value_link: Option<&'a str>,
    cursor: CursorIcon,
}

/// What the user did on a row.
struct RowInput {
    switched: bool,
    link: bool,
    picture: egui::Response,
    notches: f32,
}

fn draw_row(ui: &mut egui::Ui, row: &Row, paint: impl FnOnce(&egui::Painter, Rect, bool)) -> RowInput {
    let active = row.on.unwrap_or(true);
    let mut switched = false;
    let mut link = false;
    let picture = ui
        .horizontal(|ui| {
            ui.set_min_height(PICTURE_HEIGHT + 10.0);
            // The switch, or a gap the same width.
            let (rect, response) = ui.allocate_exact_size(Vec2::new(SWITCH_WIDTH, PICTURE_HEIGHT), Sense::click());
            if let Some(on) = row.on {
                switch(ui, rect, on, response.hovered());
                let response = response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(if on { "On: click to turn off" } else { "Off: click to turn on" });
                response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, row.title));
                switched = response.clicked();
            }
            ui.vertical(|ui| {
                ui.set_width(TITLE_WIDTH);
                let title = RichText::new(row.title).strong();
                ui.label(if active { title } else { title.weak() });
                ui.label(RichText::new(row.explain).small().weak());
            });
            let width = (ui.available_width() - VALUE_WIDTH - ui.spacing().item_spacing.x).max(80.0);
            let (rect, picture) = ui.allocate_exact_size(Vec2::new(width, PICTURE_HEIGHT), Sense::click_and_drag());
            let picture = picture.on_hover_cursor(row.cursor);
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 5.0, ui.visuals().extreme_bg_color);
            paint(&painter, rect.shrink2(Vec2::new(4.0, 2.0)), active);
            if !active {
                painter.rect_filled(rect, 5.0, ui.visuals().panel_fill.gamma_multiply(0.55));
            }
            ui.vertical(|ui| {
                ui.set_width(VALUE_WIDTH);
                let value = RichText::new(&row.value).monospace();
                ui.label(if active { value } else { value.weak() });
                if let Some(text) = row.value_link {
                    link = ui.link(RichText::new(text).small()).on_hover_text("Switch the noise model").clicked();
                }
            });
            picture
        })
        .inner;
    let notches = wheel_notches(ui, Id::new(("row wheel", row.id)), picture.hovered() && !picture.dragged());
    ui.data_mut(|d| {
        let pictures = d.get_temp_mut_or_default::<Vec<(String, Rect)>>(Id::new("row pictures"));
        pictures.retain(|(id, _)| id != row.id);
        pictures.push((row.id.to_owned(), picture.rect));
    });
    RowInput { switched, link, picture, notches }
}

/// An on/off switch drawn in `rect`.
fn switch(ui: &egui::Ui, rect: Rect, on: bool, hovered: bool) {
    let track = Rect::from_center_size(rect.center(), Vec2::new(26.0, 14.0));
    let visuals = ui.visuals();
    let fill = if on { GREEN } else { visuals.widgets.inactive.bg_fill };
    let fill = if hovered { fill.gamma_multiply(0.85) } else { fill };
    ui.painter().rect_filled(track, 7.0, fill);
    let knob = if on { track.right() - 7.0 } else { track.left() + 7.0 };
    ui.painter().circle_filled(Pos2::new(knob, track.center().y), 5.0, Color32::WHITE);
}

/// x for history frame `i` of `len`, newest at the right edge.
fn frame_x(rect: Rect, i: usize, len: usize) -> f32 {
    let slot = SCOPE_FRAMES.saturating_sub(len) + i;
    rect.left() + rect.width() * slot as f32 / (SCOPE_FRAMES - 1) as f32
}

/// y for a level in dBFS: the floor at the bottom, 0 dBFS at the top.
fn level_y(rect: Rect, db: f32) -> f32 {
    rect.bottom() - ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * rect.height()
}

fn trace(painter: &egui::Painter, rect: Rect, frames: &[ScopeFrame], value: impl Fn(&ScopeFrame) -> f32, color: Color32) {
    let points: Vec<Pos2> = frames.iter().enumerate().map(|(i, f)| Pos2::new(frame_x(rect, i, frames.len()), value(f))).collect();
    painter.add(Shape::line(points, Stroke::new(1.5, color)));
}

/// Shade the stretches of history where `when` holds.
fn shade(painter: &egui::Painter, rect: Rect, frames: &[ScopeFrame], when: impl Fn(&ScopeFrame) -> bool, color: Color32) {
    let n = frames.len();
    for (i, _) in frames.iter().enumerate().filter(|(_, f)| when(f)) {
        let right = if i + 1 < n { frame_x(rect, i + 1, n) } else { rect.right() };
        painter.rect_filled(Rect::from_x_y_ranges(frame_x(rect, i, n)..=right, rect.y_range()), 0.0, color);
    }
}

fn dashed(painter: &egui::Painter, rect: Rect, y: f32, color: Color32) {
    let line = [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)];
    painter.extend(Shape::dashed_line(&line, Stroke::new(1.0, color), 4.0, 3.0));
}

fn idle(painter: &egui::Painter, rect: Rect, text: &str, color: Color32) {
    painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(11.0), color);
}

/// Change `value` by a relative drag (up or right increases), `per_px` a pixel.
fn drag_by(picture: &egui::Response, value: &mut f32, per_px: f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let delta = picture.drag_delta();
    let pixels = if delta.x.abs() > delta.y.abs() { delta.x } else { -delta.y };
    let next = (*value + pixels * per_px).clamp(*range.start(), *range.end());
    let changed = next != *value;
    *value = next;
    changed
}

/// Set `value` from where the pointer is along one axis of the picture.
fn drag_to(picture: &egui::Response, rect: Rect, value: &mut f32, map: impl Fn(Pos2, Rect) -> f32) -> bool {
    if !picture.dragged() && !picture.clicked() {
        return false;
    }
    let Some(pos) = picture.interact_pointer_pos() else { return false };
    let next = map(pos, rect);
    let changed = next != *value;
    *value = next;
    changed
}

/// The rows. `frames` is the recent history (empty when stopped);
/// `strength_before_off` remembers the noise strength while that switch is off.
pub fn voice_rows(ui: &mut egui::Ui, frames: &[ScopeFrame], s: &mut Settings, strength_before_off: &mut f32) -> Changed {
    let mut changed = Changed::default();
    let weak = ui.visuals().weak_text_color();
    let live = !frames.is_empty();

    // ---- Your mic --------------------------------------------------------
    let row = Row {
        id: "mic",
        on: None,
        title: "Your mic",
        explain: "How loud you are going in",
        value: format!("{:+.1} dB", s.input_gain_db),
        value_link: None,
        cursor: CursorIcon::ResizeVertical,
    };
    let input = draw_row(ui, &row, |p, r, _| {
        // Near the top is too loud.
        p.rect_filled(Rect::from_x_y_ranges(r.x_range(), r.top()..=level_y(r, -6.0)), 0.0, RED.gamma_multiply(0.12));
        if live {
            trace(p, r, frames, |f| level_y(r, f.input_db), CYAN);
        } else {
            idle(p, r, "Start OpenMic to see your voice live", weak);
        }
    });
    changed.settings |= drag_by(&input.picture, &mut s.input_gain_db, 0.25, INPUT_GAIN)
        | step_by(&mut s.input_gain_db, input.notches, 0.5, INPUT_GAIN)
        | reset_on_double_click(&input.picture, &mut s.input_gain_db, 0.0);

    // ---- Remove rumble ---------------------------------------------------
    let row = Row {
        id: "rumble",
        on: Some(s.highpass),
        title: "Remove rumble",
        explain: "Cuts low thumps and hum",
        value: format!("{:.0} Hz", s.highpass_hz),
        value_link: None,
        cursor: CursorIcon::ResizeHorizontal,
    };
    let cutoff_hz = s.highpass_hz;
    let input = draw_row(ui, &row, |p, r, _| rumble_picture(p, r, cutoff_hz, weak));
    changed.settings |= toggle(input.switched, &mut s.highpass);
    changed.settings |= drag_to(&input.picture, input.picture.rect, &mut s.highpass_hz, |pos, rect| {
        freq_at(rect.shrink2(Vec2::new(4.0, 2.0)), pos.x).clamp(*HIGHPASS_RANGE.start(), *HIGHPASS_RANGE.end()).round()
    }) | step_by(&mut s.highpass_hz, input.notches, 1.0, HIGHPASS_RANGE)
        | reset_on_double_click(&input.picture, &mut s.highpass_hz, HIGHPASS_HZ);

    // ---- Remove noise ----------------------------------------------------
    let noise_on = s.strength > 0.0;
    let row = Row {
        id: "noise",
        on: Some(noise_on),
        title: "Remove noise",
        explain: "Keeps your voice, drops the room",
        value: format!("{:.0}%", s.strength * 100.0),
        value_link: Some(match s.model {
            Model::DeepFilter => "DeepFilterNet",
            Model::Rnnoise => "RNNoise",
        }),
        cursor: CursorIcon::ResizeHorizontal,
    };
    let input = draw_row(ui, &row, |p, r, _| {
        if live {
            // What came in, shaded; what went out, the line: the gap is the noise removed.
            let n = frames.len();
            for (i, f) in frames.iter().enumerate() {
                let right = if i + 1 < n { frame_x(r, i + 1, n) } else { r.right() };
                let band = Rect::from_x_y_ranges(frame_x(r, i, n)..=right, level_y(r, f.input_db)..=r.bottom());
                p.rect_filled(band, 0.0, CYAN.gamma_multiply(0.22));
            }
            trace(p, r, frames, |f| level_y(r, f.output_db), GREEN);
        } else {
            idle(p, r, "blue: what comes in · green: what Discord hears", weak);
        }
    });
    if input.switched {
        if noise_on {
            *strength_before_off = s.strength;
            s.strength = 0.0;
        } else {
            s.strength = if *strength_before_off > 0.0 { *strength_before_off } else { 1.0 };
        }
        changed.settings = true;
    }
    if input.link {
        s.model = match s.model {
            Model::DeepFilter => Model::Rnnoise,
            Model::Rnnoise => Model::DeepFilter,
        };
        changed.settings = true;
    }
    changed.settings |= drag_by(&input.picture, &mut s.strength, 0.005, 0.0..=1.0)
        | step_by(&mut s.strength, input.notches, 0.01, 0.0..=1.0)
        | reset_on_double_click(&input.picture, &mut s.strength, 1.0);

    // ---- Only my voice (voice gate) -------------------------------------
    let row = Row {
        id: "voice",
        on: Some(s.voice_gate),
        title: "Only my voice",
        explain: "Silent when you're not talking",
        value: format!("{:.0}%", s.voice_threshold * 100.0),
        value_link: None,
        cursor: CursorIcon::ResizeVertical,
    };
    let threshold = s.voice_threshold;
    let prob_y = |r: Rect, prob: f32| r.bottom() - prob.clamp(0.0, 1.0) * r.height();
    let input = draw_row(ui, &row, |p, r, _| {
        if live {
            shade(p, r, frames, |f| f.voice_gate > 0.5, GREEN.gamma_multiply(0.16));
            trace(p, r, frames, |f| prob_y(r, f.prob), VIOLET);
        }
        dashed(p, r, prob_y(r, threshold), VIOLET);
        if !live {
            idle(p, r, "green: when your voice gets through", weak);
        }
    });
    changed.settings |= toggle(input.switched, &mut s.voice_gate);
    changed.settings |= drag_to(&input.picture, input.picture.rect, &mut s.voice_threshold, |pos, rect| {
        let r = rect.shrink2(Vec2::new(4.0, 2.0));
        (((r.bottom() - pos.y) / r.height()) * 100.0).round().clamp(5.0, 95.0) / 100.0
    }) | step_by(&mut s.voice_threshold, input.notches, 0.01, VOICE_RANGE)
        | reset_on_double_click(&input.picture, &mut s.voice_threshold, VOICE_THRESHOLD);

    // ---- Quiet cut-off (level gate) --------------------------------------
    let row = Row {
        id: "level",
        on: Some(s.gate),
        title: "Quiet cut-off",
        explain: "Mutes anything below a level",
        value: format!("{:.0} dB", s.gate_threshold_db),
        value_link: None,
        cursor: CursorIcon::ResizeVertical,
    };
    let gate_db = s.gate_threshold_db;
    let input = draw_row(ui, &row, |p, r, on| {
        if live {
            if on {
                shade(p, r, frames, |f| f.level_gate < 0.5, RED.gamma_multiply(0.12));
            }
            trace(p, r, frames, |f| level_y(r, f.input_db), weak);
        }
        dashed(p, r, level_y(r, gate_db), AMBER);
        if !live {
            idle(p, r, "below the line is muted", weak);
        }
    });
    changed.settings |= toggle(input.switched, &mut s.gate);
    changed.settings |= drag_to(&input.picture, input.picture.rect, &mut s.gate_threshold_db, |pos, rect| {
        let r = rect.shrink2(Vec2::new(4.0, 2.0));
        let db = FLOOR_DB * (pos.y - r.top()) / r.height();
        db.round().clamp(*GATE_THRESHOLD.start(), *GATE_THRESHOLD.end())
    }) | step_by(&mut s.gate_threshold_db, input.notches, 1.0, GATE_THRESHOLD)
        | reset_on_double_click(&input.picture, &mut s.gate_threshold_db, -50.0);

    // ---- To Discord --------------------------------------------------------
    let row = Row {
        id: "out",
        on: None,
        title: "To Discord",
        explain: "How loud Discord hears you",
        value: format!("{:+.1} dB", s.output_gain_db),
        value_link: None,
        cursor: CursorIcon::ResizeVertical,
    };
    let input = draw_row(ui, &row, |p, r, _| {
        p.rect_filled(Rect::from_x_y_ranges(r.x_range(), r.top()..=level_y(r, -6.0)), 0.0, RED.gamma_multiply(0.12));
        if live {
            trace(p, r, frames, |f| level_y(r, f.output_db), GREEN);
        } else {
            idle(p, r, "what Discord hears, after everything above", weak);
        }
    });
    changed.settings |= drag_by(&input.picture, &mut s.output_gain_db, 0.25, OUTPUT_GAIN)
        | step_by(&mut s.output_gain_db, input.notches, 0.5, OUTPUT_GAIN)
        | reset_on_double_click(&input.picture, &mut s.output_gain_db, 0.0);

    // ---- Hear yourself (headphone monitor) --------------------------------
    let row = Row {
        id: "monitor",
        on: Some(s.monitor),
        title: "Hear yourself",
        explain: "What Discord hears, in your headphones",
        value: format!("{:.0}%", s.monitor_volume * 100.0),
        value_link: None,
        cursor: CursorIcon::ResizeHorizontal,
    };
    let volume = s.monitor_volume;
    let input = draw_row(ui, &row, |p, r, _| {
        let bar = Rect::from_center_size(r.center(), Vec2::new(r.width(), 8.0));
        p.rect_filled(bar, 4.0, weak.gamma_multiply(0.25));
        p.rect_filled(Rect::from_min_max(bar.min, Pos2::new(bar.left() + bar.width() * volume, bar.bottom())), 4.0, CYAN);
    });
    if toggle(input.switched, &mut s.monitor) {
        changed.monitor = true;
        changed.settings = true;
    }
    changed.settings |= drag_to(&input.picture, input.picture.rect, &mut s.monitor_volume, |pos, rect| {
        let r = rect.shrink2(Vec2::new(4.0, 2.0));
        (((pos.x - r.left()) / r.width()) * 100.0).round().clamp(0.0, 100.0) / 100.0
    }) | step_by(&mut s.monitor_volume, input.notches, 0.01, 0.0..=1.0)
        | reset_on_double_click(&input.picture, &mut s.monitor_volume, 1.0);

    changed.pictures = ui.data(|d| d.get_temp::<Vec<(String, Rect)>>(Id::new("row pictures"))).unwrap_or_default();
    changed
}

fn toggle(switched: bool, on: &mut bool) -> bool {
    if switched {
        *on = !*on;
    }
    switched
}

fn reset_on_double_click(picture: &egui::Response, value: &mut f32, default: f32) -> bool {
    let reset = picture.double_clicked() && *value != default;
    if reset {
        *value = default;
    }
    reset
}

/// The frequency under x in the rumble picture (log scale).
fn freq_at(rect: Rect, x: f32) -> f32 {
    let (lo, hi) = (RUMBLE_SPAN.0.log10(), RUMBLE_SPAN.1.log10());
    10f32.powf(lo + ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * (hi - lo))
}

fn freq_x(rect: Rect, hz: f32) -> f32 {
    let (lo, hi) = (RUMBLE_SPAN.0.log10(), RUMBLE_SPAN.1.log10());
    rect.left() + (hz.log10() - lo) / (hi - lo) * rect.width()
}

/// The filter's curve from low pitch (left) to high: what's under it is cut.
fn rumble_picture(p: &egui::Painter, r: Rect, cutoff_hz: f32, weak: Color32) {
    let (min_db, max_db) = (-40.0, 3.0);
    let y = |db: f32| r.top() + (max_db - db.clamp(min_db, max_db)) / (max_db - min_db) * r.height();
    let points: Vec<Pos2> = (0..=48)
        .map(|i| {
            let hz = freq_at(r, r.left() + r.width() * i as f32 / 48.0);
            Pos2::new(freq_x(r, hz), y(dsp::highpass_response_db(hz, cutoff_hz)))
        })
        .collect();
    let cut = freq_x(r, cutoff_hz);
    p.rect_filled(Rect::from_x_y_ranges(r.left()..=cut, r.y_range()), 0.0, AMBER.gamma_multiply(0.14));
    p.add(Shape::line(points, Stroke::new(1.5, AMBER)));
    p.line_segment([Pos2::new(cut, r.top()), Pos2::new(cut, r.bottom())], Stroke::new(2.0, AMBER));
    p.text(r.right_bottom() - Vec2::new(2.0, 1.0), Align2::RIGHT_BOTTOM, "low · high pitch", FontId::proportional(10.0), weak);
}

/// Consume vertical wheel input for the active control. Keep fractional
/// touchpad movement between frames until it amounts to a whole notch.
pub fn wheel_notches(ui: &egui::Ui, id: Id, active: bool) -> f32 {
    let id = id.with("wheel remainder");
    if !active {
        ui.data_mut(|d| d.remove::<f32>(id));
        return 0.0;
    }
    let delta = ui.input_mut(|i| {
        let mut notches = 0.0;
        for event in &mut i.events {
            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                notches += match unit {
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Page => delta.y * 3.0,
                    egui::MouseWheelUnit::Point => delta.y / 50.0,
                };
                delta.y = 0.0;
            }
        }
        // Don't also scroll a containing panel with the consumed gesture.
        i.smooth_scroll_delta.y = 0.0;
        notches
    });
    ui.data_mut(|d| {
        let total = d.get_temp::<f32>(id).unwrap_or_default() + delta;
        let whole = total.trunc();
        d.insert_temp(id, total - whole);
        whole
    })
}

/// Step `value` by whole `step`s on the step grid, so scrolling lands on
/// round numbers even after a drag.
pub fn step_by(value: &mut f32, notches: f32, step: f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let notches = notches.trunc();
    if notches == 0.0 {
        return false;
    }
    let next = (((*value / step).round() + notches) * step).clamp(*range.start(), *range.end());
    let changed = next != *value;
    *value = next;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_steps_land_on_the_grid_and_respect_limits() {
        let mut gain = 3.37;
        assert!(step_by(&mut gain, 1.0, 0.5, INPUT_GAIN));
        assert_eq!(gain, 4.0, "snaps a dragged value onto the 0.5 dB grid");
        step_by(&mut gain, -3.0, 0.5, INPUT_GAIN);
        assert_eq!(gain, 2.5);
        let mut top = 24.0;
        assert!(!step_by(&mut top, 1.0, 0.5, INPUT_GAIN), "clamped at the maximum");
        let mut threshold = 0.6;
        step_by(&mut threshold, 1.0, 0.01, 0.05..=0.95);
        assert!((threshold - 0.61).abs() < 1e-6);
        assert!(!step_by(&mut threshold, 0.0, 0.01, 0.05..=0.95), "no wheel, no change");
        gain = 3.37;
        assert!(!step_by(&mut gain, 0.25, 0.5, INPUT_GAIN), "a partial notch cannot snap a dragged value");
    }

    #[test]
    fn touchpad_motion_accumulates_and_is_consumed_by_one_control() {
        let ctx = egui::Context::default();
        let frame = |points, active| {
            let mut steps = 0.0;
            let _ = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(0.0, points),
                        modifiers: egui::Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                    }],
                    ..Default::default()
                },
                |ui| {
                    steps = wheel_notches(ui, Id::new("gain"), active);
                    if active {
                        assert_eq!(wheel_notches(ui, Id::new("other"), true), 0.0);
                        assert_eq!(ui.input(|i| i.smooth_scroll_delta.y), 0.0);
                    }
                },
            );
            steps
        };
        for _ in 0..3 {
            assert_eq!(frame(12.5, true), 0.0);
        }
        assert_eq!(frame(12.5, true), 1.0);
        assert_eq!(frame(-25.0, true), 0.0);
        assert_eq!(frame(-25.0, true), -1.0);
        assert_eq!(frame(25.0, true), 0.0);
        assert_eq!(frame(0.0, false), 0.0);
        assert_eq!(frame(25.0, true), 0.0, "leaving a control clears its partial gesture");
    }

    /// Run the rows for a frame with `events`; returns what changed.
    fn rows(ctx: &egui::Context, s: &mut Settings, before_off: &mut f32, frames: &[ScopeFrame], events: Vec<egui::Event>) -> Changed {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(620.0, 420.0))),
            events,
            ..Default::default()
        };
        let mut out = Changed::default();
        let _ = ctx.run_ui(input, |ui| out = voice_rows(ui, frames, s, before_off));
        out
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// Click at `pos`; returns whether anything changed.
    fn click(ctx: &egui::Context, s: &mut Settings, before_off: &mut f32, pos: Pos2) -> bool {
        let mut changed = false;
        for events in [vec![egui::Event::PointerMoved(pos)], vec![button(pos, true)], vec![button(pos, false)], vec![]] {
            changed |= rows(ctx, s, before_off, &[], events).settings;
        }
        changed
    }

    fn picture(ctx: &egui::Context, s: &mut Settings, id: &str) -> Rect {
        let out = rows(ctx, s, &mut 1.0, &[], vec![]);
        out.pictures.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("no {id} row")).1
    }

    #[test]
    fn switches_turn_settings_on_and_off() {
        let ctx = egui::Context::default();
        let mut s = Settings::default();
        let mut before_off = 1.0;
        rows(&ctx, &mut s, &mut before_off, &[], vec![]);
        // The switch sits left of each row's picture.
        let switch_at = |r: Rect| Pos2::new(r.left() - TITLE_WIDTH - SWITCH_WIDTH / 2.0 - 16.0, r.center().y);
        let rumble = picture(&ctx, &mut s, "rumble");
        assert!(s.highpass);
        assert!(click(&ctx, &mut s, &mut before_off, switch_at(rumble)));
        assert!(!s.highpass, "the rumble switch turned it off");
        let level = picture(&ctx, &mut s, "level");
        assert!(!s.gate);
        click(&ctx, &mut s, &mut before_off, switch_at(level));
        assert!(s.gate, "the quiet cut-off switch turned it on");

        // Noise off remembers the strength and brings it back.
        s.strength = 0.7;
        let noise = picture(&ctx, &mut s, "noise");
        click(&ctx, &mut s, &mut before_off, switch_at(noise));
        assert_eq!(s.strength, 0.0);
        click(&ctx, &mut s, &mut before_off, switch_at(noise));
        assert_eq!(s.strength, 0.7);
    }

    #[test]
    fn dragging_inside_a_picture_changes_its_setting() {
        let ctx = egui::Context::default();
        let mut s = Settings::default();
        let mut before_off = 1.0;
        let drag = |ctx: &egui::Context, s: &mut Settings, before_off: &mut f32, from: Pos2, to: Pos2| {
            for events in [
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true)],
                vec![egui::Event::PointerMoved(from + (to - from) * 0.5)],
                vec![egui::Event::PointerMoved(to)],
                vec![button(to, false)],
            ] {
                rows(ctx, s, before_off, &[], events);
            }
        };
        let mic = picture(&ctx, &mut s, "mic");
        drag(&ctx, &mut s, &mut before_off, mic.center(), mic.center() - Vec2::new(0.0, 20.0));
        assert!(s.input_gain_db > 2.0, "dragging up raises the gain: {}", s.input_gain_db);

        let rumble = picture(&ctx, &mut s, "rumble");
        let target = freq_x(rumble.shrink2(Vec2::new(4.0, 2.0)), 150.0);
        drag(&ctx, &mut s, &mut before_off, rumble.center(), Pos2::new(target, rumble.center().y));
        assert!((s.highpass_hz - 150.0).abs() <= 2.0, "the cutoff follows the pointer: {}", s.highpass_hz);

        let voice = picture(&ctx, &mut s, "voice");
        let quarter = Pos2::new(voice.center().x, voice.bottom() - 2.0 - (voice.height() - 4.0) * 0.25);
        drag(&ctx, &mut s, &mut before_off, voice.center(), quarter);
        assert!((s.voice_threshold - 0.25).abs() <= 0.02, "the threshold follows the pointer: {}", s.voice_threshold);
    }

    #[test]
    fn the_noise_model_switches_from_its_row() {
        let ctx = egui::Context::default();
        let mut s = Settings::default();
        let mut before_off = 1.0;
        let noise = picture(&ctx, &mut s, "noise");
        // The model link sits under the value, right of the picture.
        let link = Pos2::new(noise.right() + 30.0, noise.center().y + 8.0);
        assert_eq!(s.model, Model::DeepFilter);
        click(&ctx, &mut s, &mut before_off, link);
        assert_eq!(s.model, Model::Rnnoise);
    }

    #[test]
    fn scrolling_steps_and_double_click_resets() {
        let ctx = egui::Context::default();
        let mut s = Settings::default();
        let mut before_off = 1.0;
        let out = picture(&ctx, &mut s, "out");
        let wheel = |lines: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, lines),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        for events in [vec![egui::Event::PointerMoved(out.center())], vec![wheel(-4.0)], vec![]] {
            rows(&ctx, &mut s, &mut before_off, &[], events);
        }
        assert_eq!(s.output_gain_db, -2.0, "0.5 dB per notch");
        for events in [
            vec![button(out.center(), true)],
            vec![button(out.center(), false)],
            vec![button(out.center(), true)],
            vec![button(out.center(), false)],
            vec![],
        ] {
            rows(&ctx, &mut s, &mut before_off, &[], events);
        }
        assert_eq!(s.output_gain_db, 0.0, "a double-click resets it");
    }

    #[test]
    fn live_pictures_draw_from_the_history() {
        let ctx = egui::Context::default();
        let mut s = Settings::default();
        let frames: Vec<ScopeFrame> = (0..50)
            .map(|i| ScopeFrame { input_db: -30.0, output_db: -40.0, prob: (i % 10) as f32 / 10.0, voice_gate: 1.0, level_gate: 1.0 })
            .collect();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(620.0, 420.0))),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            voice_rows(ui, &frames, &mut s, &mut 1.0);
        });
        let texts: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        assert!(!texts.iter().any(|t| t.starts_with("Start OpenMic")), "live: no idle hints");
        for title in ["Your mic", "Remove rumble", "Remove noise", "Only my voice", "Quiet cut-off", "To Discord", "Hear yourself"] {
            assert!(texts.iter().any(|t| t == title), "{title} row missing");
        }
    }
}
