//! Speech to text: transcribe a take from the microphone with Whisper
//! (candle, pure Rust, on the CPU) and type the words into whichever app has
//! the keyboard focus. Models are downloaded once, on request, and then run
//! entirely on this PC.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self as m, Config, model::Whisper};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport};

use crate::config;
use crate::dsp;
use crate::record::Take;

/// A Whisper checkpoint from Hugging Face, pinned to a revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SpeechModel {
    TinyEn,
    #[default]
    BaseEn,
    SmallEn,
    Base,
    Small,
}

impl SpeechModel {
    pub const ALL: [SpeechModel; 5] = [Self::TinyEn, Self::BaseEn, Self::SmallEn, Self::Base, Self::Small];

    fn repo(self) -> (&'static str, &'static str) {
        match self {
            Self::TinyEn => ("whisper-tiny.en", "87c7102498dcde7456f24cfd30239ca606ed9063"),
            Self::BaseEn => ("whisper-base.en", "911407f4214e0e1d82085af863093ec0b66f9cd6"),
            Self::SmallEn => ("whisper-small.en", "e8727524f962ee844a7319d92be39ac1bd25655a"),
            Self::Base => ("whisper-base", "e37978b90ca9030d5170a5c07aadb050351a65bb"),
            Self::Small => ("whisper-small", "973afd24965f72e36ca33b3055d56a652f456b4d"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TinyEn => "Tiny · English · 151 MB",
            Self::BaseEn => "Base · English · 290 MB",
            Self::SmallEn => "Small · English · 967 MB",
            Self::Base => "Base · any language · 290 MB",
            Self::Small => "Small · any language · 967 MB",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::TinyEn => "Fastest; fine for short, clear phrases",
            Self::BaseEn => "Good accuracy; about a second per phrase",
            Self::SmallEn => "Most accurate; several seconds per phrase",
            Self::Base | Self::Small => "Detects the language you speak",
        }
    }

    fn multilingual(self) -> bool {
        matches!(self, Self::Base | Self::Small)
    }

    fn dir(self) -> PathBuf {
        config::settings_path()
            .parent()
            .map(Path::to_owned)
            .unwrap_or_default()
            .join("models")
            .join(self.repo().0)
    }

    /// All of the model's files are on disk.
    pub fn is_downloaded(self) -> bool {
        MODEL_FILES.iter().all(|name| self.dir().join(name).is_file())
    }

    /// Delete the downloaded files.
    pub fn remove(self) -> Result<()> {
        let dir = self.dir();
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        }
        Ok(())
    }
}

const MODEL_FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// Whisper listens at 16 kHz; takes are decoded at the 48 kHz DSP rate.
const DECIMATION: usize = 3;
const _: () = assert!(dsp::SR as usize == m::SAMPLE_RATE * DECIMATION);

/// Shorter takes are a stray key tap, not speech.
const MIN_SECONDS: f32 = 0.3;

/// What the background threads report back to the window.
#[derive(Debug)]
pub enum Event {
    Downloaded(SpeechModel),
    DownloadFailed(String),
    /// Keep recognized words available to copy even if Windows refuses typing.
    Transcribed { text: String, typing_error: Option<String> },
    /// The take held no speech.
    Nothing,
    Failed(String),
}

struct Job {
    model: SpeechModel,
    take: Take,
}

/// Download and transcription threads, and their progress.
pub struct Dictation {
    jobs: Option<Sender<Job>>,
    events: (Sender<Event>, Receiver<Event>),
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Takes queued or being transcribed.
    pending: Arc<AtomicU64>,
    download: Option<Download>,
}

struct Download {
    model: SpeechModel,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Dictation {
    /// `wake` is called whenever an [`Event`] is ready.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            jobs: None,
            events: crossbeam_channel::unbounded(),
            wake: Arc::new(wake),
            pending: Arc::default(),
            download: None,
        }
    }

    pub fn poll(&mut self) -> Option<Event> {
        let event = self.events.1.try_recv().ok()?;
        if matches!(event, Event::Downloaded(_) | Event::DownloadFailed(_)) {
            self.download = None;
        }
        Some(event)
    }

    /// A take is being transcribed.
    pub fn busy(&self) -> bool {
        self.pending.load(Ordering::Relaxed) > 0
    }

    /// The model being downloaded, with (bytes done, bytes expected).
    pub fn download_progress(&self) -> Option<(SpeechModel, u64, u64)> {
        let d = self.download.as_ref()?;
        Some((d.model, d.done.load(Ordering::Relaxed), d.total.load(Ordering::Relaxed)))
    }

    pub fn start_download(&mut self, model: SpeechModel) {
        if self.download.is_some() {
            return;
        }
        let download = Download {
            model,
            done: Arc::default(),
            total: Arc::default(),
            cancel: Arc::default(),
        };
        let (done, total, cancel) =
            (Arc::clone(&download.done), Arc::clone(&download.total), Arc::clone(&download.cancel));
        let (events, wake) = (self.events.0.clone(), Arc::clone(&self.wake));
        let spawned = thread::Builder::new().name("openmic-model-download".into()).spawn(move || {
            let event = match download_model(model, &done, &total, &cancel) {
                Ok(()) => Event::Downloaded(model),
                Err(e) => Event::DownloadFailed(format!("{e:#}")),
            };
            let _ = events.send(event);
            wake();
        });
        if spawned.is_ok() {
            self.download = Some(download);
        }
    }

    pub fn cancel_download(&self) {
        if let Some(d) = &self.download {
            d.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Transcribe `take` and type the words into the focused app.
    pub fn transcribe(&mut self, model: SpeechModel, take: Take) {
        let jobs = self.jobs.get_or_insert_with(|| {
            let (jobs, queue) = crossbeam_channel::unbounded();
            let (events, wake, pending) =
                (self.events.0.clone(), Arc::clone(&self.wake), Arc::clone(&self.pending));
            let _ = thread::Builder::new()
                .name("openmic-dictation".into())
                .spawn(move || transcribe_loop(&queue, &events, &*wake, &pending));
            jobs
        });
        self.pending.fetch_add(1, Ordering::Relaxed);
        if jobs.send(Job { model, take }).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            let _ = self.events.0.send(Event::Failed("speech-to-text stopped".into()));
        }
    }
}

/// Worker thread: keep the last model loaded, since loading it takes longer
/// than transcribing a phrase.
fn transcribe_loop(queue: &Receiver<Job>, events: &Sender<Event>, wake: &dyn Fn(), pending: &AtomicU64) {
    let mut loaded: Option<(SpeechModel, Transcriber)> = None;
    for Job { model, take } in queue.iter() {
        let result = (|| -> Result<String> {
            let samples = take.samples()?;
            drop(take); // deletes the temporary file
            let audio = decimate(&samples);
            if (audio.len() as f32) < MIN_SECONDS * m::SAMPLE_RATE as f32 {
                return Ok(String::new());
            }
            if loaded.as_ref().is_none_or(|(which, _)| *which != model) {
                loaded = None; // free the old model before loading the new one
                loaded = Some((model, Transcriber::load(model)?));
            }
            let (_, transcriber) = loaded.as_mut().context("model not loaded")?;
            transcriber.transcribe(&audio)
        })();
        let event = transcription_event(result, type_text);
        pending.fetch_sub(1, Ordering::Relaxed);
        let _ = events.send(event);
        wake();
    }
}

fn transcription_event(result: Result<String>, type_text: impl FnOnce(&str) -> Result<()>) -> Event {
    match result {
        Ok(text) if text.is_empty() => Event::Nothing,
        Ok(text) => {
            let typing_error = type_text(&text).err().map(|e| format!("{e:#}"));
            Event::Transcribed { text, typing_error }
        }
        Err(e) => Event::Failed(format!("{e:#}")),
    }
}

// ---- download ---------------------------------------------------------------

/// ureq's body timeout covers the whole download. Cap each I/O instead so
/// large models can keep downloading, but a stalled read cannot block Cancel
/// indefinitely. This uses ureq's public, unversioned transport extension API.
#[derive(Debug)]
struct DownloadConnector(Duration);

impl Connector for DownloadConnector {
    type Out = DownloadTransport;

    fn connect(&self, details: &ConnectionDetails, chained: Option<()>) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(DefaultConnector::default()
            .connect(details, chained)?
            .map(|inner| DownloadTransport { inner, idle_timeout: self.0 }))
    }
}

#[derive(Debug)]
struct DownloadTransport {
    inner: Box<dyn Transport>,
    idle_timeout: Duration,
}

impl DownloadTransport {
    fn timeout(&self, mut timeout: NextTimeout) -> NextTimeout {
        let idle = ureq::unversioned::transport::time::Duration::Exact(self.idle_timeout);
        if timeout.after > idle {
            timeout.after = idle;
        }
        timeout
    }
}

impl Transport for DownloadTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, self.timeout(timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.inner.await_input(self.timeout(timeout))
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

fn download_agent(idle_timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_resolve(Some(idle_timeout))
        .timeout_connect(Some(idle_timeout))
        .timeout_send_request(Some(idle_timeout))
        .timeout_recv_response(Some(idle_timeout))
        .build();
    ureq::Agent::with_parts(config, DownloadConnector(idle_timeout), ureq::unversioned::resolver::DefaultResolver::default())
}

fn check_download_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("download cancelled");
    }
    Ok(())
}

fn read_download_chunk(reader: &mut impl Read, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize> {
    check_download_cancelled(cancel)?;
    let read = reader.read(buf);
    // Prefer the requested cancellation over the I/O timeout that woke us.
    check_download_cancelled(cancel)?;
    Ok(read?)
}

fn download_model(model: SpeechModel, done: &AtomicU64, total: &AtomicU64, cancel: &AtomicBool) -> Result<()> {
    check_download_cancelled(cancel)?;
    let dir = model.dir();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let (repo, revision) = model.repo();
    let agent = download_agent(Duration::from_secs(20));
    // Weights last: the small files make the total accurate early on.
    for name in MODEL_FILES {
        check_download_cancelled(cancel)?;
        let path = dir.join(name);
        if path.is_file() {
            continue;
        }
        let url = format!("https://huggingface.co/openai/{repo}/resolve/{revision}/{name}");
        let response = agent.get(&url).call();
        check_download_cancelled(cancel)?;
        let response = response.with_context(|| format!("download {name}"))?;
        let body = response.into_body();
        let expected = body.content_length();
        if let Some(len) = expected {
            total.fetch_add(len, Ordering::Relaxed);
        }
        let partial = dir.join(format!("{name}.part"));
        let result = (|| -> Result<()> {
            let mut out = File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
            let mut reader = body.into_reader();
            let mut buf = vec![0u8; 1 << 16];
            let mut got = 0u64;
            loop {
                let n = read_download_chunk(&mut reader, &mut buf, cancel)
                    .with_context(|| format!("download {name}"))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n]).context("save model")?;
                got += n as u64;
                done.fetch_add(n as u64, Ordering::Relaxed);
            }
            out.flush().context("save model")?;
            if expected.is_some_and(|len| len != got) {
                bail!("{name} download was cut short");
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&partial);
            return Err(e);
        }
        fs::rename(&partial, &path).with_context(|| format!("save {}", path.display()))?;
    }
    Ok(())
}

// ---- transcription ----------------------------------------------------------

struct Transcriber {
    model: Whisper,
    tokens: Tokens,
    mel_filters: Vec<f32>,
    /// Logit offsets: -inf for tokens Whisper must never produce here.
    suppress: Vec<f32>,
    multilingual: bool,
}

impl Transcriber {
    fn load(which: SpeechModel) -> Result<Self> {
        let dir = which.dir();
        let config: Config = serde_json::from_str(
            &fs::read_to_string(dir.join("config.json")).context("read the speech model's config")?,
        )
        .context("read the speech model's config")?;
        let tokens = Tokens::load(&dir.join("tokenizer.json"))?;
        let device = Device::Cpu;
        // SAFETY: the weights file is ours and isn't modified while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], m::DTYPE, &device)
        }
        .context("load the speech model")?;
        let model = Whisper::load(&vb, config.clone()).context("load the speech model")?;

        // Only plain text: no timestamps, languages or task tokens.
        let suppress = (0..config.vocab_size as u32)
            .map(|id| {
                if config.suppress_tokens.contains(&id) || (id > tokens.eot && tokens.special.contains(&id)) {
                    f32::NEG_INFINITY
                } else {
                    0.0
                }
            })
            .collect();
        Ok(Self {
            mel_filters: mel_filters(config.num_mel_bins),
            model,
            tokens,
            suppress,
            multilingual: which.multilingual(),
        })
    }

    /// 16 kHz mono samples to text, 30 seconds at a time.
    fn transcribe(&mut self, audio: &[f32]) -> Result<String> {
        let mut text = String::new();
        for chunk in audio.chunks(m::N_SAMPLES) {
            let words = self.transcribe_chunk(chunk)?;
            if !words.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&words);
            }
        }
        Ok(text)
    }

    fn transcribe_chunk(&mut self, audio: &[f32]) -> Result<String> {
        let n_mels = self.model.config.num_mel_bins;
        // Whisper's default budget per 30 s window.
        let max_tokens = self.model.config.max_target_positions / 2;
        let mel = m::audio::pcm_to_mel(&self.model.config, audio, &self.mel_filters);
        let frames = mel.len() / n_mels;
        let mel = Tensor::from_vec(mel, (1, n_mels, frames), &Device::Cpu)?
            .narrow(2, 0, m::N_FRAMES.min(frames))?;
        let features = self.model.encoder.forward(&mel, true)?;

        let Tokens { sot, eot, transcribe, no_timestamps, no_speech, space, .. } = self.tokens;
        let mut tokens = vec![sot];
        if self.multilingual {
            let language = self.detect_language(&features)?;
            tokens.extend([language, transcribe]);
        }
        tokens.push(no_timestamps);
        let prompt = tokens.len();

        for step in 0..max_tokens {
            let input = Tensor::new(tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
            let ys = self.model.decoder.forward(&input, &features, step == 0)?;
            if step == 0 {
                // How sure the model is, before any words, that there's no speech.
                let first = self.logits(&ys, 0)?;
                if softmax_at(&first, no_speech) > m::NO_SPEECH_THRESHOLD as f32 {
                    return Ok(String::new());
                }
            }
            let mut logits = self.logits(&ys, tokens.len() - 1)?;
            for (logit, offset) in logits.iter_mut().zip(&self.suppress) {
                *logit += offset;
            }
            if step == 0 {
                // Don't start with a bare space or end before a word.
                for id in [eot, space] {
                    if let Some(logit) = logits.get_mut(id as usize) {
                        *logit = f32::NEG_INFINITY;
                    }
                }
            }
            let next = argmax(&logits);
            if next == eot {
                break;
            }
            tokens.push(next);
        }
        Ok(self.tokens.decode(&tokens[prompt..]).trim().to_owned())
    }

    fn logits(&self, ys: &Tensor, position: usize) -> Result<Vec<f32>> {
        let logits = self.model.decoder.final_linear(&ys.i((..1, position..position + 1))?)?;
        Ok(logits.i(0)?.i(0)?.to_dtype(DType::F32)?.to_vec1()?)
    }

    fn detect_language(&mut self, features: &Tensor) -> Result<u32> {
        let input = Tensor::new(&[self.tokens.sot], &Device::Cpu)?.unsqueeze(0)?;
        let ys = self.model.decoder.forward(&input, features, true)?;
        let logits = self.logits(&ys, 0)?;
        self.tokens
            .languages
            .iter()
            .copied()
            .max_by(|&a, &b| logits[a as usize].total_cmp(&logits[b as usize]))
            .context("the speech model has no languages")
    }
}

fn argmax(values: &[f32]) -> u32 {
    values
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i as u32)
}

fn softmax_at(logits: &[f32], index: u32) -> f32 {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|l| (l - max).exp()).sum();
    logits.get(index as usize).map_or(0.0, |l| (l - max).exp() / sum)
}

/// Whisper's byte-level BPE vocabulary: enough to decode its output.
struct Tokens {
    text: HashMap<u32, String>,
    special: HashSet<u32>,
    languages: Vec<u32>,
    sot: u32,
    eot: u32,
    transcribe: u32,
    no_timestamps: u32,
    no_speech: u32,
    space: u32,
}

impl Tokens {
    fn load(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct File {
            added_tokens: Vec<Added>,
            model: Model,
        }
        #[derive(Deserialize)]
        struct Added {
            id: u32,
            content: String,
        }
        #[derive(Deserialize)]
        struct Model {
            vocab: HashMap<String, u32>,
        }
        let file: File = serde_json::from_str(&fs::read_to_string(path).context("read the speech tokenizer")?)
            .context("read the speech tokenizer")?;
        let mut text: HashMap<u32, String> = file.model.vocab.into_iter().map(|(t, id)| (id, t)).collect();
        let mut special = HashSet::new();
        let mut named = HashMap::new();
        let mut languages = Vec::new();
        for token in file.added_tokens {
            special.insert(token.id);
            let inner = token.content.strip_prefix("<|").and_then(|s| s.strip_suffix("|>"));
            if inner.is_some_and(|s| (2..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase())) {
                languages.push(token.id);
            }
            named.insert(token.content.clone(), token.id);
            text.insert(token.id, token.content);
        }
        let id = |name: &str| named.get(name).copied().with_context(|| format!("the speech tokenizer has no {name}"));
        Ok(Self {
            sot: id(m::SOT_TOKEN)?,
            eot: id(m::EOT_TOKEN)?,
            transcribe: id(m::TRANSCRIBE_TOKEN)?,
            no_timestamps: id(m::NO_TIMESTAMPS_TOKEN)?,
            no_speech: m::NO_SPEECH_TOKENS
                .iter()
                .find_map(|name| named.get(*name).copied())
                .context("the speech tokenizer has no no-speech token")?,
            space: text.iter().find(|(_, t)| *t == "Ġ").map_or(220, |(id, _)| *id),
            text,
            special,
            languages,
        })
    }

    fn decode(&self, ids: &[u32]) -> String {
        let bytes: Vec<u8> = ids
            .iter()
            .filter(|id| !self.special.contains(id))
            .filter_map(|id| self.text.get(id))
            .flat_map(|token| token.chars())
            .filter_map(unicode_to_byte)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// GPT-2's byte-level BPE writes each byte as a printable character:
/// printable Latin-1 stands for itself, the rest are shifted up past 255.
fn unicode_to_byte(ch: char) -> Option<u8> {
    let printable = |b: u32| matches!(b, 0x21..=0x7e | 0xa1..=0xac | 0xae..=0xff);
    let c = ch as u32;
    if printable(c) {
        return Some(c as u8);
    }
    let shifted = c.checked_sub(256)?;
    (0..=255u32).filter(|&b| !printable(b)).nth(shifted as usize).map(|b| b as u8)
}

/// Whisper's mel filterbank (librosa's Slaney scale and normalisation),
/// `n_mels` rows of `N_FFT / 2 + 1` weights.
fn mel_filters(n_mels: usize) -> Vec<f32> {
    let sr = m::SAMPLE_RATE as f64;
    let bins = m::N_FFT / 2 + 1;
    let hz_to_mel = |hz: f64| {
        if hz < 1000.0 {
            hz * 3.0 / 200.0
        } else {
            15.0 + (hz / 1000.0).ln() / (6.4f64.ln() / 27.0)
        }
    };
    let mel_to_hz = |mel: f64| {
        if mel < 15.0 {
            mel * 200.0 / 3.0
        } else {
            1000.0 * ((mel - 15.0) * 6.4f64.ln() / 27.0).exp()
        }
    };
    let top = hz_to_mel(sr / 2.0);
    let edges: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(top * i as f64 / (n_mels + 1) as f64))
        .collect();
    let mut filters = vec![0.0f32; n_mels * bins];
    for i in 0..n_mels {
        let (lo, mid, hi) = (edges[i], edges[i + 1], edges[i + 2]);
        let norm = 2.0 / (hi - lo);
        for k in 0..bins {
            let hz = sr / 2.0 * k as f64 / (bins - 1) as f64;
            let weight = ((hz - lo) / (mid - lo)).min((hi - hz) / (hi - mid)).max(0.0);
            filters[i * bins + k] = (weight * norm) as f32;
        }
    }
    filters
}

/// 48 kHz to 16 kHz: a windowed-sinc low-pass at 7.2 kHz, then every
/// third sample.
fn decimate(samples: &[f32]) -> Vec<f32> {
    const TAPS: usize = 63;
    let cutoff = 7_200.0 / dsp::SR as f64;
    let half = (TAPS / 2) as isize;
    let mut kernel: Vec<f32> = (0..TAPS)
        .map(|i| {
            let n = i as f64 - half as f64;
            let sinc = if n == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * n).sin() / (std::f64::consts::PI * n)
            };
            let blackman = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (TAPS - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (TAPS - 1) as f64).cos();
            (sinc * blackman) as f32
        })
        .collect();
    let gain: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= gain);
    (0..samples.len())
        .step_by(DECIMATION)
        .map(|center| {
            kernel
                .iter()
                .enumerate()
                .filter_map(|(i, k)| {
                    let at = center as isize + i as isize - half;
                    usize::try_from(at).ok().and_then(|at| samples.get(at)).map(|s| s * k)
                })
                .sum()
        })
        .collect()
}

// ---- typing -----------------------------------------------------------------

/// Type `text` into the focused window, as if from the keyboard.
#[cfg(windows)]
fn type_text(text: &str) -> Result<()> {
    use std::time::Instant;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    // Characters typed while the shortcut's Ctrl or Alt is still held
    // would arrive as more shortcuts.
    let started = Instant::now();
    while [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
        .iter()
        .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0)
    {
        if started.elapsed() > Duration::from_secs(3) {
            bail!("Release Ctrl, Alt and Shift so the text can be typed");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let key = |unit: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let text = text.replace(['\r', '\n'], " ");
    let inputs: Vec<INPUT> = text.encode_utf16().flat_map(|unit| [key(unit, false), key(unit, true)]).collect();
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() {
        bail!("Windows blocked the typing (is the app running as administrator?)");
    }
    Ok(())
}

#[cfg(not(windows))]
fn type_text(_text: &str) -> Result<()> {
    bail!("typing text is available on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_typing_keeps_the_recognized_words() {
        let event = transcription_event(Ok("Keep my words.".into()), |text| {
            assert_eq!(text, "Keep my words.");
            bail!("Windows blocked typing")
        });
        match event {
            Event::Transcribed { text, typing_error } => {
                assert_eq!(text, "Keep my words.");
                assert_eq!(typing_error.as_deref(), Some("Windows blocked typing"));
            }
            other => panic!("transcript was lost: {other:?}"),
        }
    }

    #[test]
    fn successful_typing_keeps_the_transcript_without_an_error() {
        let event = transcription_event(Ok("Hello".into()), |_| Ok(()));
        assert!(matches!(event, Event::Transcribed { text, typing_error: None } if text == "Hello"));
    }

    #[test]
    fn silence_and_transcription_errors_never_type() {
        assert!(matches!(
            transcription_event(Ok(String::new()), |_| panic!("silence must not type")),
            Event::Nothing
        ));
        let event = transcription_event(Err(anyhow::anyhow!("model unavailable")), |_| {
            panic!("failed transcription must not type")
        });
        assert!(matches!(event, Event::Failed(error) if error == "model unavailable"));
    }

    /// A real loopback HTTP response whose body stalls after one byte.
    fn stalled_response() -> (String, std::sync::mpsc::Sender<()>, thread::JoinHandle<()>) {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let (stop, stopped) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na").unwrap();
            let _ = stopped.recv_timeout(Duration::from_secs(3));
        });
        (url, stop, server)
    }

    #[test]
    fn stalled_download_body_times_out() {
        let (url, stop, server) = stalled_response();
        let agent = download_agent(Duration::from_millis(250));
        let mut reader = agent.get(&url).call().unwrap().into_body().into_reader();
        let mut buf = [0; 1];
        reader.read_exact(&mut buf).unwrap();
        let started = std::time::Instant::now();
        let result = read_download_chunk(&mut reader, &mut buf, &AtomicBool::new(false));
        let _ = stop.send(());
        server.join().unwrap();
        assert!(result.is_err(), "a stalled body must not wait indefinitely");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancellation_during_a_stalled_read_is_reported_after_timeout() {
        let (url, stop, server) = stalled_response();
        let agent = download_agent(Duration::from_millis(250));
        let mut reader = agent.get(&url).call().unwrap().into_body().into_reader();
        let mut buf = [0; 1];
        reader.read_exact(&mut buf).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let result = read_download_chunk(&mut reader, &mut buf, &cancel);
        let _ = stop.send(());
        canceller.join().unwrap();
        server.join().unwrap();
        assert_eq!(result.unwrap_err().to_string(), "download cancelled");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn mel_filters_match_whisper() {
        // Reference values from Whisper's own mel_filters.npz (80 bins).
        let filters = mel_filters(80);
        let at = |row: usize, bin: usize| filters[row * 201 + bin];
        assert!((at(0, 1) - 0.024_862_595).abs() < 1e-6);
        assert!((at(1, 2) - 0.022_871_772).abs() < 1e-6);
        assert!((at(40, 43) - 0.014_735_566).abs() < 1e-6);
        assert!((at(79, 189) - 0.001_765_727).abs() < 1e-6);
        let total: f32 = filters.iter().sum();
        assert!((total - 1.999_024).abs() < 1e-3);
    }

    #[test]
    fn byte_level_characters_decode_to_bytes() {
        assert_eq!(unicode_to_byte('A'), Some(b'A'));
        assert_eq!(unicode_to_byte('Ġ'), Some(b' '), "GPT-2 writes a space as Ġ");
        assert_eq!(unicode_to_byte('Ċ'), Some(b'\n'));
        assert_eq!(unicode_to_byte('\u{0100}'), Some(0));
    }

    /// Downloads the tiny model (151 MB) into a temporary folder:
    /// `OPENMIC_SPEECH_WAV=speech.wav cargo test --release -- --ignored --nocapture transcribes`
    #[test]
    #[ignore]
    fn transcribes_a_recording() {
        let wav = std::env::var("OPENMIC_SPEECH_WAV").expect("set OPENMIC_SPEECH_WAV to a speech recording");
        let model = SpeechModel::TinyEn;
        let (done, total, cancel) = (AtomicU64::new(0), AtomicU64::new(0), AtomicBool::new(false));
        download_model(model, &done, &total, &cancel).unwrap();
        assert!(model.is_downloaded());
        assert_eq!(done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));

        let audio = decimate(&crate::decode::load_clip(Path::new(&wav)).unwrap());
        let mut transcriber = Transcriber::load(model).unwrap();
        let started = std::time::Instant::now();
        let text = transcriber.transcribe(&audio).unwrap();
        println!("{:.2?}: {text}", started.elapsed());
        assert!(!text.is_empty());
        assert_eq!(transcriber.transcribe(&vec![0.0; 16_000]).unwrap(), "", "silence types nothing");
    }

    #[test]
    fn decimation_keeps_speech_and_drops_what_whisper_cannot_hear() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..48_000).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin()).collect()
        };
        let rms = |s: &[f32]| (s[100..s.len() - 100].iter().map(|x| x * x).sum::<f32>() / (s.len() - 200) as f32).sqrt();
        let voice = decimate(&tone(1_000.0));
        assert_eq!(voice.len(), 16_000);
        assert!((rms(&voice) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!(rms(&decimate(&tone(12_000.0))) < 0.01, "would alias to 4 kHz");
    }
}
