//! Frame DSP: parameter blending, noise gate, soundboard mixing. Pure logic, no I/O.

pub const SR: u32 = 48_000;
pub const FRAME: usize = 480; // 10 ms at 48 kHz, the RNNoise frame size

/// Live per-frame controls shared between the GUI and the processing thread.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub strength: f32,
    pub input_gain: f32,
    pub output_gain: f32,
    pub monitor_gain: f32,
    pub sound_gain: f32,
    pub gate_enabled: bool,
    pub gate_threshold_db: f32,
    pub bypass: bool,
    pub mute: bool,
    pub monitor_on: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            strength: 1.0,
            input_gain: 1.0,
            output_gain: 1.0,
            monitor_gain: 1.0,
            sound_gain: 1.0,
            gate_enabled: false,
            gate_threshold_db: -50.0,
            bypass: false,
            mute: false,
            monitor_on: false,
        }
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Peak mapped to a 0..100 bar over a 60 dB window (matches the Python meter).
pub fn meter_percent(peak: f32) -> f32 {
    let db = 20.0 * peak.max(1e-6).log10();
    ((db + 60.0) * 100.0 / 60.0).clamp(0.0, 100.0)
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

/// One loaded soundboard clip and its playhead.
#[derive(Clone)]
pub struct Clip {
    pub samples: std::sync::Arc<Vec<f32>>,
    pub pos: usize,
}

/// Soundboard mixer state; voice-independent by design (mute silences only the mic).
#[derive(Default)]
pub struct Mixer {
    pub clip: Option<Clip>,
}

impl Mixer {
    pub fn playing(&self) -> bool {
        self.clip.is_some()
    }

    /// Mix the current clip into `frame` in place, advancing/completing playback.
    pub fn mix(&mut self, frame: &mut [f32], gain: f32) {
        let Some(mut clip) = self.clip.take() else {
            return;
        };
        let end = (clip.pos + frame.len()).min(clip.samples.len());
        let count = end - clip.pos;
        for (s, src) in frame[..count].iter_mut().zip(&clip.samples[clip.pos..end]) {
            *s = (*s + src * gain).clamp(-1.0, 1.0);
        }
        clip.pos = end;
        if end < clip.samples.len() {
            self.clip = Some(clip);
        }
    }

    /// Remaining samples from the current playhead (route-switch handoff).
    pub fn take_remaining(&mut self) -> Option<Vec<f32>> {
        let clip = self.clip.take()?;
        Some(clip.samples[clip.pos..].to_vec())
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
        let mut params = Params {
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
        let mut params = Params::default();
        params.mute = true;
        let ones = [1.0_f32; FRAME];
        assert!(apply_processing(&ones, &ones, &params, &mut 1.0f32).iter().all(|&s| s == 0.0));
    }

    // Ported from test_soundboard.py mixing section.
    #[test]
    fn mixer_plays_completes_and_outlives_mic_mute() {
        let mut mixer = Mixer::default();
        mixer.clip = Some(Clip {
            samples: std::sync::Arc::new(vec![0.4; FRAME + 120]),
            pos: 0,
        });
        let mut first = [0.0; FRAME];
        mixer.mix(&mut first, 0.5);
        assert!(first.iter().all(|&s| (s - 0.2).abs() < 1e-6));
        assert!(mixer.playing());

        let mut second = [0.0; FRAME];
        mixer.mix(&mut second, 0.5);
        assert!(second[..120].iter().all(|&s| (s - 0.2).abs() < 1e-6));
        assert!(second[120..].iter().all(|&s| s == 0.0));
        assert!(!mixer.playing());

        // Mute must not silence the soundboard.
        let mut params = Params::default();
        params.mute = true;
        let muted_mic = apply_processing(&[1.0; FRAME], &[1.0; FRAME], &params, &mut 1.0f32);
        mixer.clip = Some(Clip {
            samples: std::sync::Arc::new(vec![0.25; FRAME]),
            pos: 0,
        });
        let mut mixed = muted_mic;
        mixer.mix(&mut mixed, 0.5);
        assert!(mixed.iter().all(|&s| (s - 0.125).abs() < 1e-6));
        mixer.mix(&mut [0.0; FRAME], 0.5);
        assert!(!mixer.playing());
    }

    #[test]
    fn take_remaining_hands_off_playhead() {
        let mut mixer = Mixer::default();
        mixer.clip = Some(Clip {
            samples: std::sync::Arc::new((0..8).map(|i| i as f32).collect()),
            pos: 3,
        });
        assert_eq!(mixer.take_remaining().unwrap(), vec![3.0, 4.0, 5.0, 6.0, 7.0]);
        assert!(!mixer.playing());
    }

    #[test]
    fn meter_maps_60db_window() {
        assert_eq!(meter_percent(0.0), 0.0);
        assert_eq!(meter_percent(1.0), 100.0);
        assert!((meter_percent(10f32.powf(-30.0 / 20.0)) - 50.0).abs() < 0.01);
    }
}
