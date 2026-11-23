#!/usr/bin/env python3
"""Find the media a capture is missing, from its timestamps against its generated source.

    ./holes.py capture.ts source.ts [--shed] [--program N [--join S]]

Every audio and video stream of a generated clip has a constant frame duration, so the step
between consecutive presentation timestamps, in presentation order, is the same throughout. A
capture's step past one and a half of its source's is a hole: media the round-trip lost, whether
the path dropped it, the relay evicted it, or the exporter missed its deadline. Presentation
timestamps, because the export keeps them while it derives each group's decode timestamps from
that group's frames alone: a reordered group's first frames decode just before its earliest
presentation, not a frame apart, which leaves a decode step at every group with nothing lost. A
lost prefix or tail leaves no step behind, so each source stream must also be in the capture, on
its PID, ending where the source's does: the export keeps the source's timestamps. It may start
later, by up to JOIN: the subscriber joins at the relay's latest group once the catalog is out,
which skips the first second or two of a generated clip on a clean path. The compliance checks
grade only the access units that arrived, so a capture missing most of its video still passes
them.

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

Under `--program N` the source is a multiplex and the capture one export of its program N: only
that program's streams are graded, and the capture's PAT must list that program alone, its PMT
naming the source's PCR PID and elementary streams. `--join S` is how far into the source that
export started, which it may start later by. Each track joins at its own latest group then, so
no stream may start more than two of the source's video groups before the join (its group, and
one more for the publisher starting late on the wall clock the join is counted on), which would
be a replay from before the export started, or more than one after the capture's first video
keyframe, which would leave that stream missing what the program carries from there.

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
    """Grade the capture's `kept` PTS, in its order, against the source's frames, in decode order with
    their sync flags."""
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


def cuts(source: Span, capture: Span, frame: Fraction, join: Fraction = JOIN) -> list[str]:
    """How a capture's (first PTS, last PTS, last DTS), in seconds, falls short of its source's."""
    # Both ends: losing trailing B pictures leaves the last PTS, and losing the last P picture the last DTS.
    late, short = capture[0] - source[0], max(source[1] - capture[1], source[2] - capture[2])
    found = []
    if late > join:
        found.append(f"starts {float(late):.3f}s after its source, past the {float(join)}s join")
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


@dataclass
class Program:
    """A program as its PAT entry and PMT describe it."""

    pmt: int  # PMT PID
    pcr: int  # PCR PID
    streams: list[str]  # elementary stream PIDs, in PMT order


def programs(path: str) -> dict[int, Program]:
    """Each program the PAT lists, by program number."""
    report = json.loads(
        subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_entries",
                "program=program_num,pmt_pid,pcr_pid:stream=id",
                "-of",
                "json",
                path,
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    return {
        p["program_num"]: Program(p["pmt_pid"], p["pcr_pid"], [s["id"] for s in p.get("streams", [])])
        for p in report.get("programs", [])
    }


def tables(capture: dict[int, Program], source: dict[int, Program], number: int) -> list[str]:
    """How the capture's programs differ from the source's program `number` alone."""
    if list(capture) != [number]:
        return [f"the PAT lists programs {sorted(capture)}, not {number} alone"]
    got, want = capture[number], source[number]
    found = []
    if got.pcr != want.pcr:
        found.append(f"PCR PID {got.pcr:#x}, not the source's {want.pcr:#x}")
    if got.streams != want.streams:
        found.append(f"elementary streams {got.streams}, not the source's {want.streams}")
    return found


def probe(path: str) -> dict[str, Stream]:
    """Each audio and video stream, by PID."""
    # Probe the whole file, not FFmpeg's first 5 MB: a stream whose first frame comes after that
    # is never described, so the second frame of its first PES repeats the first's timestamp
    # and every later one in that PES runs a frame behind, which reads as a hole.
    report = json.loads(
        subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-probesize",
                str(os.path.getsize(path)),
                "-analyzeduration",
                str(2**62),
                "-show_entries",
                "stream=index,id,codec_type,codec_name,time_base:packet=stream_index,dts,pts,flags",
                "-of",
                "json",
                path,
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
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
        s["id"]: Stream(
            s,
            dts[index],
            pts[index],
            sync[index],
            tuple(t * Fraction(s["time_base"]) for t in (min(pts[index]), max(pts[index]), dts[index][-1])),
        )
        for index, s in streams.items()
        if pts[index]
    }


def starts(source: dict[str, Stream], capture: dict[str, Stream], join: Fraction) -> dict[str, list[str]]:
    """Where each of a program's streams starts against its source's video groups, by PID: under a
    `join`, no earlier than two groups before it, and in any case no later than one group after
    the capture's first video keyframe."""
    videos = [pid for pid, src in source.items() if src.info.get("codec_type") == "video"]
    if not videos:
        return {}

    def keyframes(stream: Stream) -> list[Fraction]:
        base = Fraction(stream.info["time_base"])
        return sorted(t * base for t, key in zip(stream.pts, stream.sync) if key)

    group = max(
        (b - a for pid in videos for a, b in zip(keyframes(source[pid]), keyframes(source[pid])[1:])),
        default=Fraction(0),
    )
    opened = min(
        (keyframes(capture[pid])[0] for pid in videos if pid in capture and any(capture[pid].sync)),
        default=None,
    )
    # The join is time into the program, which starts with its first stream, not each stream's own.
    origin = min(src.span[0] for src in source.values())
    found: dict[str, list[str]] = {}
    for pid in source:
        if pid not in capture:
            continue
        first = capture[pid].span[0]
        reasons = found.setdefault(pid, [])
        if join and first - origin < join - 2 * group:
            reasons.append(
                f"starts {float(first - origin):.3f}s into its program, over two {float(group)}s groups "
                f"before the {float(join)}s join"
            )
        if opened is not None and first - opened > group:
            reasons.append(
                f"starts {float(first - opened):.3f}s after the program's first video keyframe, "
                f"past a {float(group)}s group"
            )
    return found


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("capture")
    parser.add_argument("source")
    parser.add_argument("--shed", action="store_true", help="allow losing whole groups, but not the last")
    parser.add_argument("--program", type=int, help="grade this program of a multiplex, carried alone")
    parser.add_argument("--join", type=Fraction, default=Fraction(0), help="seconds into the source the export started")
    args = parser.parse_args()
    if args.join and args.program is None:
        parser.error("--join needs --program")
    capture, source = probe(args.capture), probe(args.source)
    join = JOIN + args.join
    failed = False
    if args.program is not None:
        wanted = programs(args.source)
        if args.program not in wanted:
            sys.exit(f"error: {args.source} has no program {args.program}")
        want = wanted[args.program]
        found = tables(programs(args.capture), wanted, args.program)
        print(f"### program {args.program} tables (the capture's PAT and PMT against the source's)")
        print(
            f"  {'FAIL' if found else 'PASS'}  program {args.program}: PCR PID {want.pcr:#x}, "
            f"elementary streams {' '.join(want.streams)}"
        )
        for reason in found:
            print(f"        {reason}")
        failed = bool(found)
        source = {pid: s for pid, s in source.items() if pid in want.streams}
    early = starts(source, capture, args.join) if args.program is not None else {}
    if not source:
        sys.exit(f"error: {args.source} has no audio or video stream")

    print("### media holes (capture timestamps against the source" + (", whole groups shed" if args.shed else "") + ")")
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
        span = (
            f"from {float(got.span[0]):.3f}s to {float(got.span[1]):.3f}s "
            f"of {float(src.span[0]):.3f}s to {float(src.span[1]):.3f}s"
        )
        if args.shed:
            expected = src.pts
            if src.info["codec_name"] in ("mp2", "ac3", "eac3"):
                # Verbatim audio uses Hang's legacy microsecond prefix, then TS floors
                # back to 90 kHz. Match that exact conversion, not a timestamp tolerance.
                expected = [t * 1_000_000 // 90_000 * 90_000 // 1_000_000 for t in expected]
            graded = shed(expected, src.sync, got.pts)
            reasons = [f"resumes mid-group at {float(t * base):.3f}s" for t in graded.mid_group[:5]]
            if graded.unmatched:
                reasons.append(
                    f"{len(graded.unmatched)} frames from {float(graded.unmatched[0] * base):.3f}s "
                    "are out of the source's order, repeated, or not in it"
                )
            if graded.tail:
                reasons.append(f"lost {graded.tail} frames of the source's last group")
            late = got.span[0] - src.span[0]
            if late > join:
                reasons.append(f"starts {float(late):.3f}s after its source, past the {float(join)}s join")
            summary = f"{graded.missing} frames ({float(graded.missing * frame * base):.1f} s) shed"
        else:
            found = holes(sorted(got.pts), frame)
            reasons = [f"at {float(at * base):.3f}s: {float(gap * base) * 1000:.0f} ms step" for at, gap in found[:5]]
            reasons += cuts(src.span, got.span, frame * base, join)
            missing = sum(gap - frame for _, gap in found) * base
            summary = f"{len(found)} holes, {float(missing) * 1000:.0f} ms missing"
        reasons += early.get(pid, [])
        failed |= bool(reasons)
        print(
            f"  {'FAIL' if reasons else 'PASS'}  {label}: {len(got.dts)} frames of "
            f"{float(frame * base) * 1000:.2f} ms, {summary}, {span}"
        )
        for reason in reasons:
            print(f"        {reason}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
