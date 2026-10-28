//! Realtime engine: mic capture -> voice cleaner + soundboard -> output/monitor streams.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, ErrorKind, FromSample, Host, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;

use crate::denoise::{Cleaner, ModelState};
use crate::dsp::{Clip, ClipProgress, FRAME, Mixer, Params, SR};
use crate::resample::Resampler;

/// How much processing history the GUI scope shows: 3 s of 10 ms frames.
pub const SCOPE_FRAMES: usize = 300;

/// Depth each output queue settles at: enough to ride out scheduling
/// jitter between the mic and output callbacks, and no more.
const TARGET_LATENCY_MS: usize = 25;
/// Backlog beyond the target that is dropped at once (after a stall, say)
/// rather than trimmed out gradually.
const MAX_EXTRA_LATENCY_MS: usize = 100;
/// Microphone audio waiting beyond this is dropped at once: processing fell
/// behind (an overloaded CPU, a stall), and catching up later would leave
/// the voice that far behind for good.
const MAX_INPUT_BACKLOG_MS: usize = 100;
/// What a trim leaves queued: about one microphone callback.
const INPUT_KEEP_MS: usize = 20;
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
    pub out_peak: f32,
    pub prob: f32,
    pub model: ModelState,
}

/// One device as the host reports it. `id` is the backend id (`DeviceId::id`),
/// empty when the host cannot provide one. PulseAudio monitor sources use
/// `{sink}.monitor` here; their display names are translated. Linux also
/// uses it to rank ALSA routes. Windows builds do not carry it.
pub struct ListedDevice {
    pub name: String,
    #[cfg(not(windows))]
    pub id: String,
}

/// Every device in a direction, including duplicate display names.
pub fn list_device_details(host: &Host, output: bool) -> Vec<ListedDevice> {
    let mut listed = Vec::new();
    let iter = if output {
        host.output_devices()
    } else {
        host.input_devices()
    };
    if let Ok(devices) = iter {
        for device in devices {
            #[cfg(target_os = "linux")]
            if host.id() == cpal::HostId::Alsa {
                let id = device.id().map(|id| id.id().to_owned()).unwrap_or_default();
                let description = device.description().ok();
                let name = device.to_string();
                if is_noop_alsa_device(
                    &id,
                    std::iter::once(name.as_str()).chain(
                        description
                            .iter()
                            .flat_map(|description| description.extended()),
                    ),
                ) {
                    continue;
                }
            }
            listed.push(ListedDevice {
                name: device.to_string(),
                #[cfg(not(windows))]
                id: device.id().map(|id| id.id().to_owned()).unwrap_or_default(),
            });
        }
    }
    #[cfg(target_os = "linux")]
    if host.id() == cpal::HostId::Alsa {
        // Stable ordering keeps the backend's order within each preference.
        listed.sort_by_key(|device| alsa_device_rank(&device.id));
    }
    listed
}

/// Prefer configured routing over direct hardware, then other ALSA plugins.
/// CPAL's ALSA default device is the PCM named `default`.
#[cfg(target_os = "linux")]
fn alsa_device_rank(id: &str) -> u8 {
    match id.split(':').next().unwrap_or(id).trim() {
        "default" => 0,
        "pipewire" => 1,
        "sysdefault" => 2,
        "plughw" => 3,
        "hw" => 4,
        _ => 5,
    }
}

/// ALSA's null PCM can be exposed under an alias, so inspect its description too.
#[cfg(target_os = "linux")]
pub(crate) fn is_noop_alsa_device<'a>(
    id: &str,
    descriptions: impl IntoIterator<Item = &'a str>,
) -> bool {
    id.split(':')
        .next()
        .unwrap_or(id)
        .trim()
        .eq_ignore_ascii_case("null")
        || descriptions.into_iter().any(|description| {
            let text = description.trim().to_ascii_lowercase();
            text == "null"
                || text.contains("discard all samples")
                || text.contains("generate zero samples")
        })
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn filters_alsa_noop_devices_by_id_or_description() {
    assert!(is_noop_alsa_device("null", ["Anything"]));
    assert!(is_noop_alsa_device("null:CARD=0", ["Anything"]));
    assert!(is_noop_alsa_device(
        "alias",
        ["Discard all samples (playback)"]
    ));
    assert!(is_noop_alsa_device(
        "alias",
        ["Generate zero samples (capture)"]
    ));
    assert!(is_noop_alsa_device("alias", ["Null"]));
    assert!(is_noop_alsa_device(
        "alias",
        [
            "Dummy",
            "Discard all samples (playback) or generate zero samples (capture)"
        ]
    ));
    assert!(!is_noop_alsa_device("hw:0", ["USB microphone"]));
    assert!(!is_noop_alsa_device("hw:1", ["Null Audio USB microphone"]));
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn alsa_defaults_precede_hardware_and_other_plugins() {
    let mut ids = [
        "rate",
        "hw:0",
        "plughw:1",
        "sysdefault:CARD=USB",
        "pipewire",
        "default",
        "hw:1",
    ];
    ids.sort_by_key(|id| alsa_device_rank(id));
    assert_eq!(
        ids,
        [
            "default",
            "pipewire",
            "sysdefault:CARD=USB",
            "plughw:1",
            "hw:0",
            "hw:1",
            "rate"
        ]
    );
}

/// Deduped friendly names for a direction. The first device keeps the name.
pub fn list_devices(host: &Host, output: bool) -> Vec<String> {
    device_names(list_device_details(host, output))
}

fn device_names(devices: impl IntoIterator<Item = ListedDevice>) -> Vec<String> {
    let mut names = Vec::new();
    for device in devices {
        if !names.contains(&device.name) {
            names.push(device.name);
        }
    }
    names
}

fn find_device(host: &Host, name: &str, output: bool) -> Result<Device> {
    let iter = if output {
        host.output_devices()
    } else {
        host.input_devices()
    };
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
    fn stream_error(
        self: &Arc<Self>,
        what: &'static str,
    ) -> impl FnMut(cpal::Error) + Send + 'static {
        let reports = Arc::clone(self);
        move |e: cpal::Error| {
            let slot = if is_transient(&e) {
                &reports.warn
            } else {
                &reports.lost
            };
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
        // Large device callbacks must not leave priming waiting for more
        // samples than this queue can ever hold.
        self.base_target
            .max(self.period.load(Ordering::Relaxed).saturating_mul(2))
            .min(self.samples.capacity())
    }

    fn push(&self, s: f32) {
        while self.samples.push(s).is_err() {
            self.samples.pop(); // hard ceiling; drift control keeps us far below
        }
    }

    /// Output callback: our mono signal on every channel; silence while priming.
    fn fill<T: Sample + FromSample<f32>>(&self, data: &mut [T], channels: usize) {
        self.period
            .fetch_max(data.len() / channels, Ordering::Relaxed);
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
            // Cubic rate conversion can overshoot even when DSP samples
            // were already limited to the device's full-scale range.
            frame.fill(s.clamp(-1.0, 1.0).to_sample());
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

/// Windows keeps the driver's default buffer. PulseAudio's default asks for
/// around two seconds, so Linux uses 10 ms, clamped to what the device allows.
#[cfg(windows)]
pub(crate) fn stream_buffer(
    _rate: u32,
    _supported: &cpal::SupportedBufferSize,
) -> cpal::BufferSize {
    cpal::BufferSize::Default
}

#[cfg(not(windows))]
pub(crate) fn stream_buffer(rate: u32, supported: &cpal::SupportedBufferSize) -> cpal::BufferSize {
    match supported {
        cpal::SupportedBufferSize::Range { min, max } => {
            cpal::BufferSize::Fixed((rate / 100).max(1).clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
    }
}

/// Build a stream, retrying once with the driver's default buffer if Linux
/// rejects a fixed size (some ALSA devices do).
#[cfg(windows)]
pub(crate) fn open_stream_with_fallback<E>(
    config: &mut StreamConfig,
    mut build: impl FnMut(&StreamConfig) -> std::result::Result<Stream, E>,
) -> std::result::Result<Stream, E> {
    build(config)
}

#[cfg(not(windows))]
pub(crate) fn open_stream_with_fallback<E>(
    config: &mut StreamConfig,
    mut build: impl FnMut(&StreamConfig) -> std::result::Result<Stream, E>,
) -> std::result::Result<Stream, E> {
    let fixed = matches!(config.buffer_size, cpal::BufferSize::Fixed(_));
    match build(config) {
        Ok(stream) => Ok(stream),
        Err(_) if fixed => {
            config.buffer_size = cpal::BufferSize::Default;
            build(config)
        }
        Err(e) => Err(e),
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
    Ok(device.build_input_stream(
        *config,
        input_callback::<T>(
            usize::from(config.channels),
            config.sample_rate,
            Arc::clone(queue),
            Arc::clone(waker),
        ),
        on_error,
        None,
    )?)
}

fn input_callback<T>(
    channels: usize,
    sample_rate: u32,
    queue: Arc<ArrayQueue<f32>>,
    waker: Arc<OnceLock<Thread>>,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
where
    T: SizedSample,
    f32: FromSample<T>,
{
    move |data, _| {
        crate::rt::request_audio_thread(sample_rate, "microphone");
        for frame in data.chunks(channels) {
            // Invalid device samples must not poison the resampler or the
            // persistent filter state. Sum in f64 before clipping the mix.
            let sum: f64 = frame
                .iter()
                .map(|&s| s.to_sample::<f32>())
                .filter(|s| s.is_finite())
                .map(f64::from)
                .sum();
            let mono = (sum / channels as f64).clamp(-1.0, 1.0) as f32;
            while queue.push(mono).is_err() {
                queue.pop();
            }
        }
        if let Some(thread) = waker.get() {
            thread.unpark();
        }
    }
}

fn output_stream<T>(
    device: &Device,
    config: &StreamConfig,
    queue: &Arc<OutQueue>,
    on_error: impl FnMut(cpal::Error) + Send + 'static,
    what: &'static str,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let queue = Arc::clone(queue);
    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            crate::rt::request_audio_thread(rate, what);
            queue.fill(data, channels);
        },
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

fn open_output(
    host: &Host,
    name: &str,
    what: &'static str,
    reports: &Arc<Reports>,
) -> Result<OpenOutput> {
    let device = find_device(host, name, true)?;
    let supported = device
        .default_output_config()
        .with_context(|| format!("query default format of {what} '{name}'"))?;
    let mut config = StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: stream_buffer(supported.sample_rate(), supported.buffer_size()),
    };
    let queue = Arc::new(OutQueue::new(config.sample_rate));
    let stream = open_stream_with_fallback(&mut config, |config| {
        for_format!(
            supported.sample_format(),
            output_stream(&device, config, &queue, reports.stream_error(what), what)
        )
    })
    .with_context(|| format!("open {what} '{name}'"))?;
    Ok(OpenOutput {
        stream,
        queue,
        rate: config.sample_rate,
    })
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
    /// Exercise the real mixer and GUI lifecycle without opening audio devices.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            streams: Vec::new(),
            worker: None,
            stop: Arc::default(),
            params: Arc::new(ArcSwap::from_pointee(Params::default())),
            mixer: Arc::default(),
            stats: Arc::default(),
            reports: Arc::default(),
            has_monitor: false,
        }
    }

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
        let mut mic_stream_cfg = StreamConfig {
            channels: mic_cfg.channels(),
            sample_rate: mic_cfg.sample_rate(),
            buffer_size: stream_buffer(mic_cfg.sample_rate(), mic_cfg.buffer_size()),
        };
        let mic_q = Arc::new(ArrayQueue::<f32>::new(32_768)); // mono @ mic rate
        let waker = Arc::new(OnceLock::new());
        let input = open_stream_with_fallback(&mut mic_stream_cfg, |config| {
            for_format!(
                mic_cfg.sample_format(),
                input_stream(
                    &mic_dev,
                    config,
                    &mic_q,
                    &waker,
                    reports.stream_error("microphone")
                )
            )
        })
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
            let mon = monitor
                .as_ref()
                .map(|m| Output::new(Arc::clone(&m.queue), m.rate));
            thread::Builder::new()
                .name("openmic-dsp".into())
                .spawn(move || {
                    let _ = waker.set(thread::current());
                    // Fewer dropouts while a game or browser keeps the CPU busy.
                    // Windows joins the Pro Audio class here. Linux asks
                    // RealtimeKit from a helper so this thread can start at once.
                    #[cfg(not(target_os = "linux"))]
                    let _priority = match audio_thread_priority::promote_current_thread_to_real_time(FRAME as u32, SR) {
                        Ok(handle) => Some(handle),
                        Err(_) => {
                            reports.warn.lock().get_or_insert_with(|| {
                                "processing thread: real-time priority refused".into()
                            });
                            None
                        }
                    };
                    #[cfg(target_os = "linux")]
                    {
                        let reports = Arc::clone(&reports);
                        crate::rt::request_current_thread(SR, move |error| {
                            reports.warn.lock().get_or_insert_with(|| {
                                format!("processing thread: real-time priority refused: {error}")
                            });
                        });
                    }
                    if let Err(e) = process_loop(
                        &stop, &mic_q, mic_rate, out, mon, &stats, &mixer, &params, &reports.warn,
                    ) {
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

    pub fn toggle_sound_pause(&self, key: u64, overlap: bool) -> Option<bool> {
        self.mixer.lock().toggle_pause(key, overlap)
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

    /// Progress, paused state, and loaded durations from one mixer snapshot.
    pub fn clip_progress(&self) -> Vec<ClipProgress> {
        self.mixer.lock().progress()
    }

    /// Playing and paused clips at their playheads (route-switch handoff).
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

    /// Pop a fatal processing error, including an unexpectedly exited worker.
    pub fn take_error(&mut self) -> Option<String> {
        let exited = if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            // Join only a finished worker, so polling never waits for DSP.
            let worker = self.worker.take().unwrap();
            let _ = worker.join();
            !self.stop.load(Ordering::Relaxed)
        } else {
            false
        };
        // Read after joining so a detailed error written just before exit
        // takes precedence over the fallback message.
        self.reports.fatal.lock().take().or_else(|| {
            exited.then(|| "Audio processing stopped unexpectedly; click Start to retry".into())
        })
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
    warn: &Mutex<Option<String>>,
) -> Result<()> {
    let mut cleaner = Cleaner::new();
    let mut in_res = Resampler::new(mic_rate, SR);
    let mut scratch = [0.0f32; FRAME];
    // Mic samples one 48 kHz frame needs, plus the resampler's lookahead.
    // Taking no more keeps any backlog in `mic_q`, where it can be trimmed.
    let per_frame = FRAME * mic_rate as usize / SR as usize + 16;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let dropped = trim_input_backlog(mic_q, mic_rate);
        if dropped > 0 {
            let ms = dropped * 1000 / mic_rate as usize;
            warn.lock().get_or_insert_with(|| {
                format!("processing fell behind; skipped {ms} ms of mic audio to stay in sync")
            });
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
                    if in_res.backlog() >= per_frame {
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
        stats
            .model
            .store(cleaner.state(&p) as u32, Ordering::Relaxed);
        set_f32(&stats.out_peak, peak(&z));

        out.send(&z, &mut scratch);
        if let Some(mon) = mon.as_mut().filter(|_| p.monitor_on) {
            let gained = z.map(|s| (s * p.monitor_gain).clamp(-1.0, 1.0));
            mon.send(&gained, &mut scratch);
        }
    }
}

/// Drop the oldest mic samples once the backlog passes
/// [`MAX_INPUT_BACKLOG_MS`], down to [`INPUT_KEEP_MS`]. Returns how many.
fn trim_input_backlog(queue: &ArrayQueue<f32>, rate: u32) -> usize {
    let ms = |ms: usize| rate as usize * ms / 1000;
    if queue.len() <= ms(MAX_INPUT_BACKLOG_MS) {
        return 0;
    }
    let mut dropped = 0;
    while queue.len() > ms(INPUT_KEEP_MS) && queue.pop().is_some() {
        dropped += 1;
    }
    dropped
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

    #[test]
    fn invalid_device_samples_do_not_poison_later_microphone_audio() {
        let queue = Arc::new(ArrayQueue::new(1024));
        let mut callback = input_callback::<f32>(2, 48_000, Arc::clone(&queue), Arc::default());
        let info = cpal::InputCallbackInfo::new(cpal::InputStreamTimestamp {
            callback: cpal::StreamInstant::ZERO,
            capture: cpal::StreamInstant::ZERO,
        });
        callback(
            &[
                f32::NAN,
                0.5,
                f32::INFINITY,
                0.5,
                f32::NEG_INFINITY,
                -0.5,
                f32::MAX,
                f32::MAX,
                -f32::MAX,
                -f32::MAX,
                f32::MAX,
                -f32::MAX,
                0.5,
                0.25,
            ],
            &info,
        );
        let captured: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(captured, [0.25, 0.25, -0.25, 1.0, -1.0, 0.0, 0.375]);

        let mut resampler = Resampler::new(44_100, SR);
        let mut filter = crate::dsp::HighPass::new(crate::dsp::HIGHPASS_HZ);
        resampler.push(&captured);
        for _ in 0..3 {
            callback(&[0.25; FRAME * 2], &info);
            let captured: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
            assert_eq!(captured, [0.25; FRAME]);
            resampler.push(&captured);
            let mut frame = [0.0; FRAME];
            assert_eq!(resampler.pull(&mut frame), FRAME);
            filter.process(&mut frame);
            assert!(
                frame.iter().all(|s| s.is_finite()),
                "later audio must survive a bad device sample"
            );
        }
    }

    #[test]
    fn worker_exit_is_reported_once_without_hiding_processing_errors() {
        for outcome in ["panic", "return", "reported error", "shutdown"] {
            let mut engine = Engine::for_test();
            let reports = Arc::clone(&engine.reports);
            if outcome == "shutdown" {
                engine.stop.store(true, Ordering::Relaxed);
            }
            engine.worker = Some(thread::spawn(move || match outcome {
                "panic" => panic!("simulated DSP worker failure"),
                "reported error" => *reports.fatal.lock() = Some("processing frame failed".into()),
                _ => {}
            }));
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !engine.worker.as_ref().unwrap().is_finished() {
                assert!(std::time::Instant::now() < deadline, "worker did not exit");
                thread::sleep(Duration::from_millis(1));
            }
            let error = engine.take_error();
            match outcome {
                "reported error" => assert_eq!(error.as_deref(), Some("processing frame failed")),
                "shutdown" => assert!(error.is_none(), "a requested stop is not a failure"),
                _ => assert!(error.unwrap().contains("click Start to retry")),
            }
            assert!(engine.worker.is_none());
            assert!(engine.take_error().is_none(), "report the exit only once");
        }
    }

    #[test]
    fn polling_a_live_worker_does_not_wait_for_its_exit() {
        let mut engine = Engine::for_test();
        let (release, wait) = crossbeam_channel::bounded::<()>(0);
        engine.worker = Some(thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(3));
        }));
        assert!(engine.take_error().is_none());
        assert!(engine.worker.is_some(), "keep the active worker");
        drop(release);
    }

    #[test]
    fn a_short_mic_backlog_is_left_alone_and_a_long_one_trimmed() {
        let queue = ArrayQueue::new(32_768);
        for _ in 0..4_800 {
            queue.push(0.0).unwrap(); // 100 ms at 48 kHz: at the limit
        }
        assert_eq!(trim_input_backlog(&queue, 48_000), 0);
        queue.push(0.0).unwrap();
        assert_eq!(trim_input_backlog(&queue, 48_000), 4_801 - 960);
        assert_eq!(queue.len(), 960, "keeps about one callback (20 ms)");
    }

    /// After a half-second stall the loop must skip the stale audio rather
    /// than play it all back late.
    #[test]
    fn processing_catches_up_after_a_stall_instead_of_lagging() {
        let mic_q = Arc::new(ArrayQueue::new(32_768));
        for i in 0..24_000 {
            mic_q.push((i as f32 * 0.05).sin() * 0.1).unwrap(); // 500 ms at 48 kHz
        }
        let (stop, stats, warn) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(Stats::default()),
            Arc::new(Mutex::new(None)),
        );
        let worker = {
            let (mic_q, stop, stats, warn) = (
                Arc::clone(&mic_q),
                Arc::clone(&stop),
                Arc::clone(&stats),
                Arc::clone(&warn),
            );
            thread::spawn(move || {
                let out = Output::new(Arc::new(OutQueue::new(48_000)), 48_000);
                let (mixer, params) = (
                    Mutex::new(Mixer::default()),
                    ArcSwap::from_pointee(Params::default()),
                );
                process_loop(
                    &stop, &mic_q, 48_000, out, None, &stats, &mixer, &params, &warn,
                )
            })
        };
        let started = std::time::Instant::now();
        while !mic_q.is_empty() && started.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(5));
        }
        thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Relaxed);
        worker.thread().unpark();
        worker.join().unwrap().unwrap();

        let processed = stats.history.lock().len();
        assert!(
            processed <= 3,
            "processed {processed} frames of stale audio (at most ~20 ms expected)"
        );
        let warning = warn.lock().clone().expect("the skip is reported");
        assert!(warning.contains("skipped 480 ms"), "{warning}");
    }

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

    #[cfg(not(windows))]
    #[test]
    fn linux_stream_buffers_are_ten_ms_and_stay_inside_the_supported_range() {
        let range = cpal::SupportedBufferSize::Range { min: 64, max: 1000 };
        assert_eq!(stream_buffer(48_000, &range), cpal::BufferSize::Fixed(480));
        assert_eq!(stream_buffer(44_100, &range), cpal::BufferSize::Fixed(441));
        assert_eq!(
            stream_buffer(
                8_000,
                &cpal::SupportedBufferSize::Range {
                    min: 512,
                    max: 1024
                }
            ),
            cpal::BufferSize::Fixed(512),
            "a low rate is raised to the minimum"
        );
        assert_eq!(
            stream_buffer(
                48_000,
                &cpal::SupportedBufferSize::Range { min: 64, max: 256 }
            ),
            cpal::BufferSize::Fixed(256),
            "a small maximum caps the request"
        );
        assert_eq!(
            stream_buffer(48_000, &cpal::SupportedBufferSize::Unknown),
            cpal::BufferSize::Default,
            "a device without a range keeps the driver default"
        );
    }

    #[test]
    fn glitches_are_transient_but_a_lost_device_is_not() {
        assert!(is_transient(&ErrorKind::Xrun.into()));
        assert!(is_transient(&ErrorKind::RealtimeDenied.into()));
        assert!(!is_transient(&ErrorKind::DeviceNotAvailable.into()));
        assert!(!is_transient(&ErrorKind::StreamInvalidated.into()));
    }

    #[test]
    fn device_output_clips_resampling_overshoot_on_every_channel() {
        let queue = OutQueue::new(48_000);
        for sample in [1.25, -1.25, 0.25].into_iter().cycle().take(queue.target()) {
            queue.push(sample);
        }
        let mut output = [0.0f32; 6];
        queue.fill(&mut output, 2);
        assert_eq!(output, [1.0, 1.0, -1.0, -1.0, 0.25, 0.25]);
    }

    #[test]
    fn large_device_callbacks_can_prime_within_the_queue_capacity() {
        for extra in [0, 128] {
            let queue = OutQueue::new(48_000);
            let capacity = queue.samples.capacity();
            let frames = if extra == 0 {
                capacity / 2 + 1
            } else {
                capacity + extra
            };
            let mut output = vec![1.0f32; frames * 2];
            for _ in 0..capacity {
                queue.push(0.25);
            }
            queue.fill(&mut output, 2);

            assert_eq!(
                queue.target(),
                capacity,
                "the priming target must be reachable"
            );
            let played = frames.min(capacity);
            assert!(
                output[..played * 2].iter().all(|sample| *sample == 0.25),
                "a full queue must play, even when two device periods would not fit"
            );
            assert!(
                output[played * 2..].iter().all(|sample| *sample == 0.0),
                "pad an oversized callback with silence"
            );
            assert_eq!(queue.samples.len(), capacity - played);
            assert_eq!(queue.primed.load(Ordering::Relaxed), frames <= capacity);
        }
    }

    #[test]
    fn priming_waits_for_the_target_then_plays() {
        let queue = OutQueue::new(48_000);
        let mut data = [1.0f32; 480];
        for _ in 0..600 {
            queue.push(0.5);
        }
        queue.fill(&mut data, 1);
        assert!(
            data.iter().all(|&s| s == 0.0),
            "600 < 1200-sample target: still priming"
        );
        for _ in 0..600 {
            queue.push(0.5);
        }
        queue.fill(&mut data, 2); // stereo: 240 frames
        assert!(data.iter().all(|&s| s == 0.5));
        assert_eq!(queue.samples.len(), 1200 - 240);
    }
}
