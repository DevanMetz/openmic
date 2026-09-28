//! Live scope and control surface: every processing setting is drawn as what
//! it does to the audio, and changed by dragging that drawing.
//!
//! - blue trace (your mic): drag up/down for input gain
//! - green trace (to Discord): drag up/down for output gain
//! - sliders beside the scope: input gain, output gain, noise reduction
//! - dashed amber line: level-gate threshold
//! - voice chart: drag the dashed line for the voice-gate threshold
//! - rumble curve: drag left/right for the filter cutoff
//!
//! Scroll the wheel over any of them for fine steps; double-click to reset.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Galley, Id, Pos2, Rect, Sense, Shape, Stroke, Vec2,
};

use crate::config::Settings;
use crate::dsp::{self, Model, HIGHPASS_HZ, HIGHPASS_RANGE, VOICE_THRESHOLD};
use crate::engine::{ScopeFrame, SCOPE_FRAMES};
use crate::gui::{AMBER, CYAN, GREEN, RED, VIOLET};
const FLOOR_DB: f32 = -80.0;
const LEVEL_HEIGHT: f32 = 150.0;
/// Room above the level plot for the draggable legend chips.
const HEADER: f32 = 20.0;
const SMALL_HEIGHT: f32 = 80.0;
const INPUT_GAIN: std::ops::RangeInclusive<f32> = -12.0..=24.0;
const OUTPUT_GAIN: std::ops::RangeInclusive<f32> = -24.0..=12.0;
const GATE_THRESHOLD: std::ops::RangeInclusive<f32> = -80.0..=-20.0;
/// Pointer distance (px) at which a line or trace can be grabbed.
const GRAB: f32 = 8.0;

/// A setting the user is pointing at, in the panel or on the scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    Model,
    Reduction,
    InputGain,
    OutputGain,
    LevelGate,
    VoiceGate,
    Rumble,
}

impl Focus {
    fn caption(self) -> &'static str {
        match self {
            Focus::Model => "Blue is your mic, green is what Discord hears; the gap is the noise removed.",
            Focus::Reduction => "Reduction: lower keeps some room tone, 100% removes all it can (1% per notch).",
            Focus::InputGain => "Your mic: the slider or the blue line sets input gain (0.5 dB per notch).",
            Focus::OutputGain => "To Discord: the slider or the green line sets what Discord hears (0.5 dB per notch).",
            Focus::LevelGate => "Drag or scroll the dashed line: anything quieter fades out (1 dB per notch).",
            Focus::VoiceGate => {
                "Drag or scroll: when violet (voice certainty) is above the line the gate opens (1% per notch)."
            }
            Focus::Rumble => "Drag sideways or scroll: everything left of the curve is cut (1 Hz per notch).",
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Focus::Rumble => CursorIcon::ResizeHorizontal,
            Focus::Model => CursorIcon::Default,
            _ => CursorIcon::ResizeVertical,
        }
    }
}

/// Draw the scope and apply any drags to `s`. Returns true if a setting changed.
pub fn scope(
    ui: &mut egui::Ui,
    frames: &[ScopeFrame],
    s: &mut Settings,
    panel_focus: Option<Focus>,
    advanced: bool,
) -> bool {
    let mut changed = false;
    let mut pointed = None;
    let width = ui.available_width();
    pointed = level_chart(ui, width, frames, s, panel_focus, &mut changed).or(pointed);
    // The voice and rumble charts tune the gates and the filter: Advanced only.
    if advanced {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let spacing = ui.spacing().item_spacing.x;
            let voice_width = (width - spacing) * 0.64;
            pointed = voice_chart(ui, voice_width, frames, s, panel_focus, &mut changed).or(pointed);
            pointed = rumble_chart(ui, width - spacing - voice_width, s, panel_focus, &mut changed)
                .or(pointed);
        });
    }
    let caption = panel_focus
        .or(pointed)
        .map_or("Drag the lines or scroll over them to adjust; double-click one to reset it.", Focus::caption);
    ui.weak(caption);
    changed
}

/// Room the gain sliders take to the right of the scope.
pub const SLIDERS_WIDTH: f32 = 196.0;
const METER_WIDTH: f32 = 7.0;
const METER_GAP: f32 = 4.0;
/// The bottom of the strip's level meters, dBFS.
const METER_FLOOR: f32 = -60.0;

fn to_meter_db(peak: f32) -> f32 {
    (20.0 * peak.max(1e-6).log10()).clamp(METER_FLOOR, 0.0)
}

/// A level meter filling upward: green to -18 dB, amber to -6, red above,
/// with a peak-hold tick. `db` is `None` when nothing is being processed.
fn vertical_meter(painter: &egui::Painter, rect: Rect, db: Option<f32>, hold: f32) {
    let visuals_track = Color32::from_gray(128).gamma_multiply(0.18);
    painter.rect_filled(rect, 3.0, visuals_track);
    let Some(db) = db else { return };
    let y = |d: f32| rect.bottom() - (d - METER_FLOOR) / -METER_FLOOR * rect.height();
    for (from, to, color) in [(METER_FLOOR, -18.0, GREEN), (-18.0, -6.0, AMBER), (-6.0, 0.0, RED)] {
        if db > from {
            let segment = Rect::from_x_y_ranges(rect.x_range(), y(db.min(to))..=y(from));
            painter.rect_filled(segment, 3.0, color);
        }
    }
    if hold > METER_FLOOR + 1.0 {
        let at = y(hold);
        painter.line_segment([Pos2::new(rect.left(), at), Pos2::new(rect.right(), at)], Stroke::new(2.0, Color32::WHITE));
    }
}

/// Mixer-style strip beside the scope: your mic (input gain) and to Discord
/// (output gain), each with its level meter, and noise reduction. The wheel
/// steps each slider and a double-click resets it. `levels` are the input
/// and output peaks (linear) while processing; `hold` their peak-hold marks
/// in dBFS. Returns whether a setting changed, and which one the pointer is
/// on (to highlight it on the scope).
pub fn gain_sliders(
    ui: &mut egui::Ui,
    s: &mut Settings,
    levels: [Option<f32>; 2],
    hold: &mut [f32; 2],
    advanced: bool,
) -> (bool, Option<Focus>) {
    let mut changed = false;
    let mut pointed = None;
    // As tall as the scope beside it.
    let height = if advanced { LEVEL_HEIGHT + SMALL_HEIGHT - 44.0 } else { LEVEL_HEIGHT - 40.0 };
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.spacing_mut().slider_width = height;
        let column = (SLIDERS_WIDTH - 2.0 * 6.0) / 3.0;
        let controls: [(Focus, &str, Color32); 3] = [
            (Focus::InputGain, "Your mic", CYAN),
            (Focus::OutputGain, "To Discord", GREEN),
            (Focus::Reduction, "Reduction", AMBER),
        ];
        for (target, label, color) in controls {
            let size = Vec2::new(column, height + 40.0);
            ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.set_width(column);
                ui.label(egui::RichText::new(label).small().color(color));
                let slider = match target {
                    Focus::InputGain => egui::Slider::new(&mut s.input_gain_db, INPUT_GAIN),
                    Focus::OutputGain => egui::Slider::new(&mut s.output_gain_db, OUTPUT_GAIN),
                    _ => egui::Slider::new(&mut s.strength, 0.0..=1.0),
                };
                let slider = slider.vertical().show_value(false).trailing_fill(true);
                let meter = match target {
                    Focus::InputGain => Some(0),
                    Focus::OutputGain => Some(1),
                    _ => None,
                };
                // Centred in its column under the label, with its meter.
                let thumb = ui.spacing().interact_size.y;
                let strip = thumb + if meter.is_some() { METER_GAP + METER_WIDTH } else { 0.0 };
                let response = ui
                    .horizontal(|ui| {
                        ui.add_space((column - strip) / 2.0);
                        let response = ui.add(slider);
                        if let Some(i) = meter {
                            let db = levels[i].map(to_meter_db);
                            let now = db.unwrap_or(METER_FLOOR);
                            hold[i] = if now >= hold[i] { now } else { (hold[i] - 20.0 * dt).max(now) };
                            ui.add_space(METER_GAP - ui.spacing().item_spacing.x);
                            let (rect, meter_response) =
                                ui.allocate_exact_size(Vec2::new(METER_WIDTH, response.rect.height()), Sense::hover());
                            vertical_meter(ui.painter(), rect, db, hold[i]);
                            meter_response.on_hover_text(match db {
                                Some(db) => format!("{} level {db:.0} dBFS", if i == 0 { "Input" } else { "Output" }),
                                None => "Start OpenMic to see levels".into(),
                            });
                        }
                        response
                    })
                    .inner;
                let response = response.on_hover_text("Scroll for fine steps; double-click to reset");
                changed |= response.changed();
                // Sliders only sense drags, so read the double-click directly.
                let double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
                if double && response.hovered() {
                    reset(s, target);
                    changed = true;
                }
                let notches = wheel_notches(ui, response.id, response.hovered() && !response.dragged());
                changed |= scroll(s, target, notches);
                if response.hovered() || response.dragged() {
                    pointed = Some(target);
                }
                let value = match target {
                    Focus::InputGain => format!("{:+.1} dB", s.input_gain_db),
                    Focus::OutputGain => format!("{:+.1} dB", s.output_gain_db),
                    _ => format!("{:.0}%", s.strength * 100.0),
                };
                ui.label(egui::RichText::new(value).small().monospace());
            });
        }
    });
    (changed, pointed)
}

struct Chart {
    rect: Rect,
    response: egui::Response,
    painter: egui::Painter,
    weak: Color32,
    grid: Color32,
    popup: Color32,
}

impl Chart {
    fn new(ui: &mut egui::Ui, width: f32, height: f32) -> Self {
        let (response, painter) =
            ui.allocate_painter(Vec2::new(width, height), Sense::click_and_drag());
        let visuals = ui.visuals();
        painter.rect_filled(response.rect, 4.0, visuals.extreme_bg_color);
        Self {
            rect: response.rect.shrink(1.0),
            response,
            painter,
            weak: visuals.weak_text_color(),
            grid: visuals.widgets.noninteractive.bg_stroke.color,
            popup: visuals.window_fill,
        }
    }

    fn id(&self) -> Id {
        self.response.id
    }

    /// x position of frame `i` in a history of `len`, newest at the right.
    fn x(&self, i: usize, len: usize) -> f32 {
        let slot = SCOPE_FRAMES - len + i;
        self.rect.left() + self.rect.width() * slot as f32 / (SCOPE_FRAMES - 1) as f32
    }

    fn label(&self, pos: Pos2, anchor: Align2, text: impl ToString, color: Color32) -> Rect {
        self.painter.text(pos, anchor, text, FontId::proportional(11.0), color)
    }

    fn hline_dashed(&self, y: f32, stroke: Stroke) {
        let line = [Pos2::new(self.rect.left(), y), Pos2::new(self.rect.right(), y)];
        self.painter.extend(Shape::dashed_line(&line, stroke, 5.0, 4.0));
    }

    /// Filled band between two traces, one quad per step.
    fn band(&self, xs: &[f32], top: &[f32], bottom: &[f32], fill: Color32) {
        for i in 1..xs.len() {
            let quad = vec![
                Pos2::new(xs[i - 1], top[i - 1]),
                Pos2::new(xs[i], top[i]),
                Pos2::new(xs[i], bottom[i]),
                Pos2::new(xs[i - 1], bottom[i - 1]),
            ];
            self.painter.add(Shape::convex_polygon(quad, fill, Stroke::NONE));
        }
    }

    fn trace(&self, xs: &[f32], ys: &[f32], stroke: Stroke) {
        let points = xs.iter().zip(ys).map(|(&x, &y)| Pos2::new(x, y)).collect();
        self.painter.add(Shape::line(points, stroke));
    }

    fn idle(&self, text: &str) {
        self.label(self.rect.center(), Align2::CENTER_CENTER, text, self.weak);
    }

    /// Value readout next to the pointer while dragging.
    fn drag_readout(&self, response: &egui::Response, text: String) {
        if let Some(pos) = response.interact_pointer_pos() {
            let at = pos + Vec2::new(12.0, -12.0);
            let galley = self.painter.layout_no_wrap(text, FontId::proportional(12.0), self.weak);
            let bg = Rect::from_min_size(at, galley.size()).expand(3.0);
            self.painter.rect_filled(bg, 3.0, self.popup);
            self.painter.galley(at, galley, self.weak);
        }
    }
}

/// What a drag started on a chart is moving; remembered for the whole drag.
fn drag_target(ui: &egui::Ui, id: Id, response: &egui::Response, hit: Option<Focus>) -> Option<Focus> {
    if response.drag_started() {
        ui.data_mut(|d| d.insert_temp(id, hit));
    }
    if response.dragged() {
        ui.data(|d| d.get_temp::<Option<Focus>>(id)).flatten()
    } else {
        None
    }
}

fn emphasis(focus: Option<Focus>, targets: &[Focus]) -> (f32, f32) {
    // (fill alpha multiplier, stroke width)
    if focus.is_some_and(|f| targets.contains(&f)) {
        (2.0, 2.5)
    } else if focus.is_some() {
        (0.6, 1.0)
    } else {
        (1.0, 1.5)
    }
}

fn nudge(value: &mut f32, delta: f32, range: std::ops::RangeInclusive<f32>) -> bool {
    let next = (*value + delta).clamp(*range.start(), *range.end());
    let changed = next != *value;
    *value = next;
    changed
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

/// Fine adjustment of `target` by mouse-wheel notches.
fn scroll(s: &mut Settings, target: Focus, notches: f32) -> bool {
    match target {
        Focus::InputGain => step_by(&mut s.input_gain_db, notches, 0.5, INPUT_GAIN),
        Focus::OutputGain => step_by(&mut s.output_gain_db, notches, 0.5, OUTPUT_GAIN),
        Focus::Reduction => step_by(&mut s.strength, notches, 0.01, 0.0..=1.0),
        Focus::LevelGate => step_by(&mut s.gate_threshold_db, notches, 1.0, GATE_THRESHOLD),
        Focus::VoiceGate => step_by(&mut s.voice_threshold, notches, 0.01, 0.05..=0.95),
        Focus::Rumble => step_by(&mut s.highpass_hz, notches, 1.0, HIGHPASS_RANGE),
        Focus::Model => false,
    }
}

fn reset(s: &mut Settings, target: Focus) {
    match target {
        Focus::InputGain => s.input_gain_db = 0.0,
        Focus::OutputGain => s.output_gain_db = 0.0,
        Focus::Reduction => s.strength = 1.0,
        Focus::LevelGate => s.gate_threshold_db = -50.0,
        Focus::VoiceGate => s.voice_threshold = VOICE_THRESHOLD,
        Focus::Rumble => s.highpass_hz = HIGHPASS_HZ,
        Focus::Model => {}
    }
}

/// Level chart y for `db`: 0 dBFS at the bottom of the header strip.
fn level_y(rect: Rect, db: f32) -> f32 {
    let t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
    rect.bottom() - t * (rect.height() - HEADER)
}

fn level_chart(
    ui: &mut egui::Ui,
    width: f32,
    frames: &[ScopeFrame],
    s: &mut Settings,
    panel: Option<Focus>,
    changed: &mut bool,
) -> Option<Focus> {
    let c = Chart::new(ui, width, LEVEL_HEIGHT);
    let rect = c.rect;
    let y = |db: f32| level_y(rect, db);
    let db_per_px = -FLOOR_DB / (rect.height() - HEADER);
    let n = frames.len();
    let xs: Vec<f32> = (0..n).map(|i| c.x(i, n)).collect();

    // Legend: which trace is which (the sliders beside the scope set them).
    let chip_font = FontId::proportional(11.0);
    let chips: [(Focus, String, Color32); 2] = [
        (Focus::InputGain, "your mic".to_owned(), CYAN),
        (Focus::OutputGain, "to Discord".to_owned(), GREEN),
    ];
    let mut chip_x = rect.left() + 8.0;
    let mid = rect.top() + HEADER / 2.0;
    let mut chip_layout: Vec<(Focus, Rect, Arc<Galley>, Color32)> = Vec::new();
    for (target, text, color) in chips {
        let galley = c.painter.layout_no_wrap(text, chip_font.clone(), color);
        let hit = Rect::from_min_size(
            Pos2::new(chip_x, mid - galley.size().y / 2.0),
            Vec2::new(galley.size().x + 12.0, galley.size().y),
        )
        .expand2(Vec2::new(3.0, 2.0));
        chip_x = hit.right() + 10.0;
        chip_layout.push((target, hit, galley, color));
    }

    // Body: grab the level-gate line or whichever trace is under the pointer.
    let hit_test = |pos: Pos2| -> Option<Focus> {
        if pos.y < rect.top() + HEADER {
            return None;
        }
        if s.gate && (pos.y - y(s.gate_threshold_db)).abs() < GRAB {
            return Some(Focus::LevelGate);
        }
        if n == 0 {
            return None;
        }
        let i = (((pos.x - xs[0]) / (xs.last().unwrap() - xs[0]).max(1.0)) * (n - 1) as f32)
            .round()
            .clamp(0.0, (n - 1) as f32) as usize;
        let d_in = (pos.y - y(frames[i].input_db)).abs();
        let d_out = (pos.y - y(frames[i].output_db)).abs();
        match (d_in < GRAB * 2.0, d_out < GRAB * 2.0) {
            (true, true) => Some(if d_out <= d_in { Focus::OutputGain } else { Focus::InputGain }),
            (true, false) => Some(Focus::InputGain),
            (false, true) => Some(Focus::OutputGain),
            _ => None,
        }
    };
    let body = &c.response;
    let hovered_target = body.hover_pos().and_then(hit_test);
    let press = ui.input(|i| i.pointer.press_origin()).or(body.hover_pos());
    let grabbed = drag_target(ui, c.id(), body, press.and_then(hit_test));
    let pointed = grabbed.or(hovered_target);
    let dy = -body.drag_delta().y;
    match grabbed {
        Some(Focus::LevelGate) => {
            if let Some(pos) = body.interact_pointer_pos() {
                let db = FLOOR_DB * (pos.y - (rect.top() + HEADER)) / (rect.height() - HEADER);
                let db = db.clamp(*GATE_THRESHOLD.start(), *GATE_THRESHOLD.end());
                *changed |= db != s.gate_threshold_db;
                s.gate_threshold_db = db;
            }
            c.drag_readout(body, format!("level gate {:.0} dB", s.gate_threshold_db));
        }
        Some(Focus::InputGain) => {
            *changed |= nudge(&mut s.input_gain_db, dy * db_per_px, INPUT_GAIN);
            c.drag_readout(body, format!("input gain {:+.1} dB", s.input_gain_db));
        }
        Some(Focus::OutputGain) => {
            *changed |= nudge(&mut s.output_gain_db, dy * db_per_px, OUTPUT_GAIN);
            c.drag_readout(body, format!("output gain {:+.1} dB", s.output_gain_db));
        }
        _ => {}
    }
    if body.double_clicked()
        && let Some(target) = hovered_target
    {
        reset(s, target);
        *changed = true;
    }

    let dragging = body.dragged();
    if let Some(target) = pointed {
        ui.ctx().set_cursor_icon(target.cursor());
    }
    for target in [Focus::InputGain, Focus::OutputGain, Focus::LevelGate] {
        let notches = wheel_notches(ui, c.id().with(target), pointed == Some(target) && !dragging);
        *changed |= scroll(s, target, notches);
    }
    let focus = panel.or(pointed);

    // ---- paint ----
    for db in [-20.0, -40.0, -60.0] {
        c.painter.line_segment(
            [Pos2::new(rect.left(), y(db)), Pos2::new(rect.right(), y(db))],
            Stroke::new(1.0, c.grid),
        );
        c.label(Pos2::new(rect.left() + 4.0, y(db) + 1.0), Align2::LEFT_TOP, format!("{db} dB"), c.weak);
    }

    if n == 0 {
        c.idle("Start OpenMic to see your voice being cleaned live");
    } else {
        let input: Vec<f32> = frames.iter().map(|f| y(f.input_db)).collect();
        let output: Vec<f32> = frames.iter().map(|f| y(f.output_db)).collect();
        let floor = vec![rect.bottom(); n];
        let column = |i: usize, top: f32| {
            let right = if i + 1 < n { xs[i + 1] } else { rect.right() };
            Rect::from_x_y_ranges(xs[i]..=right, top..=rect.bottom())
        };

        // Voice gate closed: everything in these stretches was muted.
        if s.voice_gate && !s.bypass {
            let alpha = if focus == Some(Focus::VoiceGate) { 0.22 } else { 0.08 };
            for i in (0..n).filter(|&i| frames[i].voice_gate < 0.5) {
                c.painter.rect_filled(column(i, rect.top() + HEADER), 0.0, RED.gamma_multiply(alpha));
            }
        }
        // Level gate closing: quiet stretches it is fading out.
        if s.gate && !s.bypass {
            let alpha = if focus == Some(Focus::LevelGate) { 0.25 } else { 0.1 };
            for i in (0..n).filter(|&i| frames[i].level_gate < 0.5) {
                c.painter.rect_filled(column(i, y(s.gate_threshold_db)), 0.0, AMBER.gamma_multiply(alpha));
            }
        }

        let (in_fill, in_width) = emphasis(focus, &[Focus::InputGain]);
        let (out_fill, out_width) = emphasis(focus, &[Focus::OutputGain, Focus::Model]);
        c.band(&xs, &input, &floor, CYAN.gamma_multiply(0.12 * in_fill));
        if matches!(focus, Some(Focus::Reduction | Focus::Model)) {
            c.band(&xs, &input, &output, AMBER.gamma_multiply(0.35));
        }
        c.band(&xs, &output, &floor, GREEN.gamma_multiply(0.22 * out_fill));
        c.trace(&xs, &input, Stroke::new(in_width, CYAN));
        c.trace(&xs, &output, Stroke::new(out_width, GREEN));

        // Reduction over the last second, in power terms.
        let recent = &frames[n.saturating_sub(100)..];
        let power = |db: f32| 10f32.powf(db / 10.0);
        let p_in: f32 = recent.iter().map(|f| power(f.input_db)).sum();
        let p_out: f32 = recent.iter().map(|f| power(f.output_db)).sum();
        let reduced = 10.0 * (p_in / p_out.max(1e-12)).log10();
        let text = if s.mute {
            "muted".to_string()
        } else if s.bypass {
            "bypassed".to_string()
        } else {
            format!("removed {:.0} dB", reduced.clamp(0.0, 99.0))
        };
        c.label(Pos2::new(rect.right() - 6.0, mid), Align2::RIGHT_CENTER, text, AMBER);
    }

    // Level gate threshold line.
    let (_, gate_width) = emphasis(focus, &[Focus::LevelGate]);
    let gate_y = y(s.gate_threshold_db);
    if s.gate {
        c.hline_dashed(gate_y, Stroke::new(gate_width, AMBER));
        let text = format!("level gate {:.0} dB", s.gate_threshold_db);
        c.label(Pos2::new(rect.right() - 6.0, gate_y - 1.0), Align2::RIGHT_BOTTOM, text, AMBER);
    } else if focus == Some(Focus::LevelGate) {
        c.hline_dashed(gate_y, Stroke::new(1.0, c.weak));
        c.label(Pos2::new(rect.right() - 6.0, gate_y - 1.0), Align2::RIGHT_BOTTOM, "level gate (off)", c.weak);
    }

    // Chips.
    for (target, hit, galley, color) in chip_layout {
        let active = focus == Some(target);
        if active {
            c.painter.rect_filled(hit, 4.0, color.gamma_multiply(0.15));
        }
        c.painter.circle_filled(Pos2::new(hit.left() + 6.0, hit.center().y), 3.5, color);
        c.painter.galley(Pos2::new(hit.left() + 13.0, hit.center().y - galley.size().y / 2.0), galley, color);
    }

    let model = match s.model {
        Model::DeepFilter => "DeepFilterNet 3",
        Model::Rnnoise => "RNNoise",
    };
    let model_color = if focus == Some(Focus::Model) { GREEN } else { c.weak };
    c.label(rect.right_bottom() + Vec2::new(-6.0, -3.0), Align2::RIGHT_BOTTOM, model, model_color);
    pointed
}

fn voice_chart(
    ui: &mut egui::Ui,
    width: f32,
    frames: &[ScopeFrame],
    s: &mut Settings,
    panel: Option<Focus>,
    changed: &mut bool,
) -> Option<Focus> {
    let c = Chart::new(ui, width, SMALL_HEIGHT);
    let strip = 7.0;
    let plot = Rect::from_min_max(
        c.rect.min + Vec2::new(0.0, 16.0),
        Pos2::new(c.rect.right(), c.rect.bottom() - strip - 2.0),
    );
    let y = |p: f32| plot.bottom() - p.clamp(0.0, 1.0) * plot.height();

    // The whole chart drags the threshold: it is the only control here.
    let body = &c.response;
    let pointed = (body.hovered() || body.dragged()).then_some(Focus::VoiceGate);
    if body.dragged() {
        if let Some(pos) = body.interact_pointer_pos() {
            let p = ((plot.bottom() - pos.y) / plot.height()).clamp(0.05, 0.95);
            *changed |= p != s.voice_threshold;
            s.voice_threshold = p;
        }
        c.drag_readout(body, format!("voice threshold {:.0}%", s.voice_threshold * 100.0));
    }
    if body.double_clicked() {
        reset(s, Focus::VoiceGate);
        *changed = true;
    }
    let notches = wheel_notches(ui, c.id(), body.hovered() && !body.dragged());
    *changed |= scroll(s, Focus::VoiceGate, notches);
    if let Some(target) = pointed {
        ui.ctx().set_cursor_icon(target.cursor());
    }
    let focus = panel.or(pointed);
    let (fill, width_px) = emphasis(focus, &[Focus::VoiceGate]);

    if frames.is_empty() {
        c.idle("voice detector");
    } else {
        let n = frames.len();
        let xs: Vec<f32> = (0..n).map(|i| c.x(i, n)).collect();
        let prob: Vec<f32> = frames.iter().map(|f| y(f.prob)).collect();
        c.band(&xs, &prob, &vec![plot.bottom(); n], VIOLET.gamma_multiply(0.25 * fill));
        c.trace(&xs, &prob, Stroke::new(width_px, VIOLET));

        // Gate state strip.
        for i in 0..n {
            let right = if i + 1 < n { xs[i + 1] } else { c.rect.right() };
            let cell = Rect::from_x_y_ranges(xs[i]..=right, (c.rect.bottom() - strip)..=c.rect.bottom());
            let color = if !s.voice_gate || s.bypass {
                c.grid
            } else if frames[i].voice_gate >= 0.5 {
                GREEN
            } else {
                RED.gamma_multiply(0.6)
            };
            c.painter.rect_filled(cell, 0.0, color);
        }
    }

    let line_color = if s.voice_gate { VIOLET } else { c.weak };
    c.hline_dashed(y(s.voice_threshold), Stroke::new(width_px.max(1.0), line_color));
    let title = if s.voice_gate {
        format!("voice gate · threshold {:.0}%", s.voice_threshold * 100.0)
    } else {
        "voice gate (off)".to_string()
    };
    c.label(c.rect.left_top() + Vec2::new(6.0, 3.0), Align2::LEFT_TOP, title, line_color);
    pointed
}

fn rumble_chart(
    ui: &mut egui::Ui,
    width: f32,
    s: &mut Settings,
    panel: Option<Focus>,
    changed: &mut bool,
) -> Option<Focus> {
    let c = Chart::new(ui, width, SMALL_HEIGHT);
    let (lo, hi) = (20f32.log10(), 1000f32.log10());
    let rect = c.rect;
    let x = |f: f32| rect.left() + (f.log10() - lo) / (hi - lo) * rect.width();
    let (min_db, max_db) = (-48.0, 3.0);
    let top = rect.top() + 16.0;
    let y = |db: f32| top + (max_db - db.clamp(min_db, max_db)) / (max_db - min_db) * (rect.bottom() - top);

    // The whole chart drags the cutoff: it is the only control here.
    let body = &c.response;
    let pointed = (body.hovered() || body.dragged()).then_some(Focus::Rumble);
    if body.dragged() {
        if let Some(pos) = body.interact_pointer_pos() {
            let f = 10f32.powf(lo + (pos.x - rect.left()) / rect.width() * (hi - lo));
            let f = f.clamp(*HIGHPASS_RANGE.start(), *HIGHPASS_RANGE.end()).round();
            *changed |= f != s.highpass_hz;
            s.highpass_hz = f;
        }
        c.drag_readout(body, format!("cut below {:.0} Hz", s.highpass_hz));
    }
    if body.double_clicked() {
        reset(s, Focus::Rumble);
        *changed = true;
    }
    let notches = wheel_notches(ui, c.id(), body.hovered() && !body.dragged());
    *changed |= scroll(s, Focus::Rumble, notches);
    if let Some(target) = pointed {
        ui.ctx().set_cursor_icon(target.cursor());
    }
    let focus = panel.or(pointed);
    let (fill, width_px) = emphasis(focus, &[Focus::Rumble]);
    let color = if s.highpass { AMBER } else { c.weak };

    let freqs: Vec<f32> = (0..=60).map(|i| 10f32.powf(lo + (hi - lo) * i as f32 / 60.0)).collect();
    let xs: Vec<f32> = freqs.iter().map(|&f| x(f)).collect();
    let curve: Vec<f32> = freqs
        .iter()
        .map(|&f| y(if s.highpass { dsp::highpass_response_db(f, s.highpass_hz) } else { 0.0 }))
        .collect();
    if s.highpass {
        // The removed region: between flat (0 dB) and the filter curve.
        c.band(&xs, &vec![y(0.0); xs.len()], &curve, AMBER.gamma_multiply(0.18 * fill));
    }
    c.trace(&xs, &curve, Stroke::new(width_px, color));

    let cutoff = x(s.highpass_hz);
    c.painter.line_segment(
        [Pos2::new(cutoff, top), Pos2::new(cutoff, rect.bottom())],
        Stroke::new(1.0, c.grid),
    );
    c.label(
        Pos2::new(cutoff + 3.0, rect.bottom() - 3.0),
        Align2::LEFT_BOTTOM,
        format!("{:.0} Hz", s.highpass_hz),
        c.weak,
    );
    let title = if s.highpass { "rumble filter" } else { "rumble filter (off)" };
    c.label(rect.left_top() + Vec2::new(6.0, 3.0), Align2::LEFT_TOP, title, color);
    pointed
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

    /// Run the sliders for a few frames with the pointer over column
    /// `column` (0 = your mic), sending `events` on the middle frame.
    fn slide(s: &mut Settings, column: usize, events: Vec<egui::Event>) -> Option<Focus> {
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(SLIDERS_WIDTH, 260.0));
        let x = (column as f32 + 0.5) * SLIDERS_WIDTH / 3.0;
        let at = Pos2::new(x, 120.0);
        let mut pointed = None;
        for events in [vec![egui::Event::PointerMoved(at)], events, Vec::new()] {
            let input = egui::RawInput { screen_rect: Some(area), events, ..Default::default() };
            let _ = ctx.run_ui(input, |ui| pointed = gain_sliders(ui, s, [Some(0.5), None], &mut [-60.0; 2], true).1);
        }
        pointed
    }

    #[test]
    fn scope_sliders_step_with_the_wheel_and_reset_on_double_click() {
        let wheel = |lines: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, lines),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        };
        let mut s = Settings::default();
        assert_eq!(slide(&mut s, 0, vec![wheel(2.0)]), Some(Focus::InputGain));
        assert_eq!(s.input_gain_db, 1.0, "0.5 dB per notch");
        assert_eq!(slide(&mut s, 1, vec![wheel(-3.0)]), Some(Focus::OutputGain));
        assert_eq!(s.output_gain_db, -1.5);
        assert_eq!(slide(&mut s, 2, vec![wheel(-5.0)]), Some(Focus::Reduction));
        assert!((s.strength - 0.95).abs() < 1e-6, "1% per notch");

        let click = |pressed| egui::Event::PointerButton {
            pos: Pos2::new(SLIDERS_WIDTH / 6.0, 120.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let ctx = egui::Context::default();
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(SLIDERS_WIDTH, 260.0));
        s.input_gain_db = 6.0;
        for events in [
            vec![egui::Event::PointerMoved(Pos2::new(SLIDERS_WIDTH / 6.0, 120.0))],
            vec![click(true)],
            vec![click(false)],
            vec![click(true)],
            vec![click(false)],
            Vec::new(),
        ] {
            let input = egui::RawInput { screen_rect: Some(area), events, ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                gain_sliders(ui, &mut s, [None; 2], &mut [-60.0; 2], true);
            });
        }
        assert_eq!(s.input_gain_db, 0.0, "a double-click resets to 0 dB");
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
}
