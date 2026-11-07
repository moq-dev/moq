#!/usr/bin/env python3
"""Install the workspace once per dependency configuration, under a checkout lock."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def fingerprint():
    digest = hashlib.sha256(subprocess.check_output(["bun", "--version"]))
    digest.update(str(shutil.which("nix") is not None).encode())
    # Include the postinstall build inputs as well as dependency manifests.
    manifest = json.loads((ROOT / "package.json").read_text())
    manifests = {b"package.json"}
    for workspace in manifest["workspaces"]:
        matches = list(ROOT.glob(workspace))
        # Exact workspace paths still contribute when their manifest was deleted.
        for directory in matches or [ROOT / workspace]:
            manifests.add(str((directory / "package.json").relative_to(ROOT)).encode())
    paths = subprocess.check_output(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "bun.lock",
            "flake.nix",
            "flake.lock",
            "nix/overlay.nix",
            "js/libav",
            "patches",
        ],
        cwd=ROOT,
    ).split(b"\0")
    paths = set(paths) | manifests
    for name in sorted(filter(None, paths)):
        digest.update(name)
        path = ROOT / name.decode()
        if path.exists():
            digest.update(path.read_bytes())
        else:
            digest.update(b"missing")
    return digest.hexdigest()


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("mode", choices=("frozen", "update"))
args = parser.parse_args()

(ROOT / ".tmp").mkdir(exist_ok=True)
with (ROOT / ".tmp/bun-install.lock").open("ab") as lock:
    # Native Windows supports sequential recipes; Unix coordinates parallel runs.
    if os.name != "nt":
        import fcntl

        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("waiting for another workspace dependency install", file=sys.stderr, flush=True)
            fcntl.flock(lock, fcntl.LOCK_EX)
    current = fingerprint()
    stamp = ROOT / "node_modules/.moq-install"
    # Identical inputs reuse the tree while other invocations consume it.
    # Dependency edits invalidate it; stop consumers before changing inputs.
    if (
        not stamp.exists()
        or stamp.read_text() != current
        or not (ROOT / "js/libav/dist/libav-opus-aac-af.mjs").exists()
    ):
        stamp.unlink(missing_ok=True)
        subprocess.run(
            ["bun", "install", *(["--frozen-lockfile"] if args.mode == "frozen" else [])], cwd=ROOT, check=True
        )
        stamp.write_text(fingerprint())
