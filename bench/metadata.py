#!/usr/bin/env python3
"""Retain the checkout and machine context for a benchmark, including failed runs."""
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
out = Path(sys.argv[1])
mode = sys.argv[3] if len(sys.argv) > 3 else ""

def command(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.STDOUT).strip()

(out / "dirty.patch").write_bytes(subprocess.check_output(["git", "diff", "--binary", "HEAD"]))
untracked = []
for name in subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"]).decode().split('\0'):
    # run.sh keeps the evidence outside the repository or git-ignored, so it never appears here.
    if not name:
        continue
    target = out / "untracked" / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(name, target, follow_symlinks=False)
    untracked.append(name)
metadata = {"untracked": untracked, "revision": command("git", "rev-parse", "HEAD"), "base": sys.argv[2],
 "rustc": command("rustc", "-vV"), "cargo": command("cargo", "-V"),
 "os": platform.platform(), "machine": platform.machine(), "processor": platform.processor(),
 "affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
 "environment": {key: value for key, value in os.environ.items() if key.startswith(("BENCH_", "MOQ_BENCH_", "CARGO_PROFILE_")) or key in ("RUSTFLAGS", "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS", "TOKIO_WORKER_THREADS", "LD_PRELOAD", "MALLOC_CONF", "GLIBC_TUNABLES", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "SESSION_CPU")},
 "profile": "profiling" if mode == "--profile" else "release",
 "features": {"--runtime": "default + moq-relay/io-uring,moq-bench/uring", "--profile": "default; heap adds moq-relay/jemalloc",
              "--criterion": "see criterion-command.json"}.get(mode, "default"),
 "mode": mode, "runtime_workers": sys.argv[4] if len(sys.argv) > 4 else None,
 "rounds": int(sys.argv[5]) if len(sys.argv) > 5 else None,
 "allocator": "system by build; preload and allocator tunables are recorded in environment"
 + ("; heap runs jemalloc, profiled per the MALLOC_CONF in harness/relay.sh" if mode == "--profile" else ""),
 "profilers": {tool: command(tool, "--version").splitlines()[0] for tool in ("perf", "jeprof") if shutil.which(tool)} if mode == "--profile" else None,
 "workload": None if mode == "--criterion" else {"startup": "2s", "duration": "10s", "report": "500ms"}}
(out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
subprocess.run(["cargo", "tree", "--locked", "-p", "moq-relay", "-e", "features"], stdout=(out / "features.txt").open("w"), check=True)
if Path("/proc/cpuinfo").exists():
    (out / "cpuinfo.txt").write_text(Path("/proc/cpuinfo").read_text())
