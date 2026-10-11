"""Verify shared Rust test inputs reach the workspace recipes."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class SelectTest(unittest.TestCase):
    def test_nextest_launcher_selects_workspace(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            just = scratch / "just"
            just.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n')
            just.chmod(0o755)
            changed = scratch / "changed"
            changed.write_text("sh/rs/nextest.sh\n")
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}", SELECT_CALLS=str(calls))
            for action in ("check", "check-test", "test"):
                with self.subTest(action=action):
                    calls.write_text("")
                    subprocess.run(
                        ["bash", "sh/rs/select.sh", action, str(changed)],
                        cwd=ROOT, env=env, check=True,
                    )
                    self.assertIn(
                        f"rs {action} --workspace --exclude moq-net-fuzz",
                        calls.read_text().splitlines(),
                    )


if __name__ == "__main__":
    unittest.main()
