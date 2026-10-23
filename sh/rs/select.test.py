"""Verify Rust selection and test target boundaries."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class SelectTest(unittest.TestCase):
    # CI runs target lints before its test build.
    def selected(self, *paths, action="lint", github_actions=False):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            just = scratch / "just"
            just.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n')
            just.chmod(0o755)
            changed = scratch / "changed"
            changed.write_text("\n".join(paths) + "\n")
            env = dict(os.environ, MOQ_LOCAL_SUPERVISED="1", PATH=f"{scratch}:{os.environ['PATH']}",
                       SELECT_CALLS=str(calls), BASH_ENV=os.devnull,
                       GITHUB_ACTIONS=str(github_actions).lower())
            subprocess.run(["bash", "sh/rs/select.sh", action, str(changed)],
                           cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            return calls.read_text().splitlines() if calls.exists() else []

    def test_nextest_launcher_selects_workspace(self):
        for github_actions in (False, True):
            for action in ("check", "check-test", "lint", "test"):
                with self.subTest(action=action, github_actions=github_actions):
                    timings = " --timings" if github_actions and action == "test" else ""
                    self.assertIn(f"rs {action} --workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz{timings}",
                                  self.selected("sh/rs/nextest.sh", action=action,
                                                github_actions=github_actions))
        selected = self.selected("sh/rs/nextest.sh")
        for recipe in ("wasm", "uring-check"):
            self.assertIn(f"rs {recipe}", selected)
        self.assertNotIn("rs capture", selected)

    def test_queue_leaves_permutations_to_feature_gate(self):
        for path in ("rs/kio/src/lib.rs", "rs/moq-tokio/src/lib.rs",
                     "rs/moq-video/src/lib.rs", "Cargo.lock", "sh/rs/nextest.sh"):
            for action in ("check", "check-test", "lint"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertFalse(any("-features" in call for call in calls), calls)
        for owner in ("tokio", "media", "net"):
            self.assertEqual(self.selected(f"sh/rs/{owner}-features.sh"), [])

    def test_demo_crate_follows_its_dependencies(self):
        for path in ("demo/carla/bridge/src/lib.rs", "rs/moq-video/src/lib.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertTrue(any("/demo/carla/bridge#moq-carla-bridge" in call for call in calls), calls)
        self.assertEqual(self.selected("demo/carla/web/src/main.ts"), [])

    def test_a_bench_selects_only_its_crate(self):
        bench = "rs/moq-stats/benches/producer_tick.rs"
        for action in ("lint", "test"):
            with self.subTest(action=action):
                calls = self.selected(bench, action=action)
                self.assertTrue(any("#moq-stats@" in call or "/rs/moq-stats#" in call for call in calls), calls)
                self.assertFalse(any("moq-relay" in call or "moq-cli" in call for call in calls), calls)
        calls = self.selected(bench, "rs/moq-stats/src/lib.rs", action="test")
        self.assertTrue(any("/rs/moq-relay#" in call for call in calls), calls)



class RecipeTest(unittest.TestCase):
    def test_unexecuted_bench_is_not_compiled(self):
        for source in ("lib.rs", "main.rs"):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory)
                (scratch / "src").mkdir()
                (scratch / "benches").mkdir()
                (scratch / "Cargo.toml").write_text(
                    '[package]\nname = "nextest-target-probe"\nversion = "0.0.0"\nedition = "2024"\n'
                    '[workspace]\n[[bench]]\nname = "unexecuted"\nharness = false\n'
                )
                (scratch / "Cargo.lock").write_text(
                    'version = 4\n[[package]]\nname = "nextest-target-probe"\nversion = "0.0.0"\n'
                )
                (scratch / "src" / source).write_text(
                    '#[test] fn runs() { assert_eq!(1 + 1, 2); }\n' +
                    ('fn main() {}\n' if source == "main.rs" else '')
                )
                (scratch / "benches/unexecuted.rs").write_text(
                    'compile_error!("nextest must not compile a benchmark it never runs");\nfn main() {}\n'
                )
                env = dict(os.environ, BASH_ENV=os.devnull, TMPDIR="/tmp",
                           NEXTEST_PROFILE="default")
                result = subprocess.run(
                    ["just", "rs", "test", "--manifest-path", str(scratch / "Cargo.toml")],
                    cwd=ROOT, env=env, capture_output=True, text=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class FeatureTest(unittest.TestCase):
    def trace(self, owner, *args):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            cargo = scratch / "cargo"
            cargo.write_text(CARGO_STUB)
            cargo.chmod(0o755)
            env = dict(os.environ, MOQ_LOCAL_SUPERVISED="1", PATH=f"{scratch}:{os.environ['PATH']}",
                       FEATURE_CALLS=str(calls), BASH_ENV=os.devnull)
            result = subprocess.run(["bash", f"sh/rs/{owner}-features.sh", *args],
                                    cwd=ROOT, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return calls.read_text().splitlines()

    def test_tokio_keeps_extremes_and_crypto_contracts(self):
        boundary = self.trace("tokio")
        exhaustive = self.trace("tokio", "--all")
        self.assertEqual(sum(call.startswith("clippy ") for call in boundary), 3)
        self.assertEqual(sum(call.startswith("clippy ") for call in exhaustive), 12)
        self.assertEqual([call for call in boundary if not call.startswith("clippy ")],
                         [call for call in exhaustive if not call.startswith("clippy ")])

    def test_media_keeps_dependency_contracts_and_vpx_tests(self):
        boundary = self.trace("media")
        exhaustive = self.trace("media", "--all")
        self.assertEqual(sum(call.startswith("check ") for call in boundary), 4)
        self.assertEqual(sum(call.startswith("check ") for call in exhaustive), 10)
        self.assertEqual([call for call in boundary if not call.startswith("check ")],
                         [call for call in exhaustive if not call.startswith("check ")])


CARGO_STUB = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FEATURE_CALLS"
if [[ "$*" == 'check --locked -p moq-tokio --no-default-features --features quic' ]]; then
 echo 'a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature' >&2
 exit 1
fi
if [[ $1 == tree ]]; then
 echo 'root v1'
 case "$*" in
  *'--features openh264'*) printf 'openh264 v1\nopenh264-sys2 v1\n' ;;
  *'--features nvidia'*) echo 'moq-nvenc v1' ;;
  *'--features vpx'*) echo 'libvpx-native-sys v1' ;;
  *'--features render'*) echo 'wgpu v1' ;;
  *'--features capture,pipewire'*) echo 'cpal v1' ;;
 esac
fi
"""


if __name__ == "__main__":
    unittest.main()
