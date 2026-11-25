"""Sample the memory of running processes and judge whether it keeps growing.

Two subcommands, so a harness samples while its workload runs and judges once it ends:

    growth.py sample --out samples.jsonl --interval 5 --seconds 600 relay=<pid>=<log> ...
    growth.py judge samples.jsonl --warmup 60 --max allocated=400 --max rss=4000

`sample` waits for each process to log that its SIGUSR1 handler is up, then on each tick reads
its resident set (VmRSS) and I/O counters from /proc, sends it SIGUSR1, and waits for the
`jemalloc heap allocated=N resident=N` line that `moq_tokio::jemalloc` logs in answer, so every
sample pairs with the exact live heap at that moment. A process that exits, or never answers,
fails the run: a gap is not a flat line.

`judge` fits each process's slope after warm-up with the Theil-Sen estimator, the median of the
slopes between every pair of samples, which in-flight data swinging between samples (cached
groups, receive buffers) moves far less than a least-squares fit. It fails naming the process
and measure whose slope exceeds its limit, in bytes per second.
"""

import argparse
import json
import os
import re
import signal
import statistics
import sys
import time
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*m")
# Logged once the SIGUSR1 handler is registered: a signal before it would kill the process.
READY = "jemalloc heap profiling"
# How long a process may take to start answering SIGUSR1.
READY_SECONDS = 30
HEAP = re.compile(r"jemalloc heap allocated=(\d+) resident=(\d+)")
# How long a process may take to log its heap after SIGUSR1; it answers on its runtime, so a
# busy process takes milliseconds.
ANSWER_SECONDS = 10


def heap_lines(text):
    """The (allocated, resident) pairs a log holds, in order."""
    return [(int(a), int(r)) for a, r in HEAP.findall(ANSI.sub("", text))]


def proc_fields(path):
    """The `name: value` fields of a /proc file, as a dict of strings."""
    fields = {}
    for line in Path(path).read_text().splitlines():
        name, _, value = line.partition(":")
        fields[name.strip()] = value.strip()
    return fields


class Process:
    def __init__(self, spec):
        try:
            self.label, pid, log = spec.split("=", 2)
            self.pid = int(pid)
        except ValueError:
            raise SystemExit(f"error: expected label=pid=log, got {spec!r}")
        self.log = Path(log)
        self.answered = 0

    def wait_ready(self):
        deadline = time.monotonic() + READY_SECONDS
        while READY not in ANSI.sub("", self.log.read_text(errors="replace")):
            if not Path(f"/proc/{self.pid}").exists():
                raise SystemExit(f"error: {self.label} (pid {self.pid}) exited before it answered SIGUSR1")
            if time.monotonic() > deadline:
                raise SystemExit(f"error: {self.label} never logged {READY!r}; is it built with the jemalloc feature?")
            time.sleep(0.05)

    def sample(self):
        try:
            status = proc_fields(f"/proc/{self.pid}/status")
            io = proc_fields(f"/proc/{self.pid}/io")
        except FileNotFoundError:
            raise SystemExit(f"error: {self.label} (pid {self.pid}) exited while being sampled")
        # VmRSS reads `1234 kB`.
        rss = int(status["VmRSS"].split()[0]) * 1024
        os.kill(self.pid, signal.SIGUSR1)
        deadline = time.monotonic() + ANSWER_SECONDS
        while True:
            answers = heap_lines(self.log.read_text(errors="replace"))
            if len(answers) > self.answered:
                break
            if time.monotonic() > deadline:
                raise SystemExit(
                    f"error: {self.label} logged no jemalloc heap within {ANSWER_SECONDS}s of SIGUSR1; "
                    "is it built with the jemalloc feature?"
                )
            time.sleep(0.02)
        if len(answers) != self.answered + 1:
            raise SystemExit(f"error: {self.label} answered {len(answers) - self.answered} heap reports to one SIGUSR1")
        self.answered = len(answers)
        allocated, resident = answers[-1]
        return {
            "label": self.label,
            "rss": rss,
            "allocated": allocated,
            "resident": resident,
            "rchar": int(io["rchar"]),
            "wchar": int(io["wchar"]),
        }


def sample(args):
    processes = [Process(spec) for spec in args.process]
    for process in processes:
        process.wait_ready()
    start = time.monotonic()
    with open(args.out, "w") as out:
        tick = 0
        while True:
            t = time.monotonic() - start
            for process in processes:
                record = process.sample()
                record["t"] = round(t, 3)
                out.write(json.dumps(record) + "\n")
            out.flush()
            tick += 1
            if tick * args.interval > args.seconds:
                return 0
            time.sleep(max(0.0, start + tick * args.interval - time.monotonic()))


def theil_sen(points):
    """The median slope between every pair of points with distinct times."""
    slopes = [(y2 - y1) / (t2 - t1) for i, (t1, y1) in enumerate(points) for t2, y2 in points[i + 1 :] if t2 != t1]
    if not slopes:
        raise ValueError("a slope needs two samples at distinct times")
    return statistics.median(slopes)


def human(rate):
    """A signed byte rate per second, as a short string."""
    for unit in ("B", "KiB", "MiB"):
        if abs(rate) < 1024 or unit == "MiB":
            return f"{rate:+.1f} {unit}/s"
        rate /= 1024


def judge(args):
    limits = {}
    for spec in args.max:
        measure, _, limit = spec.partition("=")
        try:
            limits[measure] = float(limit)
        except ValueError:
            raise SystemExit(f"error: expected measure=bytes-per-second, got {spec!r}")
    flows = {}
    for spec in args.flow:
        label, _, counter = spec.partition("=")
        if counter not in ("rchar", "wchar"):
            raise SystemExit(f"error: expected label=rchar or label=wchar, got {spec!r}")
        flows[label] = counter

    series = {}
    for line in Path(args.samples).read_text().splitlines():
        record = json.loads(line)
        series.setdefault(record["label"], []).append(record)
    if not series:
        raise SystemExit("error: no samples")

    report = {"warmup": args.warmup, "limits": limits, "processes": {}}
    failures = []
    for label, records in series.items():
        after = [r for r in records if r["t"] >= args.warmup]
        if len(after) < args.min_samples:
            raise SystemExit(
                f"error: {label} has {len(after)} samples after the {args.warmup}s warm-up; "
                f"a slope needs at least {args.min_samples}"
            )
        span = after[-1]["t"] - after[0]["t"]
        entry = {"samples": len(after), "seconds": span}
        for measure, limit in limits.items():
            points = [(r["t"], r[measure]) for r in after]
            slope = theil_sen(points)
            entry[measure] = {"slope": slope, "first": points[0][1], "last": points[-1][1]}
            verdict = "ok"
            if slope > limit:
                verdict = "GROWING"
                failures.append(f"{label} {measure} grew {human(slope)}, over its {human(limit)} limit")
            print(
                f"{label:>8} {measure:>9}: {human(slope):>14} (limit {human(limit)}), "
                f"{points[0][1] / 2**20:.1f} -> {points[-1][1] / 2**20:.1f} MiB  {verdict}"
            )
        # A process that moved no data after warm-up is idle, and an idle process has nothing to leak.
        if label in flows:
            counter = flows[label]
            moved = after[-1][counter] - after[0][counter]
            entry[counter] = moved
            rate = moved / span if span else 0
            print(f"{label:>8} {counter:>9}: {rate / 1024:.1f} KiB/s after warm-up")
            if rate < args.min_flow:
                failures.append(
                    f"{label} moved {rate:.0f} B/s after warm-up, under {args.min_flow} B/s: the chain stalled"
                )
        report["processes"][label] = entry
    for label in flows:
        if label not in series:
            failures.append(f"{label} was never sampled")

    report["failures"] = failures
    if args.report:
        Path(args.report).write_text(json.dumps(report, indent=2) + "\n")
    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    return 1 if failures else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    s = commands.add_parser("sample", help="sample processes' memory until the time is up")
    s.add_argument("--out", required=True, help="JSON lines to write, one per process per tick")
    s.add_argument("--interval", type=float, default=5, help="seconds between ticks")
    s.add_argument("--seconds", type=float, required=True, help="how long to sample")
    s.add_argument("process", nargs="+", help="label=pid=log, the log the process writes its heap reports to")

    j = commands.add_parser("judge", help="fail on a slope after warm-up")
    j.add_argument("samples", help="what `sample` wrote")
    j.add_argument("--warmup", type=float, default=0, help="seconds of samples to ignore")
    j.add_argument(
        "--max", action="append", default=[], required=True, help="measure=bytes-per-second (rss, allocated, resident)"
    )
    j.add_argument("--flow", action="append", default=[], help="label=rchar|wchar, a counter that must keep moving")
    j.add_argument("--min-flow", type=float, default=1024, help="bytes per second each --flow must move")
    j.add_argument("--min-samples", type=int, default=10, help="samples each process needs after warm-up")
    j.add_argument("--report", help="JSON summary to write")

    args = parser.parse_args(argv)
    return sample(args) if args.command == "sample" else judge(args)


if __name__ == "__main__":
    sys.exit(main())
