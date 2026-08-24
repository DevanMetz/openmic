//! Clip loading: decode any symphonia-supported format to mono 48 kHz f32.

use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Decode `path` to clipped mono samples at [`dsp::SR`].
pub fn load_clip(path: &Path) -> Result<Vec<f32>> {
    let src = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
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

    let mut interleaved: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
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
                }
                let mut buf = SampleBuffer::<f32>::new(packet.dur(), spec);
                buf.copy_interleaved_ref(decoded);
                interleaved.extend_from_slice(buf.samples());
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("decode audio packet"),
        }
    }

    if rate == 0 || interleaved.is_empty() {
        bail!("audio file is empty");
    }

    // Average channels to mono.
    let channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(1).max(1);
    let mono: Vec<f32> = if channels > 1 {
        interleaved
            .chunks(channels)
            .map(|ch| ch.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        interleaved
    };

    Ok(resample_linear(&mono, rate, crate::dsp::SR))
}

/// Naive linear interpolation resampler (matches the Python np.interp approach).
// ponytail: linear interp aliases on big downshifts; use symphonia's resampler
// or rubato if long-form/high-rate clips ever sound dull.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let n = (((input.len() as f64) * to as f64 / from as f64).round() as usize).max(1);
    let ratio = f64::from(from) / f64::from(to);
    let last = input.len() - 1;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = (pos.floor() as usize).min(last);
            let i1 = (i0 + 1).min(last);
            let frac = (pos - i0 as f64) as f32;
            input[i0] * (1.0 - frac) + input[i1] * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        f.write_all(&((rate as u32) * (channels as u32) * 2).to_le_bytes()).unwrap();
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
    fn resample_preserves_constant_and_length() {
        let input = vec![0.5f32; 2400];
        let out = resample_linear(&input, 24_000, 48_000);
        assert_eq!(out.len(), 4800);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }
}
