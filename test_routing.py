"""Hardware-free check for live route restart and clip preservation."""
import threading

import numpy as np

from openmic import App


class Widget:
    def __init__(self):
        self.values = {}

    def config(self, **values):
        self.values.update(values)


class StopEvent:
    def __init__(self):
        self.was_set = False

    def set(self):
        self.was_set = True


class OldEngine:
    def __init__(self):
        self.sound_lock = threading.Lock()
        self.sound = np.arange(8, dtype=np.float32)
        self.sound_pos = 3
        self.stop_evt = StopEvent()
        self.join_timeout = None

    def join(self, timeout):
        self.join_timeout = timeout

    def is_alive(self):
        return False


class NewEngine:
    def __init__(self):
        self.played = None

    def play_sound(self, samples):
        self.played = samples.copy()


app = App.__new__(App)
old = OldEngine()
new = NewEngine()
app.engine = old
app.status = Widget()
app.btn = Widget()


def start_engine():
    app.engine = new
    return True


app.start_engine = start_engine
assert app.restart_engine()
assert old.stop_evt.was_set, "old route was not stopped"
assert old.join_timeout == 2, "old route was not joined"
assert app.engine is new, "new route was not started"
assert np.array_equal(new.played, np.arange(3, 8, dtype=np.float32)), \
    "remaining soundboard samples were not preserved"
print("live route stop/start and soundboard continuation OK")
