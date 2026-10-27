//! The strip: OpenMic collapsed to a thin always-on-top bar for everyday
//! use. The microphone shows what Discord hears (green while your voice goes
//! out, red when muted; click it to mute), then your starred pads, the
//! preset (click for the next one) and a button to expand back to the full
//! window. Buttons are as wide as their names, and pads come first: when the
//! strip is too narrow, the preset goes first and the pads scroll. Drag the
//! strip to move it and its right edge to resize it, down to just the microphone.

use eframe::egui::{
    self, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Vec2,
    ViewportBuilder, ViewportCommand, ViewportId,
};

use super::{App, Command, AMBER, GREEN, MUTED, RED};
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
/// Window title of the strip. Windows finds the same window with `FindWindowW`,
/// which needs its own string literal (`w!("OpenMic level")`).
const STRIP_TITLE: &str = "OpenMic level";
/// Hover details. On X11 it is raised with the strip so a restack does not cover it.
const HINT_TITLE: &str = "OpenMic hint";

fn label_font() -> FontId {
    FontId::proportional(12.0)
}

/// What the microphone shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Meter {
    /// Processing stopped; retain the saved mute state for the microphone button.
    Stopped { muted: bool },
    Muted,
    /// Processing, with the processed output's peak in dBFS.
    Live(f32),
}

/// What the strip shows besides the microphone.
#[derive(Clone, Debug, Default)]
pub(super) struct Strip {
    pub preset: String,
    pub pads: Vec<StripPad>,
}

#[derive(Clone, Debug)]
pub(super) struct StripPad {
    /// File identity, independent of the board order and display name.
    pub key: u64,
    pub index: usize,
    pub name: String,
    pub playing: bool,
    pub paused: bool,
    pub loading: bool,
    pub error: Option<String>,
}

impl StripPad {
    fn failed(&self) -> bool {
        self.error.is_some() && !self.loading && !self.playing && !self.paused
    }

    fn label(&self) -> String {
        let name = truncate(&self.name, MAX_NAME);
        if self.failed() { format!("! {name}") } else { name }
    }
}

/// What the user did on the strip this frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    ToggleMute,
    Expand,
    NextPreset,
    PlayPad(usize),
    RevealPad(usize),
}

/// Put the preset after the pads only when every button fits.
fn preset_position(left: f32, right: f32, pads: &[f32], preset: f32) -> Option<f32> {
    let x = left + pads.iter().map(|width| width + GAP).sum::<f32>();
    (x + preset <= right).then_some(x)
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
            None => Meter::Stopped { muted: self.settings.mute },
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

    pub(super) fn strip_contents(&self) -> Strip {
        Strip {
            preset: self.preset_name(),
            pads: self
                .settings
                .sounds
                .iter()
                .enumerate()
                .filter(|(_, pad)| pad.starred)
                .map(|(i, pad)| {
                    let key = super::clip_key(&pad.path);
                    StripPad {
                        key,
                        index: i,
                        name: pad.display_name(),
                        playing: self.playing.contains(&key),
                        paused: self.pad_progress.get(&key).is_some_and(|clip| clip.paused),
                        loading: self.clip_loading(&pad.path),
                        error: self.clip_errors.get(&pad.path).cloned(),
                    }
                })
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
                let pads: Vec<f32> = strip.pads.iter().map(|pad| measure(&pad.label())).collect();
                fitting_width(&pads, measure(&truncate(&strip.preset, MAX_NAME)))
            });
            self.overlay_spawn_width = width.clamp(MIN_WIDTH, MAX_WIDTH);
        }
        // Constant while shown: egui turns any change into a resize or move.
        let builder = ViewportBuilder::default()
            .with_title(STRIP_TITLE)
            .with_inner_size([self.overlay_spawn_width, HEIGHT])
            .with_min_inner_size([MIN_WIDTH, HEIGHT])
            .with_max_inner_size([MAX_WIDTH, HEIGHT])
            .with_resizable(true)
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false);
        let meter = self.meter();
        let saved = self.settings.overlay_position.filter(|&px| super::on_a_monitor(px));
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
                Action::PlayPad(index) => self.toggle_clip(index, ctx),
                Action::RevealPad(index) => self.reveal_pad(index, ctx),
            }
        }
    }
}

/// Keeps the meter above other always-on-top windows. Windows raises the
/// taskbar again whenever another window comes to the front, so a meter
/// placed on the taskbar vanished under it when you clicked elsewhere.
/// X11 window managers restack the same way. Re-assert the meter's place
/// when the foreground window changes, and every couple of seconds in case
/// something else rose above it. The raise leaves keyboard focus alone.
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

#[cfg(target_os = "linux")]
fn foreground() -> Option<isize> {
    crate::x11_above::foreground_window()
}

#[cfg(not(any(windows, target_os = "linux")))]
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
        // `w!` needs a literal; this is `STRIP_TITLE`.
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

#[cfg(target_os = "linux")]
fn raise_meter() {
    crate::x11_above::raise_windows(&[STRIP_TITLE, HINT_TITLE]);
}

#[cfg(not(any(windows, target_os = "linux")))]
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
    let body = ui.interact(rect, ui.id().with("meter"), Sense::CLICK | Sense::DRAG);
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
    let muted = matches!(meter, Meter::Muted | Meter::Stopped { muted: true });
    let mic_name = if muted { "Unmute microphone" } else { "Mute microphone" };
    let mic = ui.interact(mic_rect, ui.id().with("mic"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    mic.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), mic_name));
    hover_details(ui, &mic, mic_name.to_owned());
    if mic.clicked() {
        actions.push(Action::ToggleMute);
    }
    if mic.hovered() {
        ui.painter().rect_filled(mic_rect.shrink(3.0), CornerRadius::same(5), visuals.widgets.hovered.weak_bg_fill);
    }
    focus_outline(ui, &mic);
    let (color, speaking) = match meter {
        Meter::Stopped { .. } => (MUTED.gamma_multiply(0.6), false),
        Meter::Muted => (RED, false),
        Meter::Live(db) if db > SPEAKING_DB => (GREEN, true),
        Meter::Live(_) => (visuals.text_color(), false),
    };
    if speaking {
        ui.painter().circle_filled(mic_rect.center(), 14.0, GREEN.gamma_multiply(0.18));
    }
    mic_glyph(ui.painter(), mic_rect.center(), color);
    if muted {
        let r = mic_rect.shrink(8.0);
        ui.painter().line_segment([r.left_top(), r.right_bottom()], Stroke::new(2.0, RED));
    }

    let mut right = rect.right() - GRIP;
    let expand = (rect.width() >= EXPAND_MIN_WIDTH).then(||
        Rect::from_center_size(Pos2::new(right - EXPAND_WIDTH / 2.0 - 2.0, rect.center().y), Vec2::new(EXPAND_WIDTH, 22.0)));
    if let Some(expand) = expand {
        right = expand.left() - GAP;
    }

    // Pads first, as wide as their names; scroll them when they don't fit.
    // The microphone, expand button and resize edge stay outside the scroll area.
    let names: Vec<String> = strip.pads.iter().map(StripPad::label).collect();
    let preset_name = truncate(&strip.preset, MAX_NAME);
    let measure = |text: &str| button_width(ui.painter().layout_no_wrap(text.to_owned(), label_font(), visuals.text_color()).size().x);
    let left = mic_rect.right() + GAP;
    let pads_width = (right - left).max(0.0);
    let widths: Vec<f32> = names.iter().map(|n| measure(n).min(pads_width)).collect();
    let preset_width = measure(&preset_name);
    let preset_x = preset_position(left, right, &widths, preset_width);
    let content_width = widths.iter().sum::<f32>() + GAP * widths.len().saturating_sub(1) as f32;
    let overflowing = content_width > right - left;
    let button_rect = |x: f32, width: f32| Rect::from_min_size(Pos2::new(x, rect.center().y - 11.0), Vec2::new(width, 22.0));
    if right > left && !strip.pads.is_empty() {
        let pads_rect = Rect::from_min_max(Pos2::new(left, rect.top()), Pos2::new(right, rect.bottom()));
        ui.scope_builder(egui::UiBuilder::new().id_salt("strip-pads").max_rect(pads_rect), |ui| {
            ui.style_mut().always_scroll_the_only_direction = true;
            egui::ScrollArea::horizontal()
                .id_salt("pads")
                .min_scrolled_width(0.0)
                .auto_shrink([false; 2])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL)
                .show(ui, |ui| {
                    ui.set_min_width(content_width);
                    let origin = ui.min_rect().left();
                    let mut x = 0.0;
                    for (i, pad) in strip.pads.iter().enumerate() {
                        if let Some(action) = pad_button(
                            ui, button_rect(origin + x, widths[i]), pad, &names[i], overflowing, (x, widths[i], pads_width),
                        ) {
                            actions.push(action);
                        }
                        x += widths[i] + GAP;
                    }
                });
        });
    }
    if let Some(x) = preset_x {
        let label = egui::RichText::new(&preset_name).font(label_font()).color(visuals.weak_text_color());
        let preset = ui.scope_builder(egui::UiBuilder::new().id(ui.id().with("preset")), |ui|
            ui.put(button_rect(x, preset_width), egui::Button::new(label).frame(false)))
            .inner;
        preset.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("Next preset: {}", strip.preset)));
        hover_details(ui, &preset, format!("{}\nClick for the next preset", strip.preset));
        focus_outline(ui, &preset);
        if preset.clicked() {
            actions.push(Action::NextPreset);
        }
    }
    if let Some(expand) = expand {
        let open = ui.scope_builder(egui::UiBuilder::new().id(ui.id().with("expand")), |ui|
            ui.put(expand, egui::Button::new("⛶").small())).inner;
        open.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Open the full window"));
        hover_details(ui, &open, "Open the full window".into());
        if open.clicked() { actions.push(Action::Expand); }
    }

    // The right edge resizes, down to just the microphone.
    let grip_rect = Rect::from_min_max(Pos2::new(rect.right() - GRIP, rect.top()), rect.right_bottom());
    let grip = ui.interact(grip_rect, ui.id().with("resize"), Sense::DRAG).on_hover_cursor(CursorIcon::ResizeHorizontal);
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

fn focus_outline(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect.shrink(3.0), CornerRadius::same(3), ui.visuals().selection.stroke, StrokeKind::Inside);
    }
}

/// Native hints need their own window: the strip is too short for a tooltip.
fn hover_details(ui: &egui::Ui, response: &egui::Response, text: String) {
    let ctx = ui.ctx();
    ctx.accesskit_node_builder(response.id, |node| node.set_description(text.clone()));
    let window = ctx.input(|input| input.viewport().inner_rect);
    let Some(window) = window.filter(|_| !ctx.embed_viewports()) else {
        response.clone().on_hover_text(text);
        return;
    };
    if !response.enabled() || ctx.input(|input| input.pointer.any_down())
        || !egui::Tooltip::should_show_tooltip(response, false)
    {
        return;
    }
    let anchor = Rect::from_min_max(
        Pos2::new(window.left() + response.rect.left(), window.top()),
        Pos2::new(window.left() + response.rect.right(), window.bottom()),
    );
    let work = monitor_work_area(window, ctx.pixels_per_point());
    show_hint_window(ui, text, anchor, work);
}

fn show_hint_window(ui: &egui::Ui, text: String, anchor: Rect, work: Option<Rect>) {
    let ctx = ui.ctx();
    let frame = egui::Frame::popup(ui.style());
    let margin = frame.total_margin().sum();
    let font = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let width = work.map_or(320.0, |work| (work.width() - margin.x).min(320.0)).max(1.0);
    let height = work.map_or(600.0, |work|
        (anchor.top() - work.top() - 4.0).max(work.bottom() - anchor.bottom() - 4.0).min(work.height()));
    let mut job = egui::text::LayoutJob::simple(text, font, ui.visuals().text_color(), width);
    job.wrap.max_rows = ((height - margin.y) / row_height).floor().max(1.0) as usize;
    let mut galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    // Rows are rounded to pixels; measure before fixing the window size.
    while galley.rows.len() > 1 && (galley.size().y + margin.y).ceil() > height {
        let mut job = (*galley.job).clone();
        job.wrap.max_rows = galley.rows.len() - 1;
        galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    }
    let size = (galley.size() + margin).ceil();
    let position = hint_position(anchor, size, work);
    let style = ui.style().clone();
    let builder = ViewportBuilder::default().with_title(HINT_TITLE)
        .with_inner_size(size).with_position(position).with_active(false)
        .with_mouse_passthrough(true).with_decorations(false).with_taskbar(false)
        .with_resizable(false).with_always_on_top();
    ctx.show_viewport_deferred(ViewportId::from_hash_of((ctx.viewport_id(), "strip-hint")), builder, move |ui, _| {
        ui.set_style(style.clone());
        frame.show(ui, |ui| { ui.add(egui::Label::new(galley.clone()).selectable(false)); });
    });
}

fn hint_position(anchor: Rect, size: Vec2, work: Option<Rect>) -> Pos2 {
    let mut position = anchor.left_bottom() + Vec2::new(0.0, 4.0);
    if let Some(work) = work {
        if position.y + size.y > work.bottom() { position.y = anchor.top() - size.y - 4.0; }
        position.x = position.x.clamp(work.left(), (work.right() - size.x).max(work.left()));
        position.y = position.y.clamp(work.top(), (work.bottom() - size.y).max(work.top()));
    }
    position
}

#[cfg(windows)]
fn monitor_work_area(window: Rect, ppp: f32) -> Option<Rect> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect};
    let pixels = window * ppp;
    let rect = RECT { left: pixels.left().round() as i32, top: pixels.top().round() as i32,
        right: pixels.right().round() as i32, bottom: pixels.bottom().round() as i32 };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a monitor query that writes one MONITORINFO to the supplied buffer.
    unsafe { GetMonitorInfoW(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST), &mut info) }.ok().ok()?;
    Some(Rect::from_min_max(Pos2::new(info.rcWork.left as f32, info.rcWork.top as f32),
        Pos2::new(info.rcWork.right as f32, info.rcWork.bottom as f32)) / ppp)
}

#[cfg(not(windows))]
fn monitor_work_area(_: Rect, _: f32) -> Option<Rect> { None }

fn pad_button(
    ui: &mut egui::Ui, rect: Rect, pad: &StripPad, label: &str, overflowing: bool, position: (f32, f32, f32),
) -> Option<Action> {
    let mut button = egui::Button::new(egui::RichText::new(label).font(label_font())).truncate();
    if pad.loading {
        button = button.fill(super::CYAN.gamma_multiply(0.35));
    } else if pad.playing {
        button = button.fill(GREEN.gamma_multiply(0.35));
    } else if pad.paused {
        button = button.fill(AMBER.gamma_multiply(0.25)).stroke(Stroke::new(1.0, AMBER));
    } else if pad.failed() {
        button = button.fill(RED.gamma_multiply(0.18)).stroke(Stroke::new(1.0, RED));
    }
    let (action, hint) = if pad.loading {
        ("Cancel loading", "Loading… Click to cancel")
    } else if pad.playing {
        ("Pause", "Click to pause")
    } else if pad.paused {
        ("Resume", "Paused. Click to resume")
    } else if pad.failed() {
        ("Retry", "Click to retry")
    } else {
        ("Play", "Click to play")
    };
    let hint = if overflowing { format!("{hint}\nScroll to reach more pads.") } else { hint.to_owned() };
    let hint = if !pad.loading && let Some(error) = &pad.error {
        format!("{hint}\nRight-click or Shift+F10 to show this pad on the Soundboard.\n\nCould not load this clip: {error}")
    } else {
        format!("{hint}\nRight-click or Shift+F10 to show this pad on the Soundboard.")
    };
    let id = ui.make_persistent_id(("pad", pad.key));
    let response = ui.scope_builder(egui::UiBuilder::new().id(id).max_rect(rect), |ui| {
        ui.put(rect, button)
    }).inner;
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), format!("{action} {}", pad.name)));
    hover_details(ui, &response, format!("{}\n{hint}", pad.name));
    focus_outline(ui, &response);
    if response.has_focus() {
        // Store unscrolled geometry: row edits and resizes reveal the focused file again,
        // while using the wheel can still scroll away from it.
        let position_id = ui.make_persistent_id("focused-pad-position");
        let position = (response.id, position.0, position.1, position.2);
        let moved = ui.data_mut(|data| {
            let previous = data.get_temp::<(egui::Id, f32, f32, f32)>(position_id);
            data.insert_temp(position_id, position);
            previous != Some(position)
        });
        if (response.gained_focus() || moved)
            && ui.input(|input| !input.pointer.any_down() && !input.pointer.any_released())
        {
            response.scroll_to_me(Some(egui::Align::Center));
        }
    }
    if response.secondary_clicked() || (response.has_focus()
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::F10))) {
        Some(Action::RevealPad(pad.index))
    } else if response.clicked() {
        Some(Action::PlayPad(pad.index))
    } else {
        None
    }
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
            pads: names.iter().enumerate().map(|(i, n)| StripPad {
                key: i as u64, index: i, name: n.to_string(), playing: false, paused: false, loading: false, error: None,
            }).collect(),
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
    fn the_preset_only_shows_if_every_pad_fits() {
        // Room for everything.
        assert_eq!(preset_position(0.0, 300.0, &[50.0, 60.0], 70.0), Some(110.0 + 2.0 * GAP));
        // The preset goes first, whether or not every pad fits.
        assert_eq!(preset_position(0.0, 140.0, &[50.0, 60.0], 70.0), None);
        assert_eq!(preset_position(0.0, 80.0, &[50.0, 60.0], 70.0), None);
        // No pads: just the preset.
        assert_eq!(preset_position(10.0, 100.0, &[], 70.0), Some(10.0));
    }

    #[test]
    fn there_is_no_level_meter_or_status_text() {
        for meter in [Meter::Stopped { muted: false }, Meter::Stopped { muted: true }, Meter::Muted, Meter::Live(-20.0), Meter::Live(-80.0)] {
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
    fn strip_pad_right_click_and_shift_f10_reveal_without_playing() {
        let mut s = strip(&["first", "second"]);
        s.pads[1].index = 17;
        s.pads[1].paused = true;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let render = |events| {
            let mut actions = Vec::new();
            let output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, HEIGHT))),
                events, ..Default::default()
            }, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
            (output, actions)
        };
        let (output, _) = render(Vec::new());
        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
            .find(|(_, node)| node.label() == Some("Resume second")).map(|(id, _)| id).unwrap();
        let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "second" => Some(text.visual_bounding_rect().center()),
            _ => None,
        }).unwrap();
        let mut actions = Vec::new();
        for pressed in [true, false] {
            actions.extend(render(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE,
            }]).1);
        }
        assert_eq!(actions, [Action::RevealPad(17)]);
        let key = |key, modifiers| vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }];
        let mut focused = false;
        for _ in 0..8 {
            let (output, actions) = render(key(egui::Key::Tab, egui::Modifiers::NONE));
            assert!(actions.is_empty());
            if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
        }
        assert!(focused, "keyboard navigation reaches the paused pad");
        assert_eq!(render(key(egui::Key::F10, egui::Modifiers::SHIFT)).1, [Action::RevealPad(17)]);
    }

    #[test]
    fn wheel_scrolling_reaches_overflowing_pads_and_keeps_window_controls_fixed() {
        for dark in [true, false] {
            for delta in [Vec2::new(0.0, -1000.0), Vec2::new(-1000.0, 0.0)] {
                let mut s = strip(&["one", "two", "three", "four", "five", "last"]);
                s.pads[5].index = 42;
                s.pads[5].paused = true;
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                let render = |events| {
                    let mut actions = Vec::new();
                    let output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                        time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                    }, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
                    (output, actions)
                };
                let visible_text = |output: &egui::FullOutput, label: &str| {
                    output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == label => {
                            let rect = text.visual_bounding_rect();
                            shape.clip_rect.contains_rect(rect).then_some(rect)
                        }
                        _ => None,
                    })
                };
                render(Vec::new());
                let (output, _) = render(Vec::new());
                assert!(visible_text(&output, "one").is_some());
                assert!(visible_text(&output, "last").is_none());
                let expand = visible_text(&output, "⛶").unwrap().center();
                let (output, actions) = render(vec![
                    egui::Event::PointerMoved(Pos2::new(90.0, 17.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point, delta, modifiers: egui::Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                    },
                ]);
                assert!(actions.is_empty());
                assert!(!output.viewport_output.values().any(|viewport| viewport.commands.iter().any(|command|
                    matches!(command, ViewportCommand::StartDrag | ViewportCommand::BeginResize(_)))));
                let mut output = output;
                for _ in 0..5 {
                    let (next, actions) = render(Vec::new());
                    assert!(actions.is_empty(), "scrolling never plays or reveals a pad");
                    output = next;
                }
                let last = visible_text(&output, "last").expect("the last pad scrolls fully into view").center();
                assert_eq!(visible_text(&output, "⛶").unwrap().center(), expand);
                for (pos, button, action) in [
                    (last, egui::PointerButton::Primary, Action::PlayPad(42)),
                    (last, egui::PointerButton::Secondary, Action::RevealPad(42)),
                    (Pos2::new(17.0, 17.0), egui::PointerButton::Primary, Action::ToggleMute),
                    (expand, egui::PointerButton::Primary, Action::Expand),
                ] {
                    let mut actions = Vec::new();
                    for pressed in [true, false] {
                        actions.extend(render(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                            pos, button, pressed, modifiers: egui::Modifiers::NONE,
                        }]).1);
                    }
                    assert_eq!(actions, [action]);
                }
                render(vec![egui::Event::PointerMoved(Pos2::new(90.0, 17.0)), egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point, delta: -delta, modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }]);
                for _ in 0..5 { render(Vec::new()); }
                let (output, _) = render(Vec::new());
                assert!(visible_text(&output, "one").is_some(), "scrolling back reaches the first pad");
                assert_eq!(visible_text(&output, "⛶").unwrap().center(), expand);
                render(vec![egui::Event::PointerMoved(Pos2::new(90.0, 17.0)), egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point, delta, modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                }]);
                for width in [MIN_WIDTH, 600.0, DEFAULT_WIDTH] {
                    let mut output = None;
                    for _ in 0..3 {
                        let mut actions = Vec::new();
                        output = Some(ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT))),
                            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), ..Default::default()
                        }, |ui| actions = draw(ui, Meter::Live(-20.0), &s)));
                        assert!(actions.is_empty(), "resizing never triggers a pad");
                    }
                    let output = output.unwrap();
                    if width == MIN_WIDTH {
                        assert!(!output.shapes.iter().any(|shape| matches!(shape.shape, egui::Shape::Text(_))));
                    } else {
                        assert!(visible_text(&output, "one").is_some(), "resizing restores the first pad");
                        if width == 600.0 {
                            for name in ["two", "three", "four", "five", "last", "Balanced"] {
                                assert!(visible_text(&output, name).is_some(), "{name} fits after expanding");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn keyboard_focus_scrolls_overflowing_pads_into_view_before_revealing_or_playing() {
        let mut s = strip(&["one", "two", "three", "four", "five", "last"]);
        s.pads[5].index = 42;
        s.pads[5].paused = true;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let render = |events| {
            let mut actions = Vec::new();
            let output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
            }, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
            (output, actions)
        };
        let (output, _) = render(Vec::new());
        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
            .find(|(_, node)| node.label() == Some("Resume last")).map(|(id, _)| id).unwrap();
        let key = |key, modifiers| vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }];
        let mut focused = false;
        for _ in 0..s.pads.len() + 5 {
            let (output, actions) = render(key(egui::Key::Tab, egui::Modifiers::NONE));
            assert!(actions.is_empty(), "tabbing does not trigger pads");
            if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
        }
        assert!(focused, "Tab reaches the pad that started outside the strip");
        for _ in 0..5 { render(Vec::new()); }
        let (output, _) = render(Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "last" && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
            "the focused pad is visible before it can be triggered");
        assert_eq!(render(key(egui::Key::F10, egui::Modifiers::SHIFT)).1, [Action::RevealPad(42)]);
        assert_eq!(render(key(egui::Key::Enter, egui::Modifiers::NONE)).1, [Action::PlayPad(42)]);
    }

    #[test]
    fn strip_tab_stops_name_their_actions_and_skip_drag_surfaces() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};
        let mut s = strip(&["one", "two", "three", "four", "five", "last"]);
        s.pads[0].playing = true;
        s.pads[1].paused = true;
        s.pads[3].loading = true;
        s.pads[4].error = Some("Reconnect the audio drive before retrying".into());
        for dark in [true, false] {
            for width in [MIN_WIDTH, DEFAULT_WIDTH, 600.0] {
                for meter in [Meter::Stopped { muted: false }, Meter::Stopped { muted: true }, Meter::Muted, Meter::Live(-20.0)] {
                    let ctx = egui::Context::default();
                    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                    ctx.enable_accesskit();
                    let render = |events| {
                        let mut actions = Vec::new();
                        let output = ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT))),
                            time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                        }, |ui| actions = draw(ui, meter, &s));
                        (output, actions)
                    };
                    for _ in 0..2 { render(Vec::new()); }
                    let mic_name = if matches!(meter, Meter::Muted | Meter::Stopped { muted: true }) { "Unmute microphone" } else { "Mute microphone" };
                    let mut expected = vec![mic_name.to_owned()];
                    if width > MIN_WIDTH {
                        expected.extend(["Pause one", "Resume two", "Play three", "Cancel loading four", "Retry five", "Play last"].map(str::to_owned));
                        if width == 600.0 { expected.push("Next preset: Balanced".into()); }
                        expected.push("Open the full window".into());
                    }
                    let key = |key, modifiers| vec![egui::Event::Key {
                        key, physical_key: None, pressed: true, repeat: false, modifiers,
                    }];
                    let mut stops = Vec::new();
                    for name in &expected {
                        let (_, actions) = render(key(egui::Key::Tab, egui::Modifiers::NONE));
                        assert!(actions.is_empty(), "Tab never triggers a strip action");
                        for _ in 0..6 { assert!(render(Vec::new()).1.is_empty()); }
                        let (output, _) = render(Vec::new());
                        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                        let node = &tree.nodes.iter().find(|(id, _)| *id == tree.focus)
                            .expect("each Tab stop is an accessible control").1;
                        assert_eq!(node.role(), Role::Button, "Tab skips the window's drag and resize surfaces");
                        assert_eq!(node.label(), Some(name.as_str()), "Tab follows the visible controls from left to right");
                        stops.push(tree.focus);
                    }
                    for &target_node in stops.iter().rev().skip(1) {
                        let (_, actions) = render(key(egui::Key::Tab, egui::Modifiers::SHIFT));
                        assert!(actions.is_empty());
                        for _ in 0..6 { assert!(render(Vec::new()).1.is_empty()); }
                        let (output, _) = render(Vec::new());
                        let tree = output.platform_output.accesskit_update.unwrap();
                        assert_eq!(tree.focus, target_node, "Shift+Tab returns through the same controls; width={width}, meter={meter:?}; focused={:?}",
                            tree.nodes.iter().find(|(id, _)| *id == tree.focus).map(|(_, node)| node.label()));
                    }
                    for (&target_node, name) in stops.iter().zip(&expected) {
                        let (output, actions) = render(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: AccessibleAction::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                        })]);
                        assert!(actions.is_empty());
                        assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target_node);
                        for _ in 0..6 { assert!(render(Vec::new()).1.is_empty()); }
                        let (output, _) = render(Vec::new());
                        let bounds = output.platform_output.accesskit_update.unwrap().nodes.iter()
                            .find(|(id, _)| *id == target_node).unwrap().1.bounds().unwrap();
                        let rect = Rect::from_min_max(Pos2::new(bounds.x0 as f32, bounds.y0 as f32),
                            Pos2::new(bounds.x1 as f32, bounds.y1 as f32));
                        assert!(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT)).contains_rect(rect),
                            "{name} stays visible when focused: {rect:?}");
                        if name != "Open the full window" {
                            let focus_stroke = ctx.style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }).visuals.selection.stroke;
                            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(paint)
                                if paint.rect == rect.shrink(3.0) && paint.stroke == focus_stroke)),
                                "{name} draws a visible focus indicator in each theme");
                        }
                        let action = if name == mic_name { Action::ToggleMute }
                            else if name == "Open the full window" { Action::Expand }
                            else if name.starts_with("Next preset:") { Action::NextPreset }
                            else { Action::PlayPad(expected.iter().position(|candidate| candidate == name).unwrap() - 1) };
                        for activate in [egui::Key::Enter, egui::Key::Space] {
                            assert_eq!(render(key(activate, egui::Modifiers::NONE)).1.as_slice(), std::slice::from_ref(&action));
                        }
                        assert_eq!(render(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: AccessibleAction::Click, target_tree: TreeId::ROOT, target_node, data: None,
                        })]).1, [action]);
                    }
                }
            }
        }
    }

    #[test]
    fn stopped_strip_microphone_announces_the_saved_mute_state_and_keeps_focus_when_toggled() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        for dark in [true, false] {
            for muted in [false, true] {
                let mut app = App::stopped(Settings { mute: muted, ..Settings::default() });
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.enable_accesskit();
                let render = |app: &App, events| {
                    let mut actions = Vec::new();
                    let output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                        events, ..Default::default()
                    }, |ui| actions = draw(ui, app.meter(), &app.strip_contents()));
                    (output, actions)
                };
                let label = |muted| if muted { "Unmute microphone" } else { "Mute microphone" };
                let (output, actions) = render(&app, Vec::new());
                assert!(actions.is_empty());
                let target_node = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                    .find(|(_, node)| node.label() == Some(label(muted))).map(|(id, _)| id)
                    .expect("the stopped microphone announces the saved mute state");
                let mut expected = app.settings.clone();
                assert!(render(&app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    action: AccessibleAction::Focus, target_tree: TreeId::ROOT, target_node, data: None,
                })]).1.is_empty());
                assert!(app.dirty_since.is_none());
                for key in [egui::Key::Enter, egui::Key::Space] {
                    assert_eq!(render(&app, vec![egui::Event::Key {
                        key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                    }]).1, [Action::ToggleMute]);
                    app.run_command(&ctx, Command::ToggleMute);
                    expected.mute = !expected.mute;
                    let (output, actions) = render(&app, Vec::new());
                    assert!(actions.is_empty());
                    let tree = output.platform_output.accesskit_update.unwrap();
                    assert_eq!(tree.focus, target_node, "changing the action label keeps microphone focus");
                    assert_eq!(tree.nodes.iter().find(|(id, _)| *id == target_node).unwrap().1.label(), Some(label(expected.mute)));
                    assert_eq!(serde_json::to_value(&app.settings).unwrap(), serde_json::to_value(&expected).unwrap());
                    assert!(app.engine.is_none() && app.playing.is_empty() && app.clip_loads.is_none());
                }
            }
        }
    }

    #[test]
    fn strip_pad_focus_survives_resizing_and_long_names() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        let mut s = strip(&["one", "two", "three", "four", "five", "WWWWWWWWWWWWWWWWWW — final announcement"]);
        s.pads[5].index = 42;
        s.pads[5].paused = true;
        for (dark, ppp, passes, long) in [true, false].into_iter().flat_map(|dark|
            [1.0, 1.25, 1.5].into_iter().flat_map(move |ppp|
                [1, 2].into_iter().flat_map(move |passes| [false, true].map(move |long| (dark, ppp, passes, long)))))
        {
            s.pads[5].name = if long { "WWWWWWWWWWWWWWWWWW — final announcement" } else { "last" }.into();
            let ctx = egui::Context::default();
            ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            let render = |width, events| {
                let mut actions = Vec::new();
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT))),
                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                };
                input.viewports.get_mut(&ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
                let output = ctx.run_ui(input, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
                (output, actions)
            };
            let label = format!("Resume {}", s.pads[5].name);
            let (output, _) = render(600.0, Vec::new());
            let target_node = *output.platform_output.accesskit_update.unwrap().nodes.iter()
                .find(|(_, node)| node.label() == Some(label.as_str())).map(|(id, _)| id).unwrap();
            assert!(render(600.0, vec![egui::Event::AccessKitActionRequest(ActionRequest {
                action: AccessibleAction::Focus, target_tree: TreeId::ROOT, target_node, data: None,
            })]).1.is_empty());
            for width in [600.0, DEFAULT_WIDTH, 130.0, 600.0, DEFAULT_WIDTH] {
                for _ in 0..8 { assert!(render(width, Vec::new()).1.is_empty(), "resizing never triggers a pad"); }
                let (output, actions) = render(width, Vec::new());
                assert!(actions.is_empty());
                let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                assert_eq!(tree.focus, target_node, "resizing preserves the focused file");
                let node = &tree.nodes.iter().find(|(id, _)| *id == target_node).unwrap().1;
                assert_eq!(node.label(), Some(label.as_str()), "a shortened button retains its full accessible name");
                let bounds = node.bounds().unwrap();
                let rect = Rect::from_min_max(Pos2::new(bounds.x0 as f32, bounds.y0 as f32),
                    Pos2::new(bounds.x1 as f32, bounds.y1 as f32));
                assert!(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT)).contains_rect(rect)
                    && output.shapes.iter().any(|shape| shape.clip_rect.expand(1.0 / ppp).contains_rect(rect)
                        && matches!(&shape.shape, egui::Shape::Rect(paint) if paint.rect == rect)),
                    "the complete focused button fits after resizing to {width}; ppp={ppp}, passes={passes}: {rect:?}");
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                    if text.galley.text().starts_with(s.pads[5].name.chars().next().unwrap())
                        && rect.expand(1.0 / ppp).contains_rect(text.visual_bounding_rect())
                        && shape.clip_rect.expand(1.0 / ppp).contains_rect(text.visual_bounding_rect()))),
                    "the focused pad's text fits its visible button");
                let focus_stroke = ctx.style_of(if dark { egui::Theme::Dark } else { egui::Theme::Light }).visuals.selection.stroke;
                assert!(output.shapes.iter().any(|shape| shape.clip_rect.expand(1.0 / ppp).contains_rect(rect.shrink(3.0))
                    && matches!(&shape.shape, egui::Shape::Rect(paint) if paint.rect == rect.shrink(3.0) && paint.stroke == focus_stroke)),
                    "the paused pad keeps a visible focus outline after resizing");
                for (key, modifiers, action) in [(egui::Key::Enter, egui::Modifiers::NONE, Action::PlayPad(42)),
                    (egui::Key::F10, egui::Modifiers::SHIFT, Action::RevealPad(42))]
                {
                    assert_eq!(render(width, vec![egui::Event::Key {
                        key, physical_key: None, pressed: true, repeat: false, modifiers,
                    }]).1, [action]);
                }
            }
        }
    }

    #[test]
    fn strip_keyboard_focus_follows_the_file_when_the_board_changes() {
        let sounds = ["same", "two", "three", "four", "five", "target", "seven"]
            .into_iter().enumerate().map(|(i, name)| {
                let mut pad = crate::config::Pad::new(std::path::PathBuf::from(format!("clip-{i}.wav")));
                pad.name = Some(name.into());
                pad.starred = true;
                pad
            }).collect();
        let mut app = App::stopped(Settings { sounds, ..Settings::default() });
        let path = app.settings.sounds[5].path.clone();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let render = |app: &App, events| {
            let strip = app.strip_contents();
            let mut actions = Vec::new();
            let output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
            }, |ui| actions = draw(ui, Meter::Live(-20.0), &strip));
            (output, actions)
        };
        let (output, _) = render(&app, Vec::new());
        let target = *output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
            .find(|(_, node)| node.label() == Some("Play target")).map(|(id, _)| id).unwrap();
        let key = |key, modifiers| vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }];
        let mut focused = false;
        for _ in 0..app.settings.sounds.len() + 5 {
            let (output, actions) = render(&app, key(egui::Key::Tab, egui::Modifiers::NONE));
            assert!(actions.is_empty());
            if output.platform_output.accesskit_update.unwrap().focus == target { focused = true; break; }
        }
        assert!(focused);
        for _ in 0..5 { render(&app, Vec::new()); }
        for delta in [1000.0, -1000.0] {
            render(&app, vec![egui::Event::PointerMoved(Pos2::new(90.0, 17.0)), egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, delta),
                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
            }]);
            for _ in 0..5 { render(&app, Vec::new()); }
            let (output, actions) = render(&app, Vec::new());
            assert!(actions.is_empty());
            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
            let visible = output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == "target" && shape.clip_rect.contains_rect(text.visual_bounding_rect())));
            assert_eq!(visible, delta < 0.0, "focused pads do not prevent scrolling away and back");
        }
        let pad = app.settings.sounds.remove(5);
        app.settings.sounds.insert(1, pad);
        for _ in 0..5 {
            let (output, actions) = render(&app, Vec::new());
            assert!(actions.is_empty(), "changing the board does not trigger a strip pad");
            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target);
        }
        assert_eq!(render(&app, key(egui::Key::Enter, egui::Modifiers::NONE)).1, [Action::PlayPad(1)],
            "Enter still targets the same file after it changes position");
        let (output, _) = render(&app, Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "target" && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
            "a moved focused pad is scrolled back into view");
        app.settings.sounds[0].name = Some("a much longer preceding name".into());
        for _ in 0..5 { render(&app, Vec::new()); }
        let (output, _) = render(&app, Vec::new());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == "target" && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
            "a wider preceding name keeps the same focused file visible");
        app.settings.sounds[0].starred = false;
        app.settings.sounds[1].name = Some("same".into());
        app.settings.sounds[2].name = Some("same".into());
        for _ in 0..5 { render(&app, Vec::new()); }
        assert_eq!(render(&app, key(egui::Key::Enter, egui::Modifiers::NONE)).1, [Action::PlayPad(1)],
            "un-starring another pad and using a duplicate name keep the focused file");
        app.remove_clip(0);
        assert_eq!(app.settings.sounds[0].path, path);
        for _ in 0..5 { render(&app, Vec::new()); }
        assert_eq!(render(&app, key(egui::Key::F10, egui::Modifiers::SHIFT)).1, [Action::RevealPad(0)],
            "removing an earlier file updates the action's board index");
        app.remove_clip(0);
        assert!(render(&app, key(egui::Key::Enter, egui::Modifiers::NONE)).1.is_empty(),
            "removing the focused file must not hand its keyboard action to another pad");
    }

    #[test]
    fn failed_strip_pads_show_the_error_and_remain_clickable_in_both_themes() {
        for dark in [true, false] {
            let mut s = strip(&["horn", "applause", "rim"]);
            s.pads[0].index = 7;
            let error = "Could not read D:\\Soundboard\\Clips\\horn.wav: Reconnect the audio drive before retrying";
            s.pads[0].error = Some(error.into());
            s.pads[1].playing = true;
            s.pads[2].loading = true;
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.enable_accesskit();
            let render = |events| {
                let mut actions = Vec::new();
                let output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, HEIGHT))),
                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                }, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
                (output, actions)
            };
            let (first, _) = render(Vec::new());
            let nodes = first.platform_output.accesskit_update.unwrap().nodes;
            for label in ["Retry horn", "Pause applause", "Cancel loading rim"] {
                assert!(nodes.iter().any(|(_, node)| node.label() == Some(label)));
            }
            let (output, _) = render(Vec::new());
            let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "! horn" => Some(text.visual_bounding_rect().center()),
                _ => None,
            }).expect("the strip shows an error marker beside the pad name");
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.stroke.color == RED && rect.rect.contains(pos))));
            render(vec![egui::Event::PointerMoved(pos)]);
            let mut tooltip = false;
            for _ in 0..6 {
                let (output, _) = render(Vec::new());
                tooltip |= output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.text().contains(error)));
            }
            assert!(tooltip, "the strip exposes the complete failure");
            let mut actions = Vec::new();
            for pressed in [true, false] {
                actions.extend(render(vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
                }]).1);
            }
            assert_eq!(actions, [Action::PlayPad(7)], "retry uses the pad's board index");
        }
    }

    #[test]
    fn native_strip_hints_fit_outside_the_bar_without_taking_focus() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        let name = "Quarterly meeting — opening music and final countdown for stage announcements";
        let error = "Could not read D:\\Soundboard\\Clips\\opening.wav: Reconnect the audio drive before retrying";
        let mut s = strip(&[name]);
        s.pads[0].paused = true;
        s.pads[0].error = Some(error.into());
        for (dark, ppp, zoom) in [
            (true, 1.0, 1.0), (false, 1.0, 1.0), (true, 1.5, 1.0), (false, 1.5, 1.0),
            (true, 1.0, 1.5), (false, 1.0, 1.5), (true, 1.5, 1.25), (false, 1.5, 1.25),
        ] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.set_zoom_factor(zoom);
            ctx.set_embed_viewports(false);
            ctx.enable_accesskit();
            let render = |events| {
                let mut actions = Vec::new();
                let size = Vec2::new(DEFAULT_WIDTH, HEIGHT);
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                };
                let info = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
                info.inner_rect = Some(Rect::from_min_size(Pos2::new(300.0, 200.0), size));
                info.outer_rect = info.inner_rect;
                info.native_pixels_per_point = Some(ppp);
                let output = ctx.run_ui(input, |ui| actions = draw(ui, Meter::Live(-20.0), &s));
                assert!(actions.is_empty(), "hovering never activates a strip control");
                output
            };
            for _ in 0..2 { render(Vec::new()); }
            let output = render(Vec::new());
            let label = format!("Resume {name}");
            let (target_node, node) = output.platform_output.accesskit_update.unwrap().nodes.into_iter()
                .find(|(_, node)| node.label() == Some(label.as_str())).unwrap();
            let bounds = node.bounds().unwrap();
            let pos = Pos2::new((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
            render(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                action: AccessibleAction::Focus, target_tree: TreeId::ROOT, target_node, data: None,
            })]);
            for _ in 0..6 { render(Vec::new()); }
            let first_hover = render(vec![egui::Event::PointerMoved(pos)]);
            assert!(!first_hover.viewport_output.values().any(|viewport| viewport.builder.title.as_deref() == Some("OpenMic hint")),
                "native hints retain the normal hover delay");
            let mut output = first_hover;
            for _ in 0..6 { output = render(Vec::new()); }
            let tree = output.platform_output.accesskit_update.as_ref().unwrap();
            assert_eq!(tree.focus, target_node);
            let (hint_id, hint) = output.viewport_output.iter()
                .find(|(_, viewport)| viewport.builder.title.as_deref() == Some("OpenMic hint"))
                .expect("a native hint has its own window beyond the strip's short clip rectangle");
            assert_eq!(hint.parent, ViewportId::ROOT);
            assert_eq!(hint.builder.active, Some(false));
            assert_eq!(hint.builder.mouse_passthrough, Some(true));
            assert_eq!(hint.builder.taskbar, Some(false));
            assert_eq!(hint.builder.decorations, Some(false));
            assert_eq!(hint.builder.resizable, Some(false));
            let size = hint.builder.inner_size.unwrap();
            assert!(size.y > HEIGHT && size.x <= 400.0, "the hint has enough room for its wrapped details: {size:?}");
            let hint_rect = Rect::from_min_size(hint.builder.position.unwrap(), size);
            assert!(!hint_rect.intersects(Rect::from_min_size(Pos2::new(300.0, 200.0), Vec2::new(DEFAULT_WIDTH, HEIGHT))),
                "the hint does not cover the hovered pad");
            let mut input = egui::RawInput {
                viewport_id: *hint_id, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(2.0), focused: false, ..Default::default()
            };
            input.viewports.insert(*hint_id, egui::ViewportInfo { parent: Some(ViewportId::ROOT), native_pixels_per_point: Some(ppp), ..Default::default() });
            let callback = hint.viewport_ui_cb.as_ref().unwrap();
            let child = ctx.run_ui(input, |ui| callback(ui));
            assert!(child.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                if text.galley.text().contains(name) && text.galley.text().contains(error)
                    && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                "the full name and error are painted inside the hint window");
            assert!(child.platform_output.accesskit_update.unwrap().nodes.iter().all(|(_, node)| !node.supports_action(AccessibleAction::Focus)),
                "hint text cannot take keyboard focus");
            let node = &tree.nodes.iter().find(|(id, _)| *id == target_node).unwrap().1;
            assert!(node.description().is_some_and(|description| description.contains(name) && description.contains(error)),
                "assistive technology can read the details without hovering");
            for _ in 0..3 { output = render(vec![egui::Event::PointerGone]); }
            assert!(!output.viewport_output.values().any(|viewport| viewport.builder.title.as_deref() == Some("OpenMic hint")),
                "leaving the strip closes the hint window");
            assert_eq!(output.platform_output.accesskit_update.unwrap().focus, target_node);
        }
    }

    #[test]
    fn strip_hints_stay_in_the_work_area_at_screen_edges() {
        for pixels in [
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1040.0)),
            Rect::from_min_size(Pos2::new(-1920.0, 40.0), Vec2::new(1920.0, 1040.0)),
            Rect::from_min_size(Pos2::new(0.0, -1080.0), Vec2::new(1280.0, 1040.0)),
            Rect::from_min_size(Pos2::new(40.0, 0.0), Vec2::new(600.0, 360.0)),
        ] {
            for ppp in [1.0, 1.25, 1.5, 2.0] {
                let work = pixels / ppp;
                let size = Vec2::new(320.0_f32.min(work.width()), 124.0_f32.min((work.height() - HEIGHT) / 2.0 - 4.0));
                for x in [work.left() - 30.0, work.right() - 200.0, work.right() - 170.0] {
                    for y in [work.top(), work.center().y - HEIGHT / 2.0, work.bottom() - HEIGHT] {
                        let window = Rect::from_min_size(Pos2::new(x, y), Vec2::new(200.0, HEIGHT));
                        let anchor = Rect::from_min_size(window.min + Vec2::new(34.0, 0.0), Vec2::new(56.0, HEIGHT));
                        let hint = Rect::from_min_size(hint_position(anchor, size, Some(work)), size);
                        assert!(work.contains_rect(hint), "hint {hint:?} escaped {work:?} at {ppp}x");
                        assert!(!window.intersects(hint), "hint {hint:?} covers its owner {window:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn strip_hint_layout_fits_taskbars_and_offscreen_strips() {
        let text = format!("{}.wav\n{}", "W".repeat(300),
            "Audio file unavailable. Reconnect the drive or open the Soundboard for details. ".repeat(160));
        for dark in [true, false] {
            for ppp in [1.0, 1.25, 1.5, 2.0] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                ctx.set_embed_viewports(false);
                for pixels in [
                    Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1032.0)),
                    Rect::from_min_size(Pos2::new(-1920.0, 48.0), Vec2::new(1920.0, 1032.0)),
                    Rect::from_min_size(Pos2::new(0.0, -1080.0), Vec2::new(1280.0, 1032.0)),
                    Rect::from_min_size(Pos2::new(48.0, 0.0), Vec2::new(592.0, 360.0)),
                ] {
                    let work = pixels / ppp;
                    for top in [work.top() - HEIGHT - 24.0, work.top(), work.center().y, work.bottom() - HEIGHT, work.bottom() + 24.0] {
                        let anchor = Rect::from_min_size(Pos2::new(work.left() + 40.0, top), Vec2::new(70.0, HEIGHT));
                        let mut input = egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT))), ..Default::default()
                        };
                        input.viewports.get_mut(&ViewportId::ROOT).unwrap().native_pixels_per_point = Some(ppp);
                        let output = ctx.run_ui(input, |ui| show_hint_window(ui, text.clone(), anchor, Some(work)));
                        let (id, viewport) = output.viewport_output.iter()
                            .find(|(_, viewport)| viewport.builder.title.as_deref() == Some("OpenMic hint")).unwrap();
                        let size = viewport.builder.inner_size.unwrap();
                        let hint = Rect::from_min_size(viewport.builder.position.unwrap(), size);
                        assert!(work.contains_rect(hint), "hint {hint:?} must fit {work:?} for owner {anchor:?} at {ppp}x");
                        assert!(!anchor.intersects(hint), "the hint does not cover its owner");
                        let mut input = egui::RawInput {
                            viewport_id: *id, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), focused: false, ..Default::default()
                        };
                        input.viewports.insert(*id, egui::ViewportInfo { parent: Some(ViewportId::ROOT), native_pixels_per_point: Some(ppp), ..Default::default() });
                        let callback = viewport.viewport_ui_cb.as_ref().unwrap();
                        let child = ctx.run_ui(input, |ui| callback(ui));
                        assert!(child.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
                            if text.galley.elided && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                            "long errors use an ellipsis inside the hint window");
                    }
                }
            }
        }
    }

    #[test]
    fn native_strip_hints_follow_the_pad_state_and_close_for_clicks_and_removal() {
        let name = "opening music";
        let error = "Reconnect the audio drive before retrying";
        for (state, action, hint) in [
            ("ready", "Play", "Click to play"), ("playing", "Pause", "Click to pause"),
            ("paused", "Resume", "Paused. Click to resume"), ("failed", "Retry", "Click to retry"),
            ("loading", "Cancel loading", "Loading… Click to cancel"),
        ] {
            let ctx = egui::Context::default();
            ctx.set_embed_viewports(false);
            ctx.enable_accesskit();
            let mut s = strip(&[name]);
            s.pads[0].index = 7;
            s.pads[0].playing = state == "playing";
            s.pads[0].paused = state == "paused";
            s.pads[0].loading = state == "loading";
            if matches!(state, "failed" | "loading") { s.pads[0].error = Some(error.into()); }
            let render = |s: &Strip, events| {
                let size = Vec2::new(DEFAULT_WIDTH, HEIGHT);
                let mut input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                    time: Some(ctx.cumulative_frame_nr() as f64 * 0.2), events, ..Default::default()
                };
                let info = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
                info.inner_rect = Some(Rect::from_min_size(Pos2::new(300.0, 200.0), size));
                info.native_pixels_per_point = Some(1.0);
                let mut actions = Vec::new();
                let output = ctx.run_ui(input, |ui| actions = draw(ui, Meter::Live(-20.0), s));
                (output, actions)
            };
            let (output, _) = render(&s, Vec::new());
            let label = format!("{action} {name}");
            let node = &output.platform_output.accesskit_update.unwrap().nodes.into_iter()
                .find(|(_, node)| node.label() == Some(label.as_str())).unwrap().1;
            let bounds = node.bounds().unwrap();
            let pos = Pos2::new((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
            let description = node.description().unwrap();
            assert!(description.contains(name) && description.contains(hint));
            assert_eq!(description.contains(error), state == "failed", "loading suppresses the previous failure");
            render(&s, vec![egui::Event::PointerMoved(pos)]);
            let mut output = render(&s, Vec::new()).0;
            for _ in 0..6 { output = render(&s, Vec::new()).0; }
            let has_hint = |output: &egui::FullOutput| output.viewport_output.values()
                .any(|viewport| viewport.builder.title.as_deref() == Some("OpenMic hint"));
            assert!(has_hint(&output), "{state} has a native hint");
            let click = |pressed| vec![egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
            }];
            let (mut output, actions) = render(&s, click(true));
            assert!(!has_hint(&output), "pressing closes the read-only hint");
            assert!(actions.is_empty());
            assert_eq!(render(&s, click(false)).1, [Action::PlayPad(7)], "the click still reaches the original pad");
            for _ in 0..8 { output = render(&s, Vec::new()).0; }
            assert!(!has_hint(&output), "resting the pointer after clicking keeps the hint closed");
            render(&s, vec![egui::Event::PointerMoved(pos + Vec2::new(1.0, 0.0))]);
            for _ in 0..6 { output = render(&s, Vec::new()).0; }
            assert!(has_hint(&output), "hovering again restores the hint");
            let (scrolled, actions) = render(&s, vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -40.0),
                modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
            }]);
            assert!(!has_hint(&scrolled), "scrolling closes the hint while browsing pads");
            assert!(actions.is_empty());
            for _ in 0..8 { output = render(&s, Vec::new()).0; }
            assert!(has_hint(&output), "a stationary hover restores the hint after scrolling");
            s.pads.clear();
            s.preset = "WWWWWWWWWWWWWWWWWWWWWWWW".into();
            for _ in 0..3 { output = render(&s, Vec::new()).0; }
            assert!(!has_hint(&output), "removing the hovered pad removes its native hint");
        }
    }

    #[test]
    fn native_strip_hints_bound_very_long_errors_and_keep_the_complete_description() {
        let error = "Audio file unavailable. Reconnect the drive or open the Soundboard for details. ".repeat(160);
        let mut s = strip(&["opening music"]);
        s.pads[0].error = Some(error.clone());
        let window = Rect::from_min_size(Pos2::new(300.0, 200.0), Vec2::new(DEFAULT_WIDTH, HEIGHT));
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        ctx.enable_accesskit();
        let mut output = None;
        for step in 0..8 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, window.size())), time: Some(step as f64 * 0.2),
                events: if step == 0 { vec![egui::Event::PointerMoved(Pos2::new(85.0, 17.0))] } else { Vec::new() }, ..Default::default()
            };
            let info = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
            info.inner_rect = Some(window);
            info.native_pixels_per_point = Some(1.5);
            output = Some(ctx.run_ui(input, |ui| { draw(ui, Meter::Live(-20.0), &s); }));
        }
        let output = output.unwrap();
        let node = &output.platform_output.accesskit_update.as_ref().unwrap().nodes.iter()
            .find(|(_, node)| node.label() == Some("Retry opening music")).unwrap().1;
        assert!(node.description().unwrap().contains(&error), "screen readers retain the entire error");
        let (id, hint) = output.viewport_output.iter().find(|(_, viewport)| viewport.builder.title.as_deref() == Some("OpenMic hint")).unwrap();
        let size = hint.builder.inner_size.unwrap();
        if let Some(work) = monitor_work_area(window, 1.5) {
            assert!(work.contains_rect(Rect::from_min_size(hint.builder.position.unwrap(), size)), "the hint fits the monitor work area");
        }
        let mut input = egui::RawInput {
            viewport_id: *id, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), focused: false, ..Default::default()
        };
        input.viewports.insert(*id, egui::ViewportInfo { parent: Some(ViewportId::ROOT), native_pixels_per_point: Some(1.5), ..Default::default() });
        let callback = hint.viewport_ui_cb.as_ref().unwrap();
        let child = ctx.run_ui(input, |ui| callback(ui));
        assert!(child.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.elided && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
            "very long errors end with an ellipsis within the hint window");
    }

    #[test]
    #[ignore = "writes native strip hint screenshots; needs Python with Pillow"]
    fn snapshot_native_strip_hints() {
        struct StripPreview(Strip, Meter);
        impl eframe::App for StripPreview {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let _ = draw(ui, self.1, &self.0);
            }
        }
        struct EdgePreview { text: String, anchor: Rect, work: Rect }
        impl eframe::App for EdgePreview {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                show_hint_window(ui, self.text.clone(), self.anchor, Some(self.work));
            }
        }
        let name = "Quarterly meeting — opening music and final countdown for stage announcements";
        let error = "Could not read D:\\Soundboard\\Clips\\opening.wav: Reconnect the audio drive before retrying";
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for (state, ppp) in [
                ("paused", 1.0), ("paused", 1.25), ("paused", 1.5), ("loading", 1.25),
                ("failed", 1.25), ("microphone", 1.25), ("preset", 1.25), ("expand", 1.25),
            ] {
                let mut s = strip(&[name]);
                s.pads[0].paused = state == "paused";
                s.pads[0].loading = state == "loading";
                s.pads[0].error = Some(error.into());
                let meter = Meter::Stopped { muted: true };
                let (width, target) = match state {
                    "microphone" => (MIN_WIDTH, "Unmute microphone".to_owned()),
                    "preset" => {
                        s.pads.clear();
                        s.preset = "Balanced — custom room and meeting settings".into();
                        (600.0, format!("Next preset: {}", s.preset))
                    }
                    "expand" => (DEFAULT_WIDTH, "Open the full window".to_owned()),
                    _ => (DEFAULT_WIDTH, format!("{} {name}", if state == "paused" { "Resume" }
                        else if state == "loading" { "Cancel loading" } else { "Retry" })),
                };
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT))), ..Default::default()
                }, |ui| { draw(ui, meter, &s); });
                let node = &output.platform_output.accesskit_update.unwrap().nodes.into_iter()
                    .find(|(_, node)| node.label() == Some(target.as_str())).unwrap().1;
                let bounds = node.bounds().unwrap();
                let pos = Pos2::new((bounds.x0 + bounds.x1) as f32 * 0.5, (bounds.y0 + bounds.y1) as f32 * 0.5);
                let mut preview = StripPreview(s, meter);
                let path = super::super::snapshot::snapshot_viewport_with_events(&mut preview,
                    &format!("strip-native-hint-{state}-{}-{theme}", (ppp * 100.0) as u32), Vec2::new(width, HEIGHT), ppp, dark,
                    vec![vec![egui::Event::PointerMoved(pos)], Vec::new(), Vec::new()],
                    ViewportId::from_hash_of((ViewportId::ROOT, "strip-hint")));
                println!("SNAPSHOT {}", path.display());
            }
            for ppp in [1.0, 1.5] {
                let work = Rect::from_min_size(Pos2::new(-640.0, -360.0), Vec2::new(600.0, 360.0)) / ppp;
                for (top, edge) in [(work.top() - HEIGHT - 24.0, "top"), (work.bottom() + 24.0, "bottom")] {
                    let mut preview = EdgePreview {
                        text: format!("opening music\nClick to retry\nRight-click or Shift+F10 for the Soundboard.\n\n{}", format!("{error}\n").repeat(40)),
                        anchor: Rect::from_min_size(Pos2::new(work.right() - 40.0, top), Vec2::new(70.0, HEIGHT)), work,
                    };
                    let path = super::super::snapshot::snapshot_viewport_with_events(&mut preview,
                        &format!("strip-native-hint-edge-{edge}-{}-{theme}", (ppp * 100.0) as u32), Vec2::new(DEFAULT_WIDTH, HEIGHT), ppp, dark,
                        Vec::new(), ViewportId::from_hash_of((ViewportId::ROOT, "strip-hint")));
                    println!("SNAPSHOT {}", path.display());
                }
            }
        }
    }

    #[test]
    #[ignore = "writes strip playback screenshots; needs Python with Pillow"]
    fn snapshot_strip_playback_states() {
        struct StripPreview(Strip, Meter);
        impl eframe::App for StripPreview {
            fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
                let rect = Rect::from_min_size(ui.max_rect().min, Vec2::new(ui.available_width(), HEIGHT));
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    let _ = draw(ui, self.1, &self.0);
                });
            }
        }
        let mut s = strip(&["horn", "rim", "ready", "load", "failed"]);
        s.pads[0].playing = true;
        s.pads[1].paused = true;
        let error = "Could not read D:\\Soundboard\\Clips\\rim.wav: Reconnect the audio drive before retrying";
        s.pads[1].error = Some(error.into());
        s.pads[3].loading = true;
        s.pads[4].error = Some("Audio file unavailable; reconnect its drive and retry".into());
        let (texts, _) = run(Meter::Live(-20.0), &s, 600.0, None, false);
        let pos = texts.iter().find(|(text, _)| text == "rim").unwrap().1.center();
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            let mut preview = StripPreview(s.clone(), Meter::Live(-20.0));
            for (width, name) in [(600.0, "strip-playback"), (200.0, "strip-playback-narrow")] {
                let image = super::super::snapshot::snapshot(&mut preview, &format!("{name}-{theme}"), Vec2::new(width, HEIGHT), 1.5, dark);
                println!("SNAPSHOT {}", image.display());
            }
            let image = super::super::snapshot::snapshot_with_events(
                &mut preview, &format!("strip-playback-scrolled-{theme}"), Vec2::new(200.0, HEIGHT), 1.5, dark,
                vec![vec![egui::Event::PointerMoved(Pos2::new(90.0, 17.0)), egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -1000.0),
                    modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move,
                }], vec![egui::Event::PointerGone]],
            );
            println!("SNAPSHOT {}", image.display());
            let image = super::super::snapshot::snapshot_with_events(
                &mut preview, &format!("strip-paused-source-error-{theme}"), Vec2::new(600.0, 140.0), 1.25, dark,
                vec![vec![egui::Event::PointerMoved(pos)]],
            );
            println!("SNAPSHOT {}", image.display());
            let image = super::super::snapshot::snapshot_with_events(
                &mut preview, &format!("strip-paused-source-error-native-{theme}"), Vec2::new(200.0, HEIGHT), 1.5, dark,
                vec![vec![egui::Event::PointerMoved(pos)]],
            );
            println!("SNAPSHOT {}", image.display());
            let tab = vec![egui::Event::Key {
                key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
            }];
            let focus = |count| vec![Vec::new(), Vec::new()].into_iter()
                .chain(std::iter::repeat_n(tab.clone(), count)).collect();
            for (meter, state, width) in [
                (Meter::Stopped { muted: false }, "stopped", DEFAULT_WIDTH), (Meter::Live(-20.0), "live", DEFAULT_WIDTH),
                (Meter::Muted, "muted", DEFAULT_WIDTH), (Meter::Muted, "icon", MIN_WIDTH),
                (Meter::Stopped { muted: true }, "stopped-muted", DEFAULT_WIDTH),
                (Meter::Stopped { muted: true }, "stopped-muted-icon", MIN_WIDTH),
            ] {
                let mut preview = StripPreview(s.clone(), meter);
                let image = super::super::snapshot::snapshot_with_events(&mut preview,
                    &format!("strip-focused-microphone-{state}-{theme}"), Vec2::new(width, HEIGHT), 1.5, dark, focus(1));
                println!("SNAPSHOT {}", image.display());
            }
            let mut long = strip(&["one", "two", "three", "four", "five", "WWWWWWWWWWWWWWWWWW — final announcement"]);
            long.pads[5].paused = true;
            for (width, ppp, name) in [(600.0, 1.0, "wide"), (200.0, 1.25, "medium"), (130.0, 1.5, "narrow")] {
                let mut preview = StripPreview(long.clone(), Meter::Live(-20.0));
                let image = super::super::snapshot::snapshot_with_events(&mut preview,
                    &format!("strip-focused-long-pad-{name}-{theme}"), Vec2::new(width, HEIGHT), ppp, dark, focus(long.pads.len() + 1));
                println!("SNAPSHOT {}", image.display());
            }
            for (name, count) in [("preset", s.pads.len() + 2), ("expand", s.pads.len() + 3)] {
                let mut preview = StripPreview(s.clone(), Meter::Live(-20.0));
                let image = super::super::snapshot::snapshot_with_events(&mut preview,
                    &format!("strip-focused-{name}-{theme}"), Vec2::new(600.0, HEIGHT), 1.5, dark, focus(count));
                println!("SNAPSHOT {}", image.display());
            }
        }
    }

    #[test]
    fn a_strip_resized_to_the_icon_still_mutes_and_shows_no_text() {
        for meter in [Meter::Stopped { muted: false }, Meter::Stopped { muted: true }, Meter::Muted, Meter::Live(-20.0)] {
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
        let pads: Vec<f32> = s.pads.iter().map(|pad| measure(&pad.name)).collect();
        let width = fitting_width(&pads, measure(&s.preset));
        let (texts, _) = run(Meter::Live(-20.0), &s, width, None, false);
        for label in ["airhorn", "rimshot", "bruh", "Balanced", "⛶"] {
            assert!(names(&texts).contains(&label), "{label} doesn't fit in {width}: {:?}", names(&texts));
        }
    }

    /// Press at `from`, move to `to`, release; returns the window commands
    /// sent and the actions reported.
    fn drag(from: Pos2, to: Pos2) -> (Vec<ViewportCommand>, Vec<Action>) {
        drag_strip(from, to, &strip(&[]))
    }

    fn drag_strip(from: Pos2, to: Pos2, strip: &Strip) -> (Vec<ViewportCommand>, Vec<Action>) {
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
            let output = ctx.run_ui(input, |ui| actions.extend(draw(ui, Meter::Live(-20.0), strip)));
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

        let (commands, actions) = drag_strip(
            Pos2::new(55.0, 17.0), Pos2::new(85.0, 20.0), &strip(&["one", "two", "three", "four", "last"]),
        );
        assert!(commands.contains(&ViewportCommand::StartDrag), "dragging a pad still moves the strip");
        assert!(!commands.iter().any(|command| matches!(command, ViewportCommand::BeginResize(_))));
        assert!(actions.is_empty(), "dragging a pad does not trigger it");
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

    #[cfg(windows)]
    #[test]
    fn a_strip_saved_off_every_monitor_keeps_its_default_position() {
        for saved in [[-100_000, -100_000], [i32::MAX, i32::MAX]] {
            let mut app = App::stopped(Settings { collapsed: true, overlay_position: Some(saved), ..Settings::default() });
            let ctx = egui::Context::default();
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(DEFAULT_WIDTH, HEIGHT));
            let mut input = egui::RawInput { screen_rect: Some(rect), ..Default::default() };
            let info = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
            info.outer_rect = Some(rect);
            info.native_pixels_per_point = Some(1.0);
            let output = ctx.run_ui(input, |ui| app.show_overlay(ui.ctx()));
            assert!(app.overlay_placement.placed, "the strip was shown");
            assert!(
                output.viewport_output.into_values().flat_map(|v| v.commands)
                    .all(|command| !matches!(command, ViewportCommand::OuterPosition(_))),
                "an off-screen saved position must not move the strip"
            );
        }
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

