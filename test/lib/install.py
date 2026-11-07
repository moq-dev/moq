#!/usr/bin/env python3
"""Install the workspace once per dependency configuration, under a checkout lock."""

import fcntl
import hashlib
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
(ROOT / ".tmp").mkdir(exist_ok=True)
with (ROOT / ".tmp/bun-install.lock").open("w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    digest = hashlib.sha256(subprocess.check_output(["bun", "--version"]))
    # Include the postinstall build inputs as well as dependency manifests.
    paths = subprocess.check_output(
        ["git", "ls-files", "-z", "--", "*package.json", "bun.lock",
         "flake.nix", "flake.lock", "nix", "js/libav"], cwd=ROOT,
    ).split(b"\0")
    for name in sorted(filter(None, paths)):
        digest.update(name)
        digest.update((ROOT / name.decode()).read_bytes())
    fingerprint = digest.hexdigest()
    stamp = ROOT / "node_modules/.moq-install"
    # Another invocation must not relink dependencies or rebuild libav while
    # the first invocation is importing them or playing its bundled page.
    if not stamp.exists() or stamp.read_text() != fingerprint:
        stamp.unlink(missing_ok=True)
        subprocess.run(["bun", "install", "--frozen-lockfile"], cwd=ROOT, check=True)
        stamp.write_text(fingerprint)
