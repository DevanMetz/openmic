//! Realtime engine: mic capture -> voice cleaner + soundboard -> output/monitor streams.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, ErrorKind, FromSample, Host, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;

use crate::denoise::{Cleaner, ModelState};
use crate::dsp::{Clip, Mixer, Params, FRAME, SR};
use crate::resample::Resampler;

/// How much processing history the GUI scope shows: 3 s of 10 ms frames.
pub const SCOPE_FRAMES: usize = 300;

/// Depth each output queue settles at: enough to ride out scheduling
/// jitter between the mic and output callbacks, and no more.
const TARGET_LATENCY_MS: usize = 25;
/// Backlog beyond the target that is dropped at once (after a stall, say)
/// rather than trimmed out gradually.
const MAX_EXTRA_LATENCY_MS: usize = 100;
/// Queue-depth smoothing per 10 ms frame (about a 1 s time constant).
const DRIFT_SMOOTHING: f32 = 0.01;
/// Rate trim per unit of relative depth error, and its limit (±0.2%, far
/// below an audible pitch change; real clock drift is ~0.01%).
const DRIFT_GAIN: f64 = 0.004;
const MAX_TRIM: f64 = 0.002;

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

/// A stream error the stream survives: a buffer glitch, a refused priority
/// boost, or a reroute. WASAPI ends a stream's thread after any other error.
pub fn is_transient(e: &cpal::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::Xrun | ErrorKind::RealtimeDenied | ErrorKind::DeviceChanged
    )
}

/// Error slots the stream callbacks and the processing thread report into.
#[derive(Default)]
struct Reports {
    /// A device went away or its stream died; restarting may recover.
    lost: Mutex<Option<String>>,
    /// Transient glitches (buffer underruns, priority refused); the streams
    /// keep running.
    warn: Mutex<Option<String>>,
    /// The processing thread failed; restarting will not help.
    fatal: Mutex<Option<String>>,
}

impl Reports {
    fn stream_error(self: &Arc<Self>, what: &'static str) -> impl FnMut(cpal::Error) + Send + 'static {
        let reports = Arc::clone(self);
        move |e: cpal::Error| {
            let slot = if is_transient(&e) { &reports.warn } else { &reports.lost };
            slot.lock().get_or_insert_with(|| format!("{what}: {e}"));
        }
    }
}

/// Samples bound for one output device, at its rate. Its callback waits
/// until the queue holds the target latency before playing (and again
/// after an underrun), so playback starts glitch-free at a known depth.
struct OutQueue {
    samples: ArrayQueue<f32>,
    primed: AtomicBool,
    base_target: usize,
    /// Largest callback seen, in frames; the target covers two of them.
    period: AtomicUsize,
}

impl OutQueue {
    fn new(rate: u32) -> Self {
        Self {
            samples: ArrayQueue::new(32_768),
            primed: AtomicBool::new(false),
            base_target: rate as usize * TARGET_LATENCY_MS / 1000,
            period: AtomicUsize::new(0),
        }
    }

    fn target(&self) -> usize {
        self.base_target.max(2 * self.period.load(Ordering::Relaxed))
    }

    fn push(&self, s: f32) {
        while self.samples.push(s).is_err() {
            self.samples.pop(); // hard ceiling; drift control keeps us far below
        }
    }

    /// Output callback: our mono signal on every channel; silence while priming.
    fn fill<T: Sample + FromSample<f32>>(&self, data: &mut [T], channels: usize) {
        self.period.fetch_max(data.len() / channels, Ordering::Relaxed);
        if !self.primed.load(Ordering::Relaxed) {
            if self.samples.len() < self.target() {
                data.fill(T::EQUILIBRIUM);
                return;
            }
            self.primed.store(true, Ordering::Relaxed);
        }
        let mut frames = data.chunks_mut(channels);
        for frame in frames.by_ref() {
            let Some(s) = self.samples.pop() else {
                self.primed.store(false, Ordering::Relaxed);
                frame.fill(T::EQUILIBRIUM);
                break;
            };
            frame.fill(s.to_sample());
        }
        for frame in frames {
            frame.fill(T::EQUILIBRIUM);
        }
    }
}

/// Holds an output queue at its target depth by trimming the resampler:
/// the mic's clock paces production and the output device's clock paces
/// consumption, and the two never quite agree. Without this the queue
/// slowly fills (growing delay) or runs dry (regular dropouts).
struct Drift {
    level: f32,
    max_extra: usize,
}

impl Drift {
    fn new(rate: u32) -> Self {
        Self {
            level: 0.0,
            max_extra: rate as usize * MAX_EXTRA_LATENCY_MS / 1000,
        }
    }

    /// Rate trim for the next frame (see [`Resampler::set_trim`]).
    fn trim(&mut self, queue: &OutQueue) -> f64 {
        let target = queue.target();
        if !queue.primed.load(Ordering::Relaxed) {
            self.level = target as f32; // filling up; nothing to correct yet
            return 1.0;
        }
        let mut depth = queue.samples.len();
        if depth > target + self.max_extra {
            while queue.samples.len() > target {
                queue.samples.pop();
            }
            depth = target;
            self.level = target as f32;
        }
        self.level += (depth as f32 - self.level) * DRIFT_SMOOTHING;
        let error = f64::from((self.level - target as f32) / target as f32);
        1.0 + (error * DRIFT_GAIN).clamp(-MAX_TRIM, MAX_TRIM)
    }
}

/// One output device as the processing thread sees it.
struct Output {
    queue: Arc<OutQueue>,
    res: Resampler,
    drift: Drift,
}

impl Output {
    fn new(queue: Arc<OutQueue>, rate: u32) -> Self {
        Self {
            queue,
            res: Resampler::adaptive(SR, rate),
            drift: Drift::new(rate),
        }
    }

    fn send(&mut self, frame: &[f32; FRAME], scratch: &mut [f32; FRAME]) {
        self.res.set_trim(self.drift.trim(&self.queue));
        self.res.push(frame);
        loop {
            let n = self.res.pull(scratch);
            if n == 0 {
                break;
            }
            for &s in &scratch[..n] {
                self.queue.push(s);
            }
        }
    }
}

/// Build a stream for whatever sample format the device uses.
macro_rules! for_format {
    ($format:expr, $build:ident ( $($arg:expr),* )) => {
        match $format {
            SampleFormat::I8 => $build::<i8>($($arg),*),
            SampleFormat::U8 => $build::<u8>($($arg),*),
            SampleFormat::I16 => $build::<i16>($($arg),*),
            SampleFormat::U16 => $build::<u16>($($arg),*),
            SampleFormat::I24 => $build::<cpal::I24>($($arg),*),
            SampleFormat::U24 => $build::<cpal::U24>($($arg),*),
            SampleFormat::I32 => $build::<i32>($($arg),*),
            SampleFormat::U32 => $build::<u32>($($arg),*),
            SampleFormat::I64 => $build::<i64>($($arg),*),
            SampleFormat::U64 => $build::<u64>($($arg),*),
            SampleFormat::F32 => $build::<f32>($($arg),*),
            SampleFormat::F64 => $build::<f64>($($arg),*),
            other => Err(anyhow!("unsupported sample format {other}")),
        }
    };
}

/// Mixdown to mono; overflows drop the oldest samples. Wakes the processing
/// thread as soon as audio arrives.
fn input_stream<T>(
    device: &Device,
    config: &StreamConfig,
    queue: &Arc<ArrayQueue<f32>>,
    waker: &Arc<OnceLock<Thread>>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels);
    let queue = Arc::clone(queue);
    let waker = Arc::clone(waker);
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            for frame in data.chunks(channels) {
                let mono = frame.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() / channels as f32;
                while queue.push(mono).is_err() {
                    queue.pop();
                }
            }
            if let Some(thread) = waker.get() {
                thread.unpark();
            }
        },
        on_error,
        None,
    )?)
}

fn output_stream<T>(
    device: &Device,
    config: &StreamConfig,
    queue: &Arc<OutQueue>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let queue = Arc::clone(queue);
    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| queue.fill(data, channels),
        on_error,
        None,
    )?)
}

/// An output device opened at its default format.
struct OpenOutput {
    stream: Stream,
    queue: Arc<OutQueue>,
    rate: u32,
}

fn open_output(host: &Host, name: &str, what: &'static str, reports: &Arc<Reports>) -> Result<OpenOutput> {
    let device = find_device(host, name, true)?;
    let supported = device
        .default_output_config()
        .with_context(|| format!("query default format of {what} '{name}'"))?;
    let config = StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    let queue = Arc::new(OutQueue::new(config.sample_rate));
    let stream = for_format!(
        supported.sample_format(),
        output_stream(&device, &config, &queue, reports.stream_error(what))
    )
    .with_context(|| format!("open {what} '{name}'"))?;
    Ok(OpenOutput { stream, queue, rate: config.sample_rate })
}

/// Handle to a running engine. Drop or call [`Engine::stop`] to shut down.
pub struct Engine {
    streams: Vec<Stream>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    params: Arc<ArcSwap<Params>>,
    mixer: Arc<Mutex<Mixer>>,
    stats: Arc<Stats>,
    reports: Arc<Reports>,
    has_monitor: bool,
}

impl Engine {
    /// Start capture on `mic` and playback on `out`, plus `monitor` if given.
    ///
    /// Devices are opened at their default format/rate (WASAPI rejects
    /// arbitrary formats) and the DSP core runs at 48 kHz mono, with
    /// streaming rate conversion on both sides.
    pub fn start(mic: &str, out: &str, monitor: Option<&str>, params: Params) -> Result<Self> {
        if monitor == Some(out) {
            return Err(anyhow!("monitor output must differ from processed output"));
        }
        let host = cpal::default_host();
        let reports = Arc::new(Reports::default());

        let mic_dev = find_device(&host, mic, false)?;
        let mic_cfg = mic_dev
            .default_input_config()
            .with_context(|| format!("query default format of microphone '{mic}'"))?;
        let mic_stream_cfg = StreamConfig {
            channels: mic_cfg.channels(),
            sample_rate: mic_cfg.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let mic_q = Arc::new(ArrayQueue::<f32>::new(32_768)); // mono @ mic rate
        let waker = Arc::new(OnceLock::new());
        let input = for_format!(
            mic_cfg.sample_format(),
            input_stream(&mic_dev, &mic_stream_cfg, &mic_q, &waker, reports.stream_error("microphone"))
        )
        .with_context(|| format!("open microphone '{mic}'"))?;

        let output = open_output(&host, out, "output", &reports)?;
        let monitor = monitor
            .map(|name| open_output(&host, name, "monitor", &reports))
            .transpose()?;

        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let params = Arc::new(ArcSwap::from_pointee(params));

        let worker = {
            let stop = Arc::clone(&stop);
            let stats = Arc::clone(&stats);
            let mixer = Arc::clone(&mixer);
            let params = Arc::clone(&params);
            let reports = Arc::clone(&reports);
            let mic_rate = mic_stream_cfg.sample_rate;
            let out = Output::new(Arc::clone(&output.queue), output.rate);
            let mon = monitor.as_ref().map(|m| Output::new(Arc::clone(&m.queue), m.rate));
            thread::Builder::new()
                .name("openmic-dsp".into())
                .spawn(move || {
                    let _ = waker.set(thread::current());
                    // Pro Audio scheduling class (MMCSS): fewer dropouts
                    // while a game or browser keeps the CPU busy.
                    let priority =
                        audio_thread_priority::promote_current_thread_to_real_time(FRAME as u32, SR);
                    if priority.is_err() {
                        reports.warn.lock().get_or_insert_with(|| {
                            "processing thread: real-time priority refused".into()
                        });
                    }
                    if let Err(e) =
                        process_loop(&stop, &mic_q, mic_rate, out, mon, &stats, &mixer, &params)
                    {
                        *reports.fatal.lock() = Some(format!("{e:#}"));
                    }
                })
                .context("spawn processing thread")?
        };

        let has_monitor = monitor.is_some();
        let mut streams = vec![input, output.stream];
        streams.extend(monitor.map(|m| m.stream));
        // Own the worker before starting any streams. If play() fails, Drop
        // signals and joins it instead of leaving a detached processing loop.
        let engine = Self {
            streams,
            worker: Some(worker),
            stop,
            params,
            mixer,
            stats,
            reports,
            has_monitor,
        };
        for stream in &engine.streams {
            stream.play().context("start audio stream")?;
        }
        Ok(engine)
    }

    pub fn set_params(&self, p: Params) {
        self.params.store(Arc::new(p));
    }

    /// Whether a headphone monitor stream was opened.
    pub fn has_monitor(&self) -> bool {
        self.has_monitor
    }

    /// Start a clip; see [`Mixer::play`].
    pub fn play_sound(&self, clip: Clip, overlap: bool) {
        self.mixer.lock().play(clip, overlap);
    }

    pub fn stop_sounds(&self) {
        self.mixer.lock().stop();
    }

    pub fn stop_sound(&self, key: u64) {
        self.mixer.lock().stop_key(key);
    }

    pub fn set_sound_gain(&self, key: u64, gain: f32) {
        self.mixer.lock().set_gain(key, gain);
    }

    /// Keys of the clips currently playing.
    pub fn playing(&self) -> Vec<u64> {
        self.mixer.lock().playing_keys()
    }

    /// Playing clips at their playheads (route-switch handoff).
    pub fn take_clips(&self) -> Vec<Clip> {
        self.mixer.lock().take_remaining()
    }

    pub fn resume_clips(&self, clips: Vec<Clip>) {
        self.mixer.lock().resume(clips);
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

    /// Pop the latest non-fatal stream warning (buffer underrun, ...). The
    /// streams keep running through these.
    pub fn take_warning(&self) -> Option<String> {
        self.reports.warn.lock().take()
    }

    /// Pop a lost-stream report: a device was unplugged or its stream died.
    /// The engine is no longer delivering audio; starting again may recover.
    pub fn take_lost(&self) -> Option<String> {
        self.reports.lost.lock().take()
    }

    /// Pop a fatal processing error, if any.
    pub fn take_error(&self) -> Option<String> {
        self.reports.fatal.lock().take()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
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

/// DSP core at 48 kHz mono, fed from the capture queue (native rate) and
/// drained into per-device queues (native rates) through streaming resamplers.
#[allow(clippy::too_many_arguments)]
fn process_loop(
    stop: &AtomicBool,
    mic_q: &ArrayQueue<f32>,
    mic_rate: u32,
    mut out: Output,
    mut mon: Option<Output>,
    stats: &Stats,
    mixer: &Mutex<Mixer>,
    params: &ArcSwap<Params>,
) -> Result<()> {
    let mut cleaner = Cleaner::new();
    let mut in_res = Resampler::new(mic_rate, SR);
    let mut scratch = [0.0f32; FRAME];

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
                    // The mic callback unparks us; the timeout only guards shutdown.
                    thread::park_timeout(Duration::from_millis(20));
                    if stop.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                }
            }
        }
        set_f32(&stats.in_peak, peak(&x));

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
        set_f32(&stats.out_peak, peak(&z));

        out.send(&z, &mut scratch);
        if let Some(mon) = mon.as_mut().filter(|_| p.monitor_on) {
            let gained = z.map(|s| (s * p.monitor_gain).clamp(-1.0, 1.0));
            mon.send(&gained, &mut scratch);
        }
    }
}

fn peak(frame: &[f32]) -> f32 {
    frame.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn rms_db(samples: impl ExactSizeIterator<Item = f32>) -> f32 {
    let n = samples.len().max(1) as f32;
    let mean_square = samples.map(|s| s * s).sum::<f32>() / n;
    10.0 * mean_square.max(1e-12).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a producer at 48 kHz against an output clock `ppm` off, through
    /// the real queue, callback and drift loop, and return the queue depth
    /// (ms) over the last simulated minute.
    fn simulate(ppm: f64, minutes: usize) -> (f32, f32, usize) {
        let queue = Arc::new(OutQueue::new(48_000));
        let mut out = Output::new(Arc::clone(&queue), 48_000);
        let frame = [0.1f32; FRAME];
        let mut scratch = [0.0f32; FRAME];
        let mut device = vec![0.0f32; 480];
        let mut consumed = 0.0f64;
        let (mut lo, mut hi) = (f32::MAX, 0.0f32);
        let mut underruns = 0;
        let frames = minutes * 6_000;
        for i in 0..frames {
            out.send(&frame, &mut scratch);
            // The device pulls 10 ms of its own clock per callback.
            consumed += 480.0 * (1.0 + ppm * 1e-6);
            while consumed >= 480.0 {
                consumed -= 480.0;
                let was_primed = queue.primed.load(Ordering::Relaxed);
                queue.fill(&mut device, 1);
                if was_primed && !queue.primed.load(Ordering::Relaxed) {
                    underruns += 1;
                }
            }
            if i > frames - 6_000 {
                let ms = queue.samples.len() as f32 / 48.0;
                lo = lo.min(ms);
                hi = hi.max(ms);
            }
        }
        (lo, hi, underruns)
    }

    #[test]
    fn fast_output_clock_neither_starves_nor_drifts() {
        // +300 ppm is well beyond real hardware; uncorrected it would run
        // the queue dry every ~80 s.
        let (lo, hi, underruns) = simulate(300.0, 10);
        assert_eq!(underruns, 0);
        assert!(lo > 5.0 && hi < 45.0, "depth {lo:.1}..{hi:.1} ms");
    }

    #[test]
    fn slow_output_clock_does_not_build_up_delay() {
        // Uncorrected, -300 ppm adds 18 ms of delay per minute.
        let (lo, hi, underruns) = simulate(-300.0, 10);
        assert_eq!(underruns, 0);
        assert!(lo > 5.0 && hi < 45.0, "depth {lo:.1}..{hi:.1} ms");
    }

    #[test]
    fn stalled_backlog_snaps_back_to_target() {
        let queue = Arc::new(OutQueue::new(48_000));
        let mut drift = Drift::new(48_000);
        for _ in 0..30_000 {
            queue.push(0.0); // 625 ms queued, e.g. after the device stalled
        }
        queue.primed.store(true, Ordering::Relaxed);
        drift.trim(&queue);
        assert_eq!(queue.samples.len(), queue.target());
    }

    #[test]
    fn glitches_are_transient_but_a_lost_device_is_not() {
        assert!(is_transient(&ErrorKind::Xrun.into()));
        assert!(is_transient(&ErrorKind::RealtimeDenied.into()));
        assert!(!is_transient(&ErrorKind::DeviceNotAvailable.into()));
        assert!(!is_transient(&ErrorKind::StreamInvalidated.into()));
    }

    #[test]
    fn priming_waits_for_the_target_then_plays() {
        let queue = OutQueue::new(48_000);
        let mut data = [1.0f32; 480];
        for _ in 0..600 {
            queue.push(0.5);
        }
        queue.fill(&mut data, 1);
        assert!(data.iter().all(|&s| s == 0.0), "600 < 1200-sample target: still priming");
        for _ in 0..600 {
            queue.push(0.5);
        }
        queue.fill(&mut data, 2); // stereo: 240 frames
        assert!(data.iter().all(|&s| s == 0.5));
        assert_eq!(queue.samples.len(), 1200 - 240);
    }
}
