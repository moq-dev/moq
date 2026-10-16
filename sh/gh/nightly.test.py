"""Nightly groups report a failure without hiding independent later benchmarks."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class NightlyTest(unittest.TestCase):
    def run_group(self, group, fail=""):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            for command in ("just", "cargo"):
                stub = scratch / command
                stub.write_text(
                    '#!/bin/bash\n'
                    'printf "%s %s\n" "${0##*/}" "$*" >> "$NIGHTLY_CALLS"\n'
                    '[[ "$*" != "$NIGHTLY_FAIL" ]]\n'
                )
                stub.chmod(0o755)
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                       NIGHTLY_CALLS=str(calls), NIGHTLY_FAIL=fail, BASH_ENV="/dev/null")
            result = subprocess.run(["bash", "sh/gh/nightly.sh", group], cwd=ROOT,
                                    env=env, capture_output=True, text=True)
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_failed_js_benchmark_does_not_hide_later_work(self):
        result, calls = self.run_group("js", "js bench reader")
        self.assertEqual(result.returncode, 1)
        self.assertIn("just js bench frames", calls)
        self.assertEqual(calls[-1], "just js signals-bench")
        self.assertIn("failed: just js bench reader", result.stderr)

    def test_release_benchmarks_share_one_invocation(self):
        result, calls = self.run_group("rust")
        self.assertEqual(result.returncode, 0, result.stderr)
        release = [call for call in calls if call.startswith("cargo bench")]
        self.assertEqual(len(release), 4)  # moq-net, its CPU clock, kio, then the shared build.
        self.assertTrue(any("--bench session -- session_delivery_group_cost_cpu --test" in call for call in release))
        shared = [call for call in release if "-p moq-relay" in call]
        self.assertEqual(len(shared), 1)
        self.assertIn("--no-fail-fast", shared[0])
        self.assertIn("--bench http_idle", shared[0])
        self.assertIn("--bench chunk_interleaving", shared[0])
        self.assertIn("--bench worker_turns", shared[0])
        self.assertIn("-p moq-pattern", shared[0])
        self.assertIn("--bench matching", shared[0])
        self.assertIn("--bench containers", shared[0])
        self.assertIn("just rs bench-gst-catalog -- --quick", calls)
        self.assertTrue(calls[-1].startswith("just rs test -p moq-cli"))

    def test_failed_release_build_does_not_hide_later_work(self):
        _, calls = self.run_group("rust")
        build = next(call.removeprefix("cargo ") for call in calls
                     if call.startswith("cargo bench") and "-p moq-relay" in call)
        result, calls = self.run_group("rust", build)
        self.assertEqual(result.returncode, 1)
        self.assertTrue(calls[-1].startswith("just rs test -p moq-cli"))

    def test_unknown_group_fails_before_running_anything(self):
        result, calls = self.run_group("unknown")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
