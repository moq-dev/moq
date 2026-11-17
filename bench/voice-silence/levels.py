"""Prints the ungated output's RMS per second of a run, to see what the browser's processing did.

Usage: uv run --with numpy --with soundfile python levels.py <run prefix>
"""

import sys

import numpy as np
import soundfile as sf

x, rate = sf.read(sys.argv[1] + "-auto.wav", dtype="float32")
n = len(x) // rate
levels = [10 * np.log10(np.mean(x[i * rate : (i + 1) * rate] ** 2) + 1e-12) for i in range(n)]
print(" ".join(f"{level:.0f}" for level in levels))
