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
pub(super) fn snapshot(app: &mut App, name: &str, size: egui::Vec2, ppp: f32, dark: bool) -> PathBuf {
    use eframe::App as _;
    let ctx = egui::Context::default();
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
    let mut frame = eframe::Frame::_new_kittest();
    let mut textures: HashMap<TextureId, ColorImage> = HashMap::new();
    let mut output = None;
    // A few frames, so layouts that measure themselves settle.
    for _ in 0..4 {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, size)),
            ..Default::default()
        };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        let full = ctx.run_ui(input, |ui| app.ui(ui, &mut frame));
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas {
                let egui::ImageData::Color(image) = &delta.image;
                match delta.pos {
                    None => {
                        textures.insert(*id, (**image).clone());
                    }
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
        output = Some(full);
    }
    let output = output.expect("ran a frame");
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
        let path = snapshot(&mut app, &format!("page-{}", page.title().to_lowercase()), egui::vec2(700.0, 720.0), 1.25, true);
        println!("SNAPSHOT {}", path.display());
    }
}
