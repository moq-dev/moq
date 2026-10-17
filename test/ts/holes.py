#!/usr/bin/env python3
"""Find the media a capture is missing, from its own decode timestamps.

    ./holes.py capture.ts

Every audio and video stream of a generated clip has a constant frame duration, so the step
between consecutive decode timestamps is the same throughout. A step past one and a half
frames is a hole: media the round-trip lost, whether the path dropped it, the relay evicted
it, or the exporter missed its deadline. A stream that stops for good leaves no step behind,
so each must also present as late as the others: the clip's streams are cut together, so
each one's last frame ends within its own frame plus the longest stream frame of the
latest. The compliance checks grade only the access units that arrived, so a capture
missing most of its video still passes them.

This reads the capture alone, independently of what the exporter reports, and needs only
ffprobe. A real capture's streams need not keep a constant step, so it grades generated clips.

Exit status is 0 when no stream has a hole or ends early, 1 otherwise.
"""

import json
import subprocess
import sys
from collections import Counter
from fractions import Fraction


def holes(dts: list[int]) -> tuple[int, list[tuple[int, int]]]:
    """The stream's frame step, and the (dts, step) of each step past one and a half of it."""
    steps = [b - a for a, b in zip(dts, dts[1:])]
    if not steps:
        return 0, []
    frame = Counter(steps).most_common(1)[0][0]
    return frame, [(a, b - a) for a, b in zip(dts, dts[1:]) if 2 * (b - a) > 3 * frame]


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "stream=index,codec_type,codec_name,time_base:packet=stream_index,dts,pts",
         "-of", "json", sys.argv[1]],
        check=True, capture_output=True, text=True,
    )
    report = json.loads(probe.stdout)
    streams = {s["index"]: s for s in report["streams"] if s.get("codec_type") in ("audio", "video")}
    if not streams:
        sys.exit(f"error: {sys.argv[1]} has no audio or video stream")
    dts: dict[int, list[int]] = {index: [] for index in streams}
    end: dict[int, Fraction] = {}
    for packet in report.get("packets", []):
        index = packet["stream_index"]
        if index in dts and "dts" in packet:
            dts[index].append(int(packet["dts"]))
            pts = Fraction(int(packet.get("pts", packet["dts"]))) * Fraction(streams[index]["time_base"])
            end[index] = max(end.get(index, pts), pts)
    frames = {index: holes(dts[index])[0] * Fraction(streams[index]["time_base"]) for index in streams}
    for index in end:
        end[index] += frames[index]
    latest = max(end.values(), default=Fraction(0))
    longest = max(frames.values())

    print("### media holes (capture decode timestamps)")
    failed = False
    for index, stream in sorted(streams.items()):
        base = Fraction(stream["time_base"])
        frame, found = holes(dts[index])
        ms = lambda ticks: float(ticks * base * 1000)  # noqa: E731
        label = f"stream {index} {stream['codec_name']}"
        if frame == 0:
            print(f"  FAIL  {label}: fewer than two frames")
            failed = True
            continue
        missing = sum(step - frame for _, step in found)
        short = latest - end[index]
        early = short > frames[index] + longest
        verdict = "FAIL" if found or early else "PASS"
        failed |= bool(found) or early
        print(f"  {verdict}  {label}: {len(dts[index])} frames of {ms(frame):.2f} ms, "
              f"{len(found)} holes, {ms(missing):.0f} ms missing, ends {float(short) * 1000:.0f} ms before the latest")
        for at, step in found[:5]:
            print(f"        at {ms(at) / 1000:.3f}s: {ms(step):.0f} ms step")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
