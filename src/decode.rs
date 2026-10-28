//! Clip loading: decode any symphonia-supported format to mono 48 kHz f32.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// A clip's length in seconds from its header, without decoding it (some
/// formats, like VBR MP3 without a header, don't say).
pub fn duration(path: &Path) -> Option<f32> {
    let src = File::open(path).ok()?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
    let (frames, rate) = (
        track.codec_params.n_frames?,
        track.codec_params.sample_rate?,
    );
    (rate > 0).then(|| frames as f32 / rate as f32)
}

/// Decode `path` to clipped mono samples at [`dsp::SR`].
#[cfg(test)]
pub fn load_clip(path: &Path) -> Result<Vec<f32>> {
    load_clip_cancellable(path, &AtomicBool::new(false))
}

fn check_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("clip import cancelled");
    }
    Ok(())
}

/// Background imports can stop between packets without blocking the UI.
pub fn load_clip_cancellable(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>> {
    check_cancelled(cancel)?;
    let src = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("probe audio format")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no audio track")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("create audio decoder")?;

    let mut mono: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    loop {
        check_cancelled(cancel)?;
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e).context("read audio packet"),
        };
        if packet.track_id() != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if rate == 0 {
                    rate = spec.rate;
                } else if spec.rate != rate {
                    bail!("audio sample rate changes within the file");
                }
                append_mono(&mut mono, decoded)?;
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("decode audio packet"),
        }
    }

    if rate == 0 || mono.is_empty() {
        bail!("audio file is empty");
    }

    resample_linear(mono, rate, crate::dsp::SR, cancel)
}

fn append_mono(mono: &mut Vec<f32>, decoded: AudioBufferRef<'_>) -> Result<()> {
    let spec = *decoded.spec();
    let channels = spec.channels.count();
    if channels == 0 {
        bail!("audio track has no channels");
    }
    // Packet duration is in container time-base ticks, which need not equal
    // decoded frames. Size the buffer using the decoder's actual output.
    let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
    buf.copy_interleaved_ref(decoded);
    mono.try_reserve(buf.samples().len() / channels)
        .context("not enough memory to load this clip")?;
    mono.extend(buf.samples().chunks_exact(channels).map(|frame| {
        // Float WAVs can contain non-finite or over-range samples. Silence
        // invalid channels before interpolation spreads them, and accumulate
        // in f64 so finite channels cannot overflow while being downmixed.
        let sum: f64 = frame
            .iter()
            .filter(|s| s.is_finite())
            .map(|&s| f64::from(s))
            .sum();
        (sum / channels as f64).clamp(-1.0, 1.0) as f32
    }));
    Ok(())
}

/// Naive linear interpolation resampler (matches the Python np.interp approach).
// ponytail: linear interp aliases on big downshifts; use symphonia's resampler
// or rubato if long-form/high-rate clips ever sound dull.
fn resample_linear(input: Vec<f32>, from: u32, to: u32, cancel: &AtomicBool) -> Result<Vec<f32>> {
    check_cancelled(cancel)?;
    if input.is_empty() || from == to {
        // Most recordings already use the DSP rate. Keep their allocation
        // instead of briefly needing twice the memory for the entire take.
        return Ok(input);
    }
    let n = (((input.len() as f64) * to as f64 / from as f64).round() as usize).max(1);
    let ratio = f64::from(from) / f64::from(to);
    let last = input.len() - 1;
    let mut output = Vec::new();
    output
        .try_reserve_exact(n)
        .context("not enough memory to resample this clip")?;
    for start in (0..n).step_by(4096) {
        check_cancelled(cancel)?;
        output.extend((start..n.min(start.saturating_add(4096))).map(|i| {
            let pos = i as f64 * ratio;
            let i0 = (pos.floor() as usize).min(last);
            let i1 = (i0 + 1).min(last);
            let frac = (pos - i0 as f64) as f32;
            input[i0] * (1.0 - frac) + input[i1] * frac
        }));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use symphonia::core::audio::{AudioBuffer, Channels, Signal, SignalSpec};

    #[test]
    fn downmix_uses_decoded_frames_and_channels() {
        let spec = SignalSpec::new(48_000, Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
        let mut decoded = AudioBuffer::<f32>::new(8, spec);
        decoded.render_reserved(Some(3));
        decoded.chan_mut(0).copy_from_slice(&[0.25, -0.5, 0.75]);
        decoded.chan_mut(1).copy_from_slice(&[0.75, 0.5, 0.25]);
        let mut mono = Vec::new();
        append_mono(
            &mut mono,
            AudioBufferRef::F32(std::borrow::Cow::Borrowed(&decoded)),
        )
        .unwrap();
        assert_eq!(mono, vec![0.5, 0.0, 0.5]);
    }

    #[test]
    fn an_audio_buffer_without_channels_is_refused_instead_of_dividing_by_zero() {
        let decoded = AudioBuffer::<f32>::unused();
        let mut mono = Vec::new();
        let error = append_mono(
            &mut mono,
            AudioBufferRef::F32(std::borrow::Cow::Borrowed(&decoded)),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "audio track has no channels");
        assert!(mono.is_empty());
    }

    #[test]
    fn floating_point_clips_stay_finite_and_clipped_through_playback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("float.wav");
        let frames = [
            [f32::NAN, 0.5],
            [f32::INFINITY, 0.5],
            [f32::NEG_INFINITY, -0.5],
            [2.0, 0.5],
            [-2.0, -0.5],
            [f32::MAX, f32::MAX],
            [-f32::MAX, -f32::MAX],
            [f32::MAX, -f32::MAX],
            [0.5, 0.25],
        ];
        for rate in [24_000u32, 48_000, 96_000] {
            let data_len = (frames.len() * 8) as u32;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
            bytes.extend_from_slice(&2u16.to_le_bytes()); // stereo
            bytes.extend_from_slice(&rate.to_le_bytes());
            bytes.extend_from_slice(&(rate * 8).to_le_bytes());
            bytes.extend_from_slice(&8u16.to_le_bytes());
            bytes.extend_from_slice(&32u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&data_len.to_le_bytes());
            for sample in frames.iter().flatten() {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            std::fs::write(&path, bytes).unwrap();

            let samples = load_clip(&path).unwrap();
            assert!(
                samples
                    .iter()
                    .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)),
                "{rate} Hz: {samples:?}"
            );
            if rate == crate::dsp::SR {
                assert_eq!(
                    samples,
                    [0.25, 0.25, -0.25, 1.0, -1.0, 1.0, -1.0, 0.0, 0.375]
                );
            }
            let mut mixer = crate::dsp::Mixer::default();
            let mut voice = vec![0.125; samples.len()];
            mixer.play(
                crate::dsp::Clip::new(1, std::sync::Arc::new(samples), 1.0),
                false,
            );
            mixer.mix(&mut voice, 0.0);
            assert!(
                voice.iter().all(|s| *s == 0.125),
                "a silent pad must never corrupt the microphone"
            );
        }
    }

    /// Minimal 44-byte-header PCM16 WAV writer for the test fixtures.
    fn write_wav(path: &Path, samples_i16: &[i16], rate: u16, channels: u16) {
        let mut f = File::create(path).unwrap();
        let data_len = (samples_i16.len() * 2) as u32;
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM format
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&(rate as u32).to_le_bytes()).unwrap();
        f.write_all(&((rate as u32) * (channels as u32) * 2).to_le_bytes())
            .unwrap();
        f.write_all(&(channels * 2).to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        for s in samples_i16 {
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }

    // Ported from test_soundboard.py: stereo 24 kHz clip -> mono 48 kHz at 0.3.
    #[test]
    fn decodes_stereo_24k_to_mono_48k() {
        let dir = std::env::temp_dir().join("openmic_test_decode");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");
        // 2400 stereo frames @ 24 kHz -> 4800 mono frames @ 48 kHz, amplitude preserved.
        write_wav(&path, &vec![2621i16; 4800], 24000, 2);
        let loaded = load_clip(&path).unwrap();
        assert_eq!(loaded.len(), 4800, "clip was not resampled to 48 kHz");
        let amp = loaded[1000];
        assert!((amp - 2621.0 / 32768.0).abs() < 2e-3, "amplitude {amp}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_empty_audio() {
        let dir = std::env::temp_dir().join("openmic_test_decode");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.wav");
        write_wav(&path, &[], 24000, 2);
        assert!(load_clip(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_cancelled_import_stops_before_opening_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("never-opened.wav");
        let error = load_clip_cancellable(&path, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.to_string(), "clip import cancelled");
    }

    #[test]
    fn resample_preserves_constant_and_length() {
        let input = vec![0.5f32; 2400];
        let out = resample_linear(input, 24_000, 48_000, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.len(), 4800);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn cancellation_after_decoding_never_returns_partial_resampled_audio() {
        for rate in [24_000, 48_000, 96_000] {
            let result =
                resample_linear(vec![0.5; 100], rate, crate::dsp::SR, &AtomicBool::new(true));
            assert_eq!(result.unwrap_err().to_string(), "clip import cancelled");
        }
    }
}
