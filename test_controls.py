"""Hardware-free check for the live frame controls."""
import numpy as np

from openmic import Engine, FRAME

engine = Engine(0, 0, 0)
release = 0.9
quiet = np.full(FRAME, 0.001, dtype=np.float32)  # -60 dBFS
clean = np.full(FRAME, 0.25, dtype=np.float32)
engine.strength = 1.0
engine.gate_enabled = True
engine.gate_threshold = -40.0

for _ in range(20):
    gated = engine.apply_processing(quiet, clean, release)
assert engine.gate_gain < 0.13
assert np.max(gated) < 0.04, "gate did not close on quiet input"

loud = np.full(FRAME, 0.1, dtype=np.float32)  # -20 dBFS
reopened = engine.apply_processing(loud, clean, release)
assert engine.gate_gain == 1.0
assert np.allclose(reopened, clean), "gate did not reopen immediately"

engine.bypass = True
engine.output_gain = 2.0
bypassed = engine.apply_processing(loud, np.zeros(FRAME, np.float32), release)
assert np.allclose(bypassed, 0.2), "bypass or output gain changed the dry signal"

engine.mute = True
muted = engine.apply_processing(loud, clean, release)
assert np.count_nonzero(muted) == 0, "mute did not silence the frame"
print("gate close/reopen, bypass, gain, and mute OK")
