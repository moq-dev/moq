"""Verify Rust selection and test target boundaries."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class SelectTest(unittest.TestCase):
    # CI's Check job runs `lint`; `check` is its local equivalent.
    def selected(self, *paths, action="lint"):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            just = scratch / "just"
            just.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n')
            just.chmod(0o755)
            changed = scratch / "changed"
            changed.write_text("\n".join(paths) + "\n")
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                       SELECT_CALLS=str(calls), BASH_ENV=os.devnull)
            subprocess.run(["bash", "sh/rs/select.sh", action, str(changed)],
                           cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            return calls.read_text().splitlines()

    def test_nextest_launcher_selects_workspace(self):
        for action in ("check", "check-test", "lint", "test"):
            with self.subTest(action=action):
                self.assertIn(f"rs {action} --workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz",
                              self.selected("sh/rs/nextest.sh", action=action))
        selected = self.selected("sh/rs/nextest.sh")
        for recipe in ("tokio-features --all", "net-features", "wasm", "uring-check", "media-features --all"):
            self.assertIn(f"rs {recipe}", selected)
        self.assertNotIn("rs capture", selected)

    def test_transitive_change_keeps_boundary_checks(self):
        calls = self.selected("rs/kio/src/lib.rs")
        self.assertIn("rs tokio-features", calls)
        self.assertIn("rs media-features", calls)
        self.assertNotIn("rs tokio-features --all", calls)
        self.assertNotIn("rs media-features --all", calls)

    def test_feature_owner_gets_isolated_permutations(self):
        self.assertIn("rs tokio-features --all", self.selected("rs/moq-tokio/src/lib.rs"))
        self.assertIn("rs media-features --all", self.selected("rs/moq-video/src/lib.rs"))

    def test_feature_script_diff_checks_its_own_modes(self):
        for action in ("check", "lint"):
            for owner in ("tokio", "media"):
                with self.subTest(action=action, owner=owner):
                    self.assertEqual(self.selected(f"sh/rs/{owner}-features.sh", action=action),
                                     [f"rs {owner}-features --all"])
            with self.subTest(action=action, owner="net"):
                self.assertEqual(self.selected("sh/rs/net-features.sh", action=action),
                                 ["rs net-features"])

    def test_shared_build_input_gets_all_permutations(self):
        calls = self.selected("Cargo.lock")
        self.assertIn("rs tokio-features --all", calls)
        self.assertIn("rs media-features --all", calls)


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
            env = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                       FEATURE_CALLS=str(calls), BASH_ENV=os.devnull)
            result = subprocess.run(["bash", f"sh/rs/{owner}-features.sh", *args],
                                    cwd=ROOT, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return calls.read_text().splitlines()

    def test_tokio_keeps_extremes_and_crypto_contracts(self):
        boundary = self.trace("tokio")
        exhaustive = self.trace("tokio", "--all")
        self.assertEqual(sum(call.startswith("clippy ") for call in boundary), 2)
        self.assertEqual(sum(call.startswith("clippy ") for call in exhaustive), 10)
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
