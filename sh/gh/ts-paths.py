#!/usr/bin/env python3
"""Fail when ts.yml's crate path filters differ from the workspace crates test/ts/run.sh builds.

The filter covers exactly the activated closure of run.sh's `-p` packages with their
default features: a crate outside it must not trigger the expensive workflow, and a
crate inside it must. crates.io dependencies are covered by the Cargo.lock filter.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ts.yml"
HARNESS = ROOT / "test/ts/run.sh"
# ts.yml's ubuntu-latest runner, so target-specific dependencies resolve the same on any host.
TARGET = "x86_64-unknown-linux-gnu"


def built_dirs(tree: str, workspace_root: str) -> set[str]:
    """Workspace-relative directories of the local packages in `cargo tree --format {p}` output.

    Both arguments come from cargo, so they share its path syntax, which is `C:\\...` on Windows
    even under MSYS or Cygwin.
    """
    root = workspace_root.replace("\\", "/").rstrip("/") + "/"
    sources = (source.replace("\\", "/") for source in re.findall(r"^\S+ v\S+ \(([^)]+)\)", tree, re.MULTILINE))
    return {source.removeprefix(root) for source in sources if source.startswith(root)}


def cargo(*args: str) -> str:
    return subprocess.run(["cargo", *args], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout


def main() -> None:
    packages = sorted(set(re.findall(r"(?<![\w-])-p ([\w-]+)", HARNESS.read_text())))
    if not packages:
        sys.exit(f"error: no `-p` packages found in {HARNESS.relative_to(ROOT)}")

    tree = cargo("tree", "--locked", "--target", TARGET, "--edges", "normal,build", "--prefix", "none",
                 "--format", "{p}", *(arg for p in packages for arg in ("-p", p)))
    workspace_root = json.loads(cargo("metadata", "--locked", "--no-deps", "--format-version", "1"))["workspace_root"]
    built = built_dirs(tree, workspace_root)

    # Every `rs/<dir>/**` filter, even one whose crate is gone; other filters are the harness's own inputs.
    filtered = set(re.findall(r'^\s+- "(rs/[^"*]+)/\*\*"$', WORKFLOW.read_text(), re.MULTILINE))

    missing, extra = sorted(built - filtered), sorted(filtered - built)
    for path in missing:
        print(f"error: {WORKFLOW.relative_to(ROOT)} misses \"{path}/**\", which `just test ts` builds", file=sys.stderr)
    for path in extra:
        print(f"error: {WORKFLOW.relative_to(ROOT)} filters \"{path}/**\", which `just test ts` does not build", file=sys.stderr)
    if missing or extra:
        sys.exit(1)


if __name__ == "__main__":
    main()
