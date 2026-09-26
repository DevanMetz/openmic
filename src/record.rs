//! Capture a microphone or a Windows playback device straight to a WAV file
//! on disk, so a take can run as long as the disk allows, then keep it.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, Stream};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;

use crate::config;

/// Peak blocks per second of audio (10 ms each), for drawing waveforms.
pub const PEAKS_PER_SECOND: usize = 100;

/// Largest WAV data chunk: the format stores sizes in 32 bits (about 6
/// hours of 48 kHz stereo at 16 bits).
const MAX_DATA_BYTES: u64 = u32::MAX as u64 - 36;

/// Seconds of audio the capture queue holds while the writer catches up.
const QUEUE_SECONDS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Microphone,
    Computer,
}

/// A recording in progress. Audio goes from the device callback through a
/// lock-free queue to a writer thread that appends it to a WAV file.
pub struct Recorder {
    stream: Option<Stream>,
    writer: Option<JoinHandle<Result<u64>>>,
    /// The file being written; removed if the recorder is dropped unfinished.
    path: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    peaks: Arc<Mutex<PeakTracker>>,
    bytes: Arc<AtomicU64>,
    sample_rate: u32,
    channels: u16,
    started: Instant,
    full: Arc<AtomicBool>,
    /// The writer fell more than [`QUEUE_SECONDS`] behind and audio was lost.
    skipped: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

/// A finished recording, waiting in a temporary file until it is saved,
/// added to the soundboard, or discarded (dropped).
pub struct Take {
    path: PathBuf,
    sample_rate: u32,
    frames: u64,
    /// Peak level per 10 ms block, for the waveform.
    peaks: Vec<f32>,
    /// Capture failed, but the audio that reached disk could be kept.
    warning: Option<String>,
}

/// A running peak per 10 ms block, so the window can draw a take of any
/// length without reading it back.
struct PeakTracker {
    peaks: Vec<f32>,
    block_peak: f32,
    block_filled: usize,
    /// Interleaved samples per block.
    block_len: usize,
}

impl PeakTracker {
    fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            peaks: Vec::new(),
            block_peak: 0.0,
            block_filled: 0,
            block_len: (sample_rate as usize * channels as usize / PEAKS_PER_SECOND).max(1),
        }
    }

    fn push(&mut self, samples: &[f32]) {
        for sample in samples {
            self.block_peak = self.block_peak.max(sample.abs());
            self.block_filled += 1;
            if self.block_filled == self.block_len {
                self.peaks.push(self.block_peak);
                self.block_peak = 0.0;
                self.block_filled = 0;
            }
        }
    }

    /// The last `count` blocks (all of them for `usize::MAX`), including
    /// the one still filling.
    fn recent(&self, count: usize) -> Vec<f32> {
        let partial = (self.block_filled > 0).then_some(self.block_peak);
        let total = self.peaks.len() + usize::from(partial.is_some());
        let skip = total.saturating_sub(count).min(self.peaks.len());
        self.peaks[skip..].iter().copied().chain(partial).collect()
    }
}

impl Recorder {
    /// An empty `device_name` means the Windows default device for that source.
    pub fn start(source: Source, device_name: &str) -> Result<Self> {
        #[cfg(not(windows))]
        if source == Source::Computer {
            bail!("computer audio recording is available on Windows");
        }

        let host = cpal::default_host();
        let device = match (source, device_name.is_empty()) {
            (Source::Microphone, true) => host
                .default_input_device()
                .context("no default microphone")?,
            (Source::Computer, true) => host
                .default_output_device()
                .context("no default playback device")?,
            (Source::Microphone, false) => host
                .input_devices()
                .context("list microphones")?
                .find(|device| device.to_string() == device_name)
                .with_context(|| format!("microphone '{device_name}' is not connected"))?,
            (Source::Computer, false) => host
                .output_devices()
                .context("list playback devices")?
                .find(|device| device.to_string() == device_name)
                .with_context(|| format!("playback device '{device_name}' is not connected"))?,
        };
        // CPAL's Windows backend enables WASAPI loopback when an output device
        // is opened as an input stream. Use its output mix format for that case.
        let supported = match source {
            Source::Microphone => device.default_input_config(),
            Source::Computer => device.default_output_config(),
        }
        .context("read recording device format")?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();
        let config = cpal::StreamConfig {
            channels,
            sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let path = take_path();
        let wav = WavWriter::create(&path, sample_rate, channels)?;
        let queue = Arc::new(ArrayQueue::<f32>::new(
            (sample_rate as usize * channels as usize * QUEUE_SECONDS).max(1),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let peaks = Arc::new(Mutex::new(PeakTracker::new(sample_rate, channels)));
        let bytes = Arc::new(AtomicU64::new(0));
        let full = Arc::new(AtomicBool::new(false));
        let skipped = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));

        let writer = {
            let (queue, stop, peaks, bytes, full, error) = (
                Arc::clone(&queue),
                Arc::clone(&stop),
                Arc::clone(&peaks),
                Arc::clone(&bytes),
                Arc::clone(&full),
                Arc::clone(&error),
            );
            thread::Builder::new()
                .name("openmic-recorder".into())
                .spawn(move || write_loop(wav, &queue, &stop, &peaks, &bytes, &full, &error))
                .context("start recording writer")?
        };

        macro_rules! build_stream {
            ($sample:ty) => {{
                let queue = Arc::clone(&queue);
                let skipped = Arc::clone(&skipped);
                let error = Arc::clone(&error);
                device.build_input_stream(
                    config,
                    move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                        // Whole callbacks only, so channels never shift.
                        if queue.capacity() - queue.len() < data.len() {
                            skipped.store(true, Ordering::Relaxed);
                            return;
                        }
                        for &sample in data {
                            let _ = queue.push(sample.to_sample::<f32>());
                        }
                    },
                    // Glitches (loopback reports one whenever playback
                    // starts or pauses) don't end the take; losing the device does.
                    move |err: cpal::Error| {
                        if !crate::engine::is_transient(&err) {
                            *error.lock() = Some(err.to_string());
                        }
                    },
                    None,
                )
            }};
        }
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I8 => build_stream!(i8),
            cpal::SampleFormat::U8 => build_stream!(u8),
            cpal::SampleFormat::F32 => build_stream!(f32),
            cpal::SampleFormat::F64 => build_stream!(f64),
            cpal::SampleFormat::I16 => build_stream!(i16),
            cpal::SampleFormat::U16 => build_stream!(u16),
            cpal::SampleFormat::I24 => build_stream!(cpal::I24),
            cpal::SampleFormat::U24 => build_stream!(cpal::U24),
            cpal::SampleFormat::I32 => build_stream!(i32),
            cpal::SampleFormat::U32 => build_stream!(u32),
            cpal::SampleFormat::I64 => build_stream!(i64),
            cpal::SampleFormat::U64 => build_stream!(u64),
            other => Err(cpal::Error::with_message(
                cpal::ErrorKind::UnsupportedConfig,
                format!("unsupported recording format {other}"),
            )),
        };
        let mut recorder = Self {
            stream: None,
            writer: Some(writer),
            path: Some(path),
            stop,
            peaks,
            bytes,
            sample_rate,
            channels,
            started: Instant::now(),
            full,
            skipped,
            error,
        };
        // On failure, dropping `recorder` stops the writer and deletes the file.
        let stream = stream.context("open recording stream")?;
        stream.play().context("start recording stream")?;
        recorder.stream = Some(stream);
        recorder.started = Instant::now();
        Ok(recorder)
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Bytes of audio written to disk so far.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// The take hit the WAV size limit and stopped growing.
    pub fn is_full(&self) -> bool {
        self.full.load(Ordering::Relaxed)
    }

    pub fn take_error(&self) -> Option<String> {
        self.error.lock().take()
    }

    /// Whether audio was dropped because the disk fell behind (reported once).
    pub fn take_skipped(&self) -> bool {
        self.skipped.swap(false, Ordering::Relaxed)
    }

    /// Peak level of the most recent `count` 10 ms blocks, oldest first.
    pub fn recent_peaks(&self, count: usize) -> Vec<f32> {
        self.peaks.lock().recent(count)
    }

    /// Stop capturing and close the file. `Ok(None)` if nothing was heard.
    pub fn finish(mut self) -> Result<Option<Take>> {
        let data_bytes = self.shutdown()?;
        let path = self.path.take().context("recording already finished")?;
        let frame_bytes = u64::from(self.channels) * 2;
        let take = Take {
            path,
            sample_rate: self.sample_rate,
            frames: data_bytes / frame_bytes,
            peaks: self.peaks.lock().recent(usize::MAX),
            warning: self.error.lock().take(),
        };
        Ok((take.frames > 0).then_some(take)) // an empty take deletes itself
    }

    /// Stop the stream, let the writer drain the queue, and return the
    /// bytes it wrote.
    fn shutdown(&mut self) -> Result<u64> {
        self.stream.take(); // dropping a cpal Stream stops it
        self.stop.store(true, Ordering::Relaxed);
        match self.writer.take() {
            Some(writer) => writer
                .join()
                .map_err(|_| anyhow::anyhow!("recording writer crashed"))?,
            None => Ok(0),
        }
    }
}

impl Drop for Recorder {
    /// Abandoned mid-take (the app closing, say): discard the file.
    fn drop(&mut self) {
        let _ = self.shutdown();
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Writer thread: move audio from the queue to the file until stopped,
/// then drain what's left and finalize the header.
fn write_loop(
    mut wav: WavWriter,
    queue: &ArrayQueue<f32>,
    stop: &AtomicBool,
    peaks: &Mutex<PeakTracker>,
    bytes: &AtomicU64,
    full: &AtomicBool,
    error: &Mutex<Option<String>>,
) -> Result<u64> {
    let mut chunk = Vec::with_capacity(8192);
    let result = loop {
        let stopping = stop.load(Ordering::Relaxed);
        chunk.clear();
        while chunk.len() < chunk.capacity() {
            match queue.pop() {
                Some(sample) => chunk.push(sample),
                None => break,
            }
        }
        if !chunk.is_empty() && !full.load(Ordering::Relaxed) {
            let written = match wav.write(&chunk) {
                Ok(written) => written,
                Err(err) => {
                    *error.lock() = Some(format!("{err:#}"));
                    // A full disk may still let us repair the header of the
                    // complete frames already written and keep that audio.
                    break wav.recover();
                }
            };
            peaks.lock().push(&chunk[..written]);
            bytes.store(wav.data_bytes, Ordering::Relaxed);
            if written < chunk.len() {
                full.store(true, Ordering::Relaxed);
            }
        }
        if chunk.is_empty() {
            if stopping {
                if let Err(err) = wav.out.flush() {
                    *error.lock() = Some(format!("finish recording: {err}"));
                    break wav.recover();
                }
                break wav.finish();
            }
            thread::sleep(Duration::from_millis(10));
        }
    };
    match &result {
        Ok(written) => bytes.store(*written, Ordering::Relaxed),
        Err(err) => *error.lock() = Some(format!("{err:#}")),
    }
    result
}

/// A 16-bit PCM WAV file written incrementally; the sizes in the header are
/// filled in when it's finished.
struct WavWriter {
    out: BufWriter<File>,
    channels: u16,
    data_bytes: u64,
}

impl WavWriter {
    fn create(path: &Path, sample_rate: u32, channels: u16) -> Result<Self> {
        if channels == 0 || sample_rate == 0 {
            bail!("invalid recording format");
        }
        let block_align = channels.checked_mul(2).context("too many channels")?;
        let byte_rate = sample_rate
            .checked_mul(u32::from(block_align))
            .context("sample rate too high")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut out = BufWriter::with_capacity(1 << 16, file);
        out.write_all(b"RIFF")?;
        out.write_all(&36u32.to_le_bytes())?; // patched in finish()
        out.write_all(b"WAVEfmt ")?;
        out.write_all(&16u32.to_le_bytes())?;
        out.write_all(&1u16.to_le_bytes())?; // PCM
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&sample_rate.to_le_bytes())?;
        out.write_all(&byte_rate.to_le_bytes())?;
        out.write_all(&block_align.to_le_bytes())?;
        out.write_all(&16u16.to_le_bytes())?;
        out.write_all(b"data")?;
        out.write_all(&0u32.to_le_bytes())?; // patched in finish()
        Ok(Self { out, channels, data_bytes: 0 })
    }

    /// Append interleaved samples, stopping at the size limit on a frame
    /// boundary. Returns how many samples were written.
    fn write(&mut self, samples: &[f32]) -> Result<usize> {
        let frame_bytes = u64::from(self.channels) * 2;
        let room = (MAX_DATA_BYTES - self.data_bytes) / frame_bytes * frame_bytes;
        let count = samples.len().min((room / 2) as usize);
        let mut bytes = [0u8; 8192];
        for chunk in samples[..count].chunks(4096) {
            for (i, &sample) in chunk.iter().enumerate() {
                let sample = if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 };
                let pcm = (sample * i16::MAX as f32).round() as i16;
                bytes[i * 2..i * 2 + 2].copy_from_slice(&pcm.to_le_bytes());
            }
            self.out.write_all(&bytes[..chunk.len() * 2]).context("write recording")?;
        }
        self.data_bytes += count as u64 * 2;
        Ok(count)
    }

    /// Fill in the header sizes and close the file. Returns the data bytes.
    fn finish(mut self) -> Result<u64> {
        let data = self.data_bytes as u32; // MAX_DATA_BYTES keeps this in range
        self.out.seek(SeekFrom::Start(4))?;
        self.out.write_all(&(36 + data).to_le_bytes())?;
        self.out.seek(SeekFrom::Start(40))?;
        self.out.write_all(&data.to_le_bytes())?;
        self.out.flush().context("finish recording")?;
        Ok(self.data_bytes)
    }

    /// Recover complete frames that reached disk when a buffered write failed.
    /// Discarding the remaining buffer lets the header be patched without
    /// retrying the failed append (which may need space the disk doesn't have).
    fn recover(self) -> Result<u64> {
        let (mut file, _) = self.out.into_parts();
        let length = file.metadata().context("read recording size")?.len();
        let frame_bytes = u64::from(self.channels) * 2;
        let data_bytes = length.saturating_sub(44) / frame_bytes * frame_bytes;
        if data_bytes == 0 {
            bail!("recording could not be saved: no complete audio frames reached disk");
        }
        file.set_len(44 + data_bytes).context("trim incomplete recording frame")?;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36 + data_bytes as u32).to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&(data_bytes as u32).to_le_bytes()).context("repair recording header")?;
        Ok(data_bytes)
    }
}

/// A fresh temporary file for a take in progress.
fn take_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir()
        .join("openmic-takes")
        .join(format!("take-{}-{stamp}.wav", std::process::id()))
}

/// Move `from` to `to` (replacing it), copying when they're on different drives.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).with_context(|| format!("write {}", to.display()))?;
    let _ = fs::remove_file(from);
    Ok(())
}

impl Take {
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// The take as mono samples at the DSP rate.
    pub fn samples(&self) -> Result<Vec<f32>> {
        crate::decode::load_clip(&self.path)
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
    }

    /// Keep the take at `path`. The take is spent afterwards.
    pub fn save_wav(&self, path: &Path) -> Result<()> {
        move_file(&self.path, path)
    }

    /// Store a recording in the app's recordings directory with a unique name.
    pub fn add_to_library(&self, name: &str) -> Result<PathBuf> {
        let directory = config::settings_path()
            .parent()
            .context("find OpenMic settings directory")?
            .join("recordings");
        fs::create_dir_all(&directory).context("create recordings directory")?;
        let stem = safe_stem(name);
        for number in 1..=9999 {
            let file_name = if number == 1 {
                format!("{stem}.wav")
            } else {
                format!("{stem} {number}.wav")
            };
            let path = directory.join(file_name);
            // Reserve the name, then move the take over it.
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
            }
            if let Err(err) = move_file(&self.path, &path) {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
            return Ok(path);
        }
        bail!("too many recordings named '{stem}'")
    }
}

impl Drop for Take {
    /// Discarded (or already moved): clean up the temporary file.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn safe_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .take(60)
        .map(|ch| {
            if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
                '-'
            } else {
                ch
            }
        })
        .collect();
    let stem = cleaned.trim_matches([' ', '.']);
    let stem = if stem.is_empty() { "Recording" } else { stem };
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if reserved.contains(
        &stem
            .split('.')
            .next()
            .unwrap_or(stem)
            .to_ascii_uppercase()
            .as_str(),
    ) {
        format!("Recording {stem}")
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written_take(samples: &[f32]) -> Take {
        let path = take_path();
        let mut wav = WavWriter::create(&path, 48_000, 2).unwrap();
        wav.write(samples).unwrap();
        let bytes = wav.finish().unwrap();
        Take { path, sample_rate: 48_000, frames: bytes / 4, peaks: Vec::new(), warning: None }
    }

    #[test]
    fn streamed_wav_round_trips_stereo_as_soundboard_audio() {
        let take = written_take(&[0.25, 0.75, -0.5, 0.5, 0.0, 0.0]);
        assert_eq!(take.frames, 3);
        let path = std::env::temp_dir().join(format!("openmic-record-test-{}.wav", std::process::id()));
        take.save_wav(&path).unwrap();
        assert!(!take.path.exists(), "saving moves the take");
        let decoded = crate::decode::load_clip(&path).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(decoded.len(), 3);
        assert!((decoded[0] - 0.5).abs() < 0.001);
        assert!(decoded[1].abs() < 0.001);
    }

    #[test]
    fn a_discarded_take_removes_its_file() {
        let take = written_take(&[0.1; 8]);
        let path = take.path.clone();
        assert!(path.exists());
        drop(take);
        assert!(!path.exists());
    }

    #[test]
    fn peaks_cover_each_ten_milliseconds() {
        // 1 kHz stereo: 20 interleaved samples per 10 ms block.
        let mut peaks = PeakTracker::new(1_000, 2);
        let loud_then_quiet: Vec<f32> =
            (0..30).map(|i| if i == 3 { -0.8 } else if i < 20 { 0.5 } else { 0.1 }).collect();
        peaks.push(&loud_then_quiet);
        assert_eq!(peaks.recent(usize::MAX), vec![0.8, 0.1], "a full block and a partial one");
        peaks.push(&[0.2; 30]);
        assert_eq!(peaks.recent(usize::MAX), vec![0.8, 0.2, 0.2]);
        assert_eq!(peaks.recent(2), vec![0.2, 0.2], "the newest blocks");
    }

    #[test]
    fn writer_stops_on_a_frame_at_the_size_limit() {
        let path = take_path();
        let mut wav = WavWriter::create(&path, 48_000, 2).unwrap();
        wav.data_bytes = MAX_DATA_BYTES - MAX_DATA_BYTES % 4 - 4; // room for one frame
        assert_eq!(wav.write(&[0.0; 8]).unwrap(), 2);
        assert_eq!(wav.write(&[0.0; 8]).unwrap(), 0);
        drop(wav);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn writer_failure_is_reported_without_waiting_for_stop() {
        let path = take_path();
        let mut wav = WavWriter::create(&path, 48_000, 2).unwrap();
        wav.write(&[0.5; 8]).unwrap();
        wav.finish().unwrap();
        // A read-only handle makes writes fail immediately on every platform.
        let wav = WavWriter {
            out: BufWriter::with_capacity(1, File::open(&path).unwrap()),
            channels: 2,
            data_bytes: 0,
        };
        let queue = ArrayQueue::new(8);
        for _ in 0..8 {
            queue.push(0.5).unwrap();
        }
        let error = Mutex::new(None);
        let result = write_loop(
            wav,
            &queue,
            &AtomicBool::new(false),
            &Mutex::new(PeakTracker::new(48_000, 2)),
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            &error,
        );
        assert!(result.is_err());
        assert!(error.lock().is_some(), "the UI must see a stopped writer");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn recovered_wav_keeps_only_complete_frames_on_disk() {
        let path = take_path();
        let mut wav = WavWriter::create(&path, 48_000, 2).unwrap();
        wav.write(&[0.5; 8]).unwrap();
        wav.out.write_all(&[0]).unwrap(); // a partially written stereo frame
        wav.out.flush().unwrap();
        wav.out.write_all(&[0; 8]).unwrap(); // buffered audio hasn't reached disk
        assert_eq!(wav.recover().unwrap(), 16);
        assert_eq!(fs::metadata(&path).unwrap().len(), 44 + 16);
        let samples = crate::decode::load_clip(&path).unwrap();
        assert_eq!(samples.len(), 4);
        assert!(samples.iter().all(|sample| (*sample - 0.5).abs() < 0.001));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finishing_preserves_a_warning_reported_during_shutdown() {
        let path = take_path();
        let mut wav = WavWriter::create(&path, 48_000, 2).unwrap();
        wav.write(&[0.5; 8]).unwrap();
        wav.out.flush().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let writer_stop = Arc::clone(&stop);
        let writer_error = Arc::clone(&error);
        let writer = thread::spawn(move || {
            while !writer_stop.load(Ordering::Relaxed) {
                thread::yield_now();
            }
            *writer_error.lock() = Some("disk full".into());
            wav.recover()
        });
        let recorder = Recorder {
            stream: None,
            writer: Some(writer),
            path: Some(path),
            stop,
            peaks: Arc::new(Mutex::new(PeakTracker::new(48_000, 2))),
            bytes: Arc::new(AtomicU64::new(0)),
            sample_rate: 48_000,
            channels: 2,
            started: Instant::now(),
            full: Arc::new(AtomicBool::new(false)),
            skipped: Arc::new(AtomicBool::new(false)),
            error,
        };
        assert!(recorder.take_error().is_none());
        let take = recorder.finish().unwrap().unwrap();
        assert_eq!(take.warning(), Some("disk full"));
        assert_eq!(take.samples().unwrap().len(), 4);
    }

    #[test]
    fn recording_names_are_safe_for_windows_files() {
        assert_eq!(safe_stem(" Airhorn: wow? "), "Airhorn- wow-");
        assert_eq!(safe_stem("CON"), "Recording CON");
        assert_eq!(safe_stem("AUX.mix"), "Recording AUX.mix");
        assert_eq!(safe_stem(".. "), "Recording");
    }

    #[test]
    fn a_reserved_name_with_a_dot_can_be_added_to_the_library() {
        let take = written_take(&[0.5; 8]);
        let path = take.add_to_library("AUX.mix").unwrap();
        drop(take);
        assert!(path.is_file());
        assert_eq!(crate::decode::load_clip(&path).unwrap().len(), 4);
        fs::remove_file(path).unwrap();
    }
}
