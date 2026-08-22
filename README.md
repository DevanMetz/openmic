# Discord Denoiser

A local, real-time RNNoise microphone denoiser for Windows. It captures your microphone, processes 10 ms frames, and sends the cleaned signal to VB-Audio Virtual Cable for use in Discord or any other voice application.

Audio stays on your computer. Nothing is recorded or uploaded.

## Features

- Native RNNoise processing at 48 kHz
- Selectable microphone, processed output, and monitor output
- Noise-reduction wet/dry control
- Input and output gain
- Noise gate with adjustable threshold
- Live bypass and mute
- Headphone monitoring with independent volume
- Input and output level meters
- Automatic settings persistence
- Optional Windows startup and automatic processing

## Requirements

- Windows 10 or Windows 11
- Python 3 (Python 3.13 is tested)
- [VB-Audio Virtual Cable](https://vb-audio.com/Cable/)

## Install

```bat
git clone https://github.com/DevanMetz/discord-denoiser.git
cd discord-denoiser
setup.bat
run.bat
```

`setup.bat` creates a local `.venv` and installs the pinned dependencies. No administrator access is required for the app. VB-Cable's driver installation may require administrator access.

## Discord setup

1. Run the app.
2. Select your physical microphone.
3. Set **Processed output** to **CABLE Input (VB-Audio Virtual Cable)**.
4. In Discord, open **User Settings → Voice & Video**.
5. Set **Input Device** to **CABLE Output (VB-Audio Virtual Cable)**.
6. Disable Discord/Krisp noise suppression to avoid double processing.

Use headphones before enabling Monitor to prevent feedback.

## Checks

```bat
.venv\Scripts\python.exe test_controls.py
.venv\Scripts\python.exe test_denoise.py
```

`test_controls.py` is hardware-free. `test_denoise.py` verifies the bundled RNNoise engine against deterministic noise and signal inputs.

## License

MIT. RNNoise and installed dependencies retain their own licenses.

Discord Denoiser is an unofficial community project and is not affiliated with Discord or VB-Audio.
