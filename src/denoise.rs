//! Speech denoisers: RNNoise (light, also our voice detector) and DeepFilterNet 3.

use std::time::Duration;

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};

use crate::dsp::{apply_processing, HighPass, Model, Params, VoiceGate, FRAME, HIGHPASS_HZ};

/// What the cleaner is actually running, for the GUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelState {
    Rnnoise = 0,
    DeepFilterLoading = 1,
    DeepFilter = 2,
    DeepFilterFailed = 3,
}

impl ModelState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::DeepFilterLoading,
            2 => Self::DeepFilter,
            3 => Self::DeepFilterFailed,
            _ => Self::Rnnoise,
        }
    }
}

/// RNNoise's output lags its input by two frames (measured: 958 samples,
/// the rest is its internal high-pass); the dry blend must lag equally or
/// partial strength comb-filters the voice.
const RNNOISE_DELAY_FRAMES: usize = 2;

/// RNNoise reports high voice probability for its first few frames while its
/// state settles, which would open the voice gate at every stream start.
const VAD_WARMUP_FRAMES: u32 = 20; // 200 ms

/// The full voice chain for one 10 ms frame:
/// input gain -> high-pass -> RNNoise (always: it is the voice detector)
/// -> DeepFilterNet when selected -> blend/level gate/output gain -> voice gate.
pub struct Cleaner {
    rnn: rnn::Denoiser,
    deep: Option<DeepFilterWorker>,
    deep_failed: bool,
    deep_timeout: Duration,
    highpass: HighPass,
    dry_history: [[f32; FRAME]; RNNOISE_DELAY_FRAMES],
    level_gate: f32,
    voice_gate: VoiceGate,
    frames_seen: u32,
}

impl Cleaner {
    pub fn new() -> Self {
        Self {
            rnn: rnn::Denoiser::new(),
            deep: None,
            deep_failed: false,
            deep_timeout: DEEP_FRAME_TIMEOUT,
            highpass: HighPass::new(HIGHPASS_HZ),
            dry_history: [[0.0; FRAME]; RNNOISE_DELAY_FRAMES],
            level_gate: 1.0,
            voice_gate: VoiceGate::default(),
            frames_seen: 0,
        }
    }

    /// Current (voice gate, level gate) gains, for the scope.
    pub fn gate_gains(&self, p: &Params) -> (f32, f32) {
        let voice = if p.voice_gate && !p.bypass { self.voice_gate.gain() } else { 1.0 };
        (voice, self.level_gate)
    }

    pub fn state(&self, p: &Params) -> ModelState {
        match p.model {
            Model::Rnnoise => ModelState::Rnnoise,
            Model::DeepFilter if self.deep.as_ref().is_some_and(|d| d.ready) => {
                ModelState::DeepFilter
            }
            Model::DeepFilter if self.deep_failed => ModelState::DeepFilterFailed,
            Model::DeepFilter => ModelState::DeepFilterLoading,
        }
    }

    /// Returns the processed frame and the voice probability.
    pub fn process(&mut self, mic: &[f32; FRAME], p: &Params) -> ([f32; FRAME], f32) {
        let mut dry: [f32; FRAME] =
            std::array::from_fn(|i| (mic[i] * p.input_gain).clamp(-1.0, 1.0));
        if p.highpass {
            self.highpass.set_cutoff(p.highpass_hz);
            self.highpass.process(&mut dry);
        }

        // RNNoise operates on int16-scaled samples, on its own copy.
        let mut rnn_out = dry.map(|s| s * 32768.0);
        let prob = self.rnn.process_frame(&mut rnn_out);
        for s in &mut rnn_out {
            *s /= 32768.0;
        }
        let rnn_dry = self.dry_history[0];
        self.dry_history.rotate_left(1);
        self.dry_history[RNNOISE_DELAY_FRAMES - 1] = dry;

        let deep_out = if p.model == Model::DeepFilter {
            self.deep_filter(&dry, p.strength)
        } else {
            None
        };

        let mut z = match deep_out {
            // DeepFilterNet applies strength itself (time-aligned); the dry
            // input is only used by bypass and the level gate.
            Some(wet) => apply_processing(
                &dry,
                &wet,
                &Params { strength: 1.0, ..*p },
                &mut self.level_gate,
            ),
            None if p.bypass => apply_processing(&dry, &rnn_out, p, &mut self.level_gate),
            None => apply_processing(&rnn_dry, &rnn_out, p, &mut self.level_gate),
        };
        if self.frames_seen < VAD_WARMUP_FRAMES {
            self.frames_seen += 1;
        }
        if p.voice_gate && !p.bypass {
            let gate_prob = if self.frames_seen < VAD_WARMUP_FRAMES { 0.0 } else { prob };
            self.voice_gate.process(&mut z, gate_prob, p.voice_threshold);
        } else {
            self.voice_gate.reset();
        }
        (z, prob)
    }

    /// Run DeepFilterNet on its worker, starting it on first use. `None`
    /// (use RNNoise) while it loads, if it failed, or if a frame is late.
    fn deep_filter(&mut self, dry: &[f32; FRAME], strength: f32) -> Option<[f32; FRAME]> {
        if self.deep_failed {
            return None;
        }
        let deep = match &mut self.deep {
            Some(deep) => deep,
            None => match DeepFilterWorker::spawn() {
                Ok(worker) => self.deep.insert(worker),
                Err(_) => {
                    self.deep_failed = true;
                    return None;
                }
            },
        };
        match deep.process(dry, strength, self.deep_timeout) {
            Ok(out) => out,
            Err(_) => {
                self.deep = None;
                self.deep_failed = true;
                None
            }
        }
    }

    /// Block until DeepFilterNet is adopted, and never skip its frames, so
    /// offline runs are deterministic.
    #[cfg(test)]
    pub fn wait_for_deep_filter(&mut self) {
        self.deep_timeout = Duration::from_secs(10);
        let p = Params::default();
        while self.state(&p) == ModelState::DeepFilterLoading {
            self.deep_filter(&[0.0; FRAME], 1.0);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(self.state(&p), ModelState::DeepFilter, "DeepFilterNet failed to load");
    }
}

/// FFI to the vendored xiph RNNoise compiled by build.rs.
/// Frames are 480 int16-scaled f32 samples at 48 kHz; processing is in place.
pub mod rnn {
    use std::os::raw::c_float;

    #[repr(C)]
    pub struct DenoiseState {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        fn rnnoise_create(model: *const std::ffi::c_void) -> *mut DenoiseState;
        fn rnnoise_destroy(st: *mut DenoiseState);
        fn rnnoise_process_frame(
            st: *mut DenoiseState,
            out: *mut c_float,
            input: *const c_float,
        ) -> c_float;
    }

    pub struct Denoiser {
        st: *mut DenoiseState,
    }

    // The C state is only touched from the dedicated DSP thread, but the
    // handle must cross into it once at startup.
    unsafe impl Send for Denoiser {}

    impl Denoiser {
        pub fn new() -> Self {
            // NULL model = the library's built-in trained weights.
            unsafe { Self { st: rnnoise_create(std::ptr::null()) } }
        }

        /// Process `frame` in place; returns voice probability.
        pub fn process_frame(&mut self, frame: &mut [f32]) -> f32 {
            unsafe {
                rnnoise_process_frame(self.st, frame.as_mut_ptr(), frame.as_ptr())
            }
        }
    }

    impl Drop for Denoiser {
        fn drop(&mut self) {
            if !self.st.is_null() {
                unsafe { rnnoise_destroy(self.st) }
            }
        }
    }
}

/// DeepFilterNet 3 (embedded model, pure-Rust tract runtime) on 10 ms frames.
pub struct DeepFilter {
    model: df::tract::DfTract,
    strength: f32,
}

impl DeepFilter {
    /// Build the model. Slow (graph optimisation), so call off the audio thread.
    pub fn new() -> Result<Self> {
        // The library skips stages on frames it judges clean (local SNR above
        // 20 dB skips deep filtering, above 30 dB skips everything). The
        // gains-only path comes out ~10 dB quiet, so a voice in a fairly quiet
        // room kept ducking; always run both stages. Frames below -15 dB SNR
        // are still zeroed (as the upstream realtime plugin API does).
        let runtime = df::tract::RuntimeParams::default_with_ch(1)
            .with_thresholds(-15.0, 1000.0, 1000.0)
            .with_mask_reduce(df::tract::ReduceMask::MAX);
        let model = df::tract::DfTract::new(df::tract::DfParams::default(), &runtime)
        .map_err(|e| anyhow!("load DeepFilterNet: {e:#}"))?;
        if model.hop_size != FRAME || model.sr != crate::dsp::SR as usize {
            return Err(anyhow!(
                "DeepFilterNet model is {} Hz / {} hop, expected 48 kHz / {FRAME}",
                model.sr,
                model.hop_size
            ));
        }
        Ok(Self { model, strength: 1.0 })
    }

    /// Map the 0..1 reduction strength to DeepFilterNet's attenuation limit,
    /// which blends in the time-aligned noisy signal internally. Same noise
    /// floor as a dry/wet mix: 50% -> 6 dB, 90% -> 20 dB, 100% -> unlimited.
    pub fn set_strength(&mut self, strength: f32) {
        if strength == self.strength {
            return;
        }
        self.strength = strength;
        let limit_db = if strength >= 0.99999 {
            100.0
        } else {
            (-20.0 * (1.0 - strength).log10()).clamp(0.01, 100.0)
        };
        self.model.set_atten_lim(limit_db);
    }

    /// Enhance one frame of [-1, 1] samples.
    pub fn process(&mut self, input: &[f32; FRAME], out: &mut [f32; FRAME]) -> Result<()> {
        let noisy = ndarray::ArrayView2::from_shape((1, FRAME), input)?;
        let enh = ndarray::ArrayViewMut2::from_shape((1, FRAME), out)?;
        self.model
            .process(noisy, enh)
            .map_err(|e| anyhow!("DeepFilterNet: {e:#}"))?;
        Ok(())
    }
}

/// How long the audio thread waits for a DeepFilterNet frame (~0.2 ms of
/// work) before covering that frame with RNNoise.
const DEEP_FRAME_TIMEOUT: Duration = Duration::from_millis(6);

enum DeepReply {
    Ready,
    Frame(u64, [f32; FRAME]),
    Failed(String),
}

/// DeepFilterNet on its own thread: the tract runtime is `Rc`-based, so the
/// model must be built and run on a single thread, and the slow build must
/// stay off the audio thread.
struct DeepFilterWorker {
    requests: Sender<(u64, [f32; FRAME], f32)>,
    replies: Receiver<DeepReply>,
    seq: u64,
    ready: bool,
}

impl DeepFilterWorker {
    fn spawn() -> Result<Self> {
        let (requests, request_rx) = crossbeam_channel::bounded::<(u64, [f32; FRAME], f32)>(4);
        let (reply_tx, replies) = crossbeam_channel::bounded(8);
        std::thread::Builder::new()
            .name("openmic-deepfilter".into())
            .spawn(move || {
                let mut deep = match DeepFilter::new() {
                    Ok(deep) => deep,
                    Err(e) => {
                        let _ = reply_tx.send(DeepReply::Failed(format!("{e:#}")));
                        return;
                    }
                };
                if reply_tx.send(DeepReply::Ready).is_err() {
                    return;
                }
                // Exits when the cleaner (request sender) is dropped.
                for (seq, frame, strength) in request_rx {
                    deep.set_strength(strength);
                    let mut out = [0.0; FRAME];
                    let reply = match deep.process(&frame, &mut out) {
                        Ok(()) => DeepReply::Frame(seq, out),
                        Err(e) => DeepReply::Failed(format!("{e:#}")),
                    };
                    if reply_tx.send(reply).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self { requests, replies, seq: 0, ready: false })
    }

    /// `Ok(None)` while loading or when the frame is late; `Err` is fatal.
    fn process(
        &mut self,
        dry: &[f32; FRAME],
        strength: f32,
        timeout: Duration,
    ) -> Result<Option<[f32; FRAME]>, String> {
        if !self.ready {
            return match self.replies.try_recv() {
                Ok(DeepReply::Ready) => {
                    self.ready = true;
                    Ok(None)
                }
                Ok(DeepReply::Failed(e)) => Err(e),
                Ok(DeepReply::Frame(..)) | Err(TryRecvError::Empty) => Ok(None),
                Err(TryRecvError::Disconnected) => Err("DeepFilterNet worker exited".into()),
            };
        }
        self.seq += 1;
        if self.requests.try_send((self.seq, *dry, strength)).is_err() {
            return Ok(None); // worker backed up; skip rather than block
        }
        loop {
            match self.replies.recv_timeout(timeout) {
                Ok(DeepReply::Frame(seq, out)) if seq == self.seq => return Ok(Some(out)),
                Ok(DeepReply::Frame(..)) | Ok(DeepReply::Ready) => continue, // stale late frame
                Ok(DeepReply::Failed(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("DeepFilterNet worker exited".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::SR;
    // Deterministic xorshift RNG so the test needs no rand dependency.
    struct XorShift(u64);
    impl XorShift {
        fn next_uniform(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 ^= self.0 >> 43;
            ((self.0 >> 43) as f32 / (1u32 << 21) as f32) * 2.0 - 1.0
        }
    }

    fn run_frames(input: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut denoiser = rnn::Denoiser::new();
        let mut out = Vec::with_capacity(input.len());
        let mut probs = Vec::new();
        for chunk in input.chunks_exact(FRAME) {
            let mut frame = [0.0f32; FRAME];
            frame.copy_from_slice(chunk);
            probs.push(denoiser.process_frame(&mut frame));
            out.extend_from_slice(&frame);
        }
        (out, probs)
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    // Regression: RNNoise used to denoise the dry buffer in place, so bypass
    // emitted clipped int16-scaled audio and the gate measured a signal ~90 dB
    // too hot and never closed.
    #[test]
    fn cleaner_keeps_dry_signal_for_bypass_and_gate() {
        let base = Params {
            model: Model::Rnnoise,
            highpass: false,
            voice_gate: false,
            ..Default::default()
        };
        let mut cleaner = Cleaner::new();
        let bypass = Params { bypass: true, ..base };
        let (out, _) = cleaner.process(&[0.1f32; FRAME], &bypass);
        assert!(out.iter().all(|&s| (s - 0.1).abs() < 1e-6), "bypass must pass mic through");

        let gated = Params {
            gate_enabled: true,
            gate_threshold_db: -40.0,
            ..base
        };
        let quiet = [0.001f32; FRAME]; // -60 dBFS
        for _ in 0..40 {
            cleaner.process(&quiet, &gated);
        }
        assert!(
            cleaner.level_gate < 0.13,
            "gate should close on -60 dBFS input: {}",
            cleaner.level_gate
        );
    }

    // Ported from test_denoise.py: RNNoise crushes stationary noise and
    // raises speech probability on a tone. Inputs are int16-scaled like the
    // realtime path feeds them.
    #[test]
    fn denoiser_crushes_noise_and_flags_tone() {
        const SCALE: f32 = 32768.0;
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let noise: Vec<f32> = (0..SR as usize * 2)
            .map(|_| rng.next_uniform() * 0.1 * SCALE)
            .collect();
        let (out, probs) = run_frames(&noise);
        let (rms_in, rms_out) = (rms(&noise), rms(&out));
        eprintln!("reduction {rms_in} -> {rms_out} ratio={}", rms_out / rms_in);
        for amp in [0.003f32, 0.01, 0.1] {
            let n: Vec<f32> =
                (0..SR as usize).map(|_| rng.next_uniform() * amp * SCALE).collect();
            let (o, _) = run_frames(&n);
            eprintln!("amp={amp} ratio={}", rms(&o) / rms(&n));
        }
        assert!(rms_out < rms_in * 0.3, "reduction {rms_in} -> {rms_out}");

        let tone: Vec<f32> = (0..SR as usize)
            .map(|i| {
                use std::f32::consts::PI;
                0.3 * SCALE * (2.0 * PI * 220.0 * i as f32 / SR as f32).sin()
            })
            .collect();
        let (_, tone_probs) = run_frames(&tone);
        let prob_tone = tone_probs.iter().cloned().fold(0.0f32, f32::max);
        let prob_noise = probs.last().unwrap();
        eprintln!("prob noise={prob_noise} tone={prob_tone}");
        // The current xiph model reads stationary noise as fairly speech-like,
        // but still separates a pure tone decisively.
        assert!(prob_tone > 0.9, "speech prob on tone: {prob_tone}");
        assert!(
            prob_tone > prob_noise * 1.5,
            "tone should read as more speech-like than noise"
        );
    }

    /// DeepFilterNet 3's output lag (measured 1440 samples).
    const DEEP_FILTER_DELAY_FRAMES: usize = 3;

    /// Non-periodic voiced-ish test signal: harmonics over a wandering pitch.
    fn wandering_voice(seconds: f32) -> Vec<f32> {
        use std::f32::consts::TAU;
        let n = (SR as f32 * seconds) as usize / FRAME * FRAME;
        let mut phase = 0.0f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let f0 = 140.0 + 60.0 * (t * 1.3).sin() + 25.0 * (t * 4.7).sin();
                phase += TAU * f0 / SR as f32;
                let env = 0.5 + 0.5 * (t * 3.1).sin().abs();
                0.15 * env * (1..=8).map(|h| (phase * h as f32).sin() / h as f32).sum::<f32>()
            })
            .collect()
    }

    fn best_lag(input: &[f32], output: &[f32], max_lag: usize) -> usize {
        let corr = |lag: usize| -> f32 {
            (max_lag..input.len()).map(|i| input[i - lag] * output[i]).sum()
        };
        (0..max_lag)
            .max_by(|&a, &b| corr(a).total_cmp(&corr(b)))
            .unwrap()
    }

    fn db(x: f32) -> f32 {
        10.0 * x.max(1e-12).log10()
    }

    fn energy(x: &[f32]) -> f32 {
        x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32
    }

    /// Run the whole chain offline; returns output with the model delay removed.
    fn run_cleaner(mix: &[f32], p: &Params) -> Vec<f32> {
        let mut cleaner = Cleaner::new();
        let delay = if p.model == Model::DeepFilter {
            cleaner.wait_for_deep_filter();
            DEEP_FILTER_DELAY_FRAMES
        } else {
            RNNOISE_DELAY_FRAMES
        };
        let mut out = Vec::with_capacity(mix.len());
        for chunk in mix.chunks_exact(FRAME) {
            out.extend_from_slice(&cleaner.process(chunk.try_into().unwrap(), p).0);
        }
        out.drain(..delay * FRAME);
        out.extend(std::iter::repeat_n(0.0, delay * FRAME));
        out
    }

    #[test]
    fn model_delays_match_alignment_constants() {
        let voice = wandering_voice(0.6);
        let p = Params { highpass: false, voice_gate: false, ..Default::default() };
        for (model, frames) in [
            (Model::Rnnoise, RNNOISE_DELAY_FRAMES),
            (Model::DeepFilter, DEEP_FILTER_DELAY_FRAMES),
        ] {
            // run_cleaner already removes the expected delay.
            let out = run_cleaner(&voice, &Params { model, ..p });
            let lag = best_lag(&voice, &out, 200);
            let lead = best_lag(&out, &voice, 200);
            assert!(
                lag.max(lead) < 40,
                "{model:?}: expected {frames} frames, off by +{lag}/-{lead}"
            );
        }
    }

    // Regression: with the library's default thresholds DeepFilterNet skipped
    // its deep-filter stage on frames above 20 dB local SNR, and that path
    // came out ~10 dB quiet, so a voice in a quiet room kept ducking (seen
    // on real speech with light fan noise: 279 of 630 frames dropped).
    #[test]
    fn deep_filter_runs_both_stages_whenever_it_hears_signal() {
        let deep = DeepFilter::new().unwrap();
        for lsnr in [-14.0, 0.0, 15.0, 25.0, 35.0, 60.0] {
            assert_eq!(
                deep.model.apply_stages(lsnr),
                (true, false, true),
                "lsnr {lsnr} dB must apply gains and deep filtering"
            );
        }
        // Only frames judged pure noise are zeroed.
        assert_eq!(deep.model.apply_stages(-20.0), (false, true, false));
    }

    #[test]
    fn voice_gate_mutes_clicks_but_passes_voice() {
        // Key clicks alone for a second, then voice.
        let mut rng = XorShift(11);
        let mut mix = vec![0.0f32; SR as usize];
        for start in (2_000..SR as usize - 288).step_by(7_000) {
            for k in 0..288 {
                mix[start + k] += 0.3 * rng.next_uniform() * (-(k as f32) / 60.0).exp();
            }
        }
        let voice = wandering_voice(1.0);
        mix.extend_from_slice(&voice);

        let out = run_cleaner(&mix, &Params::default());
        // The gate opens a few frames ahead of the (model-delayed) voice.
        let clicks = &out[..SR as usize - 5 * FRAME];
        assert!(
            clicks.iter().all(|&s| s == 0.0),
            "clicks leaked: {:.1} dB",
            db(energy(clicks))
        );
        // The gate itself must not cost the voice anything. (Level vs. the
        // input is the model's business, and this buzz is not real speech.)
        let ungated = run_cleaner(&mix, &Params { voice_gate: false, ..Default::default() });
        let voiced = SR as usize + SR as usize / 5..mix.len();
        let kept = db(energy(&out[voiced.clone()])) - db(energy(&ungated[voiced]));
        assert!(kept > -0.5, "voice gate cost the voice {kept:.1} dB");
    }

    /// Speech with fan hiss, rumble and key clicks around and over it,
    /// padded with 1 s of noise. Returns (clean, mix).
    fn noisy_speech(
        speech: &[f32],
        hiss: f32,
        rumble: f32,
        click: f32,
        click_every: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let pad = SR as usize;
        let mut clean = vec![0.0f32; pad];
        clean.extend_from_slice(speech);
        clean.extend(std::iter::repeat_n(0.0, pad));
        clean.truncate(clean.len() / FRAME * FRAME);

        let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
        let mut mix = clean.clone();
        for (i, s) in mix.iter_mut().enumerate() {
            let t = i as f32 / SR as f32;
            *s += hiss * rng.next_uniform();
            *s += rumble * (std::f32::consts::TAU * 35.0 * t).sin();
        }
        if click > 0.0 {
            // 6 ms decaying noise bursts at irregular intervals.
            let mut i = 0;
            while i < mix.len() {
                let amp = click * (1.0 + 0.66 * rng.next_uniform().abs());
                for k in 0..288.min(mix.len() - i) {
                    mix[i + k] += amp * rng.next_uniform() * (-(k as f32) / 60.0).exp();
                }
                i += click_every + ((rng.next_uniform() + 1.0) * click_every as f32 / 2.0) as usize;
            }
        }
        (clean, mix)
    }

    /// (residual in the noise-only pads dBFS, residual between words dBFS,
    /// speech-frame SDR dB). `clean` must carry the same high-pass as the
    /// output, or its phase shift scores as distortion.
    fn score(clean: &[f32], out: &[f32]) -> (f32, f32, f32) {
        let pad_frames = SR as usize / FRAME;
        let n = clean.len() / FRAME;
        let (mut pad, mut pad_n, mut gap, mut gap_n, mut sig, mut err) =
            (0.0, 0, 0.0, 0, 0.0, 0.0);
        for (i, (c, o)) in clean.chunks_exact(FRAME).zip(out.chunks_exact(FRAME)).enumerate() {
            if i + 10 < pad_frames || i > n - pad_frames + 40 {
                pad += energy(o);
                pad_n += 1;
            } else if energy(c) < 1e-6 {
                gap += energy(o);
                gap_n += 1;
            } else {
                sig += c.iter().map(|s| s * s).sum::<f32>();
                err += c.iter().zip(o).map(|(c, o)| (c - o) * (c - o)).sum::<f32>();
            }
        }
        (db(pad / pad_n as f32), db(gap / gap_n.max(1) as f32), db(sig / err))
    }

    /// Compare configurations on a real speech recording (any format
    /// symphonia reads; e.g. one made with Windows text-to-speech):
    /// OPENMIC_SPEECH_WAV=speech.wav cargo test --release evaluate -- --ignored --nocapture
    #[test]
    #[ignore = "needs OPENMIC_SPEECH_WAV"]
    fn evaluate_on_speech() {
        let path = std::env::var("OPENMIC_SPEECH_WAV").expect("set OPENMIC_SPEECH_WAV");
        let speech = crate::decode::load_clip(std::path::Path::new(&path)).unwrap();
        let peak = speech.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let speech: Vec<f32> = speech.iter().map(|s| s * 0.5 / peak).collect();

        let off = Params { highpass: false, voice_gate: false, ..Default::default() };
        let configs = [
            ("rnnoise", Params { model: Model::Rnnoise, ..off }),
            ("rnnoise+hp+gate", Params { model: Model::Rnnoise, ..Default::default() }),
            ("deepfilter", Params { model: Model::DeepFilter, ..off }),
            ("deepfilter+hp+gate", Params::default()),
        ];
        eprintln!("silence / between words (dBFS) / speech SDR (dB, higher is better)");
        for (scene, hiss, rumble, click, every) in [
            ("fan hiss", 0.006, 0.0, 0.0, 4_800),
            ("loud fan", 0.03, 0.0, 0.0, 4_800),
            ("light typing", 0.006, 0.0, 0.04, 9_600),
            ("hard clicks + rumble", 0.006, 0.03, 0.15, 4_800),
        ] {
            let (clean, mix) = noisy_speech(&speech, hiss, rumble, click, every);
            let mut clean_hp = clean.clone();
            HighPass::new(HIGHPASS_HZ).process(&mut clean_hp);
            let (pad, gap, sdr) = score(&clean, &mix);
            eprintln!("\n{scene} (unprocessed {pad:.0} / {gap:.0} / {sdr:.1})");
            for (name, p) in configs {
                let out = run_cleaner(&mix, &p);
                let (pad, gap, sdr) = score(if p.highpass { &clean_hp } else { &clean }, &out);
                eprintln!("  {name:<20} {pad:>6.0} {gap:>6.0} {sdr:>6.1}");
            }
        }
    }
}
