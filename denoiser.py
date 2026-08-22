"""Mic -> RNNoise -> VB-Cable router for Discord.

Select "CABLE Output (VB-Audio Virtual Cable)" as Discord's input device,
and turn OFF Discord's own noise suppression (double processing sounds bad).
"""
import importlib.util
import json
import os
import threading
import winreg
import tkinter as tk
from tkinter import ttk

import numpy as np
import sounddevice as sd
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
__version__ = "0.0.1"
STARTUP_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
STARTUP_NAME = "Discord Denoiser"
STARTUP_COMMAND = (
    f'"{os.path.join(HERE, ".venv", "Scripts", "pythonw.exe")}" '
    f'"{os.path.join(HERE, "denoiser.py")}"'
)


def startup_enabled():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, STARTUP_KEY) as key:
            value, _ = winreg.QueryValueEx(key, STARTUP_NAME)
        return value == STARTUP_COMMAND
    except OSError:
        return False


def set_startup(enabled):
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER, STARTUP_KEY) as key:
        if enabled:
            winreg.SetValueEx(key, STARTUP_NAME, 0, winreg.REG_SZ, STARTUP_COMMAND)
        else:
            try:
                winreg.DeleteValue(key, STARTUP_NAME)
            except FileNotFoundError:
                pass


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

                    z = self.apply_processing(x, y, gate_release)
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
        root.title(f"Discord Denoiser {__version__}")
        root.resizable(False, False)
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
        self.start_windows_var = tk.BooleanVar(value=startup_enabled())
        self.auto_start_var = tk.BooleanVar(value=cfg.get("auto_start", True))

        frm = ttk.Frame(root, padding=10)
        frm.grid(sticky="nsew")

        routing = ttk.LabelFrame(frm, text="Routing", padding=8)
        routing.grid(row=0, column=0, sticky="ew")
        ttk.Label(routing, text="Microphone").grid(row=0, column=0, sticky="w")
        self.dev_box = ttk.Combobox(routing, textvariable=self.dev_var, values=self.inputs,
                                    state="readonly", width=49)
        self.dev_box.grid(row=0, column=1, padx=6, pady=2)
        ttk.Label(routing, text="Processed output").grid(row=1, column=0, sticky="w")
        self.out_box = ttk.Combobox(routing, textvariable=self.out_var, values=self.outputs,
                                    state="readonly", width=49)
        self.out_box.grid(row=1, column=1, padx=6, pady=2)
        ttk.Label(routing, text="Monitor output").grid(row=2, column=0, sticky="w")
        self.mon_out_box = ttk.Combobox(routing, textvariable=self.mon_out_var,
                                        values=self.outputs, state="readonly", width=49)
        self.mon_out_box.grid(row=2, column=1, padx=6, pady=2)
        self.refresh_btn = ttk.Button(routing, text="Refresh", command=self.refresh_devices)
        for box in (self.dev_box, self.out_box, self.mon_out_box):
            box.bind("<<ComboboxSelected>>", lambda _event: self.schedule_save())
        self.refresh_btn.grid(row=0, column=2, rowspan=3, padx=(4, 0), sticky="ns")

        processing = ttk.LabelFrame(frm, text="Processing", padding=8)
        processing.grid(row=1, column=0, pady=(8, 0), sticky="ew")

        def add_scale(row, text, var, low, high):
            ttk.Label(processing, text=text).grid(row=row, column=0, sticky="w")
            scale = ttk.Scale(processing, from_=low, to=high, variable=var,
                              command=lambda _: self.apply_controls(), length=340)
            scale.grid(row=row, column=1, padx=6, pady=2)
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

        checks = ttk.Frame(processing)
        checks.grid(row=4, column=0, columnspan=3, sticky="w", pady=(5, 0))
        ttk.Checkbutton(checks, text="Noise gate", variable=self.gate_var,
                        command=self.apply_controls).grid(row=0, column=0, padx=(0, 14))
        ttk.Checkbutton(checks, text="Bypass reduction", variable=self.bypass_var,
                        command=self.apply_controls).grid(row=0, column=1, padx=(0, 14))
        ttk.Checkbutton(checks, text="Mute", variable=self.mute_var,
                        command=self.apply_controls).grid(row=0, column=2)

        preview = ttk.LabelFrame(frm, text="Preview", padding=8)
        preview.grid(row=2, column=0, pady=(8, 0), sticky="ew")
        ttk.Checkbutton(preview, text="Monitor (use headphones)", variable=self.mon_var,
                        command=self.apply_controls).grid(row=0, column=0, sticky="w")
        ttk.Label(preview, text="Volume").grid(row=0, column=1, padx=(18, 4))
        ttk.Scale(preview, from_=0, to=100, variable=self.mon_volume_var,
                  command=lambda _: self.apply_controls(), length=220).grid(row=0, column=2)
        self.mon_volume_value = ttk.Label(preview, width=6, anchor="e")
        self.mon_volume_value.grid(row=0, column=3)

        meters = ttk.LabelFrame(frm, text="Levels", padding=8)
        meters.grid(row=3, column=0, pady=(8, 0), sticky="ew")
        ttk.Label(meters, text="Input").grid(row=0, column=0, sticky="w")
        self.in_meter = ttk.Progressbar(meters, length=485, maximum=100)
        self.in_meter.grid(row=0, column=1, padx=6, pady=2)
        ttk.Label(meters, text="Output").grid(row=1, column=0, sticky="w")
        self.out_meter = ttk.Progressbar(meters, length=485, maximum=100)
        self.out_meter.grid(row=1, column=1, padx=6, pady=2)

        actions = ttk.Frame(frm)
        actions.grid(row=4, column=0, pady=(8, 0), sticky="ew")
        self.btn = ttk.Button(actions, text="Start", command=self.toggle)
        self.btn.grid(row=0, column=0, ipadx=55)
        ttk.Button(actions, text="Reset processing", command=self.reset_processing).grid(
            row=0, column=1, padx=8)
        self.status = ttk.Label(actions, text="Stopped", foreground="#666", width=38)
        self.status.grid(row=0, column=2, sticky="e")
        ttk.Checkbutton(actions, text="Start with Windows", variable=self.start_windows_var,
                        command=self.toggle_startup).grid(row=1, column=0, pady=(6, 0), sticky="w")
        ttk.Checkbutton(actions, text="Start processing automatically",
                        variable=self.auto_start_var, command=self.schedule_save).grid(
                            row=1, column=1, columnspan=2, padx=(12, 0), pady=(6, 0), sticky="w")

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
        self.gate_scale.configure(state="normal" if self.gate_var.get() else "disabled")
        if self.engine:
            self.engine.strength = self.strength_var.get() / 100.0
            self.engine.input_gain = 10.0 ** (self.input_gain_var.get() / 20.0)
            self.engine.output_gain = 10.0 ** (self.output_gain_var.get() / 20.0)
            self.engine.monitor_gain = self.mon_volume_var.get() / 100.0
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
            self.status.config(text=f"Windows startup {state}", foreground="green")
        except OSError as e:
            self.start_windows_var.set(not self.start_windows_var.get())
            self.status.config(text=str(e)[:52], foreground="red")

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

    def toggle(self):
        if self.engine:
            self.save()
            self.engine.stop_evt.set()
            self.engine.join(timeout=2)
            self.engine = None
            self.btn.config(text="Start")
            self.status.config(text="Stopped", foreground="#666")
            self.set_routing_enabled(True)
            return

        idx = device_index(self.dev_var.get())
        out = device_index(self.out_var.get(), output=True)
        monitor = device_index(self.mon_out_var.get(), output=True)
        if idx is None or out is None or monitor is None:
            self.status.config(text="Device not found", foreground="red")
            return
        if self.mon_var.get() and monitor == out:
            self.status.config(text="Monitor output must differ", foreground="red")
            return

        self.engine = Engine(idx, out, monitor)
        self.apply_controls()
        self.engine.start()
        self.btn.config(text="Stop")
        self.status.config(text="Starting…", foreground="#996600")
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
                self.btn.config(text="Start")
                self.status.config(text=eng.error[:52], foreground="red")
                self.set_routing_enabled(True)
            else:
                self.in_meter["value"] = self.meter_percent(eng.in_peak)
                self.out_meter["value"] = self.meter_percent(eng.out_peak)
                self.status.config(text=f"Running · voice {eng.prob * 100:.0f}%",
                                   foreground="green")
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
