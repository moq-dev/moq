#!/usr/bin/env python3
"""Count chat fan-out instructions per delivered group, independent of host load.

`bench/run.sh --fanout` builds each `bench/fanout/<scenario>` harness against every
revision. Each harness runs its whole fan-out on one thread, so the user instructions it
retires depend only on the code: this pins it to one CPU, counts a run of zero cycles and
one of many, and divides the difference by the groups delivered.
"""

import argparse
import json
import math
import os
import re
import subprocess
from pathlib import Path

# Group deliveries per measurement: subscribers times cycles, so every subscriber count
# runs about as long. The session harness costs about ten times the model's per group.
UNITS = {"model": 1_000_000, "session": 100_000}


def cycles(scenario, subscribers):
    return max(1, math.ceil(UNITS[scenario] / subscribers))


def parse_stat(text):
    """Sum the user instructions perf stat's CSV reports, one line per PMU on a hybrid CPU."""
    total, counted = 0, False
    for line in text.splitlines():
        fields = line.split(",")
        if len(fields) < 5 or "instructions" not in fields[2]:
            continue
        if fields[0] in ("<not counted>", "<not supported>"):
            continue
        if float(fields[4] or 0) != 100:
            raise ValueError(f"instructions were multiplexed and undercounted: {line}")
        total += int(fields[0])
        counted = True
    if not counted:
        raise ValueError(f"perf counted no instructions:\n{text}")
    return total


def count(binary, args, cpu):
    """User instructions `binary args` retires pinned to `cpu`."""
    run = subprocess.run(
        ["taskset", "-c", str(cpu), "perf", "stat", "-x,", "-e", "instructions:u", binary, *map(str, args)],
        capture_output=True,
        text=True,
        env=dict(os.environ, LC_ALL="C"),
    )
    if run.returncode != 0:
        raise ValueError(f"{binary} {args} failed:\n{run.stderr}")
    return parse_stat(run.stderr)


def per_group(scenario, binary, subscribers, cpu, extra=()):
    n = cycles(scenario, subscribers)
    base = count(binary, [subscribers, 0, *extra], cpu)
    full = count(binary, [subscribers, n, *extra], cpu)
    return (full - base) / (n * subscribers)


def callgrind(scenario, binary, subscribers, out, extra=()):
    """Exact per-function attribution: both runs under callgrind, kept for callgrind_annotate."""
    n = max(1, cycles(scenario, subscribers) // 10)
    out.mkdir(parents=True, exist_ok=True)
    for label, run in (("zero", 0), ("full", n)):
        subprocess.run(
            [
                "valgrind",
                "--tool=callgrind",
                f"--callgrind-out-file={out / f'{subscribers}.{label}.out'}",
                binary,
                str(subscribers),
                str(run),
                *extra,
            ],
            check=True,
            capture_output=True,
        )
    (out / f"{subscribers}.units").write_text(f"{n * subscribers}\n")


def functions(zero, full, units, inclusive=False):
    """Instructions per group by function, from two callgrind outputs of the same binary."""

    def load(path):
        text = subprocess.run(
            [
                "callgrind_annotate",
                f"--inclusive={'yes' if inclusive else 'no'}",
                "--threshold=100",
                "--auto=no",
                "--show-percs=no",
                str(path),
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        totals = {}
        for line in text.splitlines():
            match = re.match(r"\s*([\d,]+)\s+(\S.*)$", line)
            if not match:
                continue
            name = re.sub(r"\s+\[[^\]]*\]$", "", match.group(2))
            name = re.sub(r"^(/[^:]*|\?\?\?):", "", name)
            totals[name] = totals.get(name, 0) + int(match.group(1).replace(",", ""))
        return totals

    before, after = load(zero), load(full)
    names = set(before) | set(after)
    return {name: (after.get(name, 0) - before.get(name, 0)) / units for name in names}


def summarize(run):
    """Print and keep each scenario's instructions per group, by subscribers and revision."""
    rows = [json.loads(line) for line in (run / "fanout.jsonl").read_text().splitlines()]
    (run / "fanout-summary.json").write_text(json.dumps(rows, indent=2) + "\n")
    labels = list(dict.fromkeys(row["label"] for row in rows))
    for scenario in dict.fromkeys(row["scenario"] for row in rows):
        print(f"\n{scenario}: instructions per delivered group")
        print(f"{'subscribers':>11}" + "".join(f"{label[:16]:>18}" for label in labels))
        for subscribers in sorted({row["subscribers"] for row in rows if row["scenario"] == scenario}):
            cells = {
                row["label"]: row.get("per_group")
                for row in rows
                if row["scenario"] == scenario and row["subscribers"] == subscribers
            }
            print(
                f"{subscribers:>11}"
                + "".join(
                    f"{cells[label]:>18.1f}" if cells.get(label) is not None else f"{'unsupported':>18}"
                    for label in labels
                )
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    measure = sub.add_parser("measure")
    measure.add_argument("run", type=Path)
    measure.add_argument("label")
    measure.add_argument("revision")
    measure.add_argument("scenario", choices=sorted(UNITS))
    measure.add_argument("binary")
    measure.add_argument("cpu")
    measure.add_argument("subscribers", type=int, nargs="+")
    measure.add_argument("--version", default=None)
    measure.add_argument("--callgrind", action="store_true")
    summary = sub.add_parser("summarize")
    summary.add_argument("run", type=Path)
    top = sub.add_parser("functions")
    top.add_argument("zero", type=Path)
    top.add_argument("full", type=Path)
    top.add_argument("units", type=int)
    top.add_argument("--inclusive", action="store_true")
    top.add_argument("--top", type=int, default=40)
    args = parser.parse_args()
    try:
        if args.command == "measure":
            extra = [args.version] if args.version and args.scenario == "session" else []
            with (args.run / "fanout.jsonl").open("a") as out:
                for subscribers in args.subscribers:
                    row = dict(
                        label=args.label,
                        revision=args.revision,
                        scenario=args.scenario,
                        subscribers=subscribers,
                        cycles=cycles(args.scenario, subscribers),
                        per_group=None,
                    )
                    if args.binary != "-":
                        row["per_group"] = per_group(args.scenario, args.binary, subscribers, args.cpu, extra)
                        if args.callgrind:
                            target = args.run / "fanout" / args.label / args.scenario / "callgrind"
                            callgrind(args.scenario, args.binary, subscribers, target, extra)
                    out.write(json.dumps(row) + "\n")
        elif args.command == "summarize":
            summarize(args.run)
        else:
            ranked = functions(args.zero, args.full, args.units, args.inclusive)
            for name, value in sorted(ranked.items(), key=lambda item: -item[1])[: args.top]:
                print(f"{value:10.1f}  {name[:180]}")
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"fanout: {error}\n")


if __name__ == "__main__":
    main()
