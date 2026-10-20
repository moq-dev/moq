"""Nightly groups report a failure without hiding independent later benchmarks."""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import tomllib
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
                    '#!/usr/bin/env bash\n'
                    'printf "%s %s\n" "${0##*/}" "$*" >> "$NIGHTLY_CALLS"\n'
                    '[[ "$*" != "$NIGHTLY_FAIL" ]]\n'
                )
                stub.chmod(0o755)
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                       MOQ_BENCH_RESULTS=str(scratch / "results"), NIGHTLY_CALLS=str(calls), NIGHTLY_FAIL=fail, BASH_ENV="/dev/null")
            result = subprocess.run(["bash", "sh/gh/nightly.sh", group], cwd=ROOT,
                                    env=env, capture_output=True, text=True)
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_failed_js_benchmark_does_not_hide_later_work(self):
        result, calls = self.run_group("js", "js bench reader")
        self.assertEqual(result.returncode, 1)
        self.assertIn("just js bench frames", calls)
        self.assertEqual(calls[-1], "just js signals-bench")
        self.assertIn("failed: just js bench reader", result.stderr)

    def test_bench_targets_share_one_invocation(self):
        result, calls = self.run_group("rust-bench")
        self.assertEqual(result.returncode, 0, result.stderr)
        release = [call for call in calls if call.startswith("just gh criterion")]
        self.assertEqual(len(release), 5)  # moq-net, its CPU clock, kio, the shared build, then GStreamer.
        self.assertTrue(any("--bench session -- session_delivery_group_cost_cpu" in call for call in release))
        shared = [call for call in release if "-p moq-relay" in call]
        self.assertEqual(len(shared), 1)
        self.assertIn("--no-fail-fast", shared[0])
        self.assertIn("--bench http_idle", shared[0])
        self.assertIn("--bench chunk_interleaving", shared[0])
        self.assertIn("--bench worker_turns", shared[0])
        self.assertIn("-p moq-pattern", shared[0])
        self.assertIn("--bench matching", shared[0])
        self.assertIn("--bench containers", shared[0])
        self.assertIn("-p moq-quic", shared[0])
        self.assertIn("moq-quic/fuzz", shared[0])
        self.assertIn("--bench wire", shared[0])
        self.assertEqual(release[-1], "just gh criterion -p moq-gst --bench catalog")
        self.assertTrue(calls[-1].startswith("just gh benchmark-report rust "))

    def test_every_cargo_bench_target_runs_nightly(self):
        covered = set()
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        for group in ("rust-bench", "rust-extra", "rust-uring"):
            self.assertIn(f"recipe: gh nightly {group}\n", workflow)
            result, calls = self.run_group(group)
            self.assertEqual(result.returncode, 0, result.stderr)
            for call in calls:
                args = shlex.split(call)
                if args[:3] != ["just", "gh", "criterion"]:
                    continue
                packages = [args[i + 1] for i, arg in enumerate(args) if arg == "-p"]
                targets = [args[i + 1] for i, arg in enumerate(args) if arg == "--bench"]
                for package in packages:
                    manifest = ROOT / "rs" / package / "Cargo.toml"
                    for bench in tomllib.loads(manifest.read_text()).get("bench", []):
                        if "*" in targets or bench["name"] in targets:
                            covered.add((package, bench["name"]))
        declared = {(manifest.parent.name, bench["name"])
                    for manifest in (ROOT / "rs").glob("*/Cargo.toml")
                    for bench in tomllib.loads(manifest.read_text()).get("bench", [])}
        self.assertEqual(declared - covered, set(), "Cargo bench targets missing from Nightly")

    def test_benchmark_tests_stay_out_of_cargo_bench(self):
        result, calls = self.run_group("rust-test")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(call.startswith(("cargo bench", "just gh")) for call in calls))
        # Release builds run before the test-profile ones.
        profiles = ["--release" in call or " bench-" in call for call in calls]
        self.assertEqual(profiles, sorted(profiles, reverse=True))
        self.assertIn("just rs bench-feed-fetch", calls)
        self.assertTrue(calls[-1].startswith("just rs test -p moq-cli"))

    def test_failed_release_build_does_not_hide_later_work(self):
        _, calls = self.run_group("rust-bench")
        build = next(call.removeprefix("just ") for call in calls
                     if call.startswith("just gh criterion") and "-p moq-relay" in call)
        result, calls = self.run_group("rust-bench", build)
        self.assertEqual(result.returncode, 1)
        # A failed group publishes no report, but every later target still runs.
        self.assertEqual(calls[-1], "just gh criterion -p moq-gst --bench catalog")

    def test_unknown_group_fails_before_running_anything(self):
        result, calls = self.run_group("unknown")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
