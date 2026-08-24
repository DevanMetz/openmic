//! Streaming linear-interpolation rate converter (48 kHz DSP core <-> native device rates).

/// Converts samples pulled from a queue between two rates. Keeps a small
/// backlog of unconsumed source samples plus a fractional read position.
pub struct Resampler {
    from: u32,
    to: u32,
    buf: Vec<f32>,
    /// Read position in `buf`, in source-sample units.
    pos: f64,
    max_backlog: usize,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            from,
            to,
            buf: Vec::new(),
            pos: 0.0,
            max_backlog: 65_536,
        }
    }

    pub fn passthrough(from: u32, to: u32) -> bool {
        from == to
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.buf.extend_from_slice(samples);

        if self.buf.len() > self.max_backlog {
            let drop = self.buf.len() - 16_384;
            let drop = (drop.min(self.pos as usize)).max(0);
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
        if Self::passthrough(self.from, self.to) {
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return n;
        }
        let ratio = f64::from(self.from) / f64::from(self.to);
        let mut n = 0;
        while n < out.len() {
            let i0 = self.pos.floor();
            let need = i0 as usize + 1;
            if need >= self.buf.len() {
                break;
            }
            let frac = (self.pos - i0) as f32;
            out[n] = self.buf[i0 as usize] * (1.0 - frac) + self.buf[need] * frac;
            self.pos += ratio;
            n += 1;
        }
        // Release consumed samples behind the read position.
        let keep_from = (self.pos.floor() as usize).min(self.buf.len());
        if keep_from > 4096 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
        n
    }
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
            // The final sample or two may be held pending future input.
            assert!(out[..n].iter().all(|&s| (s - 0.5).abs() < 1e-6));
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
            assert!(out[..n].iter().all(|&s| (s - 0.25).abs() < 1e-6));
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
}
