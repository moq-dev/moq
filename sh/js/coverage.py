#!/usr/bin/env python3
"""Measure Bun unit tests in separate package processes, as the merge gate does."""

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def packages(root):
    directories = json.loads((root / "package.json").read_text())["workspaces"]
    # Shared tooling has tests but is not a published workspace.
    return [
        root / "js/common",
        *(
            root / directory
            for directory in directories
            if directory.startswith("js/") and any((root / directory / "src").glob("**/*.test.ts"))
        ),
    ]


def report(root, directories):
    # Consumer tests exercise dependencies too. Union executed lines across the
    # separate processes instead of calling a dependency untested in its own run.
    lines = {}
    for directory in directories:
        source = None
        for line in (root / "target/coverage/js" / directory.name / "lcov.info").read_text().splitlines():
            if line.startswith("SF:"):
                source = (directory / line[3:]).resolve()
            elif line.startswith("DA:") and source is not None:
                number, count, *_ = line[3:].split(",")
                found = lines.setdefault(source, {})
                found[int(number)] = found.get(int(number), False) or int(count) > 0
    summary = {}
    manifest = json.loads((root / "package.json").read_text())
    for directory in [root / "js/common", *(root / path for path in manifest["workspaces"] if path.startswith("js/"))]:
        sources = [
            path
            for path in (directory / "src").rglob("*")
            if path.suffix in (".ts", ".tsx") and not path.name.endswith((".test.ts", ".d.ts"))
        ]
        if directory.name == "common":
            sources = [path for path in directory.glob("*.ts") if not path.name.endswith(".test.ts")]
        measured = {path: hits for path, hits in lines.items() if path in sources}
        summary[directory.name] = {
            "lines": sum(len(hits) for hits in measured.values()),
            "covered": sum(sum(hits.values()) for hits in measured.values()),
            # Bun reports loaded modules only; never turn an absent module into 100%.
            "unloaded": [str(path.relative_to(directory)) for path in sorted(set(sources) - measured.keys())],
        }
    return summary


def main():
    selected = packages(ROOT)
    name = sys.argv[1] if len(sys.argv) > 1 else ""
    if name:
        selected = [directory for directory in selected if directory.name == name]
        if not selected:
            raise ValueError(f"no JS unit-test package: {name}")
    (ROOT / "target/coverage/js/summary.json").unlink(missing_ok=True)
    for directory in selected:
        (ROOT / "target/coverage/js" / directory.name / "lcov.info").unlink(missing_ok=True)
    subprocess.run(["bun", "install", "--frozen-lockfile"], cwd=ROOT, check=True)
    for directory in selected:
        out = ROOT / "target/coverage/js" / directory.name
        out.mkdir(parents=True, exist_ok=True)
        print(f"coverage: {directory.relative_to(ROOT)}", flush=True)
        with (out / "test.log").open("w") as log:
            result = subprocess.run(
                [
                    "bun",
                    "test",
                    "--only-failures",
                    "--coverage",
                    "--coverage-reporter=text",
                    "--coverage-reporter=lcov",
                    f"--coverage-dir={out}",
                ],
                cwd=directory,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        print((out / "test.log").read_text(), flush=True)
        result.check_returncode()
    summary = report(ROOT, selected)
    (ROOT / "target/coverage/js/summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    rows = [
        "## JS unit coverage",
        "",
        "| Package | Covered lines | Measured lines | Unloaded sources |",
        "| --- | ---: | ---: | ---: |",
    ]
    for name, entry in summary.items():
        rows.append(f"| {name} | {entry['covered']} | {entry['lines']} | {len(entry['unloaded'])} |")
        print(f"{name}: {entry['covered']}/{entry['lines']} measured lines, {len(entry['unloaded'])} unloaded sources")

    if destination := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(destination).open("a") as output:
            output.write("\n".join(rows) + "\n")


if __name__ == "__main__":
    main()
