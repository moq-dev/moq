#!/usr/bin/env python3
"""Retain the checkout and machine context for a benchmark, including failed runs."""
import json
import os
import platform
import subprocess
import sys
from pathlib import Path
out = Path(sys.argv[1])
mode = sys.argv[3] if len(sys.argv) > 3 else ""

def command(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT).strip()

(out / "dirty.patch").write_text(command("git", "diff", "--binary", "HEAD"))
metadata = {"revision": command("git", "rev-parse", "HEAD"), "base": sys.argv[2],
 "rustc": command("rustc", "-vV"), "cargo": command("cargo", "-V"),
 "os": platform.platform(), "machine": platform.machine(), "processor": platform.processor(),
 "affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
 "environment": {key: value for key, value in os.environ.items() if key.startswith(("MOQ_BENCH_", "CARGO_PROFILE_")) or key in ("RUSTFLAGS", "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS")},
 "profile": "release", "features": "default + moq-relay/io-uring,moq-bench/uring" if mode == "--runtime" else "default",
 "mode": mode, "runtime_workers": sys.argv[4] if len(sys.argv) > 4 else None,
 "runtime_rounds": sys.argv[5] if len(sys.argv) > 5 else None,
 "allocator": "system; moq-relay/jemalloc is not enabled by this harness",
 "workload": {"startup": "2s", "duration": "10s", "report": "500ms"}}
(out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
subprocess.run(["cargo", "tree", "--locked", "-p", "moq-relay", "-e", "features"], stdout=(out / "features.txt").open("w"), check=True)
if Path("/proc/cpuinfo").exists():
    (out / "cpuinfo.txt").write_text(Path("/proc/cpuinfo").read_text())
