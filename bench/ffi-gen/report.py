"""Combine the runners' rounds into one Markdown table (see README.md).

Each cell is the median across rounds, with the spread (min-max) beside it when there
are several. The last column per language is UniFFI's value over BoltFFI's, oriented so
that above 1 means BoltFFI is better: time and CPU divide UniFFI by BoltFFI, and
throughput divides BoltFFI by UniFFI.
"""

import json
import os
import statistics
import sys
from collections import defaultdict
from pathlib import Path

RUNNERS = ["rust", "python-uniffi", "python-boltffi", "kotlin-uniffi", "kotlin-boltffi"]


def load(directory: Path) -> dict[str, dict[str, list[float]]]:
    values: dict[str, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    for path in sorted(directory.glob("*.json")):
        report = json.loads(path.read_text())
        for result in report["results"]:
            values[report["runner"]][result["name"]].append(result["value"])
    return values


def cell(samples: list[float]) -> str:
    if not samples:
        return "-"
    median = statistics.median(samples)
    text = f"{median:.3g}"
    if len(samples) > 1:
        text += f" ({min(samples):.3g}-{max(samples):.3g})"
    return text


def advantage(name: str, uniffi: list[float], boltffi: list[float]) -> str:
    if not uniffi or not boltffi:
        return "-"
    u, b = statistics.median(uniffi), statistics.median(boltffi)
    ratio = b / u if name.startswith("throughput/") else u / b
    return f"{ratio:.3g}x"


def main() -> None:
    directory = Path(sys.argv[1])
    values = load(directory)
    names = list(dict.fromkeys(name for runner in RUNNERS for name in values.get(runner, {})))
    units = {}
    for path in directory.glob("*.json"):
        for result in json.loads(path.read_text())["results"]:
            units[result["name"]] = result["unit"]

    header = ["case", "unit", *RUNNERS, "python gain", "kotlin gain"]
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    for name in names:
        row = [name, units[name]]
        row += [cell(values.get(runner, {}).get(name, [])) for runner in RUNNERS]
        for lang in ("python", "kotlin"):
            row.append(advantage(name, values[f"{lang}-uniffi"][name], values[f"{lang}-boltffi"][name]))
        lines.append("| " + " | ".join(row) + " |")
    table = "\n".join(lines)
    (directory / "report.md").write_text(table + "\n")
    print(table)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a") as output:
            output.write("## Binding generator comparison\n\n" + table + "\n")


if __name__ == "__main__":
    main()
