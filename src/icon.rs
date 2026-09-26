//! The OpenMic icon (a microphone on a round badge), drawn in code for the
//! window and the notification area. Red with a slash while the mic is muted,
//! amber while dictation is listening.

const CYAN: [f32; 3] = [0x38 as f32, 0xbd as f32, 0xf8 as f32];
const RED: [f32; 3] = [0xf8 as f32, 0x71 as f32, 0x71 as f32];
const AMBER: [f32; 3] = [0xfb as f32, 0xbf as f32, 0x24 as f32];

/// Square RGBA pixels, `size` x `size`.
pub fn rgba(size: u32, muted: bool) -> Vec<u8> {
    draw(size, if muted { RED } else { CYAN }, muted)
}

/// The icon while speech to text is listening.
pub fn listening_rgba(size: u32) -> Vec<u8> {
    draw(size, AMBER, false)
}

fn draw(size: u32, badge: [f32; 3], muted: bool) -> Vec<u8> {
    const GLYPH: [f32; 3] = [18.0, 18.0, 18.0];
    let px = 1.0 / size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = [(x as f32 + 0.5) * px, (y as f32 + 0.5) * px];
            // Coverage from a signed distance (negative inside), antialiased
            // over one pixel.
            let cover = |d: f32| (0.5 - d / px).clamp(0.0, 1.0);
            let background = cover(length([p[0] - 0.5, p[1] - 0.5]) - 0.48);
            let mut glyph = [
                segment(p, [0.5, 0.30], [0.5, 0.47]) - 0.11, // capsule
                if p[1] >= 0.47 {
                    (length([p[0] - 0.5, p[1] - 0.47]) - 0.19).abs() - 0.035 // holder
                } else {
                    f32::MAX
                },
                segment(p, [0.5, 0.66], [0.5, 0.78]) - 0.035, // stem
                segment(p, [0.38, 0.78], [0.62, 0.78]) - 0.035, // base
            ]
            .into_iter()
            .fold(f32::MAX, f32::min);
            if muted {
                glyph = glyph.min(segment(p, [0.27, 0.24], [0.73, 0.80]) - 0.04);
            }
            let g = cover(glyph);
            for (b, c) in badge.iter().zip(GLYPH) {
                out.push((b + (c - b) * g).round() as u8);
            }
            out.push((background * 255.0).round() as u8);
        }
    }
    out
}

fn length(v: [f32; 2]) -> f32 {
    v[0].hypot(v[1])
}

/// Distance from `p` to the segment `a`-`b`.
fn segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (pa, ba) = ([p[0] - a[0], p[1] - a[1]], [b[0] - a[0], b[1] - a[1]]);
    let t = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1])).clamp(0.0, 1.0);
    length([pa[0] - ba[0] * t, pa[1] - ba[1] * t])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_badge_with_a_dark_glyph() {
        let size = 32;
        let pixels = rgba(size, false);
        assert_eq!(pixels.len(), (size * size * 4) as usize);
        let at = |x: u32, y: u32| &pixels[((y * size + x) * 4) as usize..][..4];
        assert_eq!(at(0, 0)[3], 0, "corners are transparent");
        assert_eq!(at(16, 12), [18, 18, 18, 255], "capsule");
        assert_eq!(at(4, 16), [0x38, 0xbd, 0xf8, 255], "badge");
        assert_eq!(rgba(size, true)[((16 * size + 4) * 4) as usize], 0xf8, "muted is red");
    }
}
