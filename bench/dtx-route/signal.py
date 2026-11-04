"""Build the 19 s, 48 kHz mono test signal and its segment map. Usage: signal.py <assets> <www>"""
import json, math, random, struct, sys, wave

RATE = 48000
assets, www = sys.argv[1], sys.argv[2]

def read_f32_wav(path):
    # `say` writes WAVE_FORMAT_IEEE_FLOAT, which the wave module refuses, so walk the chunks.
    data = open(path, "rb").read()
    assert data[:4] == b"RIFF" and data[8:12] == b"WAVE"
    i, fmt, pcm = 12, None, None
    while i < len(data):
        cid, size = data[i:i + 4], struct.unpack("<I", data[i + 4:i + 8])[0]
        body = data[i + 8:i + 8 + size]
        if cid == b"fmt ":
            fmt = struct.unpack("<HHIIHH", body[:16])
        elif cid == b"data":
            pcm = body
        i += 8 + size + (size & 1)
    tag, channels, rate, _, _, bits = fmt
    assert tag in (3, 0xFFFE) and channels == 1 and rate == RATE and bits == 32, fmt
    return list(struct.unpack(f"<{len(pcm) // 4}f", pcm))

def trim(x, floor=1e-4):
    start = next(i for i, v in enumerate(x) if abs(v) > floor)
    end = len(x) - next(i for i, v in enumerate(reversed(x)) if abs(v) > floor)
    return x[start:end]

def normalize(x, peak_db=-3.0):
    peak = max(abs(v) for v in x)
    gain = 10 ** (peak_db / 20) / peak
    return [v * gain for v in x]

rng = random.Random(7)
def noise(n, rms_db):
    # Uniform white noise has RMS amplitude/sqrt(3).
    a = 10 ** (rms_db / 20) * math.sqrt(3)
    return [rng.uniform(-a, a) for _ in range(n)]

def tone(n, db, f0=300.0, f1=3000.0):
    # A linear chirp: tonal like a beep, but aperiodic, so cross-correlation finds one alignment.
    a = 10 ** (db / 20)
    k = (f1 - f0) / (n / RATE)
    return [a * math.sin(2 * math.pi * (f0 * t + k * t * t / 2)) for t in (i / RATE for i in range(n))]

speech_a = normalize(trim(read_f32_wav(f"{assets}/a.wav")))
speech_b = normalize(trim(read_f32_wav(f"{assets}/b.wav")))

out, segments = [], []
def add(kind, samples, room=False):
    start = len(out)
    if room:
        floor = noise(len(samples), -60)
        samples = [s + f for s, f in zip(samples, floor)]
    out.extend(samples)
    segments.append({"kind": kind, "start": start, "end": len(out)})

def until(t):
    return int(t * RATE) - len(out)

add("tone", tone(RATE, -12))
add("silence-zero", [0.0] * until(3))
add("speech", speech_a, room=True)
add("silence-room", noise(until(10), -60))
add("noise", noise(RATE, -20))
add("silence-room", noise(until(13), -60))
add("speech", speech_b)
add("silence-zero", [0.0] * until(18))
add("tone", tone(RATE, -12))
assert len(out) == 19 * RATE, len(out)

open(f"{www}/signal.f32", "wb").write(struct.pack(f"<{len(out)}f", *out))
json.dump({"rate": RATE, "segments": segments}, open(f"{www}/segments.json", "w"), indent=1)
for s in segments:
    print(f"{s['kind']:13} {s['start'] / RATE:6.3f} - {s['end'] / RATE:6.3f}")
