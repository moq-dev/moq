#!/usr/bin/env python3
"""Compare byte counts with the previous report and a thirty-day anchor."""
import argparse
import datetime as dt
import json
from pathlib import Path


def compare(current, history):
    """Return regressions; a new artifact starts its series, but a missing one is refused."""
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
    # A rename shows up as a missing key, so it is refused along with a removal.
    if missing := sorted(tracked(history) - current["bytes"].keys()):
        raise ValueError(f"artifacts missing or renamed: {', '.join(missing)}; "
                         "review and explicitly start a new history")
    prior = sorted(history, key=lambda row: row["date"])
    if not prior:
        return []
    anchors = [row for row in prior if dt.date.fromisoformat(row["date"]) <= now - dt.timedelta(days=30)]
    comparisons = [("previous", prior[-1], 0.05)]
    if anchors:
        comparisons.append(("30 days", anchors[-1], 0.10))
    return [f"{name}: {size} > {row['bytes'][name]} bytes + {limit:.0%} ({label})"
            for label, row, limit in comparisons for name, size in current["bytes"].items()
            if name in row["bytes"] and size > row["bytes"][name] * (1 + limit)]


def tracked(history):
    """Every artifact name a retained report measured."""
    return {name for row in history for name in row["bytes"]}


def retain(current, history):
    """Keep the thirty-day anchor compare() would pick, every newer report, and the current one."""
    cutoff = dt.date.fromisoformat(current["date"]) - dt.timedelta(days=30)
    # A stable sort keeps the last of several same-date reports, matching compare().
    anchors = sorted((row for row in history if dt.date.fromisoformat(row["date"]) <= cutoff),
                     key=lambda row: row["date"])
    return anchors[-1:] + [row for row in history if dt.date.fromisoformat(row["date"]) > cutoff] + [current]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--history", type=Path)
    args = parser.parse_args()
    current = json.loads((args.directory / "sizes.json").read_text())
    history = json.loads(args.history.read_text()) if args.history else []
    failures = compare(current, history)
    (args.directory / "history.json").write_text(json.dumps(retain(current, history), indent=2) + "\n")
    summary = "| Artifact | Bytes |\n|---|---:|\n"
    summary += "".join(f"| {name} | {size:,} |\n" for name, size in sorted(current["bytes"].items()))
    if failures:
        summary += "\n" + "\n".join(failures) + "\n"
    elif not history:
        summary += "\nNo size regression; a new artifact history starts with one report.\n"
    else:
        summary += "\nNo size regression.\n"
    if history and (added := sorted(current["bytes"].keys() - tracked(history))):
        summary += "\nNew artifact series, compared from their next report:\n\n"
        summary += "".join(f"- {name}\n" for name in added)
    if current.get("seconds"):
        summary += "\n| Release build | Seconds |\n|---|---:|\n"
        summary += "".join(f"| {job} | {seconds:,} |\n" for job, seconds in sorted(current["seconds"].items()))
    (args.directory / "summary.md").write_text(summary)
    print(summary)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
