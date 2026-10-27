# Linux / Windows parity

Status of each Windows user-facing feature on Linux, against `main`. An open pull request is still missing or partial until it is merged.

| Feature | Linux | Code |
|---|---|---|
| DeepFilterNet 3 and RNNoise, voice gate, rumble filter, gains, level gate, presets | working | `src/denoise.rs`, `src/dsp.rs`, `src/gui/mic_tab.rs` |
| Live scope | working | `src/gui/mic_tab.rs`, `src/viz.rs` |
| Microphone, processed output, and headphone monitor routing, with reconnect | working | `src/engine.rs`, `src/gui/routes.rs` |
| 10 ms PulseAudio buffers | working | `src/engine.rs` |
| Real-time priority for audio threads | partial | `src/engine.rs` — the request is made, and without RealtimeKit the thread is not raised |
| Virtual cable (null sink and remap source), removed on exit | working | `src/virtual_mic.rs` |
| Virtual cable at 48 kHz with a short buffer | partial | `src/virtual_mic.rs` — PulseAudio's default rate and a long buffer |
| Virtual microphone becomes the default source while OpenMic runs | working | `src/default_mic.rs` |
| Separate communications default device | not possible | `src/default_mic.rs` — PulseAudio has one default source, so console and communications both follow it |
| Record the microphone | working | `src/record.rs` |
| Record computer audio from an output's monitor source | partial | `src/record.rs` — only names that start with the English prefix "Monitor of " |
| Test my mic | working | `src/engine.rs`, `src/playback.rs` |
| Soundboard pads, trim, search, overlap, drag-and-drop reorder, WAV/FLAC/OGG/MP3/AIFF | working | `src/gui/clips.rs`, `src/decode.rs` |
| Drag clips onto the window | working | `src/gui/mod.rs` |
| Global hotkeys on X11 (mute, bypass, stop clips, dictation, pads) | working | `src/hotkeys.rs` |
| Global hotkeys on Wayland | working | `src/portal_shortcuts.rs`, `src/hotkeys.rs` — the desktop global-shortcuts portal when it is installed (one confirmation). Without it, the X11 grab is used and only X11 windows receive the keys |
| Dictation transcription on this computer | working | `src/dictation.rs` |
| Dictation typed into the focused app | missing | `src/dictation.rs` — the transcript is shown to copy |
| Speech model download | working | `src/dictation.rs` |
| Notification area, close-to-tray, and the tray menu | missing | `src/tray.rs`, `src/gui/settings_tab.rs` — closing the window quits |
| A second launch shows the running window | missing | `src/instance.rs` — the lock is exclusive and `on_show` does nothing |
| Start on login | partial | `src/config.rs` — writes an XDG autostart entry that runs `--minimized`; any existing file counts as enabled |
| Restored window position kept on a connected monitor | partial | `src/gui/mod.rs` — `on_a_monitor` is always true |
| Initial window size from the work area | partial | `src/main.rs` — `work_area_size` is `None`, so the saved size is used as-is |
| Always-on-top strip | partial | `src/gui/overlay.rs` — X11 sets above through winit; Wayland ignores window level, and `raise_meter` does nothing |
| Strip kept inside the monitor work area | partial | `src/gui/overlay.rs` — `monitor_work_area` is `None` |
| Minimize to the strip, and always show the strip | working | `src/gui/overlay.rs`, `src/gui/settings_tab.rs` |
| System light or dark theme | working | `src/gui/mod.rs` |
| Screen reader | working | `src/main.rs` (eframe accesskit) |
| One running copy (lock) | working | `src/instance.rs` |
| Settings saved across launches | working | `src/config.rs` |
| File open and save dialogs | working | `src/gui/mod.rs` (rfd, xdg-desktop-portal) |
| Open a link, a folder, or the releases page | missing | `src/gui/mod.rs` — `open_url` starts `explorer` |
| In-app updater and the daily update check | missing | `src/update.rs` — `updates_supported` is Windows-only outside tests |
| Open log folder | missing | `src/gui/settings_tab.rs` — the button sits inside the updater section, which is hidden on Linux |
