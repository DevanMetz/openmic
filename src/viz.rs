//! The Voice page's settings, one row each, in the order your voice passes
//! through them. Each row says in a few words what the setting does and
//! shows it happening live, and the picture is also the control: drag
//! inside it to change the setting, scroll for fine steps, double-click to
//! reset. A switch turns a setting on or off; rows that are off are dimmed.

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Id, Pos2, Rect, RichText, Sense, Shape, Stroke, Vec2};

use crate::config::Settings;
use crate::dsp::{self, Model, GATE_THRESHOLD_RANGE, HIGHPASS_HZ, HIGHPASS_RANGE, INPUT_GAIN_RANGE, OUTPUT_GAIN_RANGE, VOICE_THRESHOLD};
use crate::engine::{ScopeFrame, SCOPE_FRAMES};
use crate::gui::{AMBER, CYAN, GREEN, RED, VIOLET};
use crate::widgets;

const VOICE_RANGE: std::ops::RangeInclusive<f32> = 0.05..=0.95;
/// The bottom of the level pictures, dBFS.
const FLOOR_DB: f32 = -70.0;
const PICTURE_HEIGHT: f32 = 34.0;
const MIN_PICTURE_WIDTH: f32 = 80.0;
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
    unit: &'a str,
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
    let mut active = row.on.unwrap_or(true);
    let mut switched = false;
    let mut link = false;
    let picture_id = ui.make_persistent_id(("voice-picture", row.id));
    let picture = ui
        .horizontal_wrapped(|ui| {
            ui.set_min_height(PICTURE_HEIGHT + 10.0);
            let row_width = ui.available_width();
            let spacing = ui.spacing().item_spacing.x;
            let stacked = row_width < SWITCH_WIDTH + TITLE_WIDTH + MIN_PICTURE_WIDTH + VALUE_WIDTH + 3.0 * spacing;
            // The switch, or a gap the same width.
            let sense = if row.on.is_some() { Sense::click() } else { Sense::hover() };
            let (rect, mut response) = ui.allocate_exact_size(Vec2::new(SWITCH_WIDTH, PICTURE_HEIGHT), sense);
            if let Some(on) = row.on {
                switched = response.clicked();
                // Paint and announce the new state before the caller applies it.
                let on = if switched { !on } else { on };
                active = on;
                if switched {
                    response.mark_changed();
                    ui.ctx().request_repaint();
                }
                switch(ui, rect, on, response.hovered());
                if response.has_focus() {
                    ui.painter().rect_stroke(rect, 5.0, Stroke::new(2.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
                }
                widgets::scroll_on_focus(&response);
                let response = response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .on_hover_text(if on { "On: click to turn off" } else { "Off: click to turn on" });
                response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, response.enabled(), on, row.title));
            }
            ui.vertical(|ui| {
                ui.set_width(TITLE_WIDTH.min((row_width - SWITCH_WIDTH - spacing).max(0.0)));
                let title = RichText::new(row.title).strong();
                ui.label(if active { title } else { title.weak() });
                ui.label(RichText::new(row.explain).small().weak());
            });
            // Keep the picture and its value together on a second line when
            // the window cannot fit the switch, title, picture and value.
            let available = if stacked { row_width } else { ui.available_rect_before_wrap().width() };
            let width = (available - VALUE_WIDTH - spacing).max(MIN_PICTURE_WIDTH);
            let (_, rect) = ui.allocate_space(Vec2::new(width, PICTURE_HEIGHT));
            let picture = ui.interact(rect, picture_id, Sense::click_and_drag()).on_hover_cursor(row.cursor);
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 5.0, ui.visuals().extreme_bg_color);
            paint(&painter, rect.shrink2(Vec2::new(4.0, 2.0)), active);
            if !active {
                painter.rect_filled(rect, 5.0, ui.visuals().panel_fill.gamma_multiply(0.55));
            }
            if picture.has_focus() {
                painter.rect_stroke(rect, 5.0, Stroke::new(2.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
            }
            widgets::scroll_on_focus(&picture);
            ui.vertical(|ui| {
                ui.set_width(VALUE_WIDTH);
                let value = RichText::new(&row.value).monospace();
                ui.label(if active { value } else { value.weak() });
                if let Some(text) = row.value_link {
                    // egui's text selection overwrites the link's assistive role.
                    ui.style_mut().interaction.selectable_labels = false;
                    let response = ui.link(RichText::new(text).small()).on_hover_text("Switch the noise model");
                    widgets::scroll_on_focus(&response);
                    link = response.clicked();
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
    let mut job = egui::epaint::text::LayoutJob::simple_singleline(text.to_owned(), FontId::proportional(11.0), color);
    job.wrap = egui::epaint::text::TextWrapping::truncate_at_width(rect.width());
    let galley = painter.layout_job(job);
    painter.galley(rect.center() - galley.size() * 0.5, galley, color);
}

/// Change `value` by a relative drag (up or right increases), `per_px` a pixel.
fn drag_by(picture: &egui::Response, value: &mut f32, per_px: f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let mut delta = picture.drag_delta();
    if picture.drag_stopped_by(egui::PointerButton::Primary) {
        // drag_delta is zero on release, even when the pointer moved again.
        delta = picture.ctx.input(|input| input.pointer.delta());
        if let Some(from_global) = picture.ctx.layer_transform_from_global(picture.layer_id) {
            delta *= from_global.scaling;
        }
    }
    let pixels = if delta.x.abs() > delta.y.abs() { delta.x } else { -delta.y };
    let next = (*value + pixels * per_px).clamp(*range.start(), *range.end());
    let changed = next != *value;
    *value = next;
    changed
}

/// Set `value` from where the pointer is along one axis of the picture.
fn drag_to(picture: &egui::Response, rect: Rect, value: &mut f32, map: impl Fn(Pos2, Rect) -> f32) -> bool {
    if !picture.dragged() && !picture.clicked() && !picture.drag_stopped_by(egui::PointerButton::Primary) {
        return false;
    }
    let Some(pos) = picture.interact_pointer_pos() else { return false };
    let next = map(pos, rect);
    let changed = next != *value;
    *value = next;
    changed
}

/// Wheel, keyboard and assistive adjustments share the same fine-step grid.
fn value_control(ui: &mut egui::Ui, row: &Row, input: &RowInput, value: &mut f32, step: f32, range: std::ops::RangeInclusive<f32>, default: f32) -> bool {
    use egui::accesskit::{Action, ActionData, TreeId};
    let picture = &input.picture;
    let before = *value;
    let scale = if row.unit == "%" { 100.0 } else { 1.0 };
    let requested_elsewhere = ui.input(|input| input.events.iter().rev().find_map(|event| match event {
        egui::Event::AccessKitActionRequest(request) if request.action == Action::Focus && request.target_tree == TreeId::ROOT =>
            Some(request.target_node != picture.id.accesskit_id()),
        _ => None,
    }).unwrap_or(false));
    let focused = picture.has_focus() && !requested_elsewhere;
    if focused {
        ui.memory_mut(|memory| memory.set_focus_lock_filter(picture.id, egui::EventFilter {
            horizontal_arrows: true, vertical_arrows: true, ..Default::default()
        }));
    }
    if picture.enabled() {
        step_by(value, input.notches, step, range.clone());
        let mut used_key = false;
        ui.input_mut(|input| input.events.retain(|event| {
            match event {
                egui::Event::Key { key, pressed: true, modifiers, .. } if focused && modifiers.is_none() => {
                    match key {
                        egui::Key::ArrowUp | egui::Key::ArrowRight => { step_by(value, 1.0, step, range.clone()); }
                        egui::Key::ArrowDown | egui::Key::ArrowLeft => { step_by(value, -1.0, step, range.clone()); }
                        egui::Key::Home => *value = *range.start(),
                        egui::Key::End => *value = *range.end(),
                        _ => return true,
                    }
                    used_key = true;
                    false
                }
                egui::Event::AccessKitActionRequest(request)
                    if request.target_tree == TreeId::ROOT && request.target_node == picture.id.accesskit_id() =>
                {
                    match request.action {
                        Action::Increment => { step_by(value, 1.0, step, range.clone()); }
                        Action::Decrement => { step_by(value, -1.0, step, range.clone()); }
                        Action::SetValue => {
                            if let Some(ActionData::NumericValue(next)) = &request.data && next.is_finite() {
                                *value = (next / scale).clamp(*range.start() as f64, *range.end() as f64) as f32;
                            }
                        }
                        _ => return true,
                    }
                    false
                }
                _ => true,
            }
        }));
        if used_key { ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None)); }
        reset_on_double_click(picture, value, default);
    }
    let changed = *value != before;
    let mut response = picture.clone();
    if changed { response.mark_changed(); }
    response.widget_info(|| egui::WidgetInfo::slider(picture.enabled(), *value as f64 * scale, row.title));
    ui.ctx().accesskit_node_builder(picture.id, |node| {
        node.set_description(row.explain);
        node.set_value(format!("{:.2} {}", *value as f64 * scale, row.unit));
        node.set_min_numeric_value(*range.start() as f64 * scale);
        node.set_max_numeric_value(*range.end() as f64 * scale);
        node.set_numeric_value_step(step as f64 * scale);
        if picture.enabled() {
            node.add_action(Action::SetValue);
            if *value < *range.end() { node.add_action(Action::Increment); }
            if *value > *range.start() { node.add_action(Action::Decrement); }
        }
    });
    if changed { ui.ctx().request_repaint(); }
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
        unit: "dB",
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
    changed.settings |= drag_by(&input.picture, &mut s.input_gain_db, 0.25, INPUT_GAIN_RANGE)
        | value_control(ui, &row, &input, &mut s.input_gain_db, 0.5, INPUT_GAIN_RANGE, 0.0);

    // ---- Remove rumble ---------------------------------------------------
    let row = Row {
        id: "rumble",
        on: Some(s.highpass),
        title: "Remove rumble",
        explain: "Cuts low thumps and hum",
        value: format!("{:.0} Hz", s.highpass_hz),
        unit: "Hz",
        value_link: None,
        cursor: CursorIcon::ResizeHorizontal,
    };
    let cutoff_hz = s.highpass_hz;
    let input = draw_row(ui, &row, |p, r, _| rumble_picture(p, r, cutoff_hz, weak));
    changed.settings |= toggle(input.switched, &mut s.highpass);
    changed.settings |= drag_to(&input.picture, input.picture.rect, &mut s.highpass_hz, |pos, rect| {
        freq_at(rect.shrink2(Vec2::new(4.0, 2.0)), pos.x).clamp(*HIGHPASS_RANGE.start(), *HIGHPASS_RANGE.end()).round()
    }) | value_control(ui, &row, &input, &mut s.highpass_hz, 1.0, HIGHPASS_RANGE, HIGHPASS_HZ);

    // ---- Remove noise ----------------------------------------------------
    let noise_on = s.strength > 0.0;
    let row = Row {
        id: "noise",
        on: Some(noise_on),
        title: "Remove noise",
        explain: "Keeps your voice, drops the room",
        value: format!("{:.0}%", s.strength * 100.0),
        unit: "%",
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
        | value_control(ui, &row, &input, &mut s.strength, 0.01, 0.0..=1.0, 1.0);

    // ---- Only my voice (voice gate) -------------------------------------
    let row = Row {
        id: "voice",
        on: Some(s.voice_gate),
        title: "Only my voice",
        explain: "Silent when you're not talking",
        value: format!("{:.0}%", s.voice_threshold * 100.0),
        unit: "%",
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
    }) | value_control(ui, &row, &input, &mut s.voice_threshold, 0.01, VOICE_RANGE, VOICE_THRESHOLD);

    // ---- Quiet cut-off (level gate) --------------------------------------
    let row = Row {
        id: "level",
        on: Some(s.gate),
        title: "Quiet cut-off",
        explain: "Mutes anything below a level",
        value: format!("{:.0} dB", s.gate_threshold_db),
        unit: "dB",
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
        db.round().clamp(*GATE_THRESHOLD_RANGE.start(), *GATE_THRESHOLD_RANGE.end())
    }) | value_control(ui, &row, &input, &mut s.gate_threshold_db, 1.0, GATE_THRESHOLD_RANGE, -50.0);

    // ---- To Discord --------------------------------------------------------
    let row = Row {
        id: "out",
        on: None,
        title: "To Discord",
        explain: "How loud Discord hears you",
        value: format!("{:+.1} dB", s.output_gain_db),
        unit: "dB",
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
    changed.settings |= drag_by(&input.picture, &mut s.output_gain_db, 0.25, OUTPUT_GAIN_RANGE)
        | value_control(ui, &row, &input, &mut s.output_gain_db, 0.5, OUTPUT_GAIN_RANGE, 0.0);

    // ---- Hear yourself (headphone monitor) --------------------------------
    let row = Row {
        id: "monitor",
        on: Some(s.monitor),
        title: "Hear yourself",
        explain: "What Discord hears, in your headphones",
        value: format!("{:.0}%", s.monitor_volume * 100.0),
        unit: "%",
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
    }) | value_control(ui, &row, &input, &mut s.monitor_volume, 0.01, 0.0..=1.0, 1.0);

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
        assert!(step_by(&mut gain, 1.0, 0.5, INPUT_GAIN_RANGE));
        assert_eq!(gain, 4.0, "snaps a dragged value onto the 0.5 dB grid");
        step_by(&mut gain, -3.0, 0.5, INPUT_GAIN_RANGE);
        assert_eq!(gain, 2.5);
        let mut top = 24.0;
        assert!(!step_by(&mut top, 1.0, 0.5, INPUT_GAIN_RANGE), "clamped at the maximum");
        let mut threshold = 0.6;
        step_by(&mut threshold, 1.0, 0.01, 0.05..=0.95);
        assert!((threshold - 0.61).abs() < 1e-6);
        assert!(!step_by(&mut threshold, 0.0, 0.01, 0.05..=0.95), "no wheel, no change");
        gain = 3.37;
        assert!(!step_by(&mut gain, 0.25, 0.5, INPUT_GAIN_RANGE), "a partial notch cannot snap a dragged value");
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
        rows_output(ctx, s, before_off, frames, events).0
    }

    fn rows_output(ctx: &egui::Context, s: &mut Settings, before_off: &mut f32, frames: &[ScopeFrame], events: Vec<egui::Event>) -> (Changed, egui::FullOutput) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(620.0, 420.0))),
            events,
            ..Default::default()
        };
        let mut out = Changed::default();
        let output = ctx.run_ui(input, |ui| out = voice_rows(ui, frames, s, before_off));
        (out, output)
    }

    fn setting_mut<'a>(settings: &'a mut Settings, id: &str) -> &'a mut f32 {
        match id {
            "mic" => &mut settings.input_gain_db,
            "rumble" => &mut settings.highpass_hz,
            "noise" => &mut settings.strength,
            "voice" => &mut settings.voice_threshold,
            "level" => &mut settings.gate_threshold_db,
            "out" => &mut settings.output_gain_db,
            "monitor" => &mut settings.monitor_volume,
            _ => panic!("unknown voice control {id}"),
        }
    }

    #[test]
    fn voice_switches_report_and_draw_the_new_state_in_the_activation_frame() {
        use egui::accesskit::{Action, ActionRequest, Role, Toggled, TreeId};
        for dark in [true, false] {
            for label in ["Remove rumble", "Remove noise", "Only my voice", "Quiet cut-off", "Hear yourself"] {
                for input in ["pointer", "keyboard", "assistive"] {
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.options_mut(|options| options.max_passes = 1.try_into().unwrap());
                    ctx.enable_accesskit();
                    let mut settings = Settings { strength: 0.74321, monitor_volume: 0.43561, highpass_hz: 137.25, ..Settings::default() };
                    let strength = settings.strength;
                    let mut before_off = strength;
                    for _ in 0..3 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
                    let (_, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], Vec::new());
                    let (target, node) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                        .find(|(_, node)| node.role() == Role::CheckBox && node.label() == Some(label)).unwrap();
                    let target = *target;
                    let bounds = node.bounds().unwrap();
                    let rect = Rect::from_min_max(Pos2::new(bounds.x0 as f32, bounds.y0 as f32), Pos2::new(bounds.x1 as f32, bounds.y1 as f32));
                    let action = |action| egui::Event::AccessKitActionRequest(ActionRequest {
                        action, target_tree: TreeId::ROOT, target_node: target, data: None,
                    });
                    let space = |pressed| egui::Event::Key {
                        key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE,
                    };
                    if input == "keyboard" {
                        rows(&ctx, &mut settings, &mut before_off, &[], vec![action(Action::Focus)]);
                        for _ in 0..2 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
                    }
                    for _ in 0..2 {
                        let mut expected = settings.clone();
                        let next_on = match label {
                            "Remove rumble" => { expected.highpass = !expected.highpass; expected.highpass }
                            "Remove noise" => { expected.strength = if expected.strength > 0.0 { 0.0 } else { strength }; expected.strength > 0.0 }
                            "Only my voice" => { expected.voice_gate = !expected.voice_gate; expected.voice_gate }
                            "Quiet cut-off" => { expected.gate = !expected.gate; expected.gate }
                            "Hear yourself" => { expected.monitor = !expected.monitor; expected.monitor }
                            _ => unreachable!(),
                        };
                        let events = match input {
                            "pointer" => {
                                rows(&ctx, &mut settings, &mut before_off, &[], vec![egui::Event::PointerMoved(rect.center())]);
                                rows(&ctx, &mut settings, &mut before_off, &[], vec![button(rect.center(), true)]);
                                vec![button(rect.center(), false)]
                            }
                            "keyboard" => vec![space(true)],
                            "assistive" => vec![action(Action::Click)],
                            _ => unreachable!(),
                        };
                        let (changed, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], events);
                        assert!(changed.settings);
                        assert_eq!(changed.monitor, label == "Hear yourself");
                        assert_eq!(settings.processing(), expected.processing(), "{input} changes only {label}");
                        assert_eq!(settings.monitor, expected.monitor);
                        assert_eq!(settings.monitor_volume, expected.monitor_volume);
                        let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                            .find(|(id, _)| *id == target).unwrap().1;
                        assert_eq!(node.toggled(), Some(if next_on { Toggled::True } else { Toggled::False }),
                            "{input} immediately exposes the new {label} state");
                        assert!(output.platform_output.events.iter().any(|event| matches!(event, egui::output::OutputEvent::Clicked(info)
                            if info.label.as_deref() == Some(label) && info.selected == Some(next_on))),
                            "{input} announces the new {label} state");
                        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, Shape::Circle(knob)
                            if rect.contains(knob.center) && knob.fill == Color32::WHITE && (knob.center.x > rect.center().x) == next_on)),
                            "the {label} knob agrees with its announced state");
                        if input == "assistive" {
                            assert!(ctx.memory(|memory| memory.focused()).is_none(), "assistive clicks preserve keyboard focus");
                        }
                        let events = if input == "keyboard" { vec![space(false)] } else { Vec::new() };
                        let changed = rows(&ctx, &mut settings, &mut before_off, &[], events);
                        assert!(!changed.settings && !changed.monitor, "the action only toggles once");
                    }
                }
            }
        }
    }

    #[test]
    fn voice_pictures_expose_named_values_and_keyboard_adjustments() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for dark in [true, false] {
            for (id, label, base, raised, scale, range) in [
                ("mic", "Your mic", 0.0, 1.0, 1.0, INPUT_GAIN_RANGE),
                ("rumble", "Remove rumble", 100.0, 102.0, 1.0, HIGHPASS_RANGE),
                ("noise", "Remove noise", 0.6, 0.62, 100.0, 0.0..=1.0),
                ("voice", "Only my voice", 0.5, 0.52, 100.0, VOICE_RANGE),
                ("level", "Quiet cut-off", -50.0, -48.0, 1.0, GATE_THRESHOLD_RANGE),
                ("out", "To Discord", 0.0, 1.0, 1.0, OUTPUT_GAIN_RANGE),
                ("monitor", "Hear yourself", 0.5, 0.52, 100.0, 0.0..=1.0),
            ] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let mut settings = Settings {
                    input_gain_db: 3.37, highpass_hz: 137.25, strength: 0.74321, voice_threshold: 0.61347,
                    gate_threshold_db: -43.27, output_gain_db: -2.33, monitor_volume: 0.43561,
                    ..Settings::default()
                };
                *setting_mut(&mut settings, id) = base;
                let mut before_off = settings.strength;
                let mut expected = settings.clone();
                for _ in 0..3 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
                let (_, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], Vec::new());
                let (target, node) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                    .find(|(_, node)| node.role() == Role::Slider && node.label() == Some(label))
                    .unwrap_or_else(|| panic!("{label} exposes a named slider"));
                assert_eq!(node.numeric_value(), Some(base as f64 * scale));
                rows(&ctx, &mut settings, &mut before_off, &[], vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: Action::Focus, target_tree: TreeId::ROOT, target_node: *target, data: None,
                })]);
                for _ in 0..2 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
                assert_eq!(settings.processing(), expected.processing(), "focusing {label} preserves precise values");
                assert_eq!(settings.monitor_volume, expected.monitor_volume);
                let key = |key| egui::Event::Key {
                    key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                };
                let (changed, output) = rows_output(&ctx, &mut settings, &mut before_off, &[],
                    vec![key(egui::Key::ArrowUp), key(egui::Key::ArrowRight)]);
                let actual = *setting_mut(&mut settings, id);
                assert!((actual - raised).abs() < 1e-5, "both arrow presses adjust {label}: {actual}");
                assert!(changed.settings);
                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, *target, "arrows retain focus");
                assert!(output.platform_output.events.iter().any(|event| matches!(event, egui::output::OutputEvent::ValueChanged(info)
                    if info.label.as_deref() == Some(label))), "{label} announces keyboard value changes");
                *setting_mut(&mut expected, id) = actual;
                assert_eq!(settings.processing(), expected.processing(), "arrows change only {label}");
                assert_eq!(settings.monitor_volume, expected.monitor_volume);
                assert_eq!(settings.monitor, expected.monitor);
                let (changed, output) = rows_output(&ctx, &mut settings, &mut before_off, &[],
                    vec![key(egui::Key::ArrowDown), key(egui::Key::ArrowLeft)]);
                let actual = *setting_mut(&mut settings, id);
                assert!((actual - base).abs() < 1e-5, "both reverse arrows decrease {label}: {actual}");
                assert!(changed.settings);
                assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, *target);
                *setting_mut(&mut expected, id) = actual;
                assert_eq!(settings.processing(), expected.processing());
                assert_eq!(settings.monitor_volume, expected.monitor_volume);
                for (key_name, limit) in [(egui::Key::Home, *range.start()), (egui::Key::End, *range.end())] {
                    let changed = rows(&ctx, &mut settings, &mut before_off, &[], vec![key(key_name)]);
                    assert!(changed.settings);
                    assert_eq!(*setting_mut(&mut settings, id), limit, "{key_name:?} reaches the {label} limit");
                    *setting_mut(&mut expected, id) = limit;
                    assert_eq!(settings.processing(), expected.processing());
                    assert_eq!(settings.monitor_volume, expected.monitor_volume);
                }
            }
        }
    }

    #[test]
    fn assistive_focus_and_arrow_input_edit_only_the_requested_voice_control() {
        use egui::accesskit::{Action, ActionRequest, Role, TreeId};
        for (old, new, field) in [("Your mic", "Remove rumble", "rumble"), ("Remove rumble", "Your mic", "mic")] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut settings = Settings::default();
            let mut before_off = settings.strength;
            for _ in 0..3 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
            let (_, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], Vec::new());
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let [old, new] = [old, new].map(|label| *tree.nodes.iter()
                .find(|(_, node)| node.role() == Role::Slider && node.label() == Some(label)).map(|(id, _)| id).unwrap());
            let focus = |target_node| egui::Event::AccessKitActionRequest(ActionRequest {
                action: Action::Focus, target_tree: TreeId::ROOT, target_node, data: None,
            });
            rows(&ctx, &mut settings, &mut before_off, &[], vec![focus(old)]);
            for _ in 0..2 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
            let mut expected = settings.clone();
            *setting_mut(&mut expected, field) += if field == "mic" { 0.5 } else { 1.0 };
            let (changed, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], vec![
                focus(new), egui::Event::Key {
                    key: egui::Key::ArrowRight, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                },
            ]);
            assert!(changed.settings);
            assert_eq!(settings.processing(), expected.processing(), "the arrow belongs to the requested {field} control");
            assert_eq!(output.platform_output.accesskit_update.as_ref().unwrap().focus, new);
        }
    }

    #[test]
    fn assistive_voice_adjustments_use_visible_units_and_enforce_finite_limits() {
        use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};
        for (id, label, value, scale, step, range) in [
            ("mic", "Your mic", 3.37, 1.0, 0.5, INPUT_GAIN_RANGE),
            ("rumble", "Remove rumble", 137.25, 1.0, 1.0, HIGHPASS_RANGE),
            ("noise", "Remove noise", 0.61347, 100.0, 0.01, 0.0..=1.0),
            ("voice", "Only my voice", 0.61347, 100.0, 0.01, VOICE_RANGE),
            ("level", "Quiet cut-off", -43.27, 1.0, 1.0, GATE_THRESHOLD_RANGE),
            ("out", "To Discord", -2.33, 1.0, 0.5, OUTPUT_GAIN_RANGE),
            ("monitor", "Hear yourself", 0.43561, 100.0, 0.01, 0.0..=1.0),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut settings = Settings::default();
            let mut before_off = settings.strength;
            for _ in 0..3 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
            let (_, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], Vec::new());
            let (target, node) = output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .find(|(_, node)| node.role() == Role::Slider && node.label() == Some(label)).unwrap();
            assert_eq!(node.min_numeric_value(), Some(*range.start() as f64 * scale));
            assert_eq!(node.max_numeric_value(), Some(*range.end() as f64 * scale));
            assert!((node.numeric_value_step().unwrap() - step as f64 * scale).abs() < 1e-6);
            assert!(node.supports_action(Action::SetValue));
            let action = |action, data| egui::Event::AccessKitActionRequest(ActionRequest {
                action, target_tree: TreeId::ROOT, target_node: *target, data,
            });
            let mut expected = settings.clone();
            let (changed, output) = rows_output(&ctx, &mut settings, &mut before_off, &[],
                vec![action(Action::SetValue, Some(ActionData::NumericValue(value as f64 * scale)))]);
            assert!(changed.settings);
            assert_eq!(*setting_mut(&mut settings, id), value, "assistive {label} values use its displayed unit");
            *setting_mut(&mut expected, id) = value;
            assert_eq!(settings.processing(), expected.processing());
            assert_eq!(settings.monitor_volume, expected.monitor_volume);
            let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
                .find(|(node_id, _)| node_id == target).unwrap().1;
            assert_eq!(node.numeric_value(), Some(value as f64 * scale), "the new value is exposed immediately");
            for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                assert!(!rows(&ctx, &mut settings, &mut before_off, &[],
                    vec![action(Action::SetValue, Some(ActionData::NumericValue(invalid)))]).settings);
                assert_eq!(*setting_mut(&mut settings, id), value);
            }
            for (outside, limit) in [(-1e6, *range.start()), (1e6, *range.end())] {
                rows(&ctx, &mut settings, &mut before_off, &[],
                    vec![action(Action::SetValue, Some(ActionData::NumericValue(outside)))]);
                assert_eq!(*setting_mut(&mut settings, id), limit);
                *setting_mut(&mut expected, id) = limit;
                assert_eq!(settings.processing(), expected.processing());
                assert_eq!(settings.monitor_volume, expected.monitor_volume);
            }
            assert!(!rows(&ctx, &mut settings, &mut before_off, &[], vec![action(Action::Increment, None)]).settings);
            rows(&ctx, &mut settings, &mut before_off, &[], vec![action(Action::Decrement, None)]);
            assert!((*setting_mut(&mut settings, id) - (*range.end() - step)).abs() < 1e-5);
            assert_eq!(settings.monitor, expected.monitor);
            assert!(ctx.memory(|memory| memory.focused()).is_none(), "assistive edits do not steal keyboard focus");
        }
    }

    #[test]
    fn voice_tab_navigation_skips_empty_switch_gaps() {
        use egui::accesskit::Role;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut settings = Settings::default();
        let original = settings.processing();
        let mut before_off = settings.strength;
        let mut visited = std::collections::HashSet::new();
        for _ in 0..3 { rows(&ctx, &mut settings, &mut before_off, &[], Vec::new()); }
        for _ in 0..30 {
            let (_, output) = rows_output(&ctx, &mut settings, &mut before_off, &[], vec![egui::Event::Key {
                key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
            }]);
            if ctx.memory(|memory| memory.focused()).is_none() { continue; }
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            let node = &tree.nodes.iter().find(|(id, _)| *id == tree.focus).unwrap().1;
            assert!(node.label().is_some_and(|label| !label.is_empty()), "every Tab stop names a usable control: {node:?}");
            if node.role() == Role::Slider { visited.insert(node.label().unwrap().to_owned()); }
        }
        assert_eq!(visited, ["Your mic", "Remove rumble", "Remove noise", "Only my voice", "Quiet cut-off", "To Discord", "Hear yourself"]
            .into_iter().map(str::to_owned).collect());
        assert_eq!(settings.processing(), original, "Tab never changes a setting");
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
            let mut changed = false;
            for events in [
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true)],
                vec![egui::Event::PointerMoved(from + (to - from) * 0.75)],
                vec![button(to, false)],
            ] {
                changed = rows(ctx, s, before_off, &[], events).settings;
            }
            assert!(changed, "the release reports its final movement as a settings change");
        };
        let mic = picture(&ctx, &mut s, "mic");
        drag(&ctx, &mut s, &mut before_off, mic.center(), mic.center() - Vec2::new(0.0, 20.0));
        assert!((s.input_gain_db - 5.0).abs() < 1e-4, "gain includes the final movement on release: {}", s.input_gain_db);

        let rumble = picture(&ctx, &mut s, "rumble");
        let target = freq_x(rumble.shrink2(Vec2::new(4.0, 2.0)), 150.0);
        drag(&ctx, &mut s, &mut before_off, rumble.center(), Pos2::new(target, rumble.center().y));
        assert!((s.highpass_hz - 150.0).abs() <= 2.0, "the cutoff follows the pointer: {}", s.highpass_hz);

        let voice = picture(&ctx, &mut s, "voice");
        let tenth = Pos2::new(voice.center().x, voice.bottom() - 2.0 - (voice.height() - 4.0) * 0.1);
        drag(&ctx, &mut s, &mut before_off, voice.center(), tenth);
        assert!((s.voice_threshold - 0.1).abs() <= 0.02, "the threshold follows the pointer through release: {}", s.voice_threshold);
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
