//! Realtime engine: mic capture -> voice cleaner + soundboard -> output/monitor streams.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, Sample, Stream};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;

use crate::denoise::{Cleaner, ModelState};
use crate::dsp::{Clip, Mixer, Params, FRAME, SR};

/// How much processing history the GUI scope shows: 3 s of 10 ms frames.
pub const SCOPE_FRAMES: usize = 300;

/// One processed frame as the scope draws it.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScopeFrame {
    /// RMS of the mic after input gain, dBFS.
    pub input_db: f32,
    /// RMS of the cleaned voice (before the soundboard), dBFS.
    pub output_db: f32,
    pub prob: f32,
    pub voice_gate: f32,
    pub level_gate: f32,
}

/// Live meters and voice probability for the GUI (f32 bit patterns).
#[derive(Default)]
pub struct Stats {
    in_peak: AtomicU32,
    out_peak: AtomicU32,
    prob: AtomicU32,
    model: AtomicU32,
    history: Mutex<VecDeque<ScopeFrame>>,
}

fn set_f32(atomic: &AtomicU32, value: f32) {
    atomic.store(value.to_bits(), Ordering::Relaxed);
}

fn get_f32(atomic: &AtomicU32) -> f32 {
    f32::from_bits(atomic.load(Ordering::Relaxed))
}

#[derive(Clone, Copy)]
pub struct StatsSnapshot {
    pub in_peak: f32,
    pub out_peak: f32,
    pub prob: f32,
    pub model: ModelState,
}

/// Deduped friendly names for a direction.
pub fn list_devices(host: &Host, output: bool) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let iter = if output { host.output_devices() } else { host.input_devices() };
    if let Ok(devices) = iter {
        for device in devices {
            let name = device.to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

fn find_device(host: &Host, name: &str, output: bool) -> Result<Device> {
    let iter = if output { host.output_devices() } else { host.input_devices() };
    iter.with_context(|| format!("enumerate audio devices looking for '{name}'"))?
        .find(|d| d.to_string() == name)
        .with_context(|| format!("audio device '{name}' not found"))
}

/// Handle to a running engine. Drop or call [`Engine::stop`] to shut down.
pub struct Engine {
    streams: Vec<Stream>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    params: Arc<ArcSwap<Params>>,
    mixer: Arc<Mutex<Mixer>>,
    stats: Arc<Stats>,
    error: Arc<Mutex<Option<String>>>,
    warn: Arc<Mutex<Option<String>>>,
}

impl Engine {
    /// Start capture on `mic` and playback on `out` + `monitor`.
    ///
    /// Devices are opened at their default format/rate (WASAPI rejects
    /// arbitrary formats) and the DSP core runs at 48 kHz mono, with
    /// streaming rate conversion on both sides.
    pub fn start(mic: &str, out: &str, monitor: &str, params: Params) -> Result<Self> {
        if params.monitor_on && monitor == out {
            return Err(anyhow!("monitor output must differ from processed output"));
        }
        let host = cpal::default_host();
        let mic_dev = find_device(&host, mic, false)?;
        let out_dev = find_device(&host, out, true)?;
        let mon_dev = find_device(&host, monitor, true)?;

        let mic_cfg = mic_dev
            .default_input_config()
            .with_context(|| format!("query default format of microphone '{mic}'"))?;
        let out_cfg = out_dev
            .default_output_config()
            .with_context(|| format!("query default format of output '{out}'"))?;
        let mon_cfg = mon_dev
            .default_output_config()
            .with_context(|| format!("query default format of monitor '{monitor}'"))?;

        let to_stream = |c: cpal::SupportedStreamConfig| cpal::StreamConfig {
            channels: c.channels(),
            sample_rate: c.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let mic_stream_cfg = to_stream(mic_cfg.clone());
        let out_stream_cfg = to_stream(out_cfg.clone());
        let mon_stream_cfg = to_stream(mon_cfg.clone());

        let mic_q = Arc::new(ArrayQueue::<f32>::new(32_768)); // mono @ mic rate
        let out_q = Arc::new(ArrayQueue::<f32>::new(32_768)); // mono @ out rate
        let mon_q = Arc::new(ArrayQueue::<f32>::new(32_768)); // mono @ mon rate

        // Fatal errors stop the engine; stream glitches (buffer underruns,
        // device reconfigs) are transient and only surface as warnings.
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let warn: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let stream_err = |what: &'static str| {
            let warn = Arc::clone(&warn);
            move |e: cpal::Error| {
                *warn.lock() = Some(format!("{what}: {e}"));
            }
        };

        // Mixdown to mono; overflows drop oldest samples.
        macro_rules! build_input {
            ($fmt:ident) => {{
                let channels = usize::from(mic_cfg.channels());
                mic_dev.build_input_stream(
                    mic_stream_cfg.clone(),
                    {
                        let mic_q = Arc::clone(&mic_q);
                        move |data: &[$fmt], _: &cpal::InputCallbackInfo| {
                            for frame in data.chunks(channels) {
                                let mono = frame
                                    .iter()
                                    .map(|&s| s.to_sample::<f32>())
                                    .sum::<f32>()
                                        / channels as f32;
                                while mic_q.push(mono).is_err() {
                                    mic_q.pop();
                                }
                            }
                        }
                    },
                    stream_err("microphone"),
                    None,
                )
            }};
        }
        let input_stream = match mic_cfg.sample_format() {
            cpal::SampleFormat::F32 => build_input!(f32).context(input_ctx(mic))?,
            cpal::SampleFormat::I16 => build_input!(i16).context(input_ctx(mic))?,
            cpal::SampleFormat::U16 => build_input!(u16).context(input_ctx(mic))?,
            other => return Err(anyhow!("unsupported microphone sample format {other}")),
        };

        // Duplicate our mono signal across the device's channel count;
        // underruns play silence. The output device is the master clock.
        macro_rules! build_output {
            ($dev:expr, $cfg:expr, $q:expr, $fmt:ident, $what:expr) => {{
                let channels = usize::from($cfg.channels);
                $dev.build_output_stream(
                    $cfg,
                    {
                        let q = Arc::clone($q);
                        move |data: &mut [$fmt], _: &cpal::OutputCallbackInfo| {
                            for frame in data.chunks_mut(channels) {
                                let s: $fmt = q.pop().unwrap_or(0.0).to_sample();
                                frame.fill(s);
                            }
                        }
                    },
                    stream_err($what),
                    None,
                )
            }};
        }
        let out_ctx = || format!("open output '{out}'");
        let output_stream = match out_cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                build_output!(out_dev, out_stream_cfg, &out_q, f32, "output")
                    .context(out_ctx())?
            }
            cpal::SampleFormat::I16 => {
                build_output!(out_dev, out_stream_cfg, &out_q, i16, "output")
                    .context(out_ctx())?
            }
            cpal::SampleFormat::U16 => {
                build_output!(out_dev, out_stream_cfg, &out_q, u16, "output")
                    .context(out_ctx())?
            }
            other => return Err(anyhow!("unsupported output sample format {other}")),
        };
        let mon_ctx = || format!("open monitor '{monitor}'");
        let monitor_stream = match mon_cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                build_output!(mon_dev, mon_stream_cfg, &mon_q, f32, "monitor")
                    .context(mon_ctx())?
            }
            cpal::SampleFormat::I16 => {
                build_output!(mon_dev, mon_stream_cfg, &mon_q, i16, "monitor")
                    .context(mon_ctx())?
            }
            cpal::SampleFormat::U16 => {
                build_output!(mon_dev, mon_stream_cfg, &mon_q, u16, "monitor")
                    .context(mon_ctx())?
            }
            other => return Err(anyhow!("unsupported monitor sample format {other}")),
        };

        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let params = Arc::new(ArcSwap::from_pointee(params));

        let worker = {
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let mixer = Arc::clone(&mixer);
            let params = Arc::clone(&params);
            let error = Arc::clone(&error);
            thread::Builder::new()
                .name("openmic-dsp".into())
                .spawn(move || {
                    if let Err(e) = process_loop(
                        stop,
                        &mic_q,
                        &out_q,
                        &mon_q,
                        mic_cfg.sample_rate(),
                        out_cfg.sample_rate(),
                        mon_cfg.sample_rate(),
                        stats,
                        mixer,
                        params,
                    ) {
                        *error.lock() = Some(format!("{e:#}"));
                    }
                })
                .context("spawn processing thread")?
        };

        input_stream.play().context("start microphone stream")?;
        output_stream.play().context("start output stream")?;
        monitor_stream.play().context("start monitor stream")?;

        Ok(Self {
            streams: vec![input_stream, output_stream, monitor_stream],
            worker: Some(worker),
            stop,
            params,
            mixer,
            stats,
            error,
            warn,
        })
    }

    pub fn set_params(&self, p: Params) {
        self.params.store(Arc::new(p));
    }

    /// Start playing a decoded clip from the top.
    pub fn play_sound(&self, samples: Arc<Vec<f32>>) {
        self.mixer.lock().clip = (!samples.is_empty()).then_some(Clip { samples, pos: 0 });
    }

    pub fn stop_sound(&self) {
        self.mixer.lock().clip = None;
    }

    pub fn sound_playing(&self) -> bool {
        self.mixer.lock().playing()
    }

    /// Remaining clip samples at the current playhead (route-switch handoff).
    pub fn take_remaining_clip(&self) -> Option<Vec<f32>> {
        self.mixer.lock().take_remaining()
    }

    /// The last few seconds of processing, oldest first.
    pub fn scope(&self) -> Vec<ScopeFrame> {
        self.stats.history.lock().iter().copied().collect()
    }

    pub fn stats(&self) -> StatsSnapshot {
        StatsSnapshot {
            in_peak: get_f32(&self.stats.in_peak),
            out_peak: get_f32(&self.stats.out_peak),
            prob: get_f32(&self.stats.prob),
            model: ModelState::from_u32(self.stats.model.load(Ordering::Relaxed)),
        }
    }

    /// Pop the latest non-fatal stream warning (buffer underrun, device
    /// reconfig, ...). The stream keeps running through these.
    pub fn take_warning(&self) -> Option<String> {
        self.warn.lock().take()
    }

    /// Pop a fatal stream/processing error, if any.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().take()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.streams.clear(); // dropping a cpal Stream stops it
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn input_ctx(mic: &str) -> String {
    format!("open microphone '{mic}'")
}


/// DSP core at 48 kHz mono, fed from the capture queue (native rate) and
/// drained into per-device queues (native rates) through streaming resamplers.
fn process_loop(
    stop: Arc<AtomicBool>,
    mic_q: &ArrayQueue<f32>,
    out_q: &ArrayQueue<f32>,
    mon_q: &ArrayQueue<f32>,
    mic_rate: u32,
    out_rate: u32,
    mon_rate: u32,
    stats: Arc<Stats>,
    mixer: Arc<Mutex<Mixer>>,
    params: Arc<ArcSwap<Params>>,
) -> Result<()> {
    let mut cleaner = Cleaner::new();
    let mut in_res = crate::resample::Resampler::new(mic_rate, SR);
    let mut out_res = crate::resample::Resampler::new(SR, out_rate);
    let mut mon_res = crate::resample::Resampler::new(SR, mon_rate);
    let mut out_buf = [0.0f32; FRAME];

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Feed the input resampler until it yields a full 48 kHz frame.
        let mut x = [0.0f32; FRAME];
        let mut filled = 0usize;
        while filled < FRAME {
            filled += in_res.pull(&mut x[filled..]);
            if filled < FRAME {
                let mut moved = false;
                while let Some(s) = mic_q.pop() {
                    in_res.push(&[s]);
                    moved = true;
                    if in_res.backlog() > 8_000 {
                        break;
                    }
                }
                if !moved {
                    thread::sleep(Duration::from_millis(2));
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                }
            }
        }
        set_f32(
            &stats.in_peak,
            x.iter().fold(0.0f32, |m, s| m.max(s.abs())),
        );

        let p = **params.load();
        let (mut z, prob) = cleaner.process(&x, &p);
        let (voice_gate, level_gate) = cleaner.gate_gains(&p);
        let frame = ScopeFrame {
            input_db: rms_db(x.iter().map(|s| (s * p.input_gain).clamp(-1.0, 1.0))),
            output_db: rms_db(z.iter().copied()),
            prob,
            voice_gate,
            level_gate,
        };
        // Never block the audio thread on the GUI; a skipped point is invisible.
        if let Some(mut history) = stats.history.try_lock() {
            if history.len() == SCOPE_FRAMES {
                history.pop_front();
            }
            history.push_back(frame);
        }
        mixer.lock().mix(&mut z, p.sound_gain);
        set_f32(&stats.prob, prob);
        stats.model.store(cleaner.state(&p) as u32, Ordering::Relaxed);
        set_f32(
            &stats.out_peak,
            z.iter().fold(0.0f32, |m, s| m.max(s.abs())),
        );

        // Convert the processed frame to each output device's rate.
        out_res.push(&z);
        while {
            let n = out_res.pull(&mut out_buf);
            for &s in &out_buf[..n] {
                push_drop_oldest(out_q, s);
            }
            n > 0
        } {}
        if p.monitor_on {
            let gained: Vec<f32> =
                z.iter().map(|s| (s * p.monitor_gain).clamp(-1.0, 1.0)).collect();
            mon_res.push(&gained);
            while {
                let n = mon_res.pull(&mut out_buf);
                for &s in &out_buf[..n] {
                    push_drop_oldest(mon_q, s);
                }
                n > 0
            } {}
        }
    }
}

fn rms_db(samples: impl ExactSizeIterator<Item = f32>) -> f32 {
    let n = samples.len().max(1) as f32;
    let mean_square = samples.map(|s| s * s).sum::<f32>() / n;
    10.0 * mean_square.max(1e-12).log10()
}

fn push_drop_oldest(q: &ArrayQueue<f32>, s: f32) {
    while q.push(s).is_err() {
        q.pop(); // device clock drift
    }
}
