"""Keep io_uring PR selection and the mandatory kernel floor honest."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class UringTest(unittest.TestCase):
    def test_kernel_floor(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            uname = scratch / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo "$TEST_SYSTEM";; -r) echo "$TEST_KERNEL";; esac\n')
            uname.chmod(0o755)
            for system, kernel, passes in (
                ("Linux", "6.11.9", False), ("Linux", "6.12.0-azure", True),
                ("Linux", "7.0.0", True), ("Linux", "unknown", False),
                ("Darwin", "24.0.0", False),
            ):
                with self.subTest(system=system, kernel=kernel):
                    env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                               TEST_SYSTEM=system, TEST_KERNEL=kernel)
                    result = subprocess.run(["bash", "sh/rs/uring-kernel.sh"], cwd=ROOT,
                                            env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, passes, result.stderr)

    def test_selection_follows_shared_dependencies(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            just = scratch / "just"
            just.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n')
            just.chmod(0o755)
            metadata = {"packages": [
                {"name": name, "id": f"path+file:///rs/{name}#1.0.0",
                 "manifest_path": f"/rs/{name}/Cargo.toml",
                 "dependencies": [{"name": dep} for dep in deps]}
                for name, deps in (("moq-relay", ["moq-uring"]),
                                   ("moq-uring", ["kio"]), ("kio", []), ("moq-unrelated", []))
            ]}
            fixture = scratch / "metadata"
            fixture.write_text(json.dumps(metadata))
            cargo = scratch / "cargo"
            cargo.write_text('#!/bin/sh\ncat "$TEST_METADATA"\n')
            cargo.chmod(0o755)
            changed = scratch / "changed"
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                       SELECT_CALLS=str(calls), TEST_METADATA=str(fixture))
            for path, passes in (
                ("rs/moq-uring/src/worker.rs", True), ("rs/moq-relay/src/uring.rs", True),
                ("rs/kio/src/lib.rs", True), ("rs/moq-unrelated/src/lib.rs", False),
                ("flake.lock", True), ("sh/rs/uring-kernel.sh", True),
                (".github/workflows/uring.yml", True), ("sh/changed.sh", True),
            ):
                with self.subTest(path=path):
                    changed.write_text(path + "\n")
                    calls.write_text("")
                    subprocess.run(["bash", "sh/rs/select.sh", "uring", str(changed)],
                                   cwd=ROOT, env=env, check=True)
                    self.assertEqual("rs uring" in calls.read_text().splitlines(), passes)


if __name__ == "__main__":
    unittest.main()
