//! Streaming cubic-interpolation rate converter (48 kHz DSP core <-> native
//! device rates), with a fine rate trim for clock-drift correction.

/// Converts samples pulled from a queue between two rates. Keeps a small
/// backlog of unconsumed source samples plus a fractional read position.
pub struct Resampler {
    from: u32,
    to: u32,
    /// Copy samples straight through: equal rates and no drift trim.
    passthrough: bool,
    /// Multiplies the read step; >1 consumes source faster (fewer output
    /// samples). Only an adaptive resampler moves it off 1.0.
    trim: f64,
    buf: Vec<f32>,
    /// Read position in `buf`, in source-sample units. Interpolation reads
    /// one sample behind it, so it never drops below 1.
    pos: f64,
    max_backlog: usize,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self::build(from, to, from == to)
    }

    /// A converter whose rate can be trimmed with [`Self::set_trim`], for
    /// output queues that follow another device's clock. It interpolates
    /// even at equal rates, so trimming never jumps the phase.
    pub fn adaptive(from: u32, to: u32) -> Self {
        Self::build(from, to, false)
    }

    fn build(from: u32, to: u32, passthrough: bool) -> Self {
        Self {
            from,
            to,
            passthrough,
            trim: 1.0,
            // One sample of history for the interpolator's first point.
            buf: if passthrough { Vec::new() } else { vec![0.0] },
            pos: if passthrough { 0.0 } else { 1.0 },
            max_backlog: 65_536,
        }
    }

    /// Set the rate trim (ignored by a passthrough converter).
    pub fn set_trim(&mut self, trim: f64) {
        self.trim = trim;
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);

        if self.buf.len() > self.max_backlog {
            let history = if self.passthrough { 0 } else { 1 };
            let consumed = (self.pos.floor() as usize).saturating_sub(history);
            let drop = (self.buf.len() - 16_384).min(consumed);
            self.buf.drain(..drop);
            self.pos -= drop as f64;
        }
    }

    /// Unconsumed source samples currently buffered.
    pub fn backlog(&self) -> usize {
        self.buf.len().saturating_sub(self.pos.floor() as usize)
    }

    /// Fill `out` with as many converted samples as the backlog allows and
    /// return how many were written (0 = push more source and retry).
    /// Never consumes samples without emitting them.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        if self.passthrough {
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return n;
        }
        let step = f64::from(self.from) / f64::from(self.to) * self.trim;
        let mut n = 0;
        while n < out.len() {
            let i = self.pos.floor() as usize;
            if i + 2 >= self.buf.len() {
                break;
            }
            let t = (self.pos - i as f64) as f32;
            let [y0, y1, y2, y3] = [
                self.buf[i - 1],
                self.buf[i],
                self.buf[i + 1],
                self.buf[i + 2],
            ];
            out[n] = hermite(y0, y1, y2, y3, t);
            self.pos += step;
            n += 1;
        }
        // Release consumed samples, keeping one behind the read position.
        let keep_from = (self.pos.floor() as usize).saturating_sub(1).min(self.buf.len());
        if keep_from > 4096 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        n
    }
}

/// Catmull-Rom (4-point cubic Hermite) interpolation between `y1` and `y2`.
/// Much flatter treble than linear interpolation at fractional offsets.
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_identity() {
        let mut r = Resampler::new(48_000, 48_000);
        r.push(&[0.1, 0.2, 0.3]);
        let mut out = [0.0; 3];
        assert_eq!(r.pull(&mut out), 3);
        assert_eq!(out, [0.1, 0.2, 0.3]);
        assert_eq!(r.pull(&mut [0.0; 1]), 0);
    }

    #[test]
    fn upsample_doubles_length_and_keeps_constant() {
        let mut r = Resampler::new(24_000, 48_000);
        r.push(&vec![0.5; 1200]);
        let mut total = 0usize;
        let mut out = [0.0f32; 480];
        while total < 2300 {
            let n = r.pull(&mut out);
            if n == 0 {
                break;
            }
            // Skip the ramp in from the zero history sample. The final
            // sample or two may be held pending future input.
            let settled = if total == 0 { 3 } else { 0 };
            assert!(out[settled..n].iter().all(|&s| (s - 0.5).abs() < 1e-6));
            total += n;
        }
        assert!(total >= 2300, "upsampled only {total} of 2400 expected");
    }

    #[test]
    fn downsample_scales_length() {
        let mut r = Resampler::new(48_000, 24_000);
        r.push(&vec![0.25; 4800]);
        let mut total = 0usize;
        let mut out = [0.0f32; 240];
        while total < 2300 {
            let n = r.pull(&mut out);
            if n == 0 {
                break;
            }
            // Skip the ramp in from the zero history sample.
            let settled = if total == 0 { 2 } else { 0 };
            assert!(out[settled..n].iter().all(|&s| (s - 0.25).abs() < 1e-6));
            total += n;
        }
        assert!(total >= 2300, "downsampled {total} of ~2400 expected");
    }

    #[test]
    fn streaming_across_pushes_is_continuous() {
        // A slow ramp fed in chunks must come out monotonic and near-correct.
        let mut r = Resampler::new(44_100, 48_000);
        let mut last = -1.0f32;
        let mut count = 0usize;
        let mut sample_index = 0u64;
        for _chunk in 0..40u32 {
            let src: Vec<f32> = (0..441)
                .map(|_| {
                    sample_index += 1;
                    ((sample_index as f64) * 0.001) as f32
                })
                .collect::<Vec<f32>>();
            r.push(&src);
            loop {
                let mut out = [0.0f32; 480];
                let n = r.pull(&mut out);
                if n == 0 {
                    break;
                }
                for &s in &out[..n] {
                    assert!(s >= last - 1e-4, "regressed: {s} after {last}");
                    last = s;
                    count += 1;
                }
                if count > 15_000 {
                    return;
                }
            }
        }
        panic!("only emitted {count} samples");
    }

    #[test]
    fn trim_changes_output_count_without_phase_jumps() {
        let count = |trim: f64| {
            let mut r = Resampler::adaptive(48_000, 48_000);
            r.set_trim(trim);
            let mut total = 0;
            let mut out = [0.0f32; 480];
            for _ in 0..100 {
                r.push(&[0.3; 480]);
                while let n @ 1.. = r.pull(&mut out) {
                    total += n;
                }
            }
            total
        };
        let (slow, even, fast) = (count(0.99), count(1.0), count(1.01));
        assert!((47_990..=48_000).contains(&even), "{even}");
        assert!(slow > even + 400 && fast < even - 400, "{slow} {even} {fast}");
    }

    #[test]
    fn cubic_keeps_a_high_tone_that_linear_would_dull() {
        // 12 kHz at 48 kHz read half a sample off: linear averaging loses
        // 3 dB (gain 0.71); Catmull-Rom loses about 1 dB (gain 0.88).
        let tone: Vec<f32> = (0..4800)
            .map(|i| (std::f32::consts::TAU * 12_000.0 * i as f32 / 48_000.0 + 0.3).sin())
            .collect();
        let mut r = Resampler::adaptive(48_000, 48_000);
        r.push(&tone);
        r.set_trim(0.5);
        r.pull(&mut [0.0f32; 1]); // shift the read position by half a sample
        r.set_trim(1.0);
        let mut out = vec![0.0f32; 4800];
        let n = r.pull(&mut out);
        let body = &out[100..n];
        let rms = (body.iter().map(|s| s * s).sum::<f32>() / body.len() as f32).sqrt();
        let gain = rms / std::f32::consts::FRAC_1_SQRT_2;
        assert!((0.85..0.92).contains(&gain), "gain {gain}");
    }
}
