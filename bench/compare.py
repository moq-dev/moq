#!/usr/bin/env python3
"""Reduce paired relay rounds; retain dispersion instead of implying single-run precision."""
import argparse
import json
import math
import shutil
import statistics
from pathlib import Path

METRICS = ("recv_mbps", "send_mbps", "recv_fps", "send_fps", "cpu_cores", "generator_cpu_cores", "loss_pct", "latency_p99_ms")


def value(row, metric):
    return row.get('generator', {}).get('cpu_cores') if metric == 'generator_cpu_cores' else row.get(metric)


def reduce(pairs):
    if not pairs:
        raise ValueError("no paired rounds")
    result = {"pairs": len(pairs), "estimate": "median paired percent delta; dispersion is median absolute deviation", "metrics": {}}
    for metric in METRICS:
        if metric in ("cpu_cores", "generator_cpu_cores") and all(value(pair[label], metric) is None for pair in pairs for label in ("base", "current")):
            result["metrics"][metric] = {"delta_pct": None, "status": "unavailable: no host CPU samples"}
            continue
        deltas = []
        absolute = []
        for pair in pairs:
            for label in ("base", "current"):
                sample = value(pair[label], metric)
                if not isinstance(sample, (int, float)) or isinstance(sample, bool) or not math.isfinite(sample) or sample < 0:
                    raise ValueError(f"invalid or missing {label}.{metric}: {sample}")
            base, current = value(pair["base"], metric), value(pair["current"], metric)
            absolute.append(current - base)
            if base == 0:
                continue # A ratio to zero is undefined, including zero loss on both sides.
            deltas.append(100 * (current / base - 1))
        if not deltas:
            result["metrics"][metric] = {"delta_pct": None, "status": "undefined ratio to zero", "paired_absolute_deltas": absolute}
            continue
        median = statistics.median(deltas)
        mad = statistics.median(abs(value - median) for value in deltas)
        result["metrics"][metric] = {"delta_pct": median, "mad_pct": mad,
            "range_pct": [min(deltas), max(deltas)], "paired_deltas_pct": deltas, "paired_absolute_deltas": absolute,
            "status": "insufficient samples" if len(deltas) < 3 else
                      "sign varies across pairs" if min(deltas) < 0 < max(deltas) else "consistent sign"}
    # Keep offered load and delivery/loss next to CPU; no normalized CPU claim when windows differ.
    result["cpu_efficiency"] = "not estimated: verify offered load, delivery/loss and host/load window alignment before normalizing CPU"
    return result


def load(directory, workload):
    """Pair base/current summaries in numeric round order, matching execution-order.tsv."""
    rounds = {label: {path.name for path in (directory / "relay" / label).glob("round-*")} for label in ("base", "current")}
    if rounds["base"] != rounds["current"]:
        raise ValueError(f"unpaired rounds: base {sorted(rounds['base'])}, current {sorted(rounds['current'])}")
    return [{label: json.loads((directory / "relay" / label / name / workload / "summary.json").read_text())
             for label in ("base", "current")}
            for name in sorted(rounds["base"], key=lambda name: int(name.removeprefix("round-")))]


def criterion_cases(directory):
    """Read exact IDs from a fresh target's discovery measurements."""
    ids = set()
    for path in directory.glob("**/discovery/benchmark.json"):
        benchmark = json.loads(path.read_text())["full_id"]
        if not isinstance(benchmark, str) or not benchmark or any(c in benchmark for c in "\t\r\n"):
            raise ValueError(f"invalid Criterion full_id in {path}: {benchmark!r}")
        ids.add(benchmark)
    if not ids:
        raise ValueError(f"no Criterion benchmark cases found in {directory}")
    return sorted(ids)


def criterion_baseline(directory, discovery, benchmark):
    """Place the exact baseline at the current revision's Criterion directory."""
    sources = [path.parent for path in directory.glob("**/base/benchmark.json")
               if json.loads(path.read_text())["full_id"] == benchmark]
    targets = [path.parent.parent.relative_to(discovery) for path in discovery.glob("**/discovery/benchmark.json")
               if json.loads(path.read_text())["full_id"] == benchmark]
    if len(sources) != 1 or len(targets) != 1:
        raise ValueError(f"expected one baseline and discovery case for {benchmark!r}")
    destination = directory / targets[0] / "base"
    if sources[0] != destination:
        shutil.copytree(sources[0], destination, dirs_exist_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--criterion-cases", action="store_true", help="list full IDs from isolated discovery samples")
    parser.add_argument("--criterion-baseline", nargs=2, metavar=("DISCOVERY", "ID"), help="align an exact baseline with current discovery")
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    if args.criterion_cases:
        print("\n".join(criterion_cases(args.directory)))
        return
    if args.criterion_baseline:
        discovery, benchmark = args.criterion_baseline
        criterion_baseline(args.directory, Path(discovery), benchmark)
        return
    for workload in ("video", "fanout"):
        result = reduce(load(args.directory, workload))
        (args.directory / f"comparison-{workload}.json").write_text(json.dumps(result, indent=2) + "\n")
        print(workload + ": " + json.dumps(result))


if __name__ == "__main__":
    main()
