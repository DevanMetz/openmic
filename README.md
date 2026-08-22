# OpenMic

OpenMic is a local, real-time microphone cleaner and soundboard for Windows. It processes your voice with RNNoise, mixes optional sound clips, and sends the result to VB-Audio Virtual Cable for Discord or any other voice application.

Audio stays on your computer. Nothing is recorded or uploaded.

## Features

- Native RNNoise processing at 48 kHz
- Selectable microphone, processed output, and headphone monitor
- Live routing changes with automatic audio-stream handoff
- Noise-reduction wet/dry control
- Input/output gain and adjustable noise gate
- Live bypass and microphone mute
- Persistent soundboard with independent volume
- WAV, FLAC, OGG, AIFF, and MP3 support through libsndfile
- Input and output level meters
- Automatic settings persistence
- Optional Windows startup and automatic processing
- Dark native Windows interface with separate Microphone and Soundboard tabs

## Requirements

- Windows 10 or Windows 11
- Python 3 (Python 3.13 is tested)
- [VB-Audio Virtual Cable](https://vb-audio.com/Cable/)

## Install

```bat
git clone https://github.com/DevanMetz/openmic.git
cd openmic
setup.bat
run.bat
```

`setup.bat` creates a local `.venv` and installs the pinned dependencies. OpenMic itself needs no administrator access. VB-Cable's driver installation may require it.

## Discord setup

1. Run OpenMic.
2. Select your physical microphone.
3. Set **Processed output** to **CABLE Input (VB-Audio Virtual Cable)**.
4. In Discord, open **User Settings → Voice & Video**.
5. Set **Input Device** to **CABLE Output (VB-Audio Virtual Cable)**.
6. Disable Discord/Krisp noise suppression to avoid double processing.

Use headphones before enabling the headphone monitor to prevent feedback.

## Soundboard

Open the **Soundboard** tab, add local audio clips, select one, and press **Play**. The clip is mixed into the same processed output Discord receives. Double-clicking a clip also plays it. **Mute microphone** silences your voice without silencing the soundboard.

## Checks

```bat
.venv\Scripts\python.exe test_controls.py
.venv\Scripts\python.exe test_soundboard.py
.venv\Scripts\python.exe test_denoise.py
```

The controls and soundboard checks are hardware-free. The RNNoise check uses deterministic noise and signal inputs.

## License

MIT. RNNoise and installed dependencies retain their own licenses.

OpenMic is an unofficial community project and is not affiliated with Discord or VB-Audio.
