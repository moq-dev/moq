#!/usr/bin/env python3
"""Keep every in-tree npm Playwright client on the root Nix browser's version."""

import json
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEPENDENCIES = ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies")


def check(root, version):
    files = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "package.json", "**/package.json"],
        cwd=root,
        text=True,
    ).splitlines()
    problems = []
    for name in files:
        if name.startswith("test/smoke/"):
            continue  # Smoke deliberately owns a separate flake and freshness check.
        path = root / name
        if not path.exists():
            continue  # A staged or working-tree manifest deletion removes its pin.
        package = json.loads(path.read_text())
        for field in DEPENDENCIES:
            pin = package.get(field, {}).get("playwright")
            if pin is not None and pin != version:
                problems.append(f"{name}: {field}.playwright is {pin!r}, expected exact {version!r}")
    return problems


def main():
    version = os.environ.get("PLAYWRIGHT_VERSION", "")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("PLAYWRIGHT_VERSION is missing or malformed; run inside the root nix develop shell")
    problems = check(ROOT, version)
    if problems:
        raise SystemExit("Playwright must match the root Nix browser:\n" + "\n".join(problems))
    print(f"Playwright pins match the root Nix browser: {version}")


if __name__ == "__main__":
    main()
