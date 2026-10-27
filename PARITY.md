# Linux parity

Windows is the reference. Status describes this tree: `working`, `partial`, `missing`, or `not possible`.

| Feature | Linux | Code |
|---|---|---|
| DeepFilterNet 3 noise suppression | working | `src/denoise.rs` |
| RNNoise | working | `vendor/rnnoise`, `src/denoise.rs` |
| Voice gate | working | `src/dsp.rs` |
| Level gate (quiet cut-off) | working | `src/dsp.rs` |
| Rumble filter | working | `src/dsp.rs` |
| Input gain, output gain, noise-reduction strength | working | `src/dsp.rs`, `src/denoise.rs` |
| Bypass | working | `src/dsp.rs`, `src/gui/mod.rs` |
| Microphone mute | working | `src/engine.rs`, `src/gui/mod.rs` |
| Live scope and draggable voice rows | working | `src/viz.rs`, `src/gui/mic_tab.rs` |
| Processing presets, including saved ones | working | `src/config.rs`, `src/gui/mic_tab.rs` |
| Microphone, processed output, and headphone monitor selection | working | `src/gui/routes.rs`, `src/engine.rs` |
| Virtual cable for Discord and other apps | partial | `src/virtual_mic.rs` |
| Make that cable the default microphone while running | working | `src/default_mic.rs`, `src/gui/mic_tab.rs` |
| Live routing changes, clip playheads kept | working | `src/engine.rs` |
| Reconnect after a device is unplugged | working | `src/engine.rs`, `src/gui/mod.rs` |
| Wait for a saved device that connects after launch | working | `src/gui/mod.rs` |
| Steady latency and clock-drift correction on OpenMic's own streams | working | `src/engine.rs` |
| Audio threads in the Pro Audio / real-time class | partial | `src/engine.rs`, `Cargo.toml` |
| Headphone monitor | working | `src/engine.rs`, `src/playback.rs` |
| Test my mic (5 seconds, raw and cleaned, not into the cable) | working | `src/gui/mic_test.rs`, `src/playback.rs` |
| Global hotkeys (mute, bypass, stop clips, dictation, each pad) | partial | `src/hotkeys.rs` |
| Speech to text on this PC, including model download and removal | working | `src/dictation.rs` |
| Type the transcript into the focused app | missing | `src/dictation.rs` |
| Soundboard: add, play, pause, stop, reorder, search, overlap, volume, trim, star | working | `src/gui/sound_tab.rs`, `src/gui/clips.rs` |
| Drop audio files onto the window | working | `src/gui/mod.rs` |
| File dialogs for adding clips and saving a WAV | working | `src/gui/sound_tab.rs` |
| Record the microphone to a WAV | working | `src/record.rs` |
| Record computer audio from a playback device | partial | `src/record.rs` |
| Notification area: keep running when the window closes, tray menu | missing | `src/tray.rs`, `src/gui/settings_tab.rs` |
| Always-on-top strip | partial | `src/gui/overlay.rs` |
| Minimize to the strip | working | `src/gui/mod.rs` |
| Always show the strip | working | `src/gui/overlay.rs`, `src/config.rs` |
| Strip drag, resize, pad scroll, and hover hints | partial | `src/gui/overlay.rs` |
| Reopen at the saved window size and position | partial | `src/main.rs`, `src/gui/mod.rs` |
| Start on login | partial | `src/config.rs` |
| Start processing automatically | working | `src/config.rs`, `src/gui/mod.rs` |
| One copy at a time; launching again shows the running window | partial | `src/instance.rs` |
| In-app updates (daily check, download, verify, restart) | missing | `src/update.rs`, `src/gui/updates.rs` |
| Check for updates daily | missing | `src/gui/settings_tab.rs` |
| Open the releases page, log folder, and settings folder | missing | `src/gui/mod.rs` |
| Light and dark theme following the system | working | `src/gui/mod.rs` |
| Screen reader and keyboard navigation | working | `src/gui/mod.rs` |
| Settings saved atomically, with a recovery screen if the file will not load | working | `src/config.rs`, `src/gui/startup.rs` |
| Log file | working | `src/logfile.rs` |
| First-run setup checklist | working | `src/gui/mic_tab.rs` |

## Notes

- **Virtual cable.** `pactl` creates **OpenMic Virtual Cable** and **OpenMic Microphone**, and removes them on exit. The null sink keeps the server's default rate and buffer, often 44.1 kHz and about 2 seconds. OpenMic's own streams still request 10 ms.
- **Default microphone.** PulseAudio has one default source, not Windows' separate console and communications roles. OpenMic saves and restores that one source. That is the native equivalent.
- **Real-time priority.** The processing thread calls `promote_current_thread_to_real_time`, but `audio_thread_priority` is built without dbus, so on Linux the call leaves the thread at normal priority. Capture and playback callbacks are not raised. Windows joins the Pro Audio class.
- **Hotkeys.** X11 grabs work through `global-hotkey`. Wayland sessions do not: that crate has no Wayland backend. The native equivalent is the GlobalShortcuts portal. It is not implemented, so this is missing on Wayland and working on X11.
- **Dictation typing.** Linux keeps the transcript on the page to copy. Windows types it into the focused window.
- **Computer audio.** Recording opens a monitor source whose description is `Monitor of {output}`. PulseAudio translates that description, so a non-English label does not match and can be chosen as a microphone.
- **Notification area.** `Tray::new` fails. Closing the window quits. The close-to-tray setting is hidden. There is no tray icon to reopen a hidden window.
- **Strip and window placement.** The strip asks to stay on top, but nothing re-raises it after another window or the panel covers it, and Wayland has no standard always-on-top flag. Hover hints and the restored window are not fitted to a work area: `monitor_work_area` returns nothing, `on_a_monitor` accepts every position, and `work_area_size` returns nothing. A saved position can reopen off-screen.
- **Start on login.** OpenMic writes `~/.config/autostart/openmic.desktop` and treats any existing file as enabled, including one a desktop disabled. The command passes `--minimized`, which suppresses the strip, and there is no tray for that launch to hide in.
- **Second launch.** The lock file stops another process. It does not ask the running copy to show its window.
- **Updates and links.** The in-app updater only accepts `openmic-<tag>-windows-x64.exe`. On Linux the version number is a label, **Check for updates daily** is hidden, and so is **Open log folder**. Buttons that do open something run `explorer`, which is not installed. The recovery screen's **Open settings folder** uses that same path.
