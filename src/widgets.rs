//! Small custom widgets for the main window: cards, badges, toggle pills,
//! level meters and waveforms.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect,
    Response, Sense, Stroke, StrokeKind, Vec2,
};

use crate::gui::{AMBER, CYAN, GREEN, MUTED, RED, VIOLET};

/// Text on a filled accent-colored background.
const ON_ACCENT: Color32 = Color32::from_gray(18);
const LIGHT_CYAN: Color32 = Color32::from_rgb(0x07, 0x59, 0x85);

/// Studio-grade sidebar surface fill, slightly recessed from the main canvas.
pub fn sidebar_fill(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(13, 15, 20)
    } else {
        Color32::from_rgb(235, 239, 245)
    }
}

/// Hairline separator / card border color for the current theme.
pub fn hairline(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(38, 43, 56)
    } else {
        Color32::from_rgb(216, 222, 232)
    }
}

/// Configure OpenMic's studio-grade dark & light themes and polish
/// while preserving `ThemePreference::System`, compact zoomed-window ergonomics,
/// and WCAG AA (>= 4.5:1) contrast.
pub fn configure_text_styles(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.combo_height = 240.0;

        let visuals = &mut style.visuals;
        let dark = visuals.dark_mode;
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.menu_corner_radius = CornerRadius::same(8);
        visuals.slider_trailing_fill = true;

        if dark {
            visuals.panel_fill = Color32::from_rgb(17, 19, 24);
            visuals.window_fill = Color32::from_rgb(22, 25, 33);
            visuals.faint_bg_color = Color32::from_rgb(24, 27, 35);
            visuals.extreme_bg_color = Color32::from_rgb(11, 13, 18);
            visuals.code_bg_color = Color32::from_rgb(14, 17, 24);
            visuals.override_text_color = Some(Color32::from_rgb(235, 240, 248));
            visuals.weak_text_color = Some(Color32::from_rgb(156, 166, 184));
            visuals.hyperlink_color = CYAN;
            visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(44, 50, 66));

            visuals.selection.bg_fill = Color32::from_rgb(20, 66, 92);
            visuals.selection.stroke = Stroke::new(1.5, CYAN);

            let r = CornerRadius::same(4);
            visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(24, 27, 35);
            visuals.widgets.noninteractive.weak_bg_fill = Color32::from_rgb(24, 27, 35);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, hairline(true));
            visuals.widgets.noninteractive.fg_stroke =
                Stroke::new(1.0, Color32::from_rgb(212, 220, 232));
            visuals.widgets.noninteractive.corner_radius = r;

            visuals.widgets.inactive.bg_fill = Color32::from_rgb(33, 38, 50);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(31, 36, 48);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(48, 55, 72));
            visuals.widgets.inactive.fg_stroke =
                Stroke::new(1.0, Color32::from_rgb(226, 232, 242));
            visuals.widgets.inactive.corner_radius = r;
            visuals.widgets.inactive.expansion = 0.0;

            visuals.widgets.hovered.bg_fill = Color32::from_rgb(43, 50, 66);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(41, 47, 62);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(72, 84, 108));
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_rgb(248, 250, 252));
            visuals.widgets.hovered.corner_radius = r;
            visuals.widgets.hovered.expansion = 0.0;

            visuals.widgets.active.bg_fill = Color32::from_rgb(50, 58, 78);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(48, 56, 74);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, CYAN);
            visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
            visuals.widgets.active.corner_radius = r;
            visuals.widgets.active.expansion = 0.0;

            visuals.widgets.open.bg_fill = Color32::from_rgb(41, 48, 64);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(39, 45, 60);
            visuals.widgets.open.bg_stroke = Stroke::new(1.0, CYAN);
            visuals.widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(241, 245, 249));
            visuals.widgets.open.corner_radius = r;
        } else {
            visuals.panel_fill = Color32::from_rgb(244, 246, 250);
            visuals.window_fill = Color32::WHITE;
            visuals.faint_bg_color = Color32::WHITE;
            visuals.extreme_bg_color = Color32::from_rgb(235, 239, 246);
            visuals.code_bg_color = Color32::from_rgb(238, 242, 248);
            visuals.override_text_color = Some(Color32::from_rgb(17, 24, 39));
            visuals.weak_text_color = Some(Color32::from_rgb(75, 85, 101));
            visuals.hyperlink_color = LIGHT_CYAN;
            visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(203, 213, 225));

            visuals.selection.bg_fill = Color32::from_rgb(214, 238, 252);
            visuals.selection.stroke = Stroke::new(1.5, LIGHT_CYAN);

            let r = CornerRadius::same(4);
            visuals.widgets.noninteractive.bg_fill = Color32::WHITE;
            visuals.widgets.noninteractive.weak_bg_fill = Color32::WHITE;
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, hairline(false));
            visuals.widgets.noninteractive.fg_stroke =
                Stroke::new(1.0, Color32::from_rgb(30, 41, 59));
            visuals.widgets.noninteractive.corner_radius = r;

            visuals.widgets.inactive.bg_fill = Color32::from_rgb(230, 236, 244);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(233, 238, 246);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(203, 212, 226));
            visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(17, 24, 39));
            visuals.widgets.inactive.corner_radius = r;
            visuals.widgets.inactive.expansion = 0.0;

            visuals.widgets.hovered.bg_fill = Color32::from_rgb(218, 227, 238);
            visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(222, 230, 241);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(148, 163, 184));
            visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_rgb(15, 23, 42));
            visuals.widgets.hovered.corner_radius = r;
            visuals.widgets.hovered.expansion = 0.0;

            visuals.widgets.active.bg_fill = Color32::from_rgb(204, 217, 234);
            visuals.widgets.active.weak_bg_fill = Color32::from_rgb(209, 221, 236);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, LIGHT_CYAN);
            visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::from_rgb(15, 23, 42));
            visuals.widgets.active.corner_radius = r;
            visuals.widgets.active.expansion = 0.0;

            visuals.widgets.open.bg_fill = Color32::from_rgb(220, 229, 240);
            visuals.widgets.open.weak_bg_fill = Color32::from_rgb(224, 232, 242);
            visuals.widgets.open.bg_stroke = Stroke::new(1.0, LIGHT_CYAN);
            visuals.widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(15, 23, 42));
            visuals.widgets.open.corner_radius = r;
        }
    });
}

/// Crisp vector navigation icons painted directly on the GPU (zero assets).
#[derive(Clone, Copy)]
pub enum NavIcon {
    Voice,
    Soundboard,
    Record,
    Dictation,
    Settings,
}

pub fn paint_nav_icon(painter: &egui::Painter, center: Pos2, icon: NavIcon, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    match icon {
        NavIcon::Voice => {
            let cap = Rect::from_center_size(center - Vec2::new(0.0, 2.0), Vec2::new(5.0, 8.0));
            painter.rect_filled(cap, CornerRadius::same(3), color);
            let cup = [
                center + Vec2::new(-4.5, -1.0),
                center + Vec2::new(-4.0, 2.5),
                center + Vec2::new(0.0, 4.5),
                center + Vec2::new(4.0, 2.5),
                center + Vec2::new(4.5, -1.0),
            ];
            painter.add(egui::Shape::line(cup.to_vec(), stroke));
            painter.line_segment(
                [center + Vec2::new(0.0, 4.5), center + Vec2::new(0.0, 6.8)],
                stroke,
            );
            painter.line_segment(
                [center + Vec2::new(-2.8, 6.8), center + Vec2::new(2.8, 6.8)],
                stroke,
            );
        }
        NavIcon::Soundboard => {
            for dx in [-3.4, 3.4] {
                for dy in [-3.4, 3.4] {
                    let cell =
                        Rect::from_center_size(center + Vec2::new(dx, dy), Vec2::splat(4.6));
                    painter.rect_stroke(cell, CornerRadius::same(1), stroke, StrokeKind::Inside);
                }
            }
        }
        NavIcon::Record => {
            painter.circle_stroke(center, 6.0, stroke);
            painter.circle_filled(center, 2.8, color);
        }
        NavIcon::Dictation => {
            for (dx, h) in [(-5.0, 4.0), (-1.7, 9.0), (1.7, 6.5), (5.0, 3.5)] {
                painter.line_segment(
                    [
                        center + Vec2::new(dx, -h * 0.5),
                        center + Vec2::new(dx, h * 0.5),
                    ],
                    Stroke::new(1.8, color),
                );
            }
        }
        NavIcon::Settings => {
            for (dy, knob_x) in [(-4.0, -2.0), (0.0, 2.5), (4.0, -1.0)] {
                painter.line_segment(
                    [
                        center + Vec2::new(-6.0, dy),
                        center + Vec2::new(6.0, dy),
                    ],
                    Stroke::new(1.3, color.gamma_multiply(0.65)),
                );
                painter.circle_filled(center + Vec2::new(knob_x, dy), 2.1, color);
            }
        }
    }
}

/// Minimal brand emblem for the sidebar header.
pub fn paint_brand_mark(painter: &egui::Painter, rect: Rect, accent: Color32) {
    painter.rect_filled(rect, CornerRadius::same(6), accent.gamma_multiply(0.16));
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, accent.gamma_multiply(0.45)),
        StrokeKind::Inside,
    );
    let c = rect.center();
    for (dx, h) in [(-4.2, 5.0), (-1.4, 11.0), (1.4, 8.0), (4.2, 4.5)] {
        painter.line_segment(
            [c + Vec2::new(dx, -h * 0.5), c + Vec2::new(dx, h * 0.5)],
            Stroke::new(1.8, accent),
        );
    }
}

/// Outline for unselected controls, visible in light and dark themes.
fn outline(visuals: &egui::Visuals) -> Color32 {
    visuals.weak_text_color().gamma_multiply(0.6)
}

/// Accent text needs darker shades on light panels and tinted badges.
pub fn text_color(ui: &egui::Ui, accent: Color32) -> Color32 {
    if ui.visuals().dark_mode {
        return accent;
    }
    match accent {
        CYAN => LIGHT_CYAN,
        GREEN => Color32::from_rgb(0x16, 0x65, 0x34),
        AMBER => Color32::from_rgb(0x92, 0x40, 0x0e),
        VIOLET => Color32::from_rgb(0x6d, 0x28, 0xd9),
        RED => Color32::from_rgb(0xb9, 0x1c, 0x1c),
        MUTED => Color32::from_rgb(0x47, 0x55, 0x69),
        _ => accent,
    }
}

/// Consume an action before focus moves to a replacement control.
pub fn finish_keyboard_activation(ctx: &egui::Context) {
    let id = egui::Id::new(("held-activation", ctx.viewport_id()));
    let (enter, space, space_pressed) = ctx.input(|input| {
        (
            input.key_down(egui::Key::Enter),
            input.key_down(egui::Key::Space),
            input.key_pressed(egui::Key::Space),
        )
    });
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter | egui::Key::Space,
                    pressed: true,
                    ..
                }
            ) && !((space || space_pressed)
                && matches!(event, egui::Event::Text(text) if text == " "))
        })
    });
    ctx.data_mut(|data| {
        if enter || space {
            data.insert_temp(id, (enter, space));
        } else {
            data.remove::<(bool, bool)>(id);
        }
    });
}

/// Ignore repeats of a completed action until its activation keys are released.
pub fn consume_held_activation(ctx: &egui::Context) {
    let id = egui::Id::new(("held-activation", ctx.viewport_id()));
    let Some((mut enter, mut space)) = ctx.data(|data| data.get_temp::<(bool, bool)>(id)) else {
        return;
    };
    if ctx.input(|input| input.focused) {
        ctx.input_mut(|input| {
            input.events.retain(|event| match event {
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: false,
                    ..
                } => {
                    enter = false;
                    true
                }
                egui::Event::Key {
                    key: egui::Key::Space,
                    pressed: false,
                    ..
                } => {
                    space = false;
                    true
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } => !enter,
                egui::Event::Key {
                    key: egui::Key::Space,
                    pressed: true,
                    ..
                } => !space,
                egui::Event::Text(text) if space && text == " " => false,
                _ => true,
            });
            // A release followed by a new press in this batch is a fresh action.
            enter &= input.key_down(egui::Key::Enter);
            space &= input.key_down(egui::Key::Space);
        });
    } else {
        enter = false;
        space = false;
    }
    ctx.data_mut(|data| {
        if enter || space {
            data.insert_temp(id, (enter, space));
        } else {
            data.remove::<(bool, bool)>(id);
        }
    });
}

/// Reveal focused controls when navigating or resizing the viewport.
pub fn scroll_on_focus(response: &Response) {
    let layout = (
        response.ctx.content_rect().size(),
        response.ctx.zoom_factor(),
    );
    let id = response
        .id
        .with(("focus-viewport", response.ctx.viewport_id()));
    let reflowed = response.ctx.data_mut(|data| {
        let previous = data.get_temp::<(Vec2, f32)>(id);
        data.insert_temp(id, layout);
        previous.is_some_and(|previous| previous != layout)
    });
    if response.has_focus()
        && (response.gained_focus() || reflowed)
        && response
            .ctx
            .input(|input| !input.pointer.any_down() && !input.pointer.any_released())
    {
        response.scroll_to_me(Some(Align::Center));
    }
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
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(Vec2::new(3.0, 13.0), Sense::hover());
                ui.painter().rect_filled(bar, 1.5, accent);
                ui.label(
                    egui::RichText::new(title)
                        .strong()
                        .size(12.0)
                        .color(text_color(ui, accent)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), header_right);
            });
            ui.add_space(6.0);
            body(ui)
        })
        .inner
}

/// A small rounded status badge with balanced horizontal padding and subtle border.
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    let foreground = text_color(ui, color);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(11.0), foreground);
    let size = galley.size() + Vec2::new(22.0, 6.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let radius = CornerRadius::same((size.y * 0.5).round() as u8);
    let painter = ui.painter();
    painter.rect(
        rect,
        radius,
        color.gamma_multiply(if ui.visuals().dark_mode { 0.14 } else { 0.12 }),
        Stroke::new(1.0, color.gamma_multiply(0.32)),
        StrokeKind::Inside,
    );
    painter.circle_filled(Pos2::new(rect.left() + 8.5, rect.center().y), 2.5, color);
    painter.galley(
        Pos2::new(rect.left() + 14.5, rect.center().y - galley.size().y / 2.0),
        galley,
        foreground,
    );
}

/// A toggle drawn as a pill: filled with `color` when on.
pub fn pill(ui: &mut egui::Ui, on: &mut bool, text: &str, color: Color32) -> Response {
    let font = FontId::proportional(12.5);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(text_width + 32.0, 26.0), Sense::click());
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
                color.gamma_multiply(0.88)
            } else {
                color
            },
            color,
            ON_ACCENT,
        )
    } else {
        let fill = if hovered {
            visuals.widgets.hovered.weak_bg_fill
        } else {
            visuals.widgets.inactive.weak_bg_fill
        };
        let stroke = if hovered {
            color
        } else {
            visuals.widgets.inactive.bg_stroke.color
        };
        (fill, stroke, visuals.text_color())
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
    let dot = Pos2::new(rect.left() + 12.5, rect.center().y);
    if *on {
        painter.circle_filled(dot, 3.2, ON_ACCENT);
    } else {
        painter.circle_stroke(dot, 3.2, Stroke::new(1.3, outline(visuals)));
    }
    painter.text(
        Pos2::new(rect.left() + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        text_color,
    );
    response
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
        let (rect, _) = ui.allocate_exact_size(Vec2::new(bar, 10.0), Sense::hover());
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(5),
            visuals.extreme_bg_color,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
        let inner = rect.shrink(1.0);
        let x = |d: f32| inner.left() + (d - FLOOR) / -FLOOR * inner.width();
        for (idx, (from, to, color)) in [
            (FLOOR, -18.0, GREEN),
            (-18.0, -6.0, AMBER),
            (-6.0, 0.0, RED),
        ]
        .into_iter()
        .enumerate()
        {
            if db > from && peak.is_some() {
                let seg = Rect::from_x_y_ranges(x(from)..=x(db.min(to)), inner.y_range());
                let cr = CornerRadius {
                    nw: if idx == 0 { 4 } else { 0 },
                    sw: if idx == 0 { 4 } else { 0 },
                    ne: if db <= to { 4 } else { 0 },
                    se: if db <= to { 4 } else { 0 },
                };
                painter.rect_filled(seg, cr, color);
            }
        }
        // Subtle zone ticks.
        for d in [-18.0, -6.0] {
            painter.line_segment(
                [Pos2::new(x(d), inner.top()), Pos2::new(x(d), inner.bottom())],
                Stroke::new(1.0, visuals.panel_fill.gamma_multiply(0.8)),
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
                    Pos2::new(hx, rect.top() - 1.5),
                    Pos2::new(hx, rect.bottom() + 1.5),
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
pub fn waveform(
    ui: &mut egui::Ui,
    peaks: &[f32],
    window: Option<usize>,
    height: f32,
    color: Color32,
) -> Response {
    const FLOOR_DB: f32 = -48.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(6), visuals.extreme_bg_color);
    let mid = rect.center().y;
    painter.line_segment(
        [
            Pos2::new(rect.left() + 4.0, mid),
            Pos2::new(rect.right() - 4.0, mid),
        ],
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
            [
                Pos2::new(x, mid - h.max(0.5)),
                Pos2::new(x, mid + h.max(0.5)),
            ],
            Stroke::new(2.0, color),
        );
    }
    response
}
