#!/usr/bin/env python3
"""Model-check one TLA+ spec with TLC: `sh/tlc.py <spec.tla> [tlc args...]`.

The `tla` recipes run every model through here, so a run shares the host:
TLC runs at the lowest priority on 4 workers, which Nightly's runners have and
the `verify-*` skills' times assume; core-seconds stay near flat from 2 to 24
workers, so more costs other commands their cores and saves no CPU. A 4 GB heap
fits every model here, where the JVM would otherwise take a quarter of RAM.

TLC 1.8 runs with the Community Modules on its classpath, pinned below, so
their Java overrides (such as `FoldSet`) evaluate natively.

TLC never removes a metadir it was killed in, and a model's can reach tens of
GB, so each run's lives under the checkout's `.tmp/tlc/`, is removed when the
run exits, and holds a lock the kernel drops when the run dies, however it
dies. A run first removes every metadir whose lock nobody holds.
"""

import fcntl
import json
import os
import resource
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# The newest release built for nixpkgs' TLC 1.8.0, a June 2023 build: later
# ones call `OpValue.eval`, which TLC added that November. Move both together.
COMMUNITY = (
    "https://github.com/tlaplus/CommunityModules/releases/download/202311011627/CommunityModules-deps-202311011627.jar",
    "sha256-Ca3VmiMGYV/SODGw+cI8ENQOvPZj/5TdLZOtwSz5o9s=",
)


def nix(*args):
    return subprocess.run(["nix", *args], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout


def classpath():
    """The JRE, and TLC's jar beside the Community Modules', from Nix."""
    jre, tla = nix(
        "build", "--inputs-from", ".", "--no-link", "--print-out-paths", "nixpkgs#jre", "nixpkgs#tlaplus18"
    ).split()
    url, hash = COMMUNITY
    community = json.loads(nix("store", "prefetch-file", "--json", "--expected-hash", hash, url))["storePath"]
    return f"{jre}/bin/java", f"{tla}/share/java/tla2tools.jar:{community}"


def claim(base):
    """Remove the metadirs no live run holds, then make and lock this run's.

    One lock over `base` keeps a sweep from taking a metadir between its
    creation and its own lock. The returned descriptor holds the metadir's
    lock; a child inheriting it keeps the lock while it lives.
    """
    base.mkdir(parents=True, exist_ok=True)
    with open(base / ".lock", "w") as guard:
        fcntl.flock(guard, fcntl.LOCK_EX)
        for meta in base.iterdir():
            if not meta.is_dir():
                continue
            with open(meta / "lock", "a") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue
            shutil.rmtree(meta)
        meta = Path(tempfile.mkdtemp(dir=base))
        fd = os.open(meta / "lock", os.O_WRONLY | os.O_CREAT, 0o644)
        fcntl.flock(fd, fcntl.LOCK_EX)
    return meta, fd


def main(argv):
    if not argv:
        sys.exit("usage: sh/tlc.py <spec.tla> [tlc args...]")
    spec, args = argv[0], argv[1:]
    java, cp = classpath()
    meta, fd = claim(ROOT / ".tmp/tlc")
    # Turn a SIGTERM into an ordinary exit so the metadir is removed; SIGINT
    # already reaches TLC through the terminal's process group.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    start, before = time.monotonic(), resource.getrusage(resource.RUSAGE_CHILDREN)
    try:
        command = ["nice", "-n", "19", java, "-Xmx4g", "-XX:+UseParallelGC", "-cp", cp, "tlc2.TLC"]
        command += ["-workers", "4", "-checkpoint", "0", "-cleanup", "-noGenerateSpecTE", "-metadir", str(meta)]
        status = subprocess.run([*command, *args, spec], cwd=ROOT, pass_fds=(fd,)).returncode
    except KeyboardInterrupt:
        status = 130
    finally:
        shutil.rmtree(meta, ignore_errors=True)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
    print(f"tlc: {cpu:.0f} core-seconds in {time.monotonic() - start:.0f}s", file=sys.stderr)
    # A child killed by a signal reports it negated; exit as a shell would.
    return status if status >= 0 else 128 - status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
