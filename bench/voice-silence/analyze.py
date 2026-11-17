"""Grades a drive.ts run: bytes per segment of the input, and speech the gate dropped.

The reference is the PCM the gated encoder's own capture handed it, on the same timestamps, so a
10 ms window of captured speech (above -45 dBFS) that no sent frame covers was dropped by the gate.

Usage: uv run --with numpy --with soundfile python analyze.py <input.wav> <run prefix>
"""

import json
import sys

import numpy as np
import soundfile as sf

SEGMENTS = [
    ("speech", 0, 15),
    ("room tone", 15, 20),
    ("fan alone", 20, 25),
    ("speech over fan", 25, 35),
    ("room tone", 35, 40),
    ("quiet talker", 40, 55),
    ("room tone", 55, 60),
]
RATE = 48_000
WIN = 480  # 10 ms


def env(x):
    n = len(x) // WIN
    return 10 * np.log10(np.mean(x[: n * WIN].reshape(n, WIN) ** 2, axis=1) + 1e-12)


source, _ = sf.read(sys.argv[1], dtype="float32")
prefix = sys.argv[2]
run = json.load(open(prefix + ".json"))
voice, _ = sf.read(prefix + "-voice.wav", dtype="float32")
captured, _ = sf.read(prefix + "-input.wav", dtype="float32")

# Where the recording starts in the looped input: correlate 10 ms envelopes.
src_env = np.clip(env(np.concatenate([source, source])), -80, 0)
cap_env = np.clip(env(captured), -80, 0)
n = min(len(cap_env), 5000)
scores = [np.corrcoef(cap_env[:n], src_env[o : o + n])[0, 1] for o in range(0, len(source) // WIN)]
offset = int(np.argmax(scores)) / 100
print(f"recording starts {offset * 1000:.0f} ms into the input (envelope correlation {max(scores):.3f})")


def segment_of(t):
    t = (t + offset) % 60
    for name, start, end in SEGMENTS:
        if start <= t < end:
            return f"{name} {start}-{end} s"
    return None


rows = {}
for kind in ("voice", "auto"):
    for ts, size in run[kind]["frames"]:
        key = segment_of(ts / 1e6)
        row = rows.setdefault(key, {"voice": [0, 0, 0], "auto": [0, 0, 0]})[kind]
        if size == 0:
            row[2] += 1
        else:
            row[0] += 1
            row[1] += size

print(f"\n{len(captured) / RATE:.0f} s recorded; per input segment:\n")
print("| Segment | Ungated frames, bytes | Voice frames, bytes | Declared ends | Bytes saved |")
print("| --- | --- | --- | --- | --- |")
for name, start, end in SEGMENTS:
    key = f"{name} {start}-{end} s"
    if key in rows:
        v, a = rows[key]["voice"], rows[key]["auto"]
        print(f"| {key} | {a[0]}, {a[1]} | {v[0]}, {v[1]} | {v[2]} | {100 * (1 - v[1] / max(1, a[1])):.0f}% |")
tv = sum(r["voice"][1] for r in rows.values())
ta = sum(r["auto"][1] for r in rows.values())
fv = sum(r["voice"][0] for r in rows.values())
fa = sum(r["auto"][0] for r in rows.values())
print(f"| total | {fa}, {ta} | {fv}, {tv} | {sum(r['voice'][2] for r in rows.values())} | {100 * (1 - tv / ta):.0f}% |")

ce = env(captured)
covered = np.zeros(len(ce), dtype=bool)
frames = run["voice"]["frames"]
sent = sorted(ts / 1e3 for ts, size in frames if size > 0)
for t in sent:
    start = int(round(t / 10))
    covered[max(0, start) : max(0, start + 2)] = True
speech = ce > -45
# The first frames before the gated encoder's first output are outside the comparison.
speech[: int(sent[0] / 10)] = False
dropped = np.flatnonzero(speech & ~covered)
print(f"\ncaptured speech windows (above -45 dBFS, 10 ms): {speech.sum()}, dropped by the gate: {len(dropped)}")
for i in dropped[:30]:
    t = i * 10
    after = next((s for s in sent if s > t), None)
    when = f"{after - t:.0f} ms before the gate reopened" if after is not None else "never reopened"
    print(f"  {t} ms ({segment_of(t / 1000)}): {ce[i]:.1f} dBFS, {when}")

# Comfort noise: at each declared end a player fills with uniform noise at the quietest 20 ms block's
# RMS of the audio it played since the last resume, capped at -60 dBFS (doc/concept/audio-jitter.md).
# Compare that with what the capture held during the pause.
order = sorted(frames)
BLOCK = 960
pairs = []
start = None
for i, (ts, size) in enumerate(order):
    if size > 0:
        if start is None:
            start = ts
        continue
    if start is not None:
        a, b = int(start / 1e6 * RATE), int(ts / 1e6 * RATE)
        seg = voice[a:b]
        blocks = [np.mean(seg[k : k + BLOCK] ** 2) for k in range(0, len(seg) - BLOCK + 1, BLOCK)]
        resume = next((t for t, s in order[i + 1 :] if s > 0), None)
        if blocks and resume is not None:
            level = min(np.sqrt(min(blocks)), 0.001)
            pause = captured[b : int(resume / 1e6 * RATE)]
            if len(pause) > RATE // 10:
                pairs.append((20 * np.log10(level + 1e-12), 10 * np.log10(np.mean(pause**2) + 1e-12), (resume - ts) / 1e6))
    start = None
print(f"\npauses: {len(pairs)}; comfort noise vs captured background (dBFS), pause length:")
for c, bg, dur in pairs:
    print(f"  comfort {c:.0f}, background {bg:.0f}, {dur:.1f} s")
