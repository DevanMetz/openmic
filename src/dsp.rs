//! Frame DSP: parameter blending, gates, high-pass, soundboard mixing. Pure logic, no I/O.

use serde::{Deserialize, Serialize};

pub const SR: u32 = 48_000;
pub const FRAME: usize = 480; // 10 ms at 48 kHz, the RNNoise frame size

/// Which neural denoiser cleans the voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Model {
    /// RNNoise: tiny and fast; handles steady noise (fans, hum).
    Rnnoise,
    /// DeepFilterNet 3: much stronger on transient noise (keys, clicks).
    #[default]
    DeepFilter,
}

/// Live per-frame controls shared between the GUI and the processing thread.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub model: Model,
    pub strength: f32,
    pub input_gain: f32,
    pub output_gain: f32,
    pub monitor_gain: f32,
    pub sound_gain: f32,
    pub gate_enabled: bool,
    pub gate_threshold_db: f32,
    pub highpass: bool,
    pub highpass_hz: f32,
    pub voice_gate: bool,
    pub voice_threshold: f32,
    pub bypass: bool,
    pub mute: bool,
    pub monitor_on: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            model: Model::default(),
            strength: 1.0,
            input_gain: 1.0,
            output_gain: 1.0,
            monitor_gain: 1.0,
            sound_gain: 1.0,
            gate_enabled: false,
            gate_threshold_db: -50.0,
            highpass: true,
            highpass_hz: HIGHPASS_HZ,
            voice_gate: true,
            voice_threshold: VOICE_THRESHOLD,
            bypass: false,
            mute: false,
            monitor_on: false,
        }
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Gate release coefficient per 10 ms frame for a 150 ms release time.
pub fn gate_release() -> f32 {
    (-(FRAME as f32) / (SR as f32 * 0.15)).exp()
}

/// Process one frame: wet/dry blend, gate, output gain, mute.
///
/// `x` is the input-gained mic signal, `denoised` the RNNoise output.
/// `gate_gain` carries smoothing state across frames.
pub fn apply_processing(
    x: &[f32; FRAME],
    denoised: &[f32; FRAME],
    p: &Params,
    gate_gain: &mut f32,
) -> [f32; FRAME] {
    let mut z = if p.bypass {
        *gate_gain = 1.0;
        *x
    } else {
        let mut z = std::array::from_fn(|i| x[i] * (1.0 - p.strength) + denoised[i] * p.strength);
        if p.gate_enabled {
            let rms = (x.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
            let level_db = 20.0 * rms.max(1e-6).log10();
            *gate_gain = if level_db >= p.gate_threshold_db {
                1.0
            } else {
                *gate_gain * gate_release()
            };
            for s in &mut z {
                *s *= *gate_gain;
            }
        } else {
            *gate_gain = 1.0;
        }
        z
    };
    for s in &mut z {
        *s = (*s * p.output_gain).clamp(-1.0, 1.0);
    }
    if p.mute {
        z.fill(0.0);
    }
    z
}

/// Default rumble/handling-noise cutoff; below the lowest fundamental of speech.
pub const HIGHPASS_HZ: f32 = 80.0;
/// Range the rumble cutoff can be dragged over.
pub const HIGHPASS_RANGE: std::ops::RangeInclusive<f32> = 40.0..=200.0;

/// Fourth-order Butterworth high-pass: two cascaded RBJ biquads
/// (transposed direct form II), 24 dB/octave so hum an octave below the
/// cutoff drops ~24 dB while the voice band is untouched.
pub struct HighPass {
    stages: [Biquad; 2],
    cutoff_hz: f32,
}

struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn highpass(cutoff_hz: f32, q: f32) -> Self {
        let w = std::f32::consts::TAU * cutoff_hz / SR as f32;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        let cos = w.cos();
        let b0 = (1.0 + cos) / 2.0 / a0;
        Self {
            b: [b0, -2.0 * b0, b0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }

    fn process(&mut self, frame: &mut [f32]) {
        let (b, a) = (self.b, self.a);
        for s in frame {
            let x = *s;
            let y = b[0] * x + self.z[0];
            self.z[0] = b[1] * x - a[0] * y + self.z[1];
            self.z[1] = b[2] * x - a[1] * y;
            *s = y;
        }
    }
}

impl HighPass {
    // Butterworth pole-pair Qs for order 4.
    const QS: [f32; 2] = [0.541_196, 1.306_563];

    pub fn new(cutoff_hz: f32) -> Self {
        Self {
            stages: Self::QS.map(|q| Biquad::highpass(cutoff_hz, q)),
            cutoff_hz,
        }
    }

    /// Retune without clearing the filter state, so dragging the cutoff
    /// while talking doesn't click.
    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        if cutoff_hz == self.cutoff_hz {
            return;
        }
        self.cutoff_hz = cutoff_hz;
        for (stage, q) in self.stages.iter_mut().zip(Self::QS) {
            let tuned = Biquad::highpass(cutoff_hz, q);
            stage.b = tuned.b;
            stage.a = tuned.a;
        }
    }

    pub fn process(&mut self, frame: &mut [f32]) {
        for stage in &mut self.stages {
            stage.process(frame);
        }
    }
}

/// Rumble filter gain at `freq_hz` for `cutoff_hz`, dB (4th-order
/// Butterworth magnitude).
pub fn highpass_response_db(freq_hz: f32, cutoff_hz: f32) -> f32 {
    -10.0 * (1.0 + (cutoff_hz / freq_hz).powi(8)).log10()
}

/// Default voice-probability threshold for the voice gate.
pub const VOICE_THRESHOLD: f32 = 0.6;
/// Keep the gate open this long after the last voiced frame so word endings
/// and short pauses survive (the denoisers' 20-30 ms delay also lands here).
const VOICE_HOLD_FRAMES: u32 = 30; // 300 ms
/// Open over 5 ms (click-free); the models' latency means the gate starts
/// opening before the voiced audio reaches it, so onsets are kept.
const VOICE_ATTACK_SAMPLES: f32 = 240.0;
/// Close with a 50 ms time constant, snapping to silence below -60 dB so
/// loud non-voice sounds are removed rather than just attenuated.
const VOICE_RELEASE_SECONDS: f32 = 0.05;
const VOICE_CLOSED_GAIN: f32 = 1e-3;

/// Gate driven by the model's voice probability instead of level: mutes
/// everything that is not speech (keys, clicks, breathing) however loud.
pub struct VoiceGate {
    gain: f32,
    hold: u32,
    release: f32,
}

impl Default for VoiceGate {
    /// Starts closed: nothing passes until the first voiced frame.
    fn default() -> Self {
        Self {
            gain: 0.0,
            hold: 0,
            release: (-1.0 / (SR as f32 * VOICE_RELEASE_SECONDS)).exp(),
        }
    }
}

impl VoiceGate {
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Back to closed, e.g. while bypassed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn process(&mut self, frame: &mut [f32], prob: f32, threshold: f32) {
        if prob >= threshold {
            self.hold = VOICE_HOLD_FRAMES;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        let open = self.hold > 0;
        for s in frame {
            self.gain = if open {
                (self.gain + 1.0 / VOICE_ATTACK_SAMPLES).min(1.0)
            } else if self.gain > VOICE_CLOSED_GAIN {
                self.gain * self.release
            } else {
                0.0
            };
            *s *= self.gain;
        }
    }
}

/// Most clips that can sound at once; the oldest is cut to make room.
pub const MAX_VOICES: usize = 16;

/// One playing soundboard clip and its playhead.
#[derive(Clone)]
pub struct Clip {
    /// Identifies the pad, so a pad restarts rather than stacking on itself.
    pub key: u64,
    pub samples: std::sync::Arc<Vec<f32>>,
    pub pos: usize,
    /// The pad's own volume, on top of the soundboard volume.
    pub gain: f32,
}

impl Clip {
    pub fn new(key: u64, samples: std::sync::Arc<Vec<f32>>, gain: f32) -> Self {
        Self { key, samples, pos: 0, gain }
    }
}

/// Soundboard mixer state; voice-independent by design (mute silences only the mic).
#[derive(Default)]
pub struct Mixer {
    voices: Vec<Clip>,
}

impl Mixer {
    /// Keys of the clips currently sounding.
    pub fn playing_keys(&self) -> Vec<u64> {
        self.voices.iter().map(|v| v.key).collect()
    }

    /// Start `clip`. With `overlap` it layers over other pads (the same pad
    /// restarts); without it, it replaces whatever was playing.
    pub fn play(&mut self, clip: Clip, overlap: bool) {
        if clip.samples.is_empty() {
            return;
        }
        if overlap {
            self.voices.retain(|v| v.key != clip.key);
            if self.voices.len() >= MAX_VOICES {
                self.voices.remove(0);
            }
        } else {
            self.voices.clear();
        }
        self.voices.push(clip);
    }

    pub fn stop(&mut self) {
        self.voices.clear();
    }

    /// Stop one pad's clip, leaving any others playing.
    pub fn stop_key(&mut self, key: u64) {
        self.voices.retain(|v| v.key != key);
    }

    /// Change a playing pad's volume.
    pub fn set_gain(&mut self, key: u64, gain: f32) {
        for voice in self.voices.iter_mut().filter(|v| v.key == key) {
            voice.gain = gain;
        }
    }

    /// Mix the playing clips into `frame` in place, advancing/completing playback.
    pub fn mix(&mut self, frame: &mut [f32], gain: f32) {
        if self.voices.is_empty() {
            return;
        }
        for clip in &mut self.voices {
            let end = (clip.pos + frame.len()).min(clip.samples.len());
            let g = gain * clip.gain;
            for (s, src) in frame.iter_mut().zip(&clip.samples[clip.pos..end]) {
                *s += src * g;
            }
            clip.pos = end;
        }
        for s in frame {
            *s = s.clamp(-1.0, 1.0);
        }
        self.voices.retain(|v| v.pos < v.samples.len());
    }

    /// The playing clips with their playheads (route-switch handoff).
    pub fn take_remaining(&mut self) -> Vec<Clip> {
        std::mem::take(&mut self.voices)
    }

    /// Resume clips handed over by [`Self::take_remaining`].
    pub fn resume(&mut self, clips: Vec<Clip>) {
        self.voices = clips;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from test_controls.py: gate close/reopen, bypass, gain, mute.
    #[test]
    fn gate_closes_on_quiet_and_reopens_immediately() {
        let mut params = Params {
            strength: 1.0,
            gate_enabled: true,
            gate_threshold_db: -40.0,
            ..Default::default()
        };
        params.gate_enabled = true;
        let quiet = [0.001_f32; FRAME]; // -60 dBFS
        let clean = [0.25_f32; FRAME];
        let mut gate_gain = 1.0;
        let mut gated = [0.0; FRAME];
        for _ in 0..40 {
            gated = apply_processing(&quiet, &clean, &params, &mut gate_gain);
        }
        assert!(gate_gain < 0.13, "gate gain {gate_gain}");
        assert!(gated.iter().cloned().fold(0.0f32, f32::max) < 0.04);

        let loud = [0.1_f32; FRAME]; // -20 dBFS
        let reopened = apply_processing(&loud, &clean, &params, &mut gate_gain);
        assert_eq!(gate_gain, 1.0);
        assert!(reopened.iter().zip(&clean).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn bypass_keeps_dry_signal_and_applies_output_gain() {
        let params = Params {
            bypass: true,
            output_gain: 2.0,
            ..Default::default()
        };
        let loud = [0.1_f32; FRAME];
        let silence = [0.0; FRAME];
        let out = apply_processing(&loud, &silence, &params, &mut 1.0f32);
        assert!(out.iter().all(|&s| (s - 0.2).abs() < 1e-5));
    }

    #[test]
    fn mute_silences_frame() {
        let params = Params { mute: true, ..Default::default() };
        let ones = [1.0_f32; FRAME];
        assert!(apply_processing(&ones, &ones, &params, &mut 1.0f32).iter().all(|&s| s == 0.0));
    }

    fn clip(key: u64, samples: Vec<f32>) -> Clip {
        Clip::new(key, std::sync::Arc::new(samples), 1.0)
    }

    // Ported from test_soundboard.py mixing section.
    #[test]
    fn mixer_plays_completes_and_outlives_mic_mute() {
        let mut mixer = Mixer::default();
        mixer.play(clip(1, vec![0.4; FRAME + 120]), false);
        let mut first = [0.0; FRAME];
        mixer.mix(&mut first, 0.5);
        assert!(first.iter().all(|&s| (s - 0.2).abs() < 1e-6));
        assert!(!mixer.playing_keys().is_empty());

        let mut second = [0.0; FRAME];
        mixer.mix(&mut second, 0.5);
        assert!(second[..120].iter().all(|&s| (s - 0.2).abs() < 1e-6));
        assert!(second[120..].iter().all(|&s| s == 0.0));
        assert!(mixer.playing_keys().is_empty());

        // Mute must not silence the soundboard.
        let params = Params { mute: true, ..Default::default() };
        let muted_mic = apply_processing(&[1.0; FRAME], &[1.0; FRAME], &params, &mut 1.0f32);
        mixer.play(clip(1, vec![0.25; FRAME]), false);
        let mut mixed = muted_mic;
        mixer.mix(&mut mixed, 0.5);
        assert!(mixed.iter().all(|&s| (s - 0.125).abs() < 1e-6));
        assert!(mixer.playing_keys().is_empty(), "a clip ends on its last sample");
    }

    #[test]
    fn overlapping_pads_layer_and_the_same_pad_restarts() {
        let mut mixer = Mixer::default();
        mixer.play(clip(1, vec![0.1; FRAME * 4]), true);
        mixer.mix(&mut [0.0; FRAME], 1.0);
        mixer.play(Clip::new(2, std::sync::Arc::new(vec![0.2; FRAME * 4]), 0.5), true);
        let mut frame = [0.0; FRAME];
        mixer.mix(&mut frame, 1.0);
        assert!(frame.iter().all(|&s| (s - 0.2).abs() < 1e-6), "0.1 + 0.2 * 0.5");
        assert_eq!(mixer.playing_keys(), vec![1, 2]);

        mixer.play(clip(1, vec![0.1; FRAME * 4]), true);
        assert_eq!(mixer.playing_keys(), vec![2, 1], "pad 1 restarted, not doubled");

        mixer.set_gain(2, 0.0);
        let mut frame = [0.0; FRAME];
        mixer.mix(&mut frame, 1.0);
        assert!(frame.iter().all(|&s| (s - 0.1).abs() < 1e-6));

        mixer.play(clip(3, vec![0.1; FRAME]), false);
        assert_eq!(mixer.playing_keys(), vec![3], "without overlap a pad replaces the rest");
    }

    #[test]
    fn overlap_caps_the_voice_count() {
        let mut mixer = Mixer::default();
        for key in 0..MAX_VOICES as u64 + 3 {
            mixer.play(clip(key, vec![0.0; FRAME]), true);
        }
        let keys = mixer.playing_keys();
        assert_eq!(keys.len(), MAX_VOICES);
        assert_eq!(keys[0], 3, "the oldest clips make room");
    }

    #[test]
    fn take_remaining_hands_off_playheads() {
        let mut mixer = Mixer::default();
        mixer.play(clip(7, (0..8).map(|i| i as f32 * 0.1).collect()), false);
        let mut frame = [0.0; 3];
        mixer.mix(&mut frame, 1.0);
        let handed = mixer.take_remaining();
        assert!(mixer.playing_keys().is_empty());
        assert_eq!(handed.len(), 1);
        assert_eq!((handed[0].key, handed[0].pos), (7, 3));

        let mut resumed = Mixer::default();
        resumed.resume(handed);
        let mut next = [0.0f32; 2];
        resumed.mix(&mut next, 1.0);
        assert!((next[0] - 0.3).abs() < 1e-6 && (next[1] - 0.4).abs() < 1e-6, "{next:?}");
    }

    fn tone(freq: f32) -> Vec<f32> {
        (0..SR as usize)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    fn tail_peak(x: &[f32]) -> f32 {
        x[x.len() / 2..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn highpass_cuts_rumble_and_keeps_voice_band() {
        let mut rumble = tone(30.0);
        HighPass::new(HIGHPASS_HZ).process(&mut rumble);
        assert!(tail_peak(&rumble) < 0.03, "30 Hz peak {}", tail_peak(&rumble));

        let mut voice = tone(300.0);
        HighPass::new(HIGHPASS_HZ).process(&mut voice);
        assert!(tail_peak(&voice) > 0.95, "300 Hz peak {}", tail_peak(&voice));
    }

    #[test]
    fn highpass_response_matches_the_filter() {
        for (freq, cutoff) in [(30.0, 80.0), (50.0, 80.0), (80.0, 80.0), (150.0, 80.0), (150.0, 160.0)] {
            let mut x = tone(freq);
            let mut hp = HighPass::new(HIGHPASS_HZ);
            hp.set_cutoff(cutoff); // retuning must give the same filter
            hp.process(&mut x);
            let measured = 20.0 * tail_peak(&x).log10();
            let predicted = highpass_response_db(freq, cutoff);
            assert!(
                (measured - predicted).abs() < 1.0,
                "{freq} Hz @ {cutoff} Hz: filter {measured:.1} dB vs curve {predicted:.1} dB"
            );
        }
    }

    #[test]
    fn voice_gate_holds_through_pauses_then_closes() {
        let mut gate = VoiceGate::default();
        let mut before_voice = [1.0f32; FRAME];
        gate.process(&mut before_voice, 0.1, VOICE_THRESHOLD);
        assert!(before_voice.iter().all(|&s| s == 0.0), "starts closed");

        let mut frame = [1.0f32; FRAME];
        gate.process(&mut frame, 0.9, VOICE_THRESHOLD);
        assert_eq!(gate.gain(), 1.0);

        // A 200 ms pause between words stays fully open.
        for _ in 0..20 {
            let mut frame = [1.0f32; FRAME];
            gate.process(&mut frame, 0.1, VOICE_THRESHOLD);
            assert_eq!(frame[FRAME - 1], 1.0);
        }
        // Past the hold, non-voice is muted however loud it is.
        for _ in 0..60 {
            gate.process(&mut [1.0f32; FRAME], 0.1, VOICE_THRESHOLD);
        }
        let mut loud_click = [1.0f32; FRAME];
        gate.process(&mut loud_click, 0.1, VOICE_THRESHOLD);
        assert!(loud_click.iter().all(|&s| s == 0.0), "gain {}", gate.gain());

        // Voice reopens it within one frame, without a click (ramped).
        let mut onset = [1.0f32; FRAME];
        gate.process(&mut onset, 0.9, VOICE_THRESHOLD);
        assert!(onset[0] > 0.0 && onset[0] < 0.01);
        assert_eq!(onset[FRAME - 1], 1.0);
    }
}
