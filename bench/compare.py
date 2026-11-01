#!/usr/bin/env python3
"""Reduce paired relay rounds and Criterion runs; retain dispersion instead of implying single-run precision."""
import argparse
from collections import Counter
import json
import math
import shutil
import statistics
from pathlib import Path

METRICS = ("recv_mbps", "send_mbps", "recv_fps", "send_fps", "cpu_cores", "cpu_us_per_frame", "cpu_cores_per_gbps",
           "generator_cpu_cores", "loss_pct", "latency_p99_ms")
CPU_METRICS = ("cpu_cores", "cpu_us_per_frame", "cpu_cores_per_gbps", "generator_cpu_cores")


def value(row, metric):
    return row.get('generator', {}).get('cpu_cores') if metric == 'generator_cpu_cores' else row.get(metric)


def summarize(deltas):
    """The median paired percent delta, its median absolute deviation, and whether every pair agreed."""
    median = statistics.median(deltas)
    mad = statistics.median(abs(value - median) for value in deltas)
    return {"delta_pct": median, "mad_pct": mad, "range_pct": [min(deltas), max(deltas)], "paired_deltas_pct": deltas,
            "status": "insufficient samples" if len(deltas) < 3 else
                      "sign varies across pairs" if min(deltas) < 0 < max(deltas) else "consistent sign"}


def reduce(pairs):
    if not pairs:
        raise ValueError("no paired rounds")
    result = {"pairs": len(pairs), "estimate": "median paired percent delta; dispersion is median absolute deviation", "metrics": {}}
    for metric in METRICS:
        if metric in CPU_METRICS and all(value(pair[label], metric) is None for pair in pairs for label in ("base", "current")):
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
        result["metrics"][metric] = {**summarize(deltas), "paired_absolute_deltas": absolute}
    # Keep offered load and delivery/loss next to CPU; no normalized CPU claim when windows differ.
    result["cpu_efficiency"] = "not estimated: verify offered load, delivery/loss and host/load window alignment before normalizing CPU"
    return result


def rounds(directory):
    """Every paired round's name, in numeric order, matching execution-order.tsv."""
    names = {label: {path.name for path in (directory / "relay" / label).glob("round-*")} for label in ("base", "current")}
    if names["base"] != names["current"]:
        raise ValueError(f"unpaired rounds: base {sorted(names['base'])}, current {sorted(names['current'])}")
    return sorted(names["base"], key=lambda name: int(name.removeprefix("round-")))


def cases(directory):
    """The `transport/workload` cases that every round of both revisions measured."""
    found = {frozenset(str(path.parent.relative_to(run)) for path in run.glob("*/*/summary.json"))
             for label in ("base", "current") for run in (directory / "relay" / label).glob("round-*")}
    if len(found) != 1:
        raise ValueError(f"rounds measured different cases: {sorted(map(sorted, found))}")
    return sorted(found.pop())


def load(directory, case):
    """Pair base/current summaries of one case in round order."""
    return [{label: json.loads((directory / "relay" / label / name / case / "summary.json").read_text())
             for label in ("base", "current")}
            for name in rounds(directory)]


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


# A paired case beyond this change is flagged, and fails the nightly only when its rounds agree.
# Loose until A/A runs on the hosted runner measure its noise floor.
THRESHOLD_PCT = 10
# Each alternating round costs seconds per case, so a noisy night confirms only its worst slowdowns.
CONFIRMATIONS = 20


def criterion_means(home):
    """Map each (target, full ID) measured under HOME/<package>/<target>/ to its mean nanoseconds."""
    means = {}
    for path in home.glob("*/*/**/new/estimates.json"):
        benchmark = json.loads(path.with_name("benchmark.json").read_text())["full_id"]
        if not isinstance(benchmark, str) or not benchmark or any(c in benchmark for c in "\t\r\n"):
            raise ValueError(f"invalid Criterion full_id in {path}: {benchmark!r}")
        key = ("/".join(path.relative_to(home).parts[:2]), benchmark)
        if key in means:
            raise ValueError(f"duplicate Criterion case {key} in {home}")
        mean = json.loads(path.read_text())["mean"]["point_estimate"]
        if isinstance(mean, bool) or not isinstance(mean, (int, float)) or not math.isfinite(mean) or mean <= 0:
            raise ValueError(f"invalid Criterion mean in {path}: {mean!r}")
        means[key] = mean
    return means


def percent(base, current):
    return 100 * (current / base - 1)


def criterion_screen(run):
    """The quick screen's slowdowns past the threshold, worst first, up to the confirmation budget."""
    base, current = criterion_means(run / "criterion-base"), criterion_means(run / "criterion")
    slower = sorted(((percent(base[key], current[key]), key) for key in base.keys() & current.keys()), reverse=True)
    return [key for delta, key in slower if delta > THRESHOLD_PCT][:CONFIRMATIONS]


def criterion_paired(run):
    """Classify every case from the quick screen and the alternating confirmation rounds."""
    metadata = json.loads((run / "metadata.json").read_text())
    rounds = metadata["rounds"]
    base, current = criterion_means(run / "criterion-base"), criterion_means(run / "criterion")
    unavailable = []
    if (run / "paired-unavailable.tsv").exists():
        for line in (run / "paired-unavailable.tsv").read_text().splitlines():
            target, reason = line.split("\t", 1)
            unavailable.append({"target": target, "reason": reason})
    blocked = {entry["target"] for entry in unavailable}
    confirmed = {}
    lines = (run / "confirm.tsv").read_text().splitlines() if (run / "confirm.tsv").exists() else []
    for index, line in enumerate(lines, 1):
        key = tuple(line.split("\t", 1))
        if key[0] in blocked:
            continue
        deltas = []
        for number in range(1, rounds + 1):
            sides = [criterion_means(run / "confirm" / str(index) / f"round-{number}" / side).get(key)
                     for side in ("base", "current")]
            if None in sides:
                raise ValueError(f"confirmation round {number} did not measure {key}")
            deltas.append(percent(*sides))
        confirmed[key] = summarize(deltas)
    cases = []
    for key in sorted(base.keys() | current.keys()):
        target, benchmark = key
        case = {"target": target, "id": benchmark, "base_ns": base.get(key), "current_ns": current.get(key)}
        if key not in current:
            case["verdict"] = "removed"
        elif key not in base:
            case["verdict"] = "unavailable" if target in blocked else "new"
        else:
            case["delta_pct"] = delta = percent(base[key], current[key])
            if key in confirmed:
                case["rounds"] = confirmed[key]
                # Every round strictly slower, so a tie or a single fast round rejects it at any round count.
                slower = all(delta > 0 for delta in case["rounds"]["paired_deltas_pct"])
                case["verdict"] = "regression" if slower and case["rounds"]["delta_pct"] > THRESHOLD_PCT else "noise"
            elif delta > THRESHOLD_PCT:
                case["verdict"] = "unavailable" if target in blocked else "unconfirmed"
            elif delta < -THRESHOLD_PCT:
                case["verdict"] = "faster"
            else:
                case["verdict"] = "unchanged"
        cases.append(case)
    return {"commit": metadata["revision"], "base": metadata["base"], "threshold_pct": THRESHOLD_PCT,
            "rounds": rounds, "unavailable": unavailable, "cases": cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--criterion-cases", action="store_true", help="list full IDs from isolated discovery samples")
    parser.add_argument("--criterion-baseline", nargs=2, metavar=("DISCOVERY", "ID"), help="align an exact baseline with current discovery")
    parser.add_argument("--criterion-screen", action="store_true", help="list a paired run's slowdowns to confirm")
    parser.add_argument("--criterion-paired", action="store_true", help="classify a paired run's cases into paired.json")
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    if args.criterion_cases:
        print("\n".join(criterion_cases(args.directory)))
        return
    if args.criterion_screen:
        for target, benchmark in criterion_screen(args.directory):
            print(f"{target}\t{benchmark}")
        return
    if args.criterion_paired:
        result = criterion_paired(args.directory)
        (args.directory / "paired.json").write_text(json.dumps(result, indent=2) + "\n")
        verdicts = Counter(case["verdict"] for case in result["cases"])
        print(f"\nPaired comparison against {result['base']}: "
              + ", ".join(f"{count} {verdict}" for verdict, count in sorted(verdicts.items())))
        for case in result["cases"]:
            if case["verdict"] == "regression":
                print(f"REGRESSION {case['target']} {case['id']}: "
                      f"{case['rounds']['delta_pct']:+.1f}% over {result['rounds']} rounds")
        for entry in result["unavailable"]:
            print(f"UNAVAILABLE {entry['target']}: {entry['reason']}")
        return
    if args.criterion_baseline:
        discovery, benchmark = args.criterion_baseline
        criterion_baseline(args.directory, Path(discovery), benchmark)
        return
    for case in cases(args.directory):
        result = reduce(load(args.directory, case))
        (args.directory / f"comparison-{case.replace('/', '-')}.json").write_text(json.dumps(result, indent=2) + "\n")
        print(case + ": " + json.dumps(result))


if __name__ == "__main__":
    main()
