//! Play a recording to the user's own headphones or speakers (the mic test),
//! never into VB-Cable: this audio is for you, not for Discord.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};

use crate::dsp::SR;
use crate::resample::Resampler;

/// A clip playing on an output device; stops when dropped.
pub struct Player {
    _stream: Stream,
    pos: Arc<AtomicUsize>,
    len: usize,
}

/// Whether a device name is VB-Cable (Discord's input), which the mic test
/// must never play into.
pub fn is_cable(name: &str) -> bool {
    name.to_ascii_uppercase().contains("CABLE")
}

impl Player {
    /// Play `samples` (mono at the DSP rate) on the output named `device`,
    /// or the Windows default output when it's empty.
    pub fn play(device: &str, samples: &[f32]) -> Result<Self> {
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
        if is_cable(&name) {
            return Err(anyhow!("'{name}' goes to Discord; choose headphones as the monitor output"));
        }
        let supported = device.default_output_config().context("query the output's format")?;
        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let audio = Arc::new(resample(samples, config.sample_rate));
        let pos = Arc::new(AtomicUsize::new(0));
        let len = audio.len();
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, &audio, &pos),
            SampleFormat::I16 => build::<i16>(&device, &config, &audio, &pos),
            SampleFormat::U16 => build::<u16>(&device, &config, &audio, &pos),
            SampleFormat::I32 => build::<i32>(&device, &config, &audio, &pos),
            SampleFormat::F64 => build::<f64>(&device, &config, &audio, &pos),
            other => Err(anyhow!("unsupported output format {other}")),
        }?;
        stream.play().context("start playback")?;
        Ok(Self { _stream: stream, pos, len })
    }

    /// How far through the clip playback is, 0..=1.
    pub fn progress(&self) -> f32 {
        if self.len == 0 { 1.0 } else { self.pos.load(Ordering::Relaxed) as f32 / self.len as f32 }
    }

    pub fn finished(&self) -> bool {
        self.pos.load(Ordering::Relaxed) >= self.len
    }
}

fn resample(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate == SR {
        return samples.to_vec();
    }
    let mut resampler = Resampler::new(SR, rate);
    let mut out = Vec::with_capacity(samples.len() * rate as usize / SR as usize + 64);
    let mut buf = [0.0f32; 1024];
    for chunk in samples.chunks(4096) {
        resampler.push(chunk);
        loop {
            let n = resampler.pull(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }
    out
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, audio: &Arc<Vec<f32>>, pos: &Arc<AtomicUsize>) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let (audio, pos) = (Arc::clone(audio), Arc::clone(pos));
    Ok(device.build_output_stream(
        *config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut at = pos.load(Ordering::Relaxed);
            for frame in data.chunks_mut(channels) {
                let s = audio.get(at).copied().unwrap_or(0.0);
                at = (at + 1).min(audio.len());
                frame.fill(T::from_sample(s));
            }
            pos.store(at, Ordering::Relaxed);
        },
        |_| {},
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
    fn resampling_keeps_the_duration() {
        let second = vec![0.1f32; SR as usize];
        let out = resample(&second, 44_100);
        assert!((out.len() as i64 - 44_100).abs() < 200, "{}", out.len());
        assert_eq!(resample(&second, SR).len(), SR as usize);
    }
}
