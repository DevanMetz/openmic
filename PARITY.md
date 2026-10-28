# Linux / Windows parity

Status of each Windows user-facing feature on Linux in this tree: `working`, `partial`, `missing`, or `not possible`.

| Feature | Linux | Code |
|---|---|---|
| DeepFilterNet 3 and RNNoise, voice gate, rumble filter, gains, level gate, bypass, mute, presets | working | `src/denoise.rs`, `src/dsp.rs`, `src/gui/mic_tab.rs` |
| Live scope and the voice rows | working | `src/gui/mic_tab.rs`, `src/viz.rs` |
| Microphone, processed output, and headphone monitor routing, with live changes and reconnect | working | `src/engine.rs`, `src/gui/routes.rs` |
| Devices opened at their native format and resampled to the 48 kHz DSP core | working | `src/engine.rs`, `src/resample.rs` |
| 10 ms PulseAudio buffers and clock-drift correction | working | `src/engine.rs` |
| Real-time priority for audio threads | partial | `src/engine.rs`, `Cargo.toml` — the processing thread calls `promote_current_thread_to_real_time`, but `audio_thread_priority` is built without dbus, so Linux does not ask RealtimeKit and the thread stays at normal priority |
| Virtual cable (null sink and remap source), removed on exit | working | `src/virtual_mic.rs` |
| Virtual cable at 48 kHz with a short buffer | partial | `src/virtual_mic.rs` — the null sink keeps the daemon's default rate and buffer |
| Virtual microphone becomes the default source while OpenMic runs, and the previous default is restored | working | `src/default_mic.rs` |
| Separate communications default device | not possible | `src/default_mic.rs` — PulseAudio has one default source, so console and communications both follow it |
| First-run checklist, including creating the virtual microphone | working | `src/gui/mic_tab.rs`, `src/virtual_mic.rs` |
| Record the microphone | working | `src/record.rs` |
| Record computer audio from an output's monitor source | partial | `src/record.rs` — only input names that start with the English prefix "Monitor of " |
| Test my mic (raw and cleaned, never into the virtual cable) | working | `src/playback.rs`, `src/gui/mic_test.rs` |
| Soundboard pads, trim, search, overlap, drag-and-drop reorder, WAV/FLAC/OGG/MP3/AIFF | working | `src/gui/clips.rs`, `src/decode.rs`, `src/gui/sound_tab.rs` |
| Drag clips onto the window | working | `src/gui/mod.rs` |
| Global hotkeys on X11 (mute, bypass, stop clips, dictation, pads) | working | `src/hotkeys.rs` |
| Global hotkeys on Wayland | missing | `src/hotkeys.rs` — `global-hotkey` grabs keys on X11 only |
| Dictation transcription on this computer, including model download | working | `src/dictation.rs` |
| Dictation typed into the focused app | missing | `src/dictation.rs` — `type_text` stops before any key is sent; the transcript is shown and can be copied |
| Notification area, close-to-tray, and the tray menu | missing | `src/tray.rs`, `src/gui/settings_tab.rs` — `Tray::new` fails, the checkbox is hidden, and closing the window quits |
| A second launch shows the running window | missing | `src/instance.rs` — the lock is exclusive and `on_show` does nothing |
| Start on login | partial | `src/config.rs` — writes an XDG autostart entry that runs `--minimized`; any existing file counts as enabled, including one the desktop disabled |
| Start processing automatically, and wait for a device that connects after launch | working | `src/gui/mod.rs`, `src/config.rs` |
| Window on X11 | working | `Cargo.toml`, `src/main.rs` — GLX, then EGL, on the glutin eframe already links |
| Window on Wayland | working | `Cargo.toml`, `src/main.rs` — EGL `wl_egl_window`. The compositor draws the frame, or winit's fallback frame does when the compositor asks for client-side decorations |
| Restored window position kept on a connected monitor | partial | `src/gui/mod.rs` — `on_a_monitor` is always true, so a saved position on an unplugged monitor is reused |
| Initial window size fitted to the work area | partial | `src/main.rs` — `work_area_size` is `None`, so the saved size is used as-is |
| Always-on-top strip on X11 | partial | `src/gui/overlay.rs` — `with_always_on_top` asks for `_NET_WM_STATE_ABOVE` once. `raise_meter` does not run again when another window is focused |
| Always-on-top strip on Wayland | not possible | `src/gui/overlay.rs` — xdg-shell has no stacking request. A layer-shell surface would replace the strip. The compositor's keep-above rule is the equivalent |
| Strip hover hints kept inside the monitor work area | partial | `src/gui/overlay.rs` — `monitor_work_area` is `None` |
| Minimize to the strip, and always show the strip | working | `src/gui/overlay.rs`, `src/gui/settings_tab.rs` |
| System light or dark theme | partial | winit reports no system theme on X11 (`theme` is `None`) and on Wayland only a theme this process already set, so egui stays on its dark fallback |
| Screen reader | working | `Cargo.toml` (eframe `accesskit`, `accesskit_unix`) |
| One running copy (lock) | working | `src/instance.rs` |
| Settings saved across launches | working | `src/config.rs` |
| File open and save dialogs | working | `src/gui/mod.rs` (rfd, xdg-desktop-portal) |
| Open a link, a folder, or the releases page | missing | `src/gui/mod.rs` — `open_url` starts `explorer` |
| In-app updater and the daily update check | missing | `src/update.rs` — `updates_supported` is Windows-only outside tests |
| Open log folder | missing | `src/gui/settings_tab.rs` — the button sits inside the updater section, which is hidden on Linux |
| Log file | working | `src/logfile.rs` |
