#!/usr/bin/env python3
"""Fail when a build target's dependency graph compiles C crypto.

rind is MoQ's only crypto, so on every target a workflow names, the workspace's
normal and build dependencies hold no C crypto crate, and no crypto crate
reaches `cc`. Each target is walked on its own: `--target all` turns on custom
cfgs no build sets, such as libcrux's Valgrind hooks. Run from `just rs lint`.
"""

import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# C crypto libraries, refused anywhere in the graph.
C_CRYPTO = {"aws-lc-sys", "ring", "openssl-sys"}
# Crypto crates that must compile without a C toolchain.
CRYPTO = re.compile(r"^(rind|rind-.+|libcrux-.+|rustls|rustls-.+)$")


def run(*args: str) -> str:
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout


known = set(run("rustc", "--print", "target-list").split())
named = set()
for workflow in (ROOT / ".github/workflows").glob("*.yml"):
    named |= set(re.findall(r"\b[a-z0-9_]+(?:-[a-z0-9_]+){2,3}\b", workflow.read_text()))
targets = sorted(named & known)
if not targets:
    sys.exit("error: no build target found in .github/workflows")

errors = []
for target in targets:
    graph = run(
        "cargo",
        "tree",
        "--locked",
        "--workspace",
        "--all-features",
        "--target",
        target,
        "-e",
        "normal,build",
        "--prefix",
        "depth",
        "--format",
        "{p}",
    )
    # A repeated node is printed once in full, so collecting every edge recovers the graph.
    children = defaultdict(set)
    nodes = set()
    path = []
    for line in graph.splitlines():
        match = re.match(r"^(\d+)(\S+) ", line)
        if not match:
            continue
        depth, name = int(match[1]), match[2]
        nodes.add(name)
        del path[depth:]
        if path:
            children[path[-1]].add(name)
        path.append(name)

    for name in sorted(C_CRYPTO & nodes):
        errors.append(f"{target}: {name} is C crypto; use rind")
    for root in sorted(name for name in nodes if CRYPTO.match(name)):
        seen, stack = {root}, [root]
        while stack:
            for child in children[stack.pop()] - seen:
                seen.add(child)
                stack.append(child)
        if "cc" in seen:
            errors.append(f"{target}: crypto crate {root} reaches cc")

if errors:
    print("\n".join(errors), file=sys.stderr)
    print(
        "Find the edge: cargo tree --workspace --all-features -e normal,build --target <target> -i <crate>",
        file=sys.stderr,
    )
    sys.exit(1)
print(f"no C crypto on {len(targets)} targets: {' '.join(targets)}")
