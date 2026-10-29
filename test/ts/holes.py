#!/usr/bin/env python3
"""Find the media a capture is missing, from its timestamps against its generated source.

    ./holes.py capture.ts source.ts [--shed]

Every audio and video stream of a generated clip has a constant frame duration, so the step
between consecutive presentation timestamps, in presentation order, is the same throughout. A
capture's step past one and a half of its source's is a hole: media the round-trip lost, whether
the path dropped it, the relay evicted it, or the exporter missed its deadline. Presentation
timestamps, because the export keeps them while it derives each group's decode timestamps from
that group's frames alone: a reordered group's first frames decode just before its earliest
presentation, not a frame apart, which leaves a decode step at every group with nothing lost. A lost prefix or tail leaves no step behind, so each
source stream must also be in the capture, on its PID, ending where the source's does: the
export keeps the source's timestamps. It may start later, by up to JOIN: the subscriber joins
at the relay's latest group once the catalog is out, which skips the first second or two of
a generated clip on a clean path. The compliance checks grade only the access units that
arrived, so a capture missing most of its video still passes them.

Under `--shed` the path was narrower than the media, so losing media is expected and reported,
but only by whole groups: of each group of the source, from a sync frame to the next, the
capture keeps a prefix in decode order (all of it, part of it, or none) and never resumes
mid-group, so it also joins on a sync frame. Frames are matched by presentation timestamp, since the export derives each
picture's decode timestamp from the frames around it and a group cut short moves it. Legacy
audio's source timestamps undergo its microsecond wire conversion before comparison. Matching
keeps capture order: a frame reordered, repeated, or not in the source fails. The
source's last group still has to arrive whole, so a stream that stalled, or whose latency grew
past the exporter's delay, fails.

This reads the captures alone, independently of what the exporter reports, and needs only
ffprobe. A real capture's streams need not keep a constant step, so it grades generated clips.

Exit status is 0 when every source stream is in the capture without a hole (or, under
`--shed`, losing only whole groups) or a cut, 1 otherwise.
"""

import argparse
import json
import os
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from fractions import Fraction

# How much later than its source a stream may start, in seconds: the clean-path join plus a group.
JOIN = Fraction(3)


def step(times: list[int]) -> int:
    """A stream's frame step: its most common step between ascending timestamps, or 0 with fewer than two frames."""
    steps = [b - a for a, b in zip(times, times[1:])]
    return Counter(steps).most_common(1)[0][0] if steps else 0


def holes(times: list[int], frame: int) -> list[tuple[int, int]]:
    """The (timestamp, step) of each step past one and a half frames between ascending timestamps."""
    return [(a, b - a) for a, b in zip(times, times[1:]) if 2 * (b - a) > 3 * frame]


@dataclass
class Shed:
    """Which of a source stream's frames a capture lost, after the first one it kept."""

    missing: int  # frames lost
    mid_group: list[int]  # PTS of each frame kept after its group lost an earlier one
    tail: int  # frames of the source's last group lost
    unmatched: list[int]  # PTS of each captured frame past the last one in the source's order


def shed(pts: list[int], sync: list[bool], kept: list[int]) -> Shed:
    """Grade the capture's `kept` PTS, in its order, against the source's frames, in decode order with their sync flags."""
    result = Shed(missing=0, mid_group=[], tail=0, unmatched=[])
    joined = lost = False
    i = 0
    for t, starts in zip(pts, sync):
        if starts:
            lost = False
            result.tail = 0
        # A generated stream's PTS are unique, so matching greedily finds the capture as a subsequence if it is one.
        if i < len(kept) and kept[i] == t:
            i += 1
            joined = True
            if lost:
                result.mid_group.append(t)
        else:
            # Before the join too, so a capture that opens mid-group resumes mid-group.
            lost = True
            if joined:
                result.missing += 1
                result.tail += 1
    result.unmatched = kept[i:]
    return result


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


@dataclass
class Stream:
    """An audio or video stream's frames, in decode order."""

    info: dict  # ffprobe's description
    dts: list[int]
    pts: list[int]
    sync: list[bool]
    span: Span  # first PTS, last PTS and last DTS, in seconds


def probe(path: str) -> dict[str, Stream]:
    """Each audio and video stream, by PID."""
    # Probe the whole file, not FFmpeg's first 5 MB: a stream whose first frame comes after that
    # is never described, so the second frame of its first PES repeats the first's timestamp
    # and every later one in that PES runs a frame behind, which reads as a hole.
    report = json.loads(subprocess.run(
        ["ffprobe", "-v", "error", "-probesize", str(os.path.getsize(path)), "-analyzeduration", str(2**62),
         "-show_entries", "stream=index,id,codec_type,codec_name,time_base:packet=stream_index,dts,pts,flags",
         "-of", "json", path],
        check=True, capture_output=True, text=True,
    ).stdout)
    streams = {s["index"]: s for s in report["streams"] if s.get("codec_type") in ("audio", "video")}
    dts: dict[int, list[int]] = {index: [] for index in streams}
    pts: dict[int, list[int]] = {index: [] for index in streams}
    sync: dict[int, list[bool]] = {index: [] for index in streams}
    for packet in report.get("packets", []):
        index = packet["stream_index"]
        if index in dts and "dts" in packet:
            dts[index].append(int(packet["dts"]))
            pts[index].append(int(packet.get("pts", packet["dts"])))
            sync[index].append("K" in packet.get("flags", ""))
    return {
        s["id"]: Stream(s, dts[index], pts[index], sync[index],
                        tuple(t * Fraction(s["time_base"]) for t in (min(pts[index]), max(pts[index]), dts[index][-1])))
        for index, s in streams.items() if pts[index]
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("capture")
    parser.add_argument("source")
    parser.add_argument("--shed", action="store_true", help="allow losing whole groups, but not the last")
    args = parser.parse_args()
    capture, source = probe(args.capture), probe(args.source)
    if not source:
        sys.exit(f"error: {args.source} has no audio or video stream")

    print("### media holes (capture timestamps against the source" + (", whole groups shed" if args.shed else "") + ")")
    failed = False
    for pid, src in sorted(source.items()):
        label = f"PID {pid} {src.info['codec_name']}"
        # The source's step, not the capture's: a capture that kept every other frame has a step of its own.
        frame = step(sorted(src.pts))
        if frame == 0:
            sys.exit(f"error: {label} has fewer than two frames in {args.source}")
        if pid not in capture:
            print(f"  FAIL  {label}: not in the capture")
            failed = True
            continue
        got = capture[pid]
        base = Fraction(got.info["time_base"])
        span = (f"from {float(got.span[0]):.3f}s to {float(got.span[1]):.3f}s "
                f"of {float(src.span[0]):.3f}s to {float(src.span[1]):.3f}s")
        if args.shed:
            expected = src.pts
            if src.info["codec_name"] in ("mp2", "ac3", "eac3"):
                # Verbatim audio uses Hang's legacy microsecond prefix, then TS floors
                # back to 90 kHz. Match that exact conversion, not a timestamp tolerance.
                expected = [t * 1_000_000 // 90_000 * 90_000 // 1_000_000 for t in expected]
            graded = shed(expected, src.sync, got.pts)
            reasons = [f"resumes mid-group at {float(t * base):.3f}s" for t in graded.mid_group[:5]]
            if graded.unmatched:
                reasons.append(f"{len(graded.unmatched)} frames from {float(graded.unmatched[0] * base):.3f}s "
                               "are out of the source's order, repeated, or not in it")
            if graded.tail:
                reasons.append(f"lost {graded.tail} frames of the source's last group")
            late = got.span[0] - src.span[0]
            if late > JOIN:
                reasons.append(f"starts {float(late):.3f}s after its source, past the {float(JOIN)}s join")
            summary = f"{graded.missing} frames ({float(graded.missing * frame * base):.1f} s) shed"
        else:
            found = holes(sorted(got.pts), frame)
            reasons = [f"at {float(at * base):.3f}s: {float(gap * base) * 1000:.0f} ms step" for at, gap in found[:5]]
            reasons += cuts(src.span, got.span, frame * base)
            missing = sum(gap - frame for _, gap in found) * base
            summary = f"{len(found)} holes, {float(missing) * 1000:.0f} ms missing"
        failed |= bool(reasons)
        print(f"  {'FAIL' if reasons else 'PASS'}  {label}: {len(got.dts)} frames of "
              f"{float(frame * base) * 1000:.2f} ms, {summary}, {span}")
        for reason in reasons:
            print(f"        {reason}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
