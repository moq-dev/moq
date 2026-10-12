#!/usr/bin/env python3
"""Compare byte counts with the previous report and a thirty-day anchor."""
import argparse
import datetime as dt
import json
from pathlib import Path


def compare(current, history):
    """Return regressions without comparing different artifact sets or formats."""
    def validate(row):
        if row.get("version") != 1 or not row.get("bytes"):
            raise ValueError("expected a nonempty version 1 size report")
        date = dt.date.fromisoformat(row["date"])
        for name, size in row["bytes"].items():
            if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
                raise ValueError(f"invalid byte count for {name}: {size}")
        return date

    now = validate(current)
    for row in history:
        if validate(row) > now:
            raise ValueError("history contains a future report")
        if row["bytes"].keys() != current["bytes"].keys():
            raise ValueError("artifact set changed; review and explicitly start a new history")
    prior = sorted(history, key=lambda row: row["date"])
    if not prior:
        return []
    anchors = [row for row in prior if dt.date.fromisoformat(row["date"]) <= now - dt.timedelta(days=30)]
    comparisons = [("previous", prior[-1], 0.05)]
    if anchors:
        comparisons.append(("30 days", anchors[-1], 0.10))
    return [f"{name}: {size} > {row['bytes'][name]} bytes + {limit:.0%} ({label})"
            for label, row, limit in comparisons for name, size in current["bytes"].items()
            if size > row["bytes"][name] * (1 + limit)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--history", type=Path)
    args = parser.parse_args()
    current = json.loads((args.directory / "sizes.json").read_text())
    history = json.loads(args.history.read_text()) if args.history else []
    failures = compare(current, history)
    # Keep the oldest available thirty-day anchor and every newer report.
    cutoff = dt.date.fromisoformat(current["date"]) - dt.timedelta(days=30)
    anchors = [row for row in history if dt.date.fromisoformat(row["date"]) <= cutoff]
    kept = ([max(anchors, key=lambda row: row["date"])] if anchors else [])
    kept += [row for row in history if dt.date.fromisoformat(row["date"]) > cutoff]
    (args.directory / "history.json").write_text(json.dumps(kept + [current], indent=2) + "\n")
    summary = "| Artifact | Bytes |\n|---|---:|\n"
    summary += "".join(f"| {name} | {size:,} |\n" for name, size in sorted(current["bytes"].items()))
    summary += "\n" + ("\n".join(failures) if failures else "No size regression; a new artifact history starts with one report.") + "\n"
    if current.get("seconds"):
        summary += "\n| Release build | Seconds |\n|---|---:|\n"
        summary += "".join(f"| {job} | {seconds:,} |\n" for job, seconds in sorted(current["seconds"].items()))
    (args.directory / "summary.md").write_text(summary)
    print(summary)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
