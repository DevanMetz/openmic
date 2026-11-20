//! Streaming cubic-interpolation rate converter (48 kHz DSP core <-> native
//! device rates), with a fine rate trim for clock-drift correction.

/// Initial ring buffer capacity (power of two); grows up to [`MAX_CAPACITY`] on demand.
const INITIAL_CAPACITY: usize = 2048;
/// Hard ceiling on buffered source samples (power of two, ~1.36 s at 48 kHz).
const MAX_CAPACITY: usize = 65_536;

/// Converts samples pulled from a queue between two rates. Keeps a small
/// ring buffer of unconsumed source samples plus a fractional read position,
/// avoiding allocations and `Vec::drain` shifts on the audio thread.
pub struct Resampler {
    from: u32,
    to: u32,
    /// Copy samples straight through: equal rates and no drift trim.
    passthrough: bool,
    /// Multiplies the read step; >1 consumes source faster (fewer output
    /// samples). Only an adaptive resampler moves it off 1.0.
    trim: f64,
    /// Power-of-two circular buffer.
    buf: Vec<f32>,
    /// Monotonic index of the oldest retained sample in `buf`.
    head: usize,
    /// Monotonic index one past the newest sample in `buf` (`tail - head <= buf.len()`).
    tail: usize,
    /// Read position relative to `head`, in source-sample units. Interpolation
    /// reads one sample behind it, so it stays in `[1.0, 2.0)` after each pull.
    pos: f64,
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
            buf: vec![0.0; INITIAL_CAPACITY],
            head: 0,
            // One sample of zero history for the interpolator's first point.
            tail: usize::from(!passthrough),
            pos: if passthrough { 0.0 } else { 1.0 },
        }
    }

    /// Set the rate trim (ignored by a passthrough converter). Non-finite or
    /// non-positive trims are ignored so a bad drift measurement cannot stall
    /// or panic the converter.
    pub fn set_trim(&mut self, trim: f64) {
        if trim.is_finite() && trim > 0.0 {
            self.trim = trim;
        }
    }

    fn grow_to(&mut self, needed: usize) {
        let mut new_cap = self.buf.len();
        while new_cap < needed && new_cap < MAX_CAPACITY {
            new_cap *= 2;
        }
        if new_cap == self.buf.len() {
            return;
        }
        let count = self.tail - self.head;
        let old_mask = self.buf.len() - 1;
        let mut next = vec![0.0; new_cap];
        let start = self.head & old_mask;
        let first = count.min(self.buf.len() - start);
        next[..first].copy_from_slice(&self.buf[start..start + first]);
        if first < count {
            next[first..count].copy_from_slice(&self.buf[..count - first]);
        }
        self.buf = next;
        self.head = 0;
        self.tail = count;
    }

    pub fn push(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let count = self.tail - self.head;
        if count + samples.len() > self.buf.len() {
            self.grow_to(count + samples.len());
        }
        let cap = self.buf.len();
        // If a burst exceeds the hard ceiling, drop the oldest samples to fit.
        let samples = if samples.len() >= cap {
            self.head = 0;
            self.tail = 0;
            self.pos = if self.passthrough { 0.0 } else { 1.0 };
            &samples[samples.len() - cap..]
        } else {
            let overflow = (self.tail - self.head + samples.len()).saturating_sub(cap);
            if overflow > 0 {
                let min_pos = if self.passthrough { 0.0 } else { 1.0 };
                self.head += overflow;
                self.pos = (self.pos - overflow as f64).max(min_pos);
            }
            samples
        };
        let mask = cap - 1;
        let write = self.tail & mask;
        let first = samples.len().min(cap - write);
        self.buf[write..write + first].copy_from_slice(&samples[..first]);
        if first < samples.len() {
            self.buf[..samples.len() - first].copy_from_slice(&samples[first..]);
        }
        self.tail += samples.len();
    }

    /// Unconsumed source samples currently buffered.
    pub fn backlog(&self) -> usize {
        (self.tail - self.head).saturating_sub(self.pos.floor() as usize)
    }

    /// Fill `out` with as many converted samples as the backlog allows and
    /// return how many were written (0 = push more source and retry).
    /// Never consumes samples without emitting them.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        let count = self.tail - self.head;
        let mask = self.buf.len() - 1;
        if self.passthrough {
            let n = out.len().min(count);
            if n == 0 {
                return 0;
            }
            let read = self.head & mask;
            let first = n.min(self.buf.len() - read);
            out[..first].copy_from_slice(&self.buf[read..read + first]);
            if first < n {
                out[first..n].copy_from_slice(&self.buf[..n - first]);
            }
            self.head += n;
            return n;
        }
        let step = f64::from(self.from) / f64::from(self.to) * self.trim;
        let mut n = 0;
        let mut pos = self.pos;
        let head = self.head;
        let buf = &self.buf;
        while n < out.len() {
            let i = pos.floor() as usize;
            if i + 2 >= count {
                break;
            }
            let t = (pos - i as f64) as f32;
            let base = head + i;
            let y0 = buf[(base - 1) & mask];
            let y1 = buf[base & mask];
            let y2 = buf[(base + 1) & mask];
            let y3 = buf[(base + 2) & mask];
            out[n] = hermite(y0, y1, y2, y3, t);
            pos += step;
            n += 1;
        }
        // Release consumed samples immediately, keeping one behind `pos`.
        let advance = (pos.floor() as usize).saturating_sub(1).min(count);
        self.head += advance;
        self.pos = pos - advance as f64;
        n
    }
}

/// Catmull-Rom (4-point cubic Hermite) interpolation between `y1` and `y2`.
/// Much flatter treble than linear interpolation at fractional offsets.
pub(crate) fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
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
        assert!(
            slow > even + 400 && fast < even - 400,
            "{slow} {even} {fast}"
        );
    }

    #[test]
    fn invalid_trims_are_ignored_instead_of_stalling_the_converter() {
        for trim in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            let mut r = Resampler::adaptive(48_000, 48_000);
            r.push(&[0.5; 1000]);
            r.set_trim(trim);
            let mut out = [0.0f32; 480];
            assert_eq!(r.pull(&mut out), 480, "{trim}");
            assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6), "{trim}");
            assert!(
                r.pull(&mut out) > 0,
                "a second pull must still make progress at trim {trim}"
            );
        }
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

    #[test]
    fn ring_buffer_wraps_cleanly_without_reallocating() {
        let mut r = Resampler::adaptive(44_100, 48_000);
        let initial_cap = r.buf.len();
        let mut out = [0.0f32; 480];
        let mut emitted = 0usize;
        for _ in 0..50 {
            r.push(&[0.4; 441]);
            while let n @ 1.. = r.pull(&mut out) {
                if emitted > 10 {
                    assert!(out[..n].iter().all(|&s| (s - 0.4).abs() < 1e-5));
                }
                emitted += n;
            }
        }
        assert!(emitted > 23_000, "emitted {emitted}");
        assert_eq!(
            r.buf.len(),
            initial_cap,
            "steady-state streaming should never reallocate the ring buffer"
        );
    }
}
