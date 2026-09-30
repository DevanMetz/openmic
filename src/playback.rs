//! Play a recording to the user's own headphones or speakers (the mic test),
//! never into VB-Cable: this audio is for you, not for Discord.

use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender};

use crate::dsp::SR;
use crate::resample::Resampler;

/// A clip playing on an output device; stops when dropped.
pub struct Player {
    stream: Stream,
    progress: Arc<PlaybackProgress>,
    len: usize,
    errors: Receiver<String>,
}

#[derive(Default)]
struct PlaybackProgress {
    queued: AtomicUsize,
    /// When the final sample reaches the device, on its own monotonic clock.
    ends_at: OnceLock<cpal::StreamInstant>,
}

impl PlaybackProgress {
    fn finished(&self, now: cpal::StreamInstant) -> bool {
        self.ends_at.get().is_some_and(|&end| now >= end)
    }
}

/// Whether a device name is VB-Cable (Discord's input), which the mic test
/// must never play into.
pub fn is_cable(name: &str) -> bool {
    name.to_ascii_uppercase().contains("CABLE")
}

/// Check the resolved device too: the Windows default can be the same
/// virtual output that carries processed audio into a voice call.
fn validate_output(name: &str, processed_output: &str) -> Result<()> {
    if is_cable(name) || name == processed_output {
        return Err(anyhow!("Choose headphones as the monitor output; '{name}' is a voice-call output"));
    }
    Ok(())
}

impl Player {
    /// Play `samples` (mono at the DSP rate) on the output named `device`,
    /// or the Windows default output when it's empty. The resolved device
    /// must differ from the processed output and must not be VB-Cable.
    pub fn play(device: &str, processed_output: &str, samples: &[f32]) -> Result<Self> {
        let host = cpal::default_host();
        let device = if device.is_empty() {
            host.default_output_device().context("no default output device")?
        } else {
            host.output_devices()
                .context("list output devices")?
                .find(|d| d.to_string() == device)
                .with_context(|| format!("output '{device}' not found"))?
        };
        let name = device.to_string();
        validate_output(&name, processed_output)?;
        let supported = device.default_output_config().context("query the output's format")?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let audio = Arc::new(resample(samples, config.sample_rate));
        let progress = Arc::new(PlaybackProgress::default());
        let len = audio.len();
        let (report, errors) = crossbeam_channel::bounded(1);
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, &audio, &progress, &report),
            SampleFormat::I16 => build::<i16>(&device, &config, &audio, &progress, &report),
            SampleFormat::U16 => build::<u16>(&device, &config, &audio, &progress, &report),
            SampleFormat::I32 => build::<i32>(&device, &config, &audio, &progress, &report),
            SampleFormat::F64 => build::<f64>(&device, &config, &audio, &progress, &report),
            other => Err(anyhow!("unsupported output format {other}")),
        }?;
        stream.play().context("start playback")?;
        Ok(Self { stream, progress, len, errors })
    }

    /// How far through the clip playback is, 0..=1.
    pub fn progress(&self) -> f32 {
        if self.len == 0 { 1.0 } else { self.progress.queued.load(Ordering::Relaxed) as f32 / self.len as f32 }
    }

    pub fn finished(&self) -> bool {
        self.len == 0 || self.progress.finished(self.stream.now())
    }

    /// A failed output stream needs another playback attempt; the recording
    /// itself remains available. Transient buffer glitches are ignored.
    pub fn take_error(&self) -> Option<String> {
        self.errors.try_recv().ok()
    }
}

fn resample(samples: &[f32], rate: u32) -> Vec<f32> {
    if samples.is_empty() || rate == SR {
        return samples.to_vec();
    }
    let mut resampler = Resampler::new(SR, rate);
    let expected = (samples.len() as f64 * f64::from(rate) / f64::from(SR)).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(expected);
    let mut buf = [0.0f32; 1024];
    // A streaming converter waits for two future samples. At the end of a
    // finite clip, hold its last value and emit only the intended duration.
    let padding = [samples[samples.len() - 1]; 2];
    for chunk in samples.chunks(4096).chain(std::iter::once(padding.as_slice())) {
        resampler.push(chunk);
        while out.len() < expected {
            let room = (expected - out.len()).min(buf.len());
            let n = resampler.pull(&mut buf[..room]);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    out
}

fn error_callback(report: Sender<String>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |error| {
        if !crate::engine::is_transient(&error) {
            // Keep the first failure and never block an audio callback.
            let _ = report.try_send(error.to_string());
        }
    }
}

fn output_callback<T>(audio: Arc<Vec<f32>>, channels: usize, rate: u32, progress: Arc<PlaybackProgress>)
    -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: SizedSample + FromSample<f32>,
{
    move |data, info| {
        let start = progress.queued.load(Ordering::Relaxed);
        let mut at = start;
        for frame in data.chunks_mut(channels) {
            let s = audio.get(at).copied().unwrap_or(0.0);
            at = (at + 1).min(audio.len());
            frame.fill(T::from_sample(s));
        }
        if at == audio.len() && progress.ends_at.get().is_none() {
            // Queued is not yet audible. Include the device's latency and
            // only the audio frames in this buffer, not its trailing silence.
            let tail = Duration::from_secs_f64((at - start) as f64 / f64::from(rate));
            let _ = progress.ends_at.set(info.timestamp().playback + tail);
        }
        progress.queued.store(at, Ordering::Relaxed);
    }
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, audio: &Arc<Vec<f32>>, progress: &Arc<PlaybackProgress>, report: &Sender<String>) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    Ok(device.build_output_stream(
        *config,
        output_callback::<T>(Arc::clone(audio), usize::from(config.channels), config.sample_rate, Arc::clone(progress)),
        error_callback(report.clone()),
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vb_cable_is_never_a_test_output() {
        assert!(is_cable("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_cable("Cable Output"));
        assert!(!is_cable("Speakers (JDS Labs Atom DAC)"));
    }

    #[test]
    fn a_resolved_preview_device_cannot_be_the_processed_output() {
        let processed = "Voicemeeter Input";
        assert!(!is_cable(processed), "custom virtual devices need the route comparison too");
        for output in [processed, "CABLE Input (VB-Audio Virtual Cable)", "Cable Output"] {
            let error = validate_output(output, processed).unwrap_err().to_string();
            assert!(error.starts_with("Choose headphones as the monitor output"));
            assert!(error.contains(output));
        }
        assert!(validate_output("Headphones", processed).is_ok());
        assert!(validate_output("Headphones", "").is_ok(), "a stopped app with no processed route can still preview");
        assert!(validate_output("Headphones", "Headphones").is_err(), "protect explicit routes regardless of device branding");
    }

    #[test]
    fn resampling_keeps_the_duration() {
        let second = vec![0.1f32; SR as usize];
        let out = resample(&second, 44_100);
        assert_eq!(out.len(), 44_100);
        assert_eq!(resample(&second, SR).len(), SR as usize);
    }

    #[test]
    fn resampling_flushes_the_tail_of_short_previews() {
        let tail = [0.0, 0.0, 0.5];
        let doubled = resample(&tail, 96_000);
        assert_eq!(doubled.len(), 6);
        assert_eq!(doubled[4], 0.5, "the final input sample reaches the output");
        assert!(doubled[5] > 0.4);
        let halved = resample(&tail, 24_000);
        assert_eq!(halved, [0.0, 0.5]);

        for (rate, expected) in [(8_000, 1), (44_100, 1), (96_000, 2), (192_000, 4)] {
            let single = resample(&[0.25], rate);
            assert_eq!(single.len(), expected, "a one-sample clip must not disappear at {rate} Hz");
            assert_eq!(single[0], 0.25);
            assert!(resample(&[], rate).is_empty());
        }
    }

    #[test]
    fn playback_reports_a_lost_device_but_survives_transient_glitches() {
        use cpal::ErrorKind;

        let (report, errors) = crossbeam_channel::bounded(1);
        let mut callback = error_callback(report);
        for kind in [ErrorKind::Xrun, ErrorKind::RealtimeDenied, ErrorKind::DeviceChanged] {
            callback(kind.into());
        }
        assert!(errors.try_recv().is_err());
        callback(ErrorKind::DeviceNotAvailable.into());
        callback(ErrorKind::StreamInvalidated.into());
        let expected: cpal::Error = ErrorKind::DeviceNotAvailable.into();
        assert_eq!(errors.try_recv().unwrap(), expected.to_string(), "preserve the first failure without waiting for the UI");
        assert!(errors.try_recv().is_err());
        drop(errors);
        callback(ErrorKind::StreamInvalidated.into()); // the UI already stopped playback
    }

    fn output_time(callback_ms: u64, playback_ms: u64) -> cpal::OutputCallbackInfo {
        cpal::OutputCallbackInfo::new(cpal::OutputStreamTimestamp {
            callback: cpal::StreamInstant::from_millis(callback_ms),
            playback: cpal::StreamInstant::from_millis(playback_ms),
        })
    }

    #[test]
    fn finishing_waits_until_the_device_plays_the_final_audio_frame() {
        let progress = Arc::new(PlaybackProgress::default());
        let mut callback = output_callback::<f32>(Arc::new(vec![0.25; 300]), 2, 1_000, Arc::clone(&progress));
        let mut buffer = [0.0; 400]; // 200 stereo frames
        callback(&mut buffer, &output_time(0, 30));
        assert_eq!(buffer, [0.25; 400]);
        assert!(!progress.finished(cpal::StreamInstant::from_millis(5000)), "still have audio left to queue");

        callback(&mut buffer, &output_time(200, 230));
        assert_eq!(progress.queued.load(Ordering::Relaxed), 300);
        assert_eq!(buffer[..200], [0.25; 200]);
        assert_eq!(buffer[200..], [0.0; 200]);
        assert!(!progress.finished(cpal::StreamInstant::from_millis(200)), "queueing the tail must not stop the stream");
        assert!(!progress.finished(cpal::StreamInstant::from_millis(329)));
        assert!(progress.finished(cpal::StreamInstant::from_millis(330)), "30 ms device latency plus 100 audio frames at 1 kHz");

        callback(&mut buffer, &output_time(400, 430));
        assert_eq!(buffer, [0.0; 400]);
        assert!(progress.finished(cpal::StreamInstant::from_millis(330)), "silent callbacks must not keep extending playback");
    }

    #[test]
    fn a_short_preview_is_not_finished_when_its_first_buffer_is_submitted() {
        let progress = Arc::new(PlaybackProgress::default());
        let mut callback = output_callback::<f32>(Arc::new(vec![0.5; 48]), 1, 48_000, Arc::clone(&progress));
        let mut buffer = [1.0; 480];
        callback(&mut buffer, &output_time(0, 20));
        assert_eq!(buffer[..48], [0.5; 48]);
        assert_eq!(buffer[48..], [0.0; 432]);
        assert!(!progress.finished(cpal::StreamInstant::from_millis(20)));
        assert!(progress.finished(cpal::StreamInstant::from_millis(21)));
    }
}
