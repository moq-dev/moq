#!/usr/bin/env python3
"""Find the media a capture is missing, from its timestamps against its generated source.

    ./holes.py capture.ts source.ts

Every audio and video stream of a generated clip has a constant frame duration, so the step
between consecutive decode timestamps is the same throughout. A capture's step past one and a
half of its source's is a hole: media the round-trip lost, whether the path dropped it, the relay evicted
it, or the exporter missed its deadline. A lost prefix or tail leaves no step behind, so each
source stream must also be in the capture, on its PID, ending where the source's does: the
export keeps the source's timestamps. It may start later, by up to JOIN: the subscriber joins
at the relay's latest group once the catalog is out, which skips the first second or two of
a generated clip on a clean path. The compliance checks grade only the access units that
arrived, so a capture missing most of its video still passes them.

This reads the captures alone, independently of what the exporter reports, and needs only
ffprobe. A real capture's streams need not keep a constant step, so it grades generated clips.

Exit status is 0 when every source stream is in the capture without a hole or a cut, 1 otherwise.
"""

import json
import os
import subprocess
import sys
from collections import Counter
from fractions import Fraction

# How much later than its source a stream may start, in seconds: the clean-path join plus a group.
JOIN = Fraction(3)


def step(dts: list[int]) -> int:
    """A stream's frame step: its most common decode-timestamp step, or 0 with fewer than two frames."""
    steps = [b - a for a, b in zip(dts, dts[1:])]
    return Counter(steps).most_common(1)[0][0] if steps else 0


def holes(dts: list[int], frame: int) -> list[tuple[int, int]]:
    """The (dts, step) of each step past one and a half frames."""
    return [(a, b - a) for a, b in zip(dts, dts[1:]) if 2 * (b - a) > 3 * frame]


Span = tuple[Fraction, Fraction, Fraction]


def cuts(source: Span, capture: Span, frame: Fraction) -> list[str]:
    """How a capture's (first PTS, last PTS, last DTS), in seconds, falls short of its source's."""
    # Both ends: losing trailing B pictures leaves the last PTS, and losing the last P picture the last DTS.
    late, short = capture[0] - source[0], max(source[1] - capture[1], source[2] - capture[2])
    found = []
    if late > JOIN:
        found.append(f"starts {float(late):.3f}s after its source, past the {float(JOIN)}s join")
    if 2 * short > frame:
        found.append(f"ends {float(short):.3f}s before its source")
    return found


def probe(path: str) -> dict[str, tuple[dict, list[int], Span]]:
    """Each audio and video stream by PID: the stream, its decode timestamps, and its span."""
    # Probe the whole file, not FFmpeg's first 5 MB: a stream whose first frame comes after that
    # is never described, so the second frame of its first PES repeats the first's timestamp
    # and every later one in that PES runs a frame behind, which reads as a hole.
    report = json.loads(subprocess.run(
        ["ffprobe", "-v", "error", "-probesize", str(os.path.getsize(path)), "-analyzeduration", str(2**62),
         "-show_entries", "stream=index,id,codec_type,codec_name,time_base:packet=stream_index,dts,pts",
         "-of", "json", path],
        check=True, capture_output=True, text=True,
    ).stdout)
    streams = {s["index"]: s for s in report["streams"] if s.get("codec_type") in ("audio", "video")}
    dts: dict[int, list[int]] = {index: [] for index in streams}
    pts: dict[int, list[int]] = {index: [] for index in streams}
    for packet in report.get("packets", []):
        index = packet["stream_index"]
        if index in dts and "dts" in packet:
            dts[index].append(int(packet["dts"]))
            pts[index].append(int(packet.get("pts", packet["dts"])))
    return {
        s["id"]: (s, dts[index], tuple(t * Fraction(s["time_base"]) for t in (min(pts[index]), max(pts[index]), dts[index][-1])))
        for index, s in streams.items() if pts[index]
    }


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    capture, source = probe(sys.argv[1]), probe(sys.argv[2])
    if not source:
        sys.exit(f"error: {sys.argv[2]} has no audio or video stream")

    print("### media holes (capture timestamps against the source)")
    failed = False
    for pid, (stream, source_dts, span) in sorted(source.items()):
        label = f"PID {pid} {stream['codec_name']}"
        # The source's step, not the capture's: a capture that kept every other frame has a step of its own.
        frame = step(source_dts)
        if frame == 0:
            sys.exit(f"error: {label} has fewer than two frames in {sys.argv[2]}")
        if pid not in capture:
            print(f"  FAIL  {label}: not in the capture")
            failed = True
            continue
        stream, dts, captured = capture[pid]
        base = Fraction(stream["time_base"])
        found = holes(dts, frame)
        cut = cuts(span, captured, frame * base)
        missing = sum(gap - frame for _, gap in found) * base
        verdict = "FAIL" if found or cut else "PASS"
        failed |= bool(found or cut)
        print(f"  {verdict}  {label}: {len(dts)} frames of {float(frame * base) * 1000:.2f} ms, "
              f"{len(found)} holes, {float(missing) * 1000:.0f} ms missing, "
              f"from {float(captured[0]):.3f}s to {float(captured[1]):.3f}s of {float(span[0]):.3f}s to {float(span[1]):.3f}s")
        for at, gap in found[:5]:
            print(f"        at {float(at * base):.3f}s: {float(gap * base) * 1000:.0f} ms step")
        for reason in cut:
            print(f"        {reason}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
