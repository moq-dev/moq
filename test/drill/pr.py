#!/usr/bin/env python3
"""Select PR-sensitive drill mutations and run the existing sensitivity harness."""

import argparse
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def select(changed: set[str], mutations: Path) -> list[str]:
    patches = sorted(mutations.glob("*.patch"))
    if not patches:
        raise ValueError("no drill mutations found")
    all_mutations = bool(changed & {
        "rs/moq-relay/tests/drills.rs",
        "test/drill/sensitivity.sh",
        "test/drill/pr.py",
    })
    selected = []
    for patch in patches:
        if not re.fullmatch(r"[a-z0-9-]+", patch.stem):
            raise ValueError(f"invalid mutation name: {patch.name}")
        paths = set(re.findall(r"^\+\+\+ b/(.+)$", patch.read_text(), re.MULTILINE))
        if not paths:
            raise ValueError(f"mutation names no target files: {patch.name}")
        if all_mutations or f"test/drill/mutations/{patch.name}" in changed or changed & paths:
            selected.append(patch.stem)
    return selected


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["select", "run"])
    parser.add_argument("base", nargs="?", default="")
    args = parser.parse_args()
    changed = subprocess.check_output(
        ["bash", str(ROOT / "sh/changed.sh"), args.base], cwd=ROOT, text=True,
    ).splitlines()
    mutations = select(set(changed), ROOT / "test/drill/mutations")
    if args.mode == "select":
        # Always report, including an empty diff, so this can later be a required job.
        print(f"needed={str(bool(mutations)).lower()}")
        return 0
    if not mutations:
        print("No affected drill mutations.")
        return 0
    print("Selected drill mutations: " + ", ".join(mutations), flush=True)
    return subprocess.call(["bash", str(ROOT / "test/drill/sensitivity.sh"), *mutations], cwd=ROOT)


if __name__ == "__main__":
    raise SystemExit(main())
