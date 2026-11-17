"""Compares the port's per-frame activity with libopus's SILK VAD flag, per segment of the signal.

Usage: python3 agree.py <port q8 file> <encoder output file>
"""

import sys

SEGMENTS = [
    ("speech", 0, 15),
    ("room tone", 15, 20),
    ("fan alone", 20, 25),
    ("speech over fan", 25, 35),
    ("room tone", 35, 40),
    ("quiet talker", 40, 55),
    ("room tone", 55, 60),
]

port = [int(line) >= 13 for line in open(sys.argv[1])]
opus = [tuple(int(x) for x in line.split()) for line in open(sys.argv[2])]

print("| Segment | Frames | Port active | libopus VAD active | Agree | libopus DTX |")
print("| --- | --- | --- | --- | --- | --- |")
total = agree_total = 0
for name, start, end in SEGMENTS:
    frames = range(start * 50, min(end * 50, len(port), len(opus)))
    ours = sum(port[i] for i in frames)
    flags = [opus[i][0] for i in frames]
    theirs = sum(1 for f in flags if f == 1)
    celt = sum(1 for f in flags if f == -1)
    agree = sum(1 for i in frames if opus[i][0] != -1 and port[i] == (opus[i][0] == 1))
    dtx = sum(opus[i][1] for i in frames)
    total += len(frames) - celt
    agree_total += agree
    note = f" ({celt} CELT)" if celt else ""
    print(f"| {name} {start}-{end} s | {len(frames)} | {ours} | {theirs}{note} | {100 * agree / max(1, len(frames) - celt):.1f}% | {dtx} |")
print(f"Overall agreement: {100 * agree_total / total:.1f}% of {total} SILK frames")
