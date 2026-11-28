#!/usr/bin/env python3
"""Reduce paired relay rounds; retain dispersion instead of implying single-run precision."""

import argparse
import json
import math
import statistics
from pathlib import Path

METRICS = (
    "recv_mbps",
    "send_mbps",
    "recv_fps",
    "send_fps",
    "cpu_cores",
    "cpu_us_per_frame",
    "cpu_cores_per_gbps",
    "generator_cpu_cores",
    "loss_pct",
    "latency_p99_ms",
    # The relay's peak resident memory over the window; per-connection state shows here, not in CPU.
    "rss_bytes",
)
# Read from the host sampler, which runs only on Linux.
HOST_METRICS = ("cpu_cores", "cpu_us_per_frame", "cpu_cores_per_gbps", "generator_cpu_cores", "rss_bytes")


def value(row, metric):
    return row.get("generator", {}).get("cpu_cores") if metric == "generator_cpu_cores" else row.get(metric)


def summarize(deltas):
    """The median paired percent delta, its median absolute deviation, and whether every pair agreed."""
    median = statistics.median(deltas)
    mad = statistics.median(abs(value - median) for value in deltas)
    return {
        "delta_pct": median,
        "mad_pct": mad,
        "range_pct": [min(deltas), max(deltas)],
        "paired_deltas_pct": deltas,
        "status": "insufficient samples"
        if len(deltas) < 3
        else "sign varies across pairs"
        if min(deltas) < 0 < max(deltas)
        else "consistent sign",
    }


def reduce(pairs):
    if not pairs:
        raise ValueError("no paired rounds")
    result = {
        "pairs": len(pairs),
        "estimate": "median paired percent delta; dispersion is median absolute deviation",
        "metrics": {},
    }
    for metric in METRICS:
        if metric in HOST_METRICS and all(
            value(pair[label], metric) is None for pair in pairs for label in ("base", "current")
        ):
            result["metrics"][metric] = {"delta_pct": None, "status": "unavailable: no host samples"}
            continue
        deltas = []
        absolute = []
        for pair in pairs:
            for label in ("base", "current"):
                sample = value(pair[label], metric)
                if (
                    not isinstance(sample, (int, float))
                    or isinstance(sample, bool)
                    or not math.isfinite(sample)
                    or sample < 0
                ):
                    raise ValueError(f"invalid or missing {label}.{metric}: {sample}")
            base, current = value(pair["base"], metric), value(pair["current"], metric)
            absolute.append(current - base)
            if base == 0:
                continue  # A ratio to zero is undefined, including zero loss on both sides.
            deltas.append(100 * (current / base - 1))
        if not deltas:
            result["metrics"][metric] = {
                "delta_pct": None,
                "status": "undefined ratio to zero",
                "paired_absolute_deltas": absolute,
            }
            continue
        result["metrics"][metric] = {**summarize(deltas), "paired_absolute_deltas": absolute}
    # Keep offered load and delivery/loss next to CPU; no normalized CPU claim when windows differ.
    result["cpu_efficiency"] = (
        "not estimated: verify offered load, delivery/loss and host/load window alignment before normalizing CPU"
    )
    return result


def rounds(directory):
    """Every paired round's name, in numeric order, matching execution-order.tsv."""
    names = {
        label: {path.name for path in (directory / "relay" / label).glob("round-*")} for label in ("base", "current")
    }
    if names["base"] != names["current"]:
        raise ValueError(f"unpaired rounds: base {sorted(names['base'])}, current {sorted(names['current'])}")
    return sorted(names["base"], key=lambda name: int(name.removeprefix("round-")))


def cases(directory):
    """The `transport/workload` cases that every round of both revisions measured."""
    found = {
        frozenset(str(path.parent.relative_to(run)) for path in run.glob("*/*/summary.json"))
        for label in ("base", "current")
        for run in (directory / "relay" / label).glob("round-*")
    }
    if len(found) != 1:
        raise ValueError(f"rounds measured different cases: {sorted(map(sorted, found))}")
    return sorted(found.pop())


def load(directory, case):
    """Pair base/current summaries of one case in round order."""
    return [
        {
            label: json.loads((directory / "relay" / label / name / case / "summary.json").read_text())
            for label in ("base", "current")
        }
        for name in rounds(directory)
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    for case in cases(args.directory):
        result = reduce(load(args.directory, case))
        (args.directory / f"comparison-{case.replace('/', '-')}.json").write_text(json.dumps(result, indent=2) + "\n")
        print(case + ": " + json.dumps(result))


if __name__ == "__main__":
    main()
