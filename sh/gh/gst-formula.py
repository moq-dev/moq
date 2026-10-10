#!/usr/bin/env python3
"""Install and test the GStreamer formula with the just-built macOS tarball."""

import hashlib
import os
from pathlib import Path
import subprocess
import sys

version, target, release_dir = sys.argv[1:]
archive = (Path(release_dir) / f"moq-gst-v{version}-{target}.tar.gz").resolve(strict=True)
template = Path(".github/homebrew/Formula/moq-gst.rb.tmpl").read_text()
sha = hashlib.sha256(archive.read_bytes()).hexdigest()
formula = template.replace("__VERSION__", version)
for platform in ["AARCH64_APPLE_DARWIN", "AARCH64_UNKNOWN_LINUX_GNU", "X86_64_UNKNOWN_LINUX_GNU"]:
    formula = formula.replace(f"__SHA256_{platform}__", sha)
remote = f"https://github.com/moq-dev/moq/releases/download/moq-gst-v#{{version}}/moq-gst-v#{{version}}-{target}.tar.gz"
formula = formula.replace(remote, archive.as_uri())
environment = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1")
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
