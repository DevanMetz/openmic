//! Small custom widgets for the main window: cards, toggle pills, a
//! segmented picker and level meters.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect,
    Response, Sense, Stroke, StrokeKind, Vec2,
};

use crate::gui::{AMBER, GREEN, RED};

/// Text on a filled accent-colored background.
const ON_ACCENT: Color32 = Color32::from_gray(18);

/// Outline for unselected controls, visible in light and dark themes.
fn outline(visuals: &egui::Visuals) -> Color32 {
    visuals.weak_text_color().gamma_multiply(0.6)
}

/// A full-width panel with an accent bar, a title, optional content on the
/// right of the title row, and a body.
pub fn card<R>(
    ui: &mut egui::Ui,
    title: &str,
    accent: Color32,
    header_right: impl FnOnce(&mut egui::Ui),
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let visuals = ui.visuals().clone();
    Frame::new()
        .fill(visuals.faint_bg_color)
        .stroke(Stroke::new(
            1.0,
            visuals.widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 14.0), Sense::hover());
                ui.painter().rect_filled(bar, 1.5, accent);
                ui.label(egui::RichText::new(title).strong().size(13.0).color(accent));
                ui.with_layout(Layout::right_to_left(Align::Center), header_right);
            });
            ui.add_space(6.0);
            body(ui)
        })
        .inner
}

/// A small rounded status badge.
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(11.0), color);
    let size = galley.size() + Vec2::new(14.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(9), color.gamma_multiply(0.15));
    ui.painter()
        .circle_filled(Pos2::new(rect.left() + 7.0, rect.center().y), 2.5, color);
    ui.painter().galley(
        Pos2::new(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// A toggle drawn as a pill: filled with `color` when on.
pub fn pill(ui: &mut egui::Ui, on: &mut bool, text: &str, color: Color32) -> Response {
    let font = FontId::proportional(13.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(text_width + 30.0, 26.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, text)
    });
    let visuals = ui.visuals();
    let hovered = response.hovered();
    let (fill, stroke, text_color) = if *on {
        (
            if hovered {
                color.gamma_multiply(0.85)
            } else {
                color
            },
            color,
            ON_ACCENT,
        )
    } else {
        let stroke = if hovered { color } else { outline(visuals) };
        (Color32::TRANSPARENT, stroke, visuals.text_color())
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(13),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            15,
            visuals.selection.stroke,
            StrokeKind::Outside,
        );
    }
    // Indicator dot: solid when on, hollow when off.
    let dot = Pos2::new(rect.left() + 12.0, rect.center().y);
    if *on {
        painter.circle_filled(dot, 3.5, ON_ACCENT);
    } else {
        painter.circle_stroke(dot, 3.5, Stroke::new(1.2, stroke));
    }
    painter.text(
        Pos2::new(rect.left() + 21.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        text_color,
    );
    response
}

/// Segmented picker: one pill per option, the selected one filled.
/// Returns (changed, hovered).
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str, &str)],
    color: Color32,
) -> (bool, bool) {
    let mut changed = false;
    let mut hovered = false;
    let visuals = ui.visuals().clone();
    let title_font = FontId::proportional(13.0);
    let sub_font = FontId::proportional(11.0);
    ui.spacing_mut().item_spacing.x = 0.0;
    for (i, (option, title, sub)) in options.iter().enumerate() {
        let selected = *value == *option;
        let title_w = ui
            .painter()
            .layout_no_wrap(title.to_string(), title_font.clone(), color)
            .size()
            .x;
        let sub_w = ui
            .painter()
            .layout_no_wrap(sub.to_string(), sub_font.clone(), color)
            .size()
            .x;
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::new(title_w + sub_w + 34.0, 28.0), Sense::click());
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
            response.mark_changed();
        }
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton,
                ui.is_enabled(),
                selected,
                *title,
            )
        });
        hovered |= response.hovered();
        let radius = match i {
            0 => CornerRadius {
                nw: 8,
                sw: 8,
                ne: 0,
                se: 0,
            },
            _ if i + 1 == options.len() => CornerRadius {
                nw: 0,
                sw: 0,
                ne: 8,
                se: 8,
            },
            _ => CornerRadius::ZERO,
        };
        let (fill, text_color, sub_color) = if selected {
            (color, ON_ACCENT, ON_ACCENT.gamma_multiply(0.75))
        } else if response.hovered() {
            (
                color.gamma_multiply(0.15),
                visuals.text_color(),
                visuals.weak_text_color(),
            )
        } else {
            (
                Color32::TRANSPARENT,
                visuals.text_color(),
                visuals.weak_text_color(),
            )
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            radius,
            fill,
            Stroke::new(1.0, if selected { color } else { outline(&visuals) }),
            StrokeKind::Inside,
        );
        if response.has_focus() {
            painter.rect_stroke(
                rect.expand(2.0),
                radius,
                visuals.selection.stroke,
                StrokeKind::Outside,
            );
        }
        let title_pos = Pos2::new(rect.left() + 12.0, rect.center().y);
        let title_rect = painter.text(
            title_pos,
            Align2::LEFT_CENTER,
            *title,
            title_font.clone(),
            text_color,
        );
        painter.text(
            Pos2::new(title_rect.right() + 8.0, rect.center().y + 1.0),
            Align2::LEFT_CENTER,
            *sub,
            sub_font.clone(),
            sub_color,
        );
    }
    (changed, hovered)
}

/// Level meter over -60..0 dBFS with green/amber/red zones, a decaying
/// peak-hold tick (`hold`, dBFS) and a numeric readout.
pub fn level_meter(ui: &mut egui::Ui, label: &str, peak: Option<f32>, hold: &mut f32, width: f32) {
    const FLOOR: f32 = -60.0;
    let db = peak
        .map_or(FLOOR, |p| 20.0 * p.max(1e-6).log10())
        .clamp(FLOOR, 0.0);
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    *hold = if db >= *hold {
        db
    } else {
        (*hold - 20.0 * dt).max(db)
    }; // falls 20 dB/s

    let visuals = ui.visuals().clone();
    const LABEL: f32 = 48.0;
    const READOUT: f32 = 56.0;
    let gap = ui.spacing().item_spacing.x;
    ui.horizontal(|ui| {
        ui.set_width(width);
        ui.add_sized(
            [LABEL, 18.0],
            egui::Label::new(egui::RichText::new(label).weak()),
        );
        let bar = (width - LABEL - READOUT - 2.0 * gap).max(40.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(bar, 12.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(6), visuals.extreme_bg_color);
        let x = |d: f32| rect.left() + (d - FLOOR) / -FLOOR * rect.width();
        for (from, to, color) in [
            (FLOOR, -18.0, GREEN),
            (-18.0, -6.0, AMBER),
            (-6.0, 0.0, RED),
        ] {
            if db > from && peak.is_some() {
                let seg = Rect::from_x_y_ranges(x(from)..=x(db.min(to)), rect.y_range());
                painter.rect_filled(seg, CornerRadius::same(6), color);
            }
        }
        // Zone ticks, faint.
        for d in [-18.0, -6.0] {
            painter.line_segment(
                [Pos2::new(x(d), rect.top()), Pos2::new(x(d), rect.bottom())],
                Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            );
        }
        if peak.is_some() && *hold > FLOOR {
            let hx = x(*hold);
            let color = if *hold > -6.0 {
                RED
            } else if *hold > -18.0 {
                AMBER
            } else {
                GREEN
            };
            painter.line_segment(
                [
                    Pos2::new(hx, rect.top() - 2.0),
                    Pos2::new(hx, rect.bottom() + 2.0),
                ],
                Stroke::new(2.0, color),
            );
        }
        let text = if peak.is_some() && db > FLOOR {
            format!("{db:.0} dB")
        } else {
            "—".into()
        };
        ui.add_sized(
            [READOUT, 18.0],
            egui::Label::new(egui::RichText::new(text).weak().monospace()),
        );
    });
}

/// Peak waveform, mirrored around a center line, on a -48..0 dBFS scale so
/// quiet speech still shows. `peaks` holds one level per time block. With
/// `window` set, that many blocks span the width and the newest sits at the
/// right edge (a live, scrolling view); otherwise all of `peaks` is fitted.
pub fn waveform(ui: &mut egui::Ui, peaks: &[f32], window: Option<usize>, height: f32, color: Color32) -> Response {
    const FLOOR_DB: f32 = -48.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), visuals.extreme_bg_color);
    let mid = rect.center().y;
    painter.line_segment(
        [Pos2::new(rect.left() + 4.0, mid), Pos2::new(rect.right() - 4.0, mid)],
        Stroke::new(1.0, outline(visuals).gamma_multiply(0.5)),
    );

    let span = window.unwrap_or(peaks.len()).max(1);
    // Blocks before the first peak (an empty live window) draw nothing.
    let first = peaks.len() as isize - span as isize;
    let columns = ((rect.width() - 8.0) / 3.0).max(1.0) as usize;
    let half = height / 2.0 - 3.0;
    for column in 0..columns {
        let from = first + (column * span / columns) as isize;
        let to = first + ((column + 1) * span / columns).max(column * span / columns + 1) as isize;
        let level = (from.max(0)..to.max(0))
            .filter_map(|i| peaks.get(i as usize))
            .fold(None, |m: Option<f32>, &p| Some(m.map_or(p, |m| m.max(p))));
        let Some(level) = level else { continue };
        let db = 20.0 * level.max(1e-6).log10();
        let h = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * half;
        let x = rect.left() + 4.0 + column as f32 * 3.0 + 1.0;
        painter.line_segment(
            [Pos2::new(x, mid - h.max(0.5)), Pos2::new(x, mid + h.max(0.5))],
            Stroke::new(2.0, color),
        );
    }
    response
}
