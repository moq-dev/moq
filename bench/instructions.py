#!/usr/bin/env python3
"""Count relay instructions and sample their callers on disjoint pinned cores."""

import argparse
import bisect
import json
import math
import os
import profile as profiles
import signal
import struct
import subprocess
import time
from pathlib import Path


def cpus(value):
    if not value:
        raise ValueError("instructions mode requires BENCH_RELAY_CPUS and BENCH_GENERATOR_CPUS")
    result = set()
    for part in value.split(","):
        first, _, last = part.partition("-")
        first, last = int(first), int(last or first)
        if first < 0 or last < first:
            raise ValueError(f"invalid CPU range: {part}")
        result.update(range(first, last + 1))
    return result


def settings(workers):
    relay = cpus(os.environ.get("BENCH_RELAY_CPUS", ""))
    generator = cpus(os.environ.get("BENCH_GENERATOR_CPUS", ""))
    allowed = os.sched_getaffinity(0)
    if relay & generator or not (relay | generator) <= allowed:
        raise ValueError("relay and generator CPU sets must be disjoint subsets of the allowed CPUs")
    if len(relay) < max(map(int, workers.split())):
        raise ValueError("relay CPU set has fewer CPUs than workers")
    # A hybrid event silently counts nothing when its task runs on the other core type.
    core = Path("/sys/devices/cpu_core/cpus")
    if core.exists() and not (relay | generator) <= cpus(core.read_text().strip()):
        raise ValueError("instructions mode requires performance cores on a hybrid CPU")
    period = int(os.environ.get("BENCH_INSTRUCTION_PERIOD", "1000003"))
    if period <= 0:
        raise ValueError("BENCH_INSTRUCTION_PERIOD must be positive")
    return dict(relay_cpus=sorted(relay), generator_cpus=sorted(generator), period=period)


def events():
    prefix = "cpu_core/" if Path("/sys/devices/cpu_core").exists() else ""
    return [f"{prefix}{name}{'/' if prefix else ''}" for name in ("instructions", "cycles")]


class Controlled:
    """An acknowledged perf control channel bounds the actual counter window."""

    def __init__(self, command, log):
        self.record = command[1] == "record"
        read, self.write = os.pipe()
        self.read, write = os.pipe()
        try:
            self.process = subprocess.Popen(
                [*command, "--delay", "-1", "--control", f"fd:{read},{write}"],
                pass_fds=(read, write),
                stdout=log,
                stderr=log,
                env=dict(os.environ, LC_ALL="C", DEBUGINFOD_URLS=""),
            )
        finally:
            os.close(read)
            os.close(write)

    def command(self, command):
        before = time.time_ns()
        os.write(self.write, command.encode() + b"\n")
        ack = b""
        while not ack.endswith(b"\0"):
            if not (part := os.read(self.read, 1)):
                raise ValueError(f"perf exited before acknowledging {command}; see perf.log")
            ack += part
        after = time.time_ns()
        if ack != b"ack\n\0":
            raise ValueError(f"unexpected perf acknowledgement: {ack!r}")
        return [before / 1e6, after / 1e6]

    def close(self):
        try:
            if self.process.poll() is None:
                if self.record:
                    # Record must finish its header before exiting; stat exits on SIGINT.
                    self.command("stop")
                else:
                    self.process.send_signal(signal.SIGINT)
        except (OSError, ValueError):
            self.process.kill()
            raise
        finally:
            os.close(self.write)
            os.close(self.read)
            self.process.wait()
        status = self.process.wait()
        if status != 0 and (self.record or status != -signal.SIGINT):
            raise ValueError(f"perf failed ({status}); see perf.log")


def capture(pid, directory, delay, duration):
    if not math.isfinite(delay) or delay < 0 or not math.isfinite(duration) or duration <= 0:
        raise ValueError("capture needs a nonnegative delay and a positive duration")
    config = settings("1")
    for task in (Path("/proc") / str(pid) / "task").iterdir():
        if os.sched_getaffinity(int(task.name)) != set(config["relay_cpus"]):
            raise ValueError(f"relay thread {task.name} has unexpected affinity")
    directory.mkdir(parents=True, exist_ok=True)
    counters = events()
    names = profiles.threads(pid)
    running = []
    try:
        with (directory / "perf.log").open("w") as log:
            stat = Controlled(
                [
                    "perf",
                    "stat",
                    "-j",
                    "-o",
                    str(directory / "stat.json"),
                    "-p",
                    str(pid),
                    "-e",
                    ",".join([*counters, "task-clock"]),
                ],
                log,
            )
            running.append(stat)
            record = Controlled(
                [
                    "perf",
                    "record",
                    "--quiet",
                    "--no-buildid-cache",
                    "-p",
                    str(pid),
                    "-e",
                    counters[0],
                    "-c",
                    str(config["period"]),
                    "--call-graph",
                    "fp",
                    "-k",
                    "monotonic",
                    "-o",
                    str(directory / "perf.data"),
                ],
                log,
            )
            running.append(record)
            # These also prove both processes have attached before the warmup starts.
            stat.command("disable")
            record.command("disable")
            time.sleep(delay)
            record.command("enable")
            start = stat.command("enable")
            time.sleep(duration)
            end = stat.command("disable")
            record.command("disable")
    finally:
        errors = []
        for process in reversed(running):
            try:
                process.close()
            except (OSError, ValueError) as error:
                errors.append(str(error))
        if errors:
            raise ValueError("; ".join(errors))
    names.update(profiles.threads(pid))
    (directory / "window.json").write_text(
        json.dumps(
            dict(
                **config,
                start_ms=start,
                end_ms=end,
                threads=names,
                events=counters,
                sample_rate_limit=int(Path("/proc/sys/kernel/perf_event_max_sample_rate").read_text()),
            ),
            indent=2,
        )
        + "\n"
    )


def delivered(rows, at):
    """Interpolate delivery, never extrapolate past the generator's reports."""
    index = bisect.bisect_left([row["timestamp_ms"] for row in rows], at)
    if index == len(rows) or at < rows[0]["timestamp_ms"]:
        raise ValueError("generator reports do not bracket the counter window")
    if rows[index]["timestamp_ms"] == at:
        return rows[index]["frames_recv"]
    first, last = rows[index - 1], rows[index]
    weight = (at - first["timestamp_ms"]) / (last["timestamp_ms"] - first["timestamp_ms"])
    return first["frames_recv"] + weight * (last["frames_recv"] - first["frames_recv"])


def reduce(window, stats, rows):
    if len(rows) < 2:
        raise ValueError("fewer than two generator reports")
    for row in rows:
        for key in ("timestamp_ms", "frames_recv"):
            if not isinstance(row[key], (int, float)) or not math.isfinite(row[key]) or row[key] < 0:
                raise ValueError(f"invalid generator counter: {key}")
    for first, last in zip(rows, rows[1:]):
        if last["timestamp_ms"] <= first["timestamp_ms"] or last["frames_recv"] < first["frames_recv"]:
            raise ValueError("generator counters moved backwards")
    start, end = [sum(window[key]) / 2 for key in ("start_ms", "end_ms")]
    if end <= start:
        raise ValueError("counter window did not advance")
    frames = delivered(rows, end) - delivered(rows, start)
    if frames <= 0:
        raise ValueError("no frames delivered in the counter window")
    counts = {}
    for event in [*window["events"], "task-clock"]:
        matches = [row for row in stats if row["event"] == event]
        if len(matches) != 1:
            raise ValueError(f"missing or duplicate perf counter: {event}")
        row = matches[0]
        value = float(row["counter-value"])
        if not math.isfinite(value) or value <= 0 or float(row["pcnt-running"]) != 100:
            raise ValueError(f"invalid or multiplexed perf counter: {event}")
        counts[event] = value
    return dict(
        version=1,
        **window,
        window_start_ms=start,
        window_end_ms=end,
        boundary_uncertainty_ms=[b - a for a, b in (window["start_ms"], window["end_ms"])],
        delivered_frames=frames,
        recv_fps=frames * 1000 / (end - start),
        counters=counts,
        instructions_per_frame=counts[window["events"][0]] / frames,
        cycles_per_frame=counts[window["events"][1]] / frames,
        scope="all relay threads, user and kernel; delivery interpolated at acknowledged counter boundaries",
    )


def report(directory, load):
    validate_data(directory / "perf.data")
    window = json.loads((directory / "window.json").read_text())
    stats = [
        json.loads(line.rstrip(","))
        for line in (directory / "stat.json").read_text().splitlines()
        if line.strip().startswith("{")
    ]
    result = reduce(window, stats, [json.loads(line) for line in load.read_text().splitlines()])
    records = profiles.parse(profiles.perf_script(directory / "perf.data"), kernel=True)
    records = [row for row in records if row[2].startswith(window["events"][0])]
    if not records or not any(profiles.RESOLVED.search(frame) for _, _, _, frames in records for frame in frames):
        raise ValueError("no instruction samples have resolved MoQ frames")
    from collections import Counter

    folded = Counter()
    for tid, _, _, frames in records:
        folded[";".join([window["threads"].get(tid, f"tid-{tid}"), *reversed(frames)])] += window["period"]
    result["instruction_samples"] = len(records)
    (directory / "instructions.folded").write_text(
        "".join(f"{stack} {count}\n" for stack, count in folded.most_common())
    )
    (directory / "instructions.json").write_text(json.dumps(result, indent=2) + "\n")


def validate_data(path):
    """Reject lost or throttled samples before expensive symbolization."""
    data = path.read_bytes()
    if data[:8] != b"PERFILE2" or len(data) < 56:
        raise ValueError("unsupported or incomplete perf.data header")
    offset, size = struct.unpack_from("<QQ", data, 40)
    end = offset + size
    if not size or end > len(data):
        raise ValueError("incomplete perf.data recording")
    while offset < end:
        if end - offset < 8:
            raise ValueError("incomplete perf.data record header")
        kind, _, size = struct.unpack_from("<IHH", data, offset)
        if size < 8 or offset + size > end:
            raise ValueError("invalid perf.data record")
        # perf_event_header types: LOST, THROTTLE, LOST_SAMPLES. perf script's
        # --show-lost-events omits THROTTLE, which would bias fixed-period attribution.
        if kind in (2, 5, 13):
            raise ValueError(
                f"perf lost or throttled samples (record type {kind}); inspect the host's "
                "kernel.perf_event_max_sample_rate and capture buffers before choosing a fixed period"
            )
        offset += size


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check")
    check.add_argument("workers")
    capture_args = sub.add_parser("capture")
    capture_args.add_argument("pid", type=int)
    capture_args.add_argument("directory", type=Path)
    capture_args.add_argument("--delay", type=float, default=7)
    capture_args.add_argument("--duration", type=float, default=5)
    report_args = sub.add_parser("report")
    report_args.add_argument("directory", type=Path)
    report_args.add_argument("load", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "check":
            settings(args.workers)
        elif args.command == "capture":
            capture(args.pid, args.directory, args.delay, args.duration)
        else:
            report(args.directory, args.load)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"instructions: {error}\n")


if __name__ == "__main__":
    main()
