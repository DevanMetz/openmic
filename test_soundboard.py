"""Hardware-free checks for clip loading and soundboard mixing."""
import os
import tempfile

import numpy as np
import soundfile as sf

from openmic import Engine, FRAME, load_clip

with tempfile.TemporaryDirectory() as tmp:
    path = os.path.join(tmp, "stereo.wav")
    stereo = np.column_stack((
        np.full(2400, 0.2, np.float32),
        np.full(2400, 0.4, np.float32),
    ))
    sf.write(path, stereo, 24000)
    loaded = load_clip(path)
    assert len(loaded) == 4800, "clip was not resampled to 48 kHz"
    assert np.allclose(loaded[100:-100], 0.3, atol=1e-3), "stereo was not mixed to mono"

engine = Engine(0, 0, 0)
engine.sound_gain = 0.5
engine.play_sound(np.full(FRAME + 120, 0.4, np.float32))
first = engine.mix_sound(np.zeros(FRAME, np.float32))
assert np.allclose(first, 0.2)
assert engine.sound_playing
second = engine.mix_sound(np.zeros(FRAME, np.float32))
assert np.allclose(second[:120], 0.2)
assert np.count_nonzero(second[120:]) == 0
assert not engine.sound_playing

engine.mute = True
muted_mic = engine.apply_processing(
    np.ones(FRAME, np.float32), np.ones(FRAME, np.float32), 0.9)
engine.play_sound(np.full(FRAME, 0.25, np.float32))
mixed = engine.mix_sound(muted_mic)
assert np.allclose(mixed, 0.125), "microphone mute also silenced the soundboard"
engine.stop_sound()
assert not engine.sound_playing
print("clip load/resample, playback, volume, completion, and mute isolation OK")
