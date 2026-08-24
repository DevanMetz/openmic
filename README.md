# OpenMic

OpenMic is a local, real-time microphone cleaner and soundboard for Windows — now written entirely in Rust. It processes your voice with RNNoise (the current xiph model, compiled from source), mixes optional sound clips, and sends the result to VB-Audio Virtual Cable for Discord or any other voice application.

Audio stays on your computer. Nothing is recorded or uploaded.

![OpenMic running](docs/screenshot.png)

## Features

- Native RNNoise processing at 48 kHz (vendored xiph source, built via `cc` — no Python, no prebuilt DLLs)
- Opens devices at their native formats and resamples to/from the 48 kHz DSP core
- Selectable microphone, processed output, and headphone monitor
- Live routing changes with automatic stream handoff (soundboard playhead preserved)
- Noise-reduction wet/dry control
- Input/output gain and adjustable noise gate
- Live bypass and microphone mute
- Persistent soundboard with independent volume; WAV, FLAC, OGG, MP3, AIFF via symphonia
- Input and output level meters plus live voice-probability readout
- Automatic settings persistence
- Optional Windows startup and automatic processing
- Dark native GUI (egui)

## Requirements

- Windows 10 or 11
- [Rust](https://rustup.rs) (stable) and MSVC build tools
- A C compiler for the vendored RNNoise: MSVC `cl.exe` cannot compile the VLA usage in `pitch.c`, so the build uses `clang` (e.g. the one shipped with AMD ROCm, or LLVM/Clang for Windows). It must target the MSVC ABI.
- [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (recommended output)

## Build & run

```bat
git clone https://github.com/DevanMetz/openmic.git
cd openmic
cargo run --release
```

The binary is `target\release\openmic.exe`. No administrator access needed; VB-Cable's driver install may require it.

## Discord setup

1. Run OpenMic.
2. Select your physical microphone.
3. Set **Processed output** to **CABLE Input (VB-Audio Virtual Cable)**.
4. In Discord, open **User Settings → Voice & Video**.
5. Set **Input Device** to **CABLE Output (VB-Audio Virtual Cable)**.
6. Disable Discord/Krisp noise suppression to avoid double processing.

Use headphones before enabling the headphone monitor to prevent feedback.

## Soundboard

Open the **Soundboard** tab, add local audio clips, select one, press **Play** (or double-click). The clip is mixed into the same processed output Discord receives. Route changes hand the clip playhead to the new engine. **Mute microphone** silences your voice without silencing the soundboard.

## Checks

```bat
cargo test
```

The suite covers the gate/bypass/mute DSP, soundboard mixing and mute isolation, clip decode/resample, streaming resampler continuity, settings round-trip, startup registry handling, and that RNNoise crushes stationary noise while flagging tones as speech.

## License

MIT. The vendored xiph RNNoise under `vendor/rnnoise/` keeps its own BSD-style license (see `vendor/rnnoise/COPYING`).
