#!/usr/bin/env python3
"""Install the workspace once per dependency configuration, under a checkout lock."""

import argparse
import fcntl
import hashlib
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def fingerprint():
    digest = hashlib.sha256(subprocess.check_output(["bun", "--version"]))
    digest.update(str(shutil.which("nix")).encode())
    # Include the postinstall build inputs as well as dependency manifests.
    paths = subprocess.check_output(
        [
            "git",
            "ls-files",
            "-z",
            "--",
            "*package.json",
            "bun.lock",
            "flake.nix",
            "flake.lock",
            "nix",
            "js/libav",
            "patches",
        ],
        cwd=ROOT,
    ).split(b"\0")
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
with (ROOT / ".tmp/bun-install.lock").open("w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    current = fingerprint()
    stamp = ROOT / "node_modules/.moq-install"
    # Another invocation must not relink dependencies or rebuild libav while
    # the first invocation is importing them or playing its bundled page.
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
