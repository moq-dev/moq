#!/usr/bin/env python3
"""Install and test the GStreamer formula with the just-built macOS tarball."""

import hashlib
import os
import subprocess
import sys
from pathlib import Path

version, target, release_dir = sys.argv[1:]
if target != "aarch64-apple-darwin":
    raise ValueError("the Homebrew smoke test requires the macOS arm64 archive")
archive = (Path(release_dir) / f"moq-gst-v{version}-{target}.tar.gz").resolve(strict=True)
template = Path(".github/homebrew/Formula/moq-gst.rb.tmpl").read_text()
sha = hashlib.sha256(archive.read_bytes()).hexdigest()
formula = template.replace("__VERSION__", version)
for platform in ["AARCH64_APPLE_DARWIN", "AARCH64_UNKNOWN_LINUX_GNU", "X86_64_UNKNOWN_LINUX_GNU"]:
    formula = formula.replace(f"__SHA256_{platform}__", sha)
remote = f"https://github.com/moq-dev/moq/releases/download/moq-gst-v#{{version}}/moq-gst-v#{{version}}-{target}.tar.gz"
formula = formula.replace(remote, archive.as_uri())
environment = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1")
installed = subprocess.run(
    ["brew", "list", "--formula", "--versions"], capture_output=True, text=True, env=environment, check=True
)
if any(line.split()[0].split("/")[-1] == "moq-gst" for line in installed.stdout.splitlines() if line.strip()):
    raise ValueError("moq-gst is already installed; run this test in a clean Homebrew environment")
subprocess.run(["brew", "tap-new", "moq-dev/ci-gst"], check=True, env=environment)
try:
    tap = subprocess.check_output(["brew", "--repository", "moq-dev/ci-gst"], text=True, env=environment).strip()
    output = Path(tap) / "Formula/moq-gst.rb"
    output.write_text(formula)
    subprocess.run(["brew", "install", "--build-from-source", "moq-dev/ci-gst/moq-gst"], check=True, env=environment)
    subprocess.run(["brew", "test", "moq-dev/ci-gst/moq-gst"], check=True, env=environment)
finally:
    subprocess.run(["brew", "uninstall", "--force", "moq-dev/ci-gst/moq-gst"], check=False, env=environment)
    subprocess.run(["brew", "untap", "moq-dev/ci-gst"], check=True, env=environment)
