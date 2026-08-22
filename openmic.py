"""OpenMic: local RNNoise microphone processing and soundboard for Windows."""
import importlib.util
import json
import os
import threading
import winreg
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

import numpy as np
import sounddevice as sd
import soundfile as sf
from importlib.metadata import distribution

# pyrnnoise's high-level API pulls in broken deps; use its self-contained ctypes core.
_rn = os.path.join(str(distribution("pyrnnoise").locate_file("pyrnnoise")), "rnnoise.py")
_spec = importlib.util.spec_from_file_location("rnnoise", _rn)
rnnoise = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rnnoise)

FRAME = rnnoise.FRAME_SIZE  # 480 samples @ 48 kHz = 10 ms
SR = 48000
HERE = os.path.dirname(os.path.abspath(__file__))
SETTINGS = os.path.join(HERE, "settings.json")
__version__ = "0.1.0"
STARTUP_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
STARTUP_NAME = "OpenMic"
LEGACY_STARTUP_NAME = "Discord Denoiser"
STARTUP_COMMAND = (
    f'"{os.path.join(HERE, ".venv", "Scripts", "pythonw.exe")}" '
    f'"{os.path.join(HERE, "openmic.py")}"'
)


def startup_enabled():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, STARTUP_KEY) as key:
            for name in (STARTUP_NAME, LEGACY_STARTUP_NAME):
                try:
                    value, _ = winreg.QueryValueEx(key, name)
                except FileNotFoundError:
                    continue
                if value in {
                    STARTUP_COMMAND,
                    STARTUP_COMMAND.replace("openmic.py", "denoiser.py"),
                }:
                    return True
    except OSError:
        pass
    return False


def set_startup(enabled):
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, STARTUP_KEY) as key:
        for name in (STARTUP_NAME, LEGACY_STARTUP_NAME):
            try:
                winreg.DeleteValue(key, name)
            except FileNotFoundError:
                pass
        if enabled:
            winreg.SetValueEx(key, STARTUP_NAME, 0, winreg.REG_SZ, STARTUP_COMMAND)


def list_devices(output=False):
    """Deduped WASAPI device names for a direction."""
    key = "max_output_channels" if output else "max_input_channels"
    wasapi = next((i for i, h in enumerate(sd.query_hostapis()) if "WASAPI" in h["name"]), None)
    seen, names = set(), []
    for d in sd.query_devices():
        if d[key] > 0 and (wasapi is None or d["hostapi"] == wasapi) and d["name"] not in seen:
            seen.add(d["name"])
            names.append(d["name"])
    return names


def device_index(name, output=False):
    key = "max_output_channels" if output else "max_input_channels"
    for i, d in enumerate(sd.query_devices()):
        if d[key] > 0 and d["name"].startswith(name[:30]):
            return i
    return None


def load_clip(path):
    # ponytail: clips load into memory; stream if long-form audio becomes a use case.
    data, rate = sf.read(path, dtype="float32", always_2d=True)
    if len(data) == 0:
        raise ValueError("Audio file is empty")
    mono = np.mean(data, axis=1, dtype=np.float32)
    if rate != SR:
        length = max(1, round(len(mono) * SR / rate))
        mono = np.interp(
            np.arange(length, dtype=np.float64) * rate / SR,
            np.arange(len(mono), dtype=np.float64),
            mono,
        ).astype(np.float32)
    return np.clip(mono, -1.0, 1.0)


class Engine(threading.Thread):
    def __init__(self, in_idx, out_idx, monitor_idx):
        super().__init__(daemon=True)
        self.in_idx, self.out_idx, self.monitor_idx = in_idx, out_idx, monitor_idx
        self.stop_evt = threading.Event()
        self.strength = 1.0
        self.input_gain = 1.0
        self.output_gain = 1.0
        self.monitor_gain = 1.0
        self.gate_enabled = False
        self.gate_threshold = -50.0
        self.gate_gain = 1.0
        self.bypass = False
        self.mute = False
        self.monitor = False
        self.prob = 0.0
        self.in_peak = 0.0
        self.out_peak = 0.0
        self.error = None
        self.sound_lock = threading.Lock()
        self.sound = None
        self.sound_pos = 0
        self.sound_gain = 1.0

    def apply_processing(self, x, denoised, gate_release):
        if self.bypass:
            z = x.copy()
            self.gate_gain = 1.0
        else:
            z = x * (1.0 - self.strength) + denoised * self.strength
            if self.gate_enabled:
                level = 20.0 * np.log10(max(float(np.sqrt(np.mean(x * x))), 1e-6))
                self.gate_gain = 1.0 if level >= self.gate_threshold else \
                    self.gate_gain * gate_release
                z *= self.gate_gain
            else:
                self.gate_gain = 1.0
        z = np.clip(z * self.output_gain, -1.0, 1.0)
        if self.mute:
            z[:] = 0.0
        return z

    def play_sound(self, samples):
        with self.sound_lock:
            self.sound = np.asarray(samples, dtype=np.float32).reshape(-1)
            self.sound_pos = 0

    def stop_sound(self):
        with self.sound_lock:
            self.sound = None
            self.sound_pos = 0

    @property
    def sound_playing(self):
        with self.sound_lock:
            return self.sound is not None

    def mix_sound(self, mic):
        with self.sound_lock:
            if self.sound is None:
                return mic
            end = min(self.sound_pos + len(mic), len(self.sound))
            count = end - self.sound_pos
            mixed = mic.copy()
            mixed[:count] += self.sound[self.sound_pos:end] * self.sound_gain
            self.sound_pos = end
            if end == len(self.sound):
                self.sound = None
                self.sound_pos = 0
        return np.clip(mixed, -1.0, 1.0)

    def run(self):
        state = rnnoise.create()
        silence = np.zeros((FRAME, 1), dtype=np.int16)
        gate_release = float(np.exp(-FRAME / (SR * 0.15)))
        try:
            with sd.InputStream(device=self.in_idx, channels=1, samplerate=SR,
                                dtype="int16", blocksize=FRAME) as inp, \
                 sd.OutputStream(device=self.out_idx, channels=1, samplerate=SR,
                                 dtype="int16", blocksize=FRAME) as out, \
                 sd.OutputStream(device=self.monitor_idx, channels=1, samplerate=SR,
                                 dtype="int16", blocksize=FRAME) as mon:
                while not self.stop_evt.is_set():
                    data, _ = inp.read(FRAME)
                    raw = data[:, 0].astype(np.float32) / 32768.0
                    self.in_peak = float(np.max(np.abs(raw)))
                    x = np.clip(raw * self.input_gain, -1.0, 1.0)
                    y16, prob = rnnoise.process_mono_frame(state, x)
                    self.prob = float(prob)
                    y = y16.astype(np.float32) / 32768.0

                    z = self.mix_sound(self.apply_processing(x, y, gate_release))
                    self.out_peak = float(np.max(np.abs(z)))
                    pcm = (z * 32767).astype("int16").reshape(-1, 1)
                    out.write(pcm)
                    if self.monitor:
                        monitor_pcm = (np.clip(z * self.monitor_gain, -1.0, 1.0)
                                       * 32767).astype("int16").reshape(-1, 1)
                        mon.write(monitor_pcm)
                    else:
                        mon.write(silence)
        except Exception as e:
            self.error = str(e)
        finally:
            rnnoise.destroy(state)


class App:
    def __init__(self, root):
        self.root = root
        root.title(f"OpenMic {__version__}")
        root.configure(bg="#0b1020")
        root.resizable(False, False)

        style = ttk.Style(root)
        style.theme_use("clam")
        style.configure(".", font=("Segoe UI", 10))
        style.configure("App.TFrame", background="#0b1020")
        style.configure("Card.TFrame", background="#111827")
        style.configure("TLabel", background="#111827", foreground="#e5e7eb")
        style.configure("Header.TLabel", background="#0b1020", foreground="#f8fafc",
                        font=("Segoe UI Semibold", 22))
        style.configure("Subheader.TLabel", background="#0b1020", foreground="#94a3b8")
        style.configure("Status.TLabel", background="#0b1020", foreground="#94a3b8",
                        font=("Segoe UI Semibold", 10))
        style.configure("Card.TLabelframe", background="#111827", bordercolor="#263247",
                        lightcolor="#263247", darkcolor="#263247", relief="solid")
        style.configure("Card.TLabelframe.Label", background="#111827", foreground="#7dd3fc",
                        font=("Segoe UI Semibold", 10))
        style.configure("TButton", background="#1f2937", foreground="#e5e7eb",
                        bordercolor="#334155", padding=(12, 7))
        style.map("TButton", background=[("active", "#334155")])
        style.configure("Accent.TButton", background="#22c55e", foreground="#052e16",
                        bordercolor="#22c55e", font=("Segoe UI Semibold", 10))
        style.map("Accent.TButton", background=[("active", "#4ade80")])
        style.configure("Danger.TButton", background="#7f1d1d", foreground="#fecaca",
                        bordercolor="#991b1b")
        style.map("Danger.TButton", background=[("active", "#991b1b")])
        style.configure("TCheckbutton", background="#111827", foreground="#d1d5db")
        style.map("TCheckbutton", background=[("active", "#111827")])
        style.configure("Footer.TCheckbutton", background="#0b1020", foreground="#94a3b8")
        style.map("Footer.TCheckbutton", background=[("active", "#0b1020")])
        style.configure("TCombobox", fieldbackground="#0f172a", background="#1e293b",
                        foreground="#f8fafc", arrowcolor="#94a3b8", bordercolor="#334155")
        style.configure("TScale", background="#111827", troughcolor="#1e293b")
        style.configure("Vertical.TScrollbar", background="#1e293b",
                        troughcolor="#0f172a", arrowcolor="#94a3b8",
                        bordercolor="#334155", lightcolor="#1e293b", darkcolor="#1e293b")
        style.map("Vertical.TScrollbar", background=[("active", "#334155")])
        style.configure("TNotebook", background="#0b1020", borderwidth=0)
        style.configure("TNotebook.Tab", background="#111827", foreground="#94a3b8",
                        padding=(18, 9), borderwidth=0)
        style.map("TNotebook.Tab", background=[("selected", "#1e293b")],
                  foreground=[("selected", "#f8fafc")])
        style.configure("Input.Horizontal.TProgressbar", background="#38bdf8",
                        troughcolor="#1e293b", bordercolor="#1e293b")
        style.configure("Output.Horizontal.TProgressbar", background="#22c55e",
                        troughcolor="#1e293b", bordercolor="#1e293b")
        root.option_add("*TCombobox*Listbox.background", "#0f172a")
        root.option_add("*TCombobox*Listbox.foreground", "#f8fafc")
        root.option_add("*TCombobox*Listbox.selectBackground", "#2563eb")

        self.inputs = list_devices()
        self.outputs = list_devices(output=True)
        self.engine = None
        self._save_job = None
        try:
            with open(SETTINGS) as f:
                cfg = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            cfg = {}

        cable = next((n for n in self.outputs if "CABLE Input" in n), None)
        try:
            default_monitor = sd.query_devices(sd.default.device[1])["name"]
        except Exception:
            default_monitor = self.outputs[0] if self.outputs else ""

        self.dev_var = tk.StringVar(value=cfg.get("mic") or (self.inputs[0] if self.inputs else ""))
        self.out_var = tk.StringVar(value=cfg.get("out") or cable or (self.outputs[0] if self.outputs else ""))
        self.mon_out_var = tk.StringVar(value=cfg.get("monitor_out") or default_monitor)
        self.strength_var = tk.DoubleVar(value=cfg.get("strength", 100))
        self.input_gain_var = tk.DoubleVar(value=cfg.get("input_gain", 0))
        self.output_gain_var = tk.DoubleVar(value=cfg.get("output_gain", 0))
        self.gate_var = tk.BooleanVar(value=cfg.get("gate", False))
        self.gate_threshold_var = tk.DoubleVar(value=cfg.get("gate_threshold", -50))
        self.bypass_var = tk.BooleanVar(value=cfg.get("bypass", False))
        self.mute_var = tk.BooleanVar(value=cfg.get("mute", False))
        self.mon_var = tk.BooleanVar(value=cfg.get("monitor", False))
        self.mon_volume_var = tk.DoubleVar(value=cfg.get("monitor_volume", 100))
        self.sound_volume_var = tk.DoubleVar(value=cfg.get("sound_volume", 80))
        self.sound_paths = [p for p in cfg.get("sounds", []) if os.path.isfile(p)]
        start_with_windows = startup_enabled()
        if start_with_windows:
            set_startup(True)
        self.start_windows_var = tk.BooleanVar(value=start_with_windows)
        self.auto_start_var = tk.BooleanVar(value=cfg.get("auto_start", True))

        shell = ttk.Frame(root, style="App.TFrame", padding=16)
        shell.grid(sticky="nsew")
        header = ttk.Frame(shell, style="App.TFrame")
        header.grid(row=0, column=0, sticky="ew", pady=(0, 12))
        ttk.Label(header, text="OpenMic", style="Header.TLabel").grid(row=0, column=0, sticky="w")
        ttk.Label(header, text="Clean voice. Instant sounds. Fully local.",
                  style="Subheader.TLabel").grid(row=1, column=0, sticky="w")
        ttk.Label(header, text=f"v{__version__}", style="Subheader.TLabel").grid(
            row=0, column=1, rowspan=2, sticky="e")
        header.columnconfigure(0, weight=1)

        notebook = ttk.Notebook(shell, width=690, height=485)
        notebook.grid(row=1, column=0)
        mic_tab = ttk.Frame(notebook, style="App.TFrame", padding=12)
        sound_tab = ttk.Frame(notebook, style="App.TFrame", padding=12)
        notebook.add(mic_tab, text="Microphone")
        notebook.add(sound_tab, text="Soundboard")

        routing = ttk.LabelFrame(mic_tab, text="ROUTING", style="Card.TLabelframe", padding=10)
        routing.grid(row=0, column=0, sticky="ew")
        for row, text in enumerate(("Microphone", "Processed output", "Monitor output")):
            ttk.Label(routing, text=text).grid(row=row, column=0, sticky="w", padx=(0, 8))
        self.dev_box = ttk.Combobox(routing, textvariable=self.dev_var, values=self.inputs,
                                    state="readonly", width=51)
        self.out_box = ttk.Combobox(routing, textvariable=self.out_var, values=self.outputs,
                                    state="readonly", width=51)
        self.mon_out_box = ttk.Combobox(routing, textvariable=self.mon_out_var,
                                        values=self.outputs, state="readonly", width=51)
        for row, box in enumerate((self.dev_box, self.out_box, self.mon_out_box)):
            box.grid(row=row, column=1, padx=6, pady=3)
            box.bind("<<ComboboxSelected>>", lambda _event: self.schedule_save())
        self.refresh_btn = ttk.Button(routing, text="Refresh", command=self.refresh_devices)
        self.refresh_btn.grid(row=0, column=2, rowspan=3, padx=(6, 0), sticky="ns")

        processing = ttk.LabelFrame(mic_tab, text="PROCESSING", style="Card.TLabelframe",
                                    padding=10)
        processing.grid(row=1, column=0, pady=(10, 0), sticky="ew")

        def add_scale(row, text, var, low, high):
            ttk.Label(processing, text=text).grid(row=row, column=0, sticky="w")
            scale = ttk.Scale(processing, from_=low, to=high, variable=var,
                              command=lambda _: self.apply_controls(), length=390)
            scale.grid(row=row, column=1, padx=8, pady=3)
            value = ttk.Label(processing, width=8, anchor="e")
            value.grid(row=row, column=2)
            return scale, value

        self.strength_scale, self.strength_value = add_scale(
            0, "Noise reduction", self.strength_var, 0, 100)
        self.input_gain_scale, self.input_gain_value = add_scale(
            1, "Input gain", self.input_gain_var, -12, 24)
        self.output_gain_scale, self.output_gain_value = add_scale(
            2, "Output gain", self.output_gain_var, -24, 12)
        self.gate_scale, self.gate_value = add_scale(
            3, "Gate threshold", self.gate_threshold_var, -80, -20)

        checks = ttk.Frame(processing, style="Card.TFrame")
        checks.grid(row=4, column=0, columnspan=3, sticky="ew", pady=(7, 0))
        ttk.Checkbutton(checks, text="Noise gate", variable=self.gate_var,
                        command=self.apply_controls).grid(row=0, column=0, padx=(0, 18))
        ttk.Checkbutton(checks, text="Bypass reduction", variable=self.bypass_var,
                        command=self.apply_controls).grid(row=0, column=1, padx=(0, 18))
        ttk.Checkbutton(checks, text="Mute microphone", variable=self.mute_var,
                        command=self.apply_controls).grid(row=0, column=2)
        ttk.Button(checks, text="Reset", command=self.reset_processing).grid(
            row=0, column=3, padx=(30, 0))

        levels = ttk.LabelFrame(mic_tab, text="MONITOR & LEVELS", style="Card.TLabelframe",
                                padding=10)
        levels.grid(row=2, column=0, pady=(10, 0), sticky="ew")
        ttk.Checkbutton(levels, text="Headphone monitor", variable=self.mon_var,
                        command=self.apply_controls).grid(row=0, column=0, sticky="w")
        ttk.Label(levels, text="Volume").grid(row=0, column=1, padx=(16, 4))
        ttk.Scale(levels, from_=0, to=100, variable=self.mon_volume_var,
                  command=lambda _: self.apply_controls(), length=180).grid(row=0, column=2)
        self.mon_volume_value = ttk.Label(levels, width=6, anchor="e")
        self.mon_volume_value.grid(row=0, column=3)
        ttk.Label(levels, text="Input").grid(row=1, column=0, sticky="w", pady=(8, 0))
        self.in_meter = ttk.Progressbar(levels, style="Input.Horizontal.TProgressbar",
                                        length=500, maximum=100)
        self.in_meter.grid(row=1, column=1, columnspan=3, padx=(8, 0), pady=(8, 0))
        ttk.Label(levels, text="Output").grid(row=2, column=0, sticky="w", pady=(5, 0))
        self.out_meter = ttk.Progressbar(levels, style="Output.Horizontal.TProgressbar",
                                         length=500, maximum=100)
        self.out_meter.grid(row=2, column=1, columnspan=3, padx=(8, 0), pady=(5, 0))

        sounds = ttk.LabelFrame(sound_tab, text="SOUND CLIPS", style="Card.TLabelframe",
                                padding=12)
        sounds.grid(row=0, column=0, sticky="nsew")
        ttk.Label(sounds, text="Play clips directly into your processed microphone output."
                  ).grid(row=0, column=0, columnspan=2, sticky="w", pady=(0, 9))
        list_frame = ttk.Frame(sounds, style="Card.TFrame")
        list_frame.grid(row=1, column=0, columnspan=2, sticky="nsew")
        self.sound_list = tk.Listbox(
            list_frame, height=14, width=67, bg="#0f172a", fg="#e5e7eb",
            selectbackground="#2563eb", selectforeground="#ffffff", relief="flat",
            highlightthickness=1, highlightbackground="#334155",
            font=("Segoe UI", 10), activestyle="none")
        scrollbar = ttk.Scrollbar(list_frame, orient="vertical", command=self.sound_list.yview)
        self.sound_list.configure(yscrollcommand=scrollbar.set)
        self.sound_list.grid(row=0, column=0, sticky="nsew")
        scrollbar.grid(row=0, column=1, sticky="ns")
        self.sound_list.bind("<Double-Button-1>", lambda _event: self.play_selected_sound())
        self.sound_list.bind("<Return>", lambda _event: self.play_selected_sound())
        self.sound_list.bind("<Delete>", lambda _event: self.remove_sound())

        sound_buttons = ttk.Frame(sounds, style="Card.TFrame")
        sound_buttons.grid(row=2, column=0, columnspan=2, sticky="ew", pady=(10, 0))
        ttk.Button(sound_buttons, text="Add clips", command=self.add_sounds).grid(row=0, column=0)
        ttk.Button(sound_buttons, text="Play", style="Accent.TButton",
                   command=self.play_selected_sound).grid(row=0, column=1, padx=7)
        ttk.Button(sound_buttons, text="Stop", command=self.stop_sound).grid(row=0, column=2)
        ttk.Button(sound_buttons, text="Remove", style="Danger.TButton",
                   command=self.remove_sound).grid(row=0, column=3, padx=7)

        ttk.Label(sounds, text="Sound volume").grid(row=3, column=0, sticky="w", pady=(14, 0))
        volume_row = ttk.Frame(sounds, style="Card.TFrame")
        volume_row.grid(row=3, column=1, sticky="e", pady=(14, 0))
        ttk.Scale(volume_row, from_=0, to=100, variable=self.sound_volume_var,
                  command=lambda _: self.apply_controls(), length=300).grid(row=0, column=0)
        self.sound_volume_value = ttk.Label(volume_row, width=6, anchor="e")
        self.sound_volume_value.grid(row=0, column=1, padx=(6, 0))
        self.sound_status = ttk.Label(sounds, text="Ready")
        self.sound_status.grid(row=4, column=0, columnspan=2, sticky="w", pady=(12, 0))
        ttk.Label(sounds, text="Supported formats depend on libsndfile (WAV, FLAC, OGG, "
                  "and MP3 on current Windows wheels).", foreground="#94a3b8").grid(
                      row=5, column=0, columnspan=2, sticky="w", pady=(6, 0))
        self.update_sound_list()

        footer = ttk.Frame(shell, style="App.TFrame")
        footer.grid(row=2, column=0, sticky="ew", pady=(12, 0))
        self.btn = ttk.Button(footer, text="Start OpenMic", style="Accent.TButton",
                              command=self.toggle)
        self.btn.grid(row=0, column=0, ipadx=18)
        self.status = ttk.Label(footer, text="Stopped", style="Status.TLabel", width=34)
        self.status.grid(row=0, column=1, padx=12, sticky="w")
        ttk.Checkbutton(footer, text="Start with Windows", style="Footer.TCheckbutton",
                        variable=self.start_windows_var, command=self.toggle_startup).grid(
                            row=1, column=0, pady=(8, 0), sticky="w")
        ttk.Checkbutton(footer, text="Start processing automatically",
                        style="Footer.TCheckbutton", variable=self.auto_start_var,
                        command=self.schedule_save).grid(
                            row=1, column=1, pady=(8, 0), sticky="w")

        self.apply_controls()
        root.protocol("WM_DELETE_WINDOW", self.close)
        root.after(100, self.tick)
        if self.auto_start_var.get():
            root.after(500, self.auto_start)


    def auto_start(self):
        if self.engine is None:
            self.toggle()

    def apply_controls(self):
        self.strength_value.config(text=f"{self.strength_var.get():.0f}%")
        self.input_gain_value.config(text=f"{self.input_gain_var.get():+.1f} dB")
        self.output_gain_value.config(text=f"{self.output_gain_var.get():+.1f} dB")
        self.gate_value.config(text=f"{self.gate_threshold_var.get():.0f} dB")
        self.mon_volume_value.config(text=f"{self.mon_volume_var.get():.0f}%")
        self.sound_volume_value.config(text=f"{self.sound_volume_var.get():.0f}%")
        self.gate_scale.configure(state="normal" if self.gate_var.get() else "disabled")
        if self.engine:
            self.engine.strength = self.strength_var.get() / 100.0
            self.engine.input_gain = 10.0 ** (self.input_gain_var.get() / 20.0)
            self.engine.output_gain = 10.0 ** (self.output_gain_var.get() / 20.0)
            self.engine.monitor_gain = self.mon_volume_var.get() / 100.0
            self.engine.sound_gain = self.sound_volume_var.get() / 100.0
            self.engine.gate_enabled = self.gate_var.get()
            self.engine.gate_threshold = self.gate_threshold_var.get()
            self.engine.bypass = self.bypass_var.get()
            self.engine.mute = self.mute_var.get()
            self.engine.monitor = self.mon_var.get()
        self.schedule_save()

    def schedule_save(self):
        if self._save_job:
            self.root.after_cancel(self._save_job)
        self._save_job = self.root.after(300, self.save)

    def toggle_startup(self):
        try:
            set_startup(self.start_windows_var.get())
            state = "enabled" if self.start_windows_var.get() else "disabled"
            self.status.config(text=f"Windows startup {state}", foreground="#22c55e")
        except OSError as e:
            self.start_windows_var.set(not self.start_windows_var.get())
            self.status.config(text=str(e)[:52], foreground="#f87171")

    def refresh_devices(self):
        self.inputs = list_devices()
        self.outputs = list_devices(output=True)
        self.dev_box.configure(values=self.inputs)
        self.out_box.configure(values=self.outputs)
        self.mon_out_box.configure(values=self.outputs)
        if self.dev_var.get() not in self.inputs and self.inputs:
            self.dev_var.set(self.inputs[0])
        if self.out_var.get() not in self.outputs and self.outputs:
            self.out_var.set(self.outputs[0])
        if self.mon_out_var.get() not in self.outputs and self.outputs:
            self.mon_out_var.set(self.outputs[0])
        self.schedule_save()

    def set_routing_enabled(self, enabled):
        state = "readonly" if enabled else "disabled"
        for box in (self.dev_box, self.out_box, self.mon_out_box):
            box.configure(state=state)
        self.refresh_btn.configure(state="normal" if enabled else "disabled")

    def reset_processing(self):
        self.strength_var.set(100)
        self.input_gain_var.set(0)
        self.output_gain_var.set(0)
        self.gate_var.set(False)
        self.gate_threshold_var.set(-50)
        self.bypass_var.set(False)
        self.mute_var.set(False)
        self.mon_volume_var.set(100)
        self.apply_controls()

    def update_sound_list(self):
        self.sound_list.delete(0, tk.END)
        for path in self.sound_paths:
            self.sound_list.insert(tk.END, os.path.basename(path))
        if self.sound_paths:
            self.sound_list.selection_set(0)

    def add_sounds(self):
        paths = filedialog.askopenfilenames(
            parent=self.root,
            title="Add sound clips",
            filetypes=[
                ("Audio files", "*.wav *.flac *.ogg *.mp3 *.aiff *.aif"),
                ("All files", "*.*"),
            ],
        )
        invalid = []
        for path in paths:
            if path in self.sound_paths:
                continue
            try:
                sf.info(path)
            except Exception:
                invalid.append(os.path.basename(path))
            else:
                self.sound_paths.append(path)
        self.update_sound_list()
        self.schedule_save()
        if invalid:
            messagebox.showerror("Unsupported audio", "Could not read:\n" + "\n".join(invalid))

    def remove_sound(self):
        selected = self.sound_list.curselection()
        if not selected:
            return
        del self.sound_paths[selected[0]]
        self.update_sound_list()
        self.schedule_save()

    def play_selected_sound(self):
        if not self.engine:
            self.sound_status.config(text="Start OpenMic before playing a clip",
                                     foreground="#fbbf24")
            return
        selected = self.sound_list.curselection()
        if not selected:
            self.sound_status.config(text="Select a clip first", foreground="#fbbf24")
            return
        path = self.sound_paths[selected[0]]
        try:
            samples = load_clip(path)
        except Exception as e:
            messagebox.showerror("Could not play clip", str(e), parent=self.root)
            return
        self.engine.play_sound(samples)
        self.playing_name = os.path.basename(path)
        self.sound_status.config(text=f"Playing · {self.playing_name}", foreground="#22c55e")

    def stop_sound(self):
        if self.engine:
            self.engine.stop_sound()
        self.playing_name = None
        self.sound_status.config(text="Ready", foreground="#e5e7eb")

    def toggle(self):
        if self.engine:
            self.save()
            self.engine.stop_evt.set()
            self.engine.join(timeout=2)
            self.engine = None
            self.btn.config(text="Start OpenMic")
            self.status.config(text="Stopped", foreground="#94a3b8")
            self.sound_status.config(text="Ready", foreground="#e5e7eb")
            self.set_routing_enabled(True)
            return

        idx = device_index(self.dev_var.get())
        out = device_index(self.out_var.get(), output=True)
        monitor = device_index(self.mon_out_var.get(), output=True)
        if idx is None or out is None or monitor is None:
            self.status.config(text="Device not found", foreground="#f87171")
            return
        if self.mon_var.get() and monitor == out:
            self.status.config(text="Monitor output must differ", foreground="#f87171")
            return

        self.engine = Engine(idx, out, monitor)
        self.apply_controls()
        self.engine.start()
        self.btn.config(text="Stop OpenMic")
        self.status.config(text="Starting…", foreground="#fbbf24")
        self.set_routing_enabled(False)

    @staticmethod
    def meter_percent(peak):
        db = 20.0 * np.log10(max(peak, 1e-6))
        return min(100.0, max(0.0, (db + 60.0) * 100.0 / 60.0))

    def tick(self):
        eng = self.engine
        if eng:
            if eng.error:
                self.engine = None
                self.btn.config(text="Start OpenMic")
                self.status.config(text=eng.error[:52], foreground="#f87171")
                self.set_routing_enabled(True)
            else:
                self.in_meter["value"] = self.meter_percent(eng.in_peak)
                self.out_meter["value"] = self.meter_percent(eng.out_peak)
                self.status.config(text=f"Running · voice {eng.prob * 100:.0f}%",
                                   foreground="#22c55e")
                if getattr(self, "playing_name", None) and not eng.sound_playing:
                    self.playing_name = None
                    self.sound_status.config(text="Ready", foreground="#e5e7eb")
        else:
            self.in_meter["value"] = 0
            self.out_meter["value"] = 0
        self.root.after(100, self.tick)

    def save(self):
        if self._save_job:
            self.root.after_cancel(self._save_job)
            self._save_job = None
        cfg = {
            "mic": self.dev_var.get(),
            "out": self.out_var.get(),
            "monitor_out": self.mon_out_var.get(),
            "strength": round(self.strength_var.get(), 1),
            "input_gain": round(self.input_gain_var.get(), 1),
            "output_gain": round(self.output_gain_var.get(), 1),
            "gate": bool(self.gate_var.get()),
            "gate_threshold": round(self.gate_threshold_var.get(), 1),
            "bypass": bool(self.bypass_var.get()),
            "mute": bool(self.mute_var.get()),
            "monitor": bool(self.mon_var.get()),
            "monitor_volume": round(self.mon_volume_var.get(), 1),
            "sound_volume": round(self.sound_volume_var.get(), 1),
            "sounds": self.sound_paths,
            "auto_start": bool(self.auto_start_var.get()),
        }
        with open(SETTINGS, "w") as f:
            json.dump(cfg, f)

    def close(self):
        self.save()
        if self.engine:
            self.engine.stop_evt.set()
            self.engine.join(timeout=1)
        self.root.destroy()

if __name__ == "__main__":
    r = tk.Tk()
    App(r)
    r.mainloop()
