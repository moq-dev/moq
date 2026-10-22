"""Judge the audio an interop subscriber decoded against the tone its publisher sent.

Every publisher sends a sine per channel (channel i at TONES[i] Hz, from interop.sh), and every
subscriber decodes its first --audio-ms of audio to two files under one prefix:

    PREFIX.txt  "<sample rate> <channels>", then one "<timestamp us> <samples per channel>" line per frame
    PREFIX.pcm  the decoded samples, f32le interleaved, in the same order

This checks the layout matches the tone list, the frames are contiguous in time, and each channel
carries its own tone, using a Goertzel filter at that channel's frequency. A garbled decode (wrong
channel order or sample rate), silence, or a timestamp gap fails. A misapplied pre-skip does
not: it only shifts a steady tone's phase, which this does not anchor to the timestamps.

    python3 tone.py --tone 440,660 /run/rust-python-audio

Prints one `audio ok:` line, or `audio check failed: <why>` and exits 1.
"""

import argparse
import math
import struct
import sys
from pathlib import Path

# Decoder warm-up and an encoder's fade-in land here; judged audio starts after it.
SKIP_MS = 100
# The least audio judged after the skip. Long enough that a tone 1% off frequency (a 44.1 kHz
# stream read as 48 kHz is 9% off) has drifted out of phase with the filter many times over.
WINDOW_MS = 400
# A frame starting further than this from where the previous one ended is a gap (or an overlap).
# Timestamps are microseconds, and some ingest paths stamp milliseconds.
CONTINUITY_US = 1000
# The tone is generated at 0.5 amplitude (0.35 RMS); a decoder emitting silence or near-silence fails.
MIN_RMS = 0.05
# Share of a channel's energy at its own frequency. A clean decode measures above 0.95; a channel
# swap, a wrong sample rate, or another channel's tone bleeding in measures far lower.
MIN_SHARE = 0.8


class Failure(Exception):
    pass


def goertzel_share(samples, rate, hz):
    """Fraction of the samples' energy at `hz`: 1.0 for a pure tone at that frequency."""
    coeff = 2 * math.cos(2 * math.pi * hz / rate)
    s1 = s2 = 0.0
    energy = 0.0
    for x in samples:
        s1, s2 = x + coeff * s1 - s2, s1
        energy += x * x
    if energy == 0:
        return 0.0
    power = s1 * s1 + s2 * s2 - coeff * s1 * s2
    return 2 * power / (len(samples) * energy)


def dominant_hz(samples, rate):
    """The strongest frequency in 20 Hz steps, to say what a failing channel carries instead."""
    probe = samples[: rate // 10]
    return max(range(100, 4000, 20), key=lambda hz: goertzel_share(probe, rate, hz))


def check(prefix, tones):
    lines = Path(f"{prefix}.txt").read_text().split("\n")
    try:
        rate, channels = (int(v) for v in lines[0].split())
        frames = [(int(ts), int(count)) for ts, count in (line.split() for line in lines[1:] if line.strip())]
    except ValueError as err:
        raise Failure(f"malformed frame log: {err}") from None
    if channels != len(tones):
        raise Failure(f"decoded {channels} channels, the publisher sent {len(tones)}")
    if rate <= 0 or not frames:
        raise Failure("no audio decoded")

    for (ts, count), (next_ts, _) in zip(frames, frames[1:]):
        expected = ts + count * 1_000_000 / rate
        if abs(next_ts - expected) > CONTINUITY_US:
            raise Failure(f"timestamp gap: a frame at {next_ts}us, the previous one ended at {expected:.0f}us")

    pcm = Path(f"{prefix}.pcm").read_bytes()
    total = sum(count for _, count in frames)
    if len(pcm) != total * channels * 4:
        raise Failure(f"{len(pcm)} PCM bytes for {total} logged samples of {channels} channels")

    skip = rate * SKIP_MS // 1000
    if total - skip < rate * WINDOW_MS // 1000:
        raise Failure(f"only {total * 1000 // rate}ms decoded, need {SKIP_MS + WINDOW_MS}ms")
    samples = struct.unpack(f"<{total * channels}f", pcm)

    shares = []
    for channel, hz in enumerate(tones):
        data = samples[skip * channels + channel :: channels]
        rms = math.sqrt(sum(x * x for x in data) / len(data))
        if not math.isfinite(rms) or rms < MIN_RMS:
            raise Failure(f"channel {channel} is silent (rms {rms:.4f})")
        share = goertzel_share(data, rate, hz)
        if share < MIN_SHARE:
            raise Failure(
                f"wrong tone: channel {channel} carries ~{dominant_hz(data, rate)} Hz, not {hz} Hz "
                f"({share:.2f} of its energy at {hz} Hz)"
            )
        shares.append((share, rms))

    window = (total - skip) * 1000 // rate
    summary = ", ".join(f"{hz} Hz {share:.2f} (rms {rms:.2f})" for hz, (share, rms) in zip(tones, shares))
    return f"{channels} ch @ {rate} Hz, {window}ms judged, {len(frames)} contiguous frames: {summary}"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tone", required=True, help="comma-separated frequency of each channel, in Hz")
    parser.add_argument("prefix", help="the subscriber's --audio-out prefix")
    args = parser.parse_args()
    tones = [int(hz) for hz in args.tone.split(",")]
    try:
        print(f"audio ok: {check(args.prefix, tones)}")
    except (Failure, OSError) as err:
        print(f"audio check failed: {err}")
        sys.exit(1)


if __name__ == "__main__":
    main()
