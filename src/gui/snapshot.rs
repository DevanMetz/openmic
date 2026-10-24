//! Test-only software renderer: turns an egui frame into an image, so the
//! interface can be looked at without a window or a GPU. egui hands over
//! triangle meshes with anti-aliasing already baked in (feathered edges),
//! and its font atlas as plain RGBA, so filling triangles is enough.
//!
//! `cargo test --release -- --ignored snapshot` writes PNGs (via a PPM and
//! Python's Pillow) to `target/snapshots/`.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, Color32, ColorImage, Pos2, TextureId};

use super::App;
use crate::config::Settings;
use crate::engine::ScopeFrame;

#[cfg(windows)]
const DEFAULT_MIC_HELP: &str = "In Discord: leave Voice & Video > Input Device on Default";
#[cfg(not(windows))]
const DEFAULT_MIC_HELP: &str = "Make OpenMic Microphone my default mic while running";

#[cfg(windows)]
const DICTATION_HELP: &str = "Your words are typed into the app you're using. Set your Shortcut above.";
#[cfg(not(windows))]
const DICTATION_HELP: &str = "Your words appear below to copy into the app you're using. Set your Shortcut above.";

struct Canvas {
    width: usize,
    height: usize,
    /// Premultiplied RGBA in 0..1, gamma space (as egui blends).
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(width: usize, height: usize, background: Color32) -> Self {
        let bg = to_f(background);
        Self { width, height, pixels: vec![bg; width * height] }
    }

    fn triangle(&mut self, v: [&egui::epaint::Vertex; 3], ppp: f32, clip: egui::Rect, texture: Option<&ColorImage>) {
        let p: Vec<Pos2> = v.iter().map(|v| Pos2::new(v.pos.x * ppp, v.pos.y * ppp)).collect();
        let clip = egui::Rect::from_min_max(clip.min * ppp, clip.max * ppp);
        let min_x = p.iter().map(|q| q.x).fold(f32::MAX, f32::min).max(clip.left()).max(0.0).floor() as i32;
        let max_x = p.iter().map(|q| q.x).fold(f32::MIN, f32::max).min(clip.right()).min(self.width as f32).ceil() as i32;
        let min_y = p.iter().map(|q| q.y).fold(f32::MAX, f32::min).max(clip.top()).max(0.0).floor() as i32;
        let max_y = p.iter().map(|q| q.y).fold(f32::MIN, f32::max).min(clip.bottom()).min(self.height as f32).ceil() as i32;
        let area = (p[1].x - p[0].x) * (p[2].y - p[0].y) - (p[2].x - p[0].x) * (p[1].y - p[0].y);
        if area.abs() < 1e-6 {
            return;
        }
        let colors = v.map(|v| to_f(v.color));
        for y in min_y..max_y {
            for x in min_x..max_x {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((p[1].x - px) * (p[2].y - py) - (p[2].x - px) * (p[1].y - py)) / area;
                let w1 = ((p[2].x - px) * (p[0].y - py) - (p[0].x - px) * (p[2].y - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let mut c = [0.0f32; 4];
                for k in 0..4 {
                    c[k] = colors[0][k] * w0 + colors[1][k] * w1 + colors[2][k] * w2;
                }
                if let Some(tex) = texture {
                    let u = v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2;
                    let t = v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2;
                    let s = sample(tex, u, t);
                    for k in 0..4 {
                        c[k] *= s[k];
                    }
                }
                let dst = &mut self.pixels[y as usize * self.width + x as usize];
                for k in 0..4 {
                    dst[k] = c[k] + dst[k] * (1.0 - c[3]);
                }
            }
        }
    }

    fn save_png(&self, name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        let ppm = dir.join(format!("{name}.ppm"));
        let mut bytes = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for p in &self.pixels {
            bytes.extend(p[..3].iter().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }
        std::fs::write(&ppm, bytes).unwrap();
        let png = dir.join(format!("{name}.png"));
        let status = std::process::Command::new("python")
            .args(["-c", "import sys; from PIL import Image; Image.open(sys.argv[1]).save(sys.argv[2])"])
            .arg(&ppm)
            .arg(&png)
            .status();
        if status.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_file(&ppm);
            png
        } else {
            ppm
        }
    }
}

fn to_f(c: Color32) -> [f32; 4] {
    let [r, g, b, a] = c.to_array();
    [r, g, b, a].map(|x| x as f32 / 255.0)
}

/// Measure authored text colors against the painted background, before font smoothing.
fn assert_text_contrast(output: &egui::FullOutput, label: &str) -> Color32 {
    let (index, text) = output.shapes.iter().enumerate().find_map(|(index, shape)| match &shape.shape {
        egui::Shape::Text(text) if text.galley.text() == label => Some((index, text)),
        _ => None,
    }).unwrap_or_else(|| panic!("missing text: {label}"));
    let pos = text.visual_bounding_rect().center();
    let mut background = [0.0; 3];
    let mut opaque = false;
    for shape in &output.shapes[..index] {
        if shape.clip_rect.contains(pos)
            && let egui::Shape::Rect(rect) = &shape.shape
            && rect.rect.contains(pos)
        {
            let color = to_f(rect.fill);
            opaque |= rect.fill.a() == 255;
            for channel in 0..3 {
                background[channel] = (color[channel] + background[channel] * (1.0 - color[3])).clamp(0.0, 1.0);
            }
        }
    }
    assert!(opaque, "{label} has a painted background");
    let color = text.override_text_color.unwrap_or_else(|| {
        let color = text.galley.job.sections[0].format.color;
        if color == Color32::PLACEHOLDER { text.fallback_color } else { color }
    });
    assert_eq!(text.opacity_factor, 1.0);
    // https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html
    let luminance = |rgb: [f32; 3]| {
        let [r, g, b] = rgb.map(|value| {
            if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    };
    let [r, g, b, alpha] = to_f(color);
    let foreground = luminance([
        r + background[0] * (1.0 - alpha),
        g + background[1] * (1.0 - alpha),
        b + background[2] * (1.0 - alpha),
    ]);
    let background = luminance(background);
    let ratio = (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05);
    assert!(ratio >= 4.5, "{label}: contrast {ratio:.2}:1 is below 4.5:1");
    color
}

#[test]
fn native_helper_text_and_links_remain_readable_when_the_system_theme_changes() {
    for passes in [1, 2] {
        let ctx = egui::Context::default();
        crate::widgets::configure_text_styles(&ctx);
        ctx.options_mut(|options| options.max_passes = passes.try_into().unwrap());
        ctx.enable_accesskit();
        let mut app = App::stopped(Settings {
            microphone: "Test microphone".into(),
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            sounds: vec![crate::config::Pad::new("C:/clips/sample.wav".into())],
            ..Default::default()
        });
        app.inputs = vec![app.settings.microphone.clone()];
        app.outputs = vec![app.settings.output.clone()];
        app.settings.setup.discord = true;
        app.settings.setup.tested = true;
        let original = serde_json::to_value(&app.settings).unwrap();
        let mut frame = eframe::Frame::_new_kittest();
        let mut link_id = None;
        for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            for page in super::Page::ALL {
                app.page = page;
                let mut output = None;
                for _ in 0..3 {
                    output = Some(ctx.run_ui(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(760.0, 1400.0))),
                        system_theme: Some(theme),
                        ..Default::default()
                    }, |ui| eframe::App::ui(&mut app, ui, &mut frame)));
                }
                assert_eq!(ctx.theme(), theme);
                assert_eq!(ctx.options(|options| options.theme_preference), egui::ThemePreference::System);
                let output = output.unwrap();
                assert_text_contrast(&output, "Clean voice. Instant sounds. Fully local.");
                let labels: &[&str] = match page {
                    super::Page::Voice => &[
                        "Microphone", "Processed output", "Monitor output",
                        DEFAULT_MIC_HELP,
                        "How loud you are going in", "Keeps your voice, drops the room",
                        "Start OpenMic to see your voice live", "DeepFilterNet",
                    ],
                    super::Page::Soundboard => &[
                        "Drop audio files to add clips. Click a pad to play, pause or resume.",
                        "Click a name to rename; drag to reorder.", "Clips only", "WAV",
                    ],
                    super::Page::Record => &[
                        "Record your microphone or what's playing on your computer, then save it or add it to your pads.",
                        "Source", "Device", "Records the raw mic, before OpenMic processing.",
                        "Recording stays on this computer.",
                    ],
                    super::Page::Dictation => &[
                        "Shortcut", "Model", DICTATION_HELP,
                    ],
                    super::Page::Settings => &[
                        "Mute mic", "Bypass processing", "Stop all clips", "Speech to text",
                        "Soundboard pads get their own hotkeys: right-click a pad.",
                        "Click the version number at the top to check now.",
                    ],
                };
                for label in labels { assert_text_contrast(&output, label); }
                if page == super::Page::Voice {
                    let tree = output.platform_output.accesskit_update.as_ref().unwrap();
                    let id = *tree.nodes.iter().find(|(_, node)| node.role() == egui::accesskit::Role::Link
                        && node.label() == Some("DeepFilterNet")).map(|(id, _)| id).unwrap();
                    if let Some(previous) = link_id { assert_eq!(id, previous, "the model link keeps its identity across themes"); }
                    link_id = Some(id);
                }
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                assert!(app.dirty_since.is_none() && app.engine.is_none() && app.capturing.is_none());
            }
        }
    }
}

#[test]
fn accent_headings_and_badges_have_readable_contrast_in_both_themes() {
    let ctx = egui::Context::default();
    for dark in [true, false, true] {
        ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        let accents = [super::CYAN, super::GREEN, super::AMBER, super::VIOLET, super::RED, super::MUTED];
        let mut output = None;
        for _ in 0..3 {
            output = Some(ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 720.0))),
                ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    for (index, accent) in accents.into_iter().enumerate() {
                        crate::widgets::card(ui, &format!("Heading {index}"), accent,
                            |ui| crate::widgets::badge(ui, &format!("Badge {index}"), accent), |_| {});
                        ui.add_space(8.0);
                    }
                });
            }));
        }
        for (index, accent) in accents.into_iter().enumerate() {
            for label in [format!("Heading {index}"), format!("Badge {index}")] {
                let color = assert_text_contrast(output.as_ref().unwrap(), &label);
                if dark { assert_eq!(color, accent, "dark-theme text keeps its accent"); }
            }
        }
    }
}

#[test]
fn page_status_messages_have_readable_contrast_in_both_themes() {
    for dark in [true, false] {
        for page in [super::Page::Settings, super::Page::Dictation, super::Page::Record, super::Page::Soundboard] {
            for accent in [super::CYAN, super::GREEN, super::AMBER, super::VIOLET, super::RED, super::MUTED] {
                let mut app = App::stopped(Settings::default());
                app.page = page;
                app.status = ("Device message".into(), accent);
                let message = ("Page message".into(), accent);
                match page {
                    super::Page::Settings => app.hotkey_status = message,
                    super::Page::Dictation => app.dictation_status = message,
                    super::Page::Record => app.record_status = message,
                    _ => app.sound_status = message,
                }
                let original = serde_json::to_value(&app.settings).unwrap();
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                let mut frame = eframe::Frame::_new_kittest();
                let mut output = None;
                for _ in 0..3 {
                    output = Some(ctx.run_ui(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 1000.0))),
                        ..Default::default()
                    }, |ui| eframe::App::ui(&mut app, ui, &mut frame)));
                }
                for label in ["Device message", "Page message"] {
                    let color = assert_text_contrast(output.as_ref().unwrap(), label);
                    if dark { assert_eq!(color, accent); }
                }
                assert_eq!(app.status.1, accent, "rendering preserves the status itself");
                assert_eq!(serde_json::to_value(&app.settings).unwrap(), original);
                assert!(app.dirty_since.is_none());
            }
        }
    }
}

/// Bilinear sample of a premultiplied texture.
fn sample(tex: &ColorImage, u: f32, v: f32) -> [f32; 4] {
    let [w, h] = tex.size;
    let x = (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
    let y = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize| to_f(tex.pixels[y * w + x]);
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0.0; 4];
    for k in 0..4 {
        out[k] = (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy;
    }
    out
}

/// Draw the app at `size` points and `ppp` pixels per point, and save it.
pub(super) fn snapshot(app: &mut impl eframe::App, name: &str, size: egui::Vec2, ppp: f32, dark: bool) -> PathBuf {
    snapshot_with_events(app, name, size, ppp, dark, Vec::new())
}

/// Replay input first, then let menus and animations settle before rendering.
pub(super) fn snapshot_with_events(app: &mut impl eframe::App, name: &str, size: egui::Vec2, ppp: f32, dark: bool, events: Vec<Vec<egui::Event>>) -> PathBuf {
    let inputs = events.into_iter().map(|events| egui::RawInput { events, ..Default::default() }).collect();
    snapshot_frame(app, name, size, ppp, dark, inputs, None)
}

/// Replay complete input frames, including window resizes, before rendering.
pub(super) fn snapshot_with_inputs(app: &mut impl eframe::App, name: &str, size: egui::Vec2, ppp: f32, dark: bool, inputs: Vec<egui::RawInput>) -> PathBuf {
    snapshot_frame(app, name, size, ppp, dark, inputs, None)
}

/// Render a deferred native window using the callback its parent registered.
pub(super) fn snapshot_viewport_with_events(app: &mut impl eframe::App, name: &str, size: egui::Vec2, ppp: f32, dark: bool, events: Vec<Vec<egui::Event>>, viewport: egui::ViewportId) -> PathBuf {
    let inputs = events.into_iter().map(|events| egui::RawInput { events, ..Default::default() }).collect();
    snapshot_frame(app, name, size, ppp, dark, inputs, Some(viewport))
}

fn snapshot_frame(app: &mut impl eframe::App, name: &str, size: egui::Vec2, ppp: f32, dark: bool, inputs: Vec<egui::RawInput>, viewport: Option<egui::ViewportId>) -> PathBuf {
    let ctx = egui::Context::default();
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    if viewport.is_some() { ctx.set_embed_viewports(false); }
    crate::widgets::configure_text_styles(&ctx);
    let mut frame = eframe::Frame::_new_kittest();
    let mut textures: HashMap<TextureId, ColorImage> = HashMap::new();
    let mut output = None;
    let frames = inputs.len() + 4;
    let mut inputs = inputs.into_iter();
    for step in 0..frames {
        let mut input = inputs.next().unwrap_or_default();
        input.screen_rect.get_or_insert(egui::Rect::from_min_size(Pos2::ZERO, size));
        input.time.get_or_insert(step as f64 * 0.2);
        let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        info.native_pixels_per_point = Some(ppp);
        if viewport.is_some() {
            info.inner_rect = Some(egui::Rect::from_min_size(egui::pos2(300.0, 200.0), size));
            info.outer_rect = info.inner_rect;
        }
        let mut full = ctx.run_ui(input, |ui| app.ui(ui, &mut frame));
        update_textures(&mut textures, &full);
        let mut render_size = size;
        if let Some(viewport) = viewport {
            let Some(child) = full.viewport_output.get(&viewport) else { output = None; continue };
            render_size = child.builder.inner_size.expect("native snapshot specifies its size");
            let callback = child.viewport_ui_cb.as_ref().expect("native snapshot has a deferred callback").clone();
            let mut input = egui::RawInput {
                viewport_id: viewport, screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, render_size)),
                time: Some(step as f64 * 0.2), focused: false, ..Default::default()
            };
            input.viewports.insert(viewport, egui::ViewportInfo {
                parent: Some(egui::ViewportId::ROOT), native_pixels_per_point: Some(ppp), ..Default::default()
            });
            full = ctx.run_ui(input, |ui| callback(ui));
            update_textures(&mut textures, &full);
        }
        output = Some((full, render_size));
    }
    let (output, size) = output.expect("requested snapshot window was rendered");
    let primitives = ctx.tessellate(output.shapes, ppp);
    let background = ctx.global_style().visuals.panel_fill;
    let mut canvas = Canvas::new((size.x * ppp) as usize, (size.y * ppp) as usize, background);
    for clipped in &primitives {
        let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive else { continue };
        let texture = textures.get(&mesh.texture_id);
        for tri in mesh.indices.chunks(3) {
            let v = [&mesh.vertices[tri[0] as usize], &mesh.vertices[tri[1] as usize], &mesh.vertices[tri[2] as usize]];
            canvas.triangle(v, ppp, clipped.clip_rect, texture);
        }
    }
    canvas.save_png(name)
}

fn update_textures(textures: &mut HashMap<TextureId, ColorImage>, full: &egui::FullOutput) {
    for (id, deltas) in &full.textures_delta.set {
        for delta in deltas {
            let egui::ImageData::Color(image) = &delta.image;
            match delta.pos {
                None => { textures.insert(*id, (**image).clone()); }
                Some([x0, y0]) => {
                    let tex = textures.get_mut(id).expect("patch to a known texture");
                    for y in 0..image.size[1] {
                        for x in 0..image.size[0] {
                            tex.pixels[(y0 + y) * tex.size[0] + x0 + x] = image.pixels[y * image.size[0] + x];
                        }
                    }
                }
            }
        }
    }
}

/// Speech-like history for the live pictures: syllables with gaps, a
/// room-noise floor, and the voice gate closing between phrases.
pub(super) fn demo_frames() -> Vec<ScopeFrame> {
    (0..crate::engine::SCOPE_FRAMES)
        .map(|i| {
            let t = i as f32 / 100.0;
            let phrase = (t * 1.3).sin() > -0.2;
            let syllable = ((t * 11.0).sin() * 0.5 + 0.5) * if phrase { 1.0 } else { 0.0 };
            let noise = -46.0 + (i as f32 * 12.9898).sin() * 3.0;
            let voice = -32.0 + syllable * 18.0;
            let input_db = if syllable > 0.15 { voice } else { noise };
            let prob = if phrase { 0.55 + syllable * 0.4 } else { 0.05 + ((i as f32 * 3.7).sin() * 0.5 + 0.5) * 0.15 };
            let voice_gate = if prob > 0.6 { 1.0 } else { 0.0 };
            ScopeFrame {
                input_db,
                output_db: if voice_gate > 0.5 && syllable > 0.15 { voice - 2.0 } else { -70.0 },
                prob,
                voice_gate,
                level_gate: if input_db > -50.0 { 1.0 } else { 0.0 },
            }
        })
        .collect()
}

#[test]
#[ignore]
fn snapshot_voice_page() {
    for (dark, theme) in [(true, "dark"), (false, "light")] {
        let mut app = App::stopped(Settings {
            microphone: "Microphone (Yeti Stereo Microphone)".into(),
            output: "CABLE Input (VB-Audio Virtual Cable)".into(),
            ..Settings::default()
        });
        app.inputs = vec![app.settings.microphone.clone()];
        app.outputs = vec![app.settings.output.clone()];
        app.settings.setup.discord = true;
        app.settings.setup.tested = true;
        app.demo_frames = demo_frames();
        let path = snapshot(&mut app, &format!("voice-{theme}"), egui::vec2(760.0, 880.0), 1.5, dark);
        println!("SNAPSHOT {}", path.display());
    }
}

#[test]
#[ignore]
fn snapshot_quit_recording() {
    for active in [true, false] {
        let mut app = App::stopped(Settings::default());
        let recorder = crate::record::Recorder::from_samples_for_test(&vec![0.25; 48_000]);
        if active {
            app.recorder = Some(recorder);
            app.record_status = ("Recording…".into(), super::RED);
        } else {
            app.take = recorder.finish().unwrap().map(std::sync::Arc::new);
            app.record_status = ("Take ready".into(), super::GREEN);
        }
        app.run_command(&egui::Context::default(), super::Command::Quit);
        let name = if active { "quit-active-recording" } else { "quit-unsaved-recording" };
        let path = snapshot(&mut app, name, egui::vec2(700.0, 420.0), 1.25, true);
        println!("SNAPSHOT {}", path.display());
    }
}

#[test]
#[ignore]
fn snapshot_pages() {
    for page in [super::Page::Soundboard, super::Page::Record, super::Page::Dictation, super::Page::Settings] {
        let mut app = App::stopped(Settings {
            sounds: ["airhorn", "rimshot", "bruh", "sad trombone", "applause"]
                .iter()
                .map(|n| {
                    let mut pad = crate::config::Pad::new(format!("C:/clips/{n}.mp3").into());
                    pad.starred = *n != "applause";
                    pad
                })
                .collect(),
            ..Settings::default()
        });
        app.page = page;
        if page == super::Page::Soundboard {
            let playing = super::clip_key(&app.settings.sounds[0].path);
            let paused = super::clip_key(&app.settings.sounds[1].path);
            app.playing.push(playing);
            app.pad_progress.insert(playing, crate::dsp::ClipProgress { key: playing, fraction: 0.35, paused: false, duration: 1.2 });
            app.pad_progress.insert(paused, crate::dsp::ClipProgress { key: paused, fraction: 0.6, paused: true, duration: 0.8 });
        }
        let path = snapshot(&mut app, &format!("page-{}", page.title().to_lowercase()), egui::vec2(700.0, 720.0), 1.25, true);
        println!("SNAPSHOT {}", path.display());
        if page == super::Page::Record {
            let recorder = crate::record::Recorder::from_samples_for_test(&vec![0.25; 96_000]);
            recorder.report_error_for_test("Microphone disconnected");
            app.recorder = Some(recorder);
            app.poll_recording();
            let path = snapshot(&mut app, "record-partial-take", egui::vec2(700.0, 420.0), 1.25, true);
            println!("SNAPSHOT {}", path.display());
        }
        if page == super::Page::Soundboard {
            app.begin_pad_rename(&app.settings.sounds[0].clone());
            let path = snapshot(&mut app, "soundboard-rename", egui::vec2(700.0, 720.0), 1.25, true);
            println!("SNAPSHOT {}", path.display());
        }
    }
}
