"""Self-check: RNNoise crushes stationary noise and flags signal as speech."""
import importlib.util
import os

import numpy as np
from importlib.metadata import distribution

_rn = os.path.join(str(distribution("pyrnnoise").locate_file("pyrnnoise")), "rnnoise.py")
spec = importlib.util.spec_from_file_location("rnnoise", _rn)
rn = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rn)

rng = np.random.default_rng(0)
st = rn.create()
noise = (rng.standard_normal(48000 * 2) * 0.1).astype(np.float32)
out, prob_noise = [], 0.0
for i in range(0, len(noise), 480):
    y16, p = rn.process_mono_frame(st, noise[i:i + 480])
    out.append(y16.astype(np.float32) / 32768)
    prob_noise = float(p)
y = np.concatenate(out)
rms_in = np.sqrt(np.mean(noise**2))
rms_out = np.sqrt(np.mean(y**2))
print(f"noise RMS in={rms_in:.4f} out={rms_out:.5f} "
      f"reduction={20*np.log10(rms_in/max(rms_out, 1e-9)):.1f} dB, speech_prob={prob_noise:.3f}")
assert rms_out < rms_in * 0.3, "denoiser failed to reduce stationary noise"
assert prob_noise < 0.2, "speech probability high on pure noise"

st2 = rn.create()
tone = (0.3 * np.sin(2 * np.pi * 220 * np.arange(48000) / 48000)).astype(np.float32)
prob_tone = max(float(p) for i in range(0, len(tone), 480)
                for _, p in [rn.process_mono_frame(st2, tone[i:i + 480])])
print(f"speech prob on tone: {prob_tone:.3f}")
assert prob_tone > 0.4, "speech probability did not rise on signal"
print("OK")
