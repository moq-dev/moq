#!/usr/bin/env python3
"""Generate an unflagged encoder switch: the whole multiplex steps onto another clock at one packet.

    ./splice.py out.ts [back|forward] [--duration 20] [--bitrate 10000000] [--step 16844.5]

Two encodes of the harness clip (H.264 and AAC under PAT, PMT and SDT), muxed alike at a constant
rate on clocks `--step` seconds apart and with their GOPs out of phase, are cut into one stream at
one packet, halfway through. There every PID changes encode and breaks its continuity counter once;
the video and the audio are cut mid-PES on both sides; the second encode's video joins mid-GOP,
its first keyframe 0.08 s after the switch in decode order; the tables are byte-identical either
side; and no packet sets `discontinuity_indicator`. It is what a contribution feed carries when it
switches between two encoders whose clocks are not locked: nothing in the stream announces it, so
an importer that reads the stream's time sees the clock jump by `--step` mid-group. Each encode is
paced as a broadcast multiplexer paces it, so either side of the switch passes the compliance
checks alone, and whatever the whole fails is the switch's.

`back` puts the second encode's clock `--step` below the first's, `forward` above it.
"""

import argparse
import bisect
import subprocess
import sys
import tempfile
from pathlib import Path

SIZE = 188
WRAP = 1 << 33
NULL = 0x1FFF
# The lower of the two clocks, so a step back stays well above zero.
BASE = 33155.5
# The second encode's first keyframe after the switch, in decode order, and how close to it the
# search must land.
KEY = 0.08
KEY_SLACK = 0.02
# The second encode starts this far into the programme and cuts its GOPs at another length (frames
# at 25 Hz), so the two encodes' keyframes do not line up at the switch.
SKEW = 0.28
GOP_A, GOP_B = 24, 30
FPS = 25
# The video's share of the multiplex.
VIDEO = 0.7
# How much faster than its average rate a PID may arrive: the video's transport buffer drains at
# 1.2 times its HRD rate.
PACE = 1.1
# How long before it decodes each audio PES arrives: the AAC buffer holds about 0.2 s.
AUDIO_LEAD = 0.15


def pid(p: bytes) -> int:
    return (p[1] & 0x1F) << 8 | p[2]


def pusi(p: bytes) -> bool:
    return bool(p[1] & 0x40)


def payload_at(p: bytes) -> int | None:
    afc = (p[3] >> 4) & 3
    if not afc & 1:
        return None
    off = 5 + p[4] if afc & 2 else 4
    return off if off < SIZE else None


def flags(p: bytes) -> int:
    """The adaptation field's flag byte, or 0."""
    return p[5] if (p[3] & 0x20) and p[4] else 0


def pcr(p: bytes) -> int | None:
    """The PCR's 90 kHz base."""
    if not flags(p) & 0x10 or p[4] < 7:
        return None
    return p[6] << 25 | p[7] << 17 | p[8] << 9 | p[9] << 1 | p[10] >> 7


def pcr27(p: bytes) -> int | None:
    """The full PCR, in 27 MHz ticks."""
    base = pcr(p)
    return None if base is None else base * 300 + ((p[10] & 1) << 8 | p[11])


def stamp(p: bytes) -> int | None:
    """The decode time of a PES starting in this packet (its DTS, else its PTS), when its header is
    whole."""
    off = payload_at(p)
    if off is None or not pusi(p) or p[off : off + 3] != b"\0\0\1" or not p[off + 7] & 0x80:
        return None
    b = p[off + 14 : off + 19] if p[off + 7] & 0x40 else p[off + 9 : off + 14]
    return ((b[0] >> 1) & 7) << 30 | b[1] << 22 | (b[2] >> 1) << 15 | b[3] << 7 | b[4] >> 1


def section(p: bytes) -> bytes:
    off = payload_at(p)
    return p[off + 1 + p[off] :]


def streams(pkts: list[bytearray]) -> tuple[int, int, list[tuple[int, int]]]:
    """The PMT PID, its PCR PID and its (elementary PID, stream type) pairs, from the first PAT and
    PMT."""
    pat = section(next(p for p in pkts if pid(p) == 0 and pusi(p)))
    pmt_pid = next(
        (pat[i + 2] & 0x1F) << 8 | pat[i + 3]
        for i in range(8, 3 + ((pat[1] & 0x0F) << 8 | pat[2]) - 4, 4)
        if pat[i] << 8 | pat[i + 1]
    )
    s = section(next(p for p in pkts if pid(p) == pmt_pid and pusi(p)))
    pcr_pid = (s[8] & 0x1F) << 8 | s[9]
    i = 12 + ((s[10] & 0x0F) << 8 | s[11])
    end = 3 + ((s[1] & 0x0F) << 8 | s[2]) - 4
    es = []
    while i < end:
        es.append(((s[i + 1] & 0x1F) << 8 | s[i + 2], s[i]))
        i += 5 + ((s[i + 3] & 0x0F) << 8 | s[i + 4])
    return pmt_pid, pcr_pid, es


def encode(path: Path, offset: float, skew: float, gop: int, muxdelay: float, args) -> list[bytearray]:
    video = int(args.bitrate * VIDEO)
    cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y"]
    for graph in (f"testsrc=size=1280x720:rate={FPS}", "sine=frequency=1000:sample_rate=48000"):
        cmd += ["-ss", str(skew), "-t", str(args.duration), "-f", "lavfi", "-i", graph]
    cmd += [
        "-map", "0:v", "-map", "1:a",
        "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high", "-pix_fmt", "yuv420p",
        "-g", str(gop), "-keyint_min", str(gop), "-sc_threshold", "0", "-bf", "2",
        "-b:v", str(video), "-maxrate", str(video), "-bufsize", str(video), "-x264-params", "nal-hrd=cbr",
        "-c:a", "aac", "-b:a", "128k",
        "-f", "mpegts", "-muxrate", str(args.bitrate), "-pcr_period", "20", "-pes_payload_size", "0",
        "-muxdelay", str(muxdelay), "-output_ts_offset", str(offset), str(path),
    ]  # fmt: skip
    subprocess.run(cmd, check=True)
    data = path.read_bytes()
    return [bytearray(data[i : i + SIZE]) for i in range(0, len(data) - SIZE + 1, SIZE)]


def interleave(pkts: list[bytearray], es: list[int], audio: int) -> list[bytearray]:
    """Rebuild the multiplex as a broadcast multiplexer paces it, and re-stamp each PCR for its
    packet's new place at the constant rate. FFmpeg writes each PES whole, at the multiplex rate,
    faster than the T-STD drains a transport buffer, and sends the audio as far ahead as the
    video, more than its buffer holds. Here a PID's packets arrive at most PACE times its average
    rate, never earlier than FFmpeg put them, and each audio PES AUDIO_LEAD before it decodes;
    nulls fill the rest."""
    stamped = [(k, pcr27(p)) for k, p in enumerate(pkts) if pcr27(p) is not None]
    (k0, c0), (k1, c1) = stamped[0], stamped[-1]
    per = (c1 - c0) / (k1 - k0)  # 27 MHz ticks a packet
    slot = [float(k) for k in range(len(pkts))]
    for q in es:
        run = [k for k, p in enumerate(pkts) if pid(p) == q]
        gap = (run[-1] - run[0]) / (len(run) - 1) / PACE
        due = slot[run[0]]
        for prev, k in zip([None, *run], run):
            if q == audio and (t := stamp(pkts[k])) is not None:
                due = k0 + ((t - AUDIO_LEAD * 90000) * 300 - c0) / per
            slot[k] = max(slot[k], due, 0 if prev is None else slot[prev] + gap)
    out: list[bytearray] = []
    for k in sorted((k for k, p in enumerate(pkts) if pid(p) != NULL), key=slot.__getitem__):
        while len(out) < slot[k]:
            out.append(bytearray(b"\x47\x1f\xff\x10" + b"\xff" * (SIZE - 4)))
        out.append(pkts[k])
    for k, p in enumerate(out):
        if pcr27(p) is not None:
            c = c0 + round((k - k0) * per)
            base, ext = (c // 300) % WRAP, c % 300
            p[6:12] = bytes(
                [base >> 25 & 0xFF, base >> 17 & 0xFF, base >> 9 & 0xFF, base >> 1 & 0xFF]
                + [(base & 1) << 7 | 0x7E | ext >> 8, ext & 0xFF]
            )
    return out


def first_from(pkts: list[bytearray], i: int, want: int) -> int:
    return next(k for k in range(i, len(pkts)) if pid(pkts[k]) == want and payload_at(pkts[k]) is not None)


def seconds_in(pkts: list[bytearray], offset: float, video: int, at: float) -> int:
    """Index of the first packet whose PCR, less the encode's clock offset, is past `at`."""
    for k, p in enumerate(pkts):
        base = pcr(p)
        if pid(p) == video and base is not None and base / 90000 - offset >= at:
            return k
    sys.exit(f"splice: an encode ends before {at} s")


def splice(a: list[bytearray], b: list[bytearray], off_a: float, off_b: float, at: float) -> list[bytearray]:
    """`a` up to about `at` s, then `b` from about the same point of the programme."""
    _, video, es = streams(a)
    if streams(b)[1:] != (video, es):
        sys.exit("splice: the two encodes' PMTs differ")
    audio = next(q for q, _ in es if q != video)
    a, b = (interleave(x, [q for q, _ in es], audio) for x in (a, b))

    # The first encode stops before i, mid-PES on the video and the audio.
    i = seconds_in(a, off_a, video, at)
    while any(pusi(a[first_from(a, i, s)]) for s in (video, audio)):
        i += 1
    # The second takes over at j, mid-PES on both, its first whole video PES not a keyframe and its
    # first keyframe KEY later in decode order.
    starts = [k for k, p in enumerate(b) if pid(p) == video and pusi(p)]
    keys = [k for k in starts if flags(b[k]) & 0x40]
    j = seconds_in(b, off_b - SKEW - GOP_B / FPS / 2, video, at)
    while True:
        nv, na = first_from(b, j, video), first_from(b, j, audio)
        if not pusi(b[nv]) and not pusi(b[na]):
            k = starts[bisect.bisect_left(starts, nv)]
            key = keys[bisect.bisect_left(keys, nv)]
            if k != key and abs((stamp(b[key]) - stamp(b[k])) / 90000 - KEY) < KEY_SLACK:
                break
        j += 1

    # Every PID breaks its continuity counter once, as at a real switch.
    last = {pid(p): p[3] & 0x0F for p in a[:i] if payload_at(p) is not None}
    seen = set()
    for p in b[j:]:
        q = pid(p)
        if q == NULL or q not in last or payload_at(p) is None or q in seen:
            continue
        seen.add(q)
        cc = p[3] & 0x0F
        if cc in (last[q], (last[q] + 1) & 0x0F):
            bump = (last[q] + 9 - cc) & 0x0F
            for r in b[j:]:
                if pid(r) == q:
                    r[3] = (r[3] & 0xF0) | ((r[3] + bump) & 0x0F)
    out = a[:i] + b[j:]
    if any(flags(p) & 0x80 for p in out):
        sys.exit("splice: a packet sets discontinuity_indicator")
    print(f"splice at packet {i} of {len(out)}, {at:g} s in")
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("direction", nargs="?", choices=("back", "forward"), default="back")
    parser.add_argument("--duration", type=float, default=20.0)
    parser.add_argument("--bitrate", type=int, default=10_000_000)
    parser.add_argument("--step", type=float, default=16844.5, help="seconds between the two clocks")
    args = parser.parse_args()

    low, high = BASE, BASE + args.step
    off_a, off_b = (high, low) if args.direction == "back" else (low, high)
    with tempfile.TemporaryDirectory() as tmp:
        a = encode(Path(tmp) / "a.ts", off_a, 0, GOP_A, 0.7, args)
        b = encode(Path(tmp) / "b.ts", off_b, SKEW, GOP_B, 0.9, args)
    args.out.write_bytes(b"".join(splice(a, b, off_a, off_b, args.duration / 2)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
