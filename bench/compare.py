#!/usr/bin/env python3
"""Reduce paired relay rounds; retain dispersion instead of implying single-run precision."""
import argparse
import json
import math
import statistics
from pathlib import Path

METRICS = ("recv_mbps", "send_mbps", "recv_fps", "send_fps", "cpu_cores", "loss_pct", "latency_p99_ms")


def reduce(pairs):
    if not pairs:
        raise ValueError("no paired rounds")
    result = {"pairs": len(pairs), "estimate": "median paired percent delta; dispersion is median absolute deviation", "metrics": {}}
    for metric in METRICS:
        if metric == "cpu_cores" and all(pair[label].get(metric) is None for pair in pairs for label in ("base", "current")):
            result["metrics"][metric] = {"delta_pct": None, "status": "unavailable: no host CPU samples"}
            continue
        deltas = []
        for pair in pairs:
            for label in ("base", "current"):
                value = pair[label].get(metric)
                if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value) or value < 0:
                    raise ValueError(f"invalid or missing {label}.{metric}: {value}")
            base, current = pair["base"][metric], pair["current"][metric]
            if base == 0:
                continue # A ratio to zero is undefined, including zero loss on both sides.
            deltas.append(100 * (current / base - 1))
        if not deltas:
            result["metrics"][metric] = {"delta_pct": None, "status": "undefined ratio to zero"}
            continue
        median = statistics.median(deltas)
        mad = statistics.median(abs(value - median) for value in deltas)
        result["metrics"][metric] = {"delta_pct": median, "mad_pct": mad,
            "range_pct": [min(deltas), max(deltas)], "paired_deltas_pct": deltas,
            "status": "insufficient samples" if len(deltas) < 3 else
                      "sign varies across pairs" if min(deltas) < 0 < max(deltas) else "consistent sign"}
    # Keep offered load and delivery/loss next to CPU; no normalized CPU claim when windows differ.
    result["cpu_efficiency"] = "not estimated: verify offered load, delivery/loss and host/load window alignment before normalizing CPU"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    for workload in ("video", "fanout"):
        pairs = []
        rounds = sorted((args.directory / "relay/base").glob("round-*"))
        for base in rounds:
            current = args.directory / "relay/current" / base.name
            pairs.append({label: json.loads((path / workload / "summary.json").read_text())
                          for label, path in (("base", base), ("current", current))})
        result = reduce(pairs)
        (args.directory / f"comparison-{workload}.json").write_text(json.dumps(result, indent=2) + "\n")
        print(workload + ": " + json.dumps(result))


if __name__ == "__main__":
    main()
