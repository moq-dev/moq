"""Verify Rust selection and test target boundaries."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
DEVICE_FEATURES = "moq-video/capture moq-audio/capture moq-audio/playback moq-cli/play moq-cli/openh264"


class SelectTest(unittest.TestCase):
    # CI runs target lints before its test build.
    def selected(self, *paths, action="lint", github_actions=False, feature_scope=None):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            just = scratch / "just"
            just.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n')
            just.chmod(0o755)
            changed = scratch / "changed"
            changed.write_text("\n".join(paths) + "\n")
            env = dict(os.environ, LOCAL_SUPERVISED="1", PATH=f"{scratch}:{os.environ['PATH']}",
                       SELECT_CALLS=str(calls), BASH_ENV=os.devnull,
                       GITHUB_ACTIONS=str(github_actions).lower())
            env.pop("RS_FEATURE_SCOPE", None)
            if feature_scope:
                env["RS_FEATURE_SCOPE"] = feature_scope
            subprocess.run(["bash", "sh/rs/select.sh", action, str(changed)],
                           cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            return calls.read_text().splitlines() if calls.exists() else []

    def test_nextest_launcher_selects_workspace(self):
        for github_actions in (False, True):
            for action in ("check", "check-test", "lint", "test"):
                with self.subTest(action=action, github_actions=github_actions):
                    suffix = f" --features {DEVICE_FEATURES} moq-tokio/webrtc" if action == "test" else ""
                    suffix += " --timings" if github_actions and action == "test" else ""
                    self.assertIn(f"rs {action} --workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz{suffix}",
                                  self.selected("sh/rs/nextest.sh", action=action,
                                                github_actions=github_actions))
        selected = self.selected("sh/rs/nextest.sh")
        for recipe in ("wasm", "uring-check"):
            self.assertIn(f"rs {recipe}", selected)
        self.assertNotIn("rs capture", selected)

    def test_cargo_configuration_selects_workspace(self):
        for path in (".cargo/config.toml", ".cargo/config"):
            for action in ("lint", "test"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertTrue(any(call.startswith(f"rs {action} --workspace ") for call in calls), calls)

    def test_queue_checks_selected_feature_shapes(self):
        for path in ("rs/kio/src/lib.rs", "rs/moq-tokio/src/lib.rs",
                     "rs/moq-video/src/lib.rs", "Cargo.lock", "sh/rs/nextest.sh"):
            for action in ("check", "check-test"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    defaults = next(call for call in calls if call.startswith(f"rs {action} "))
                    self.assertIn(defaults.replace(f"rs {action} ", "rs feature-shapes ", 1), calls)
            # The dispatcher runs lint and the shapes as separate modules.
            with self.subTest(path=path, action="feature-shapes"):
                lint = self.selected(path)
                defaults = next(call for call in lint if call.startswith("rs lint "))
                self.assertFalse(any(call.startswith("rs feature-shapes") for call in lint), lint)
                self.assertEqual(self.selected(path, action="feature-shapes"),
                                 [defaults.replace("rs lint ", "rs feature-shapes ", 1)])
        for owner in ("tokio", "media", "net"):
            calls = self.selected(f"sh/rs/{owner}-features.sh", action="feature-shapes")
            self.assertIn("rs feature-shapes --workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz", calls)

    def test_boundary_checks_follow_reverse_dependencies(self):
        self.assertIn("rs tokio-features", self.selected("rs/moq-tokio/src/lib.rs"))
        self.assertIn("rs net-features", self.selected("rs/moq-net/src/lib.rs"))
        self.assertIn("rs quic-features", self.selected("rs/moq-quic/src/lib.rs"))
        self.assertIn("rs media-features", self.selected("rs/moq-video/src/lib.rs"))
        calls = self.selected("rs/kio/src/lib.rs")
        for owner in ("tokio", "net", "quic", "media"):
            self.assertIn(f"rs {owner}-features", calls)
        calls = self.selected("rs/moq-stats/benches/producer_tick.rs")
        self.assertFalse(any(call.endswith("-features") for call in calls), calls)

    def test_local_feature_contracts_skip_dependents(self):
        # `just check` lints feature shapes for the changed crates alone; their
        # dependents keep the default-feature lint and the landing worker's shapes.
        for action in ("check", "feature-shapes"):
            with self.subTest(action=action):
                calls = self.selected("rs/kio/src/lib.rs", action=action, feature_scope="changed")
                shapes = next(call for call in calls if call.startswith("rs feature-shapes "))
                self.assertIn("/rs/kio#", shapes)
                self.assertNotIn("/rs/moq-net#", shapes)
        calls = self.selected("rs/kio/src/lib.rs", feature_scope="changed")
        self.assertTrue(any("/rs/moq-net#" in call for call in calls if call.startswith("rs lint ")), calls)
        self.assertIn("rs loom-check", calls)
        self.assertFalse(any(call.endswith("-features") for call in calls), calls)
        for path, owner in (("rs/moq-net/src/lib.rs", "net"), ("rs/moq-tokio/src/lib.rs", "tokio"),
                            ("rs/moq-quic/src/lib.rs", "quic"), ("rs/moq-video/src/lib.rs", "media")):
            with self.subTest(path=path):
                calls = self.selected(path, feature_scope="changed")
                self.assertEqual([call for call in calls if call.endswith("-features")], [f"rs {owner}-features"])
        # Inputs every crate shares still select the workspace.
        self.assertIn("rs feature-shapes --workspace --exclude moq-net-fuzz --exclude moq-quic-fuzz",
                      self.selected("Cargo.lock", action="feature-shapes", feature_scope="changed"))

    def test_loom_compiles_only_when_its_model_changes(self):
        for action in ("check", "check-test", "lint"):
            for path in ("rs/kio/src/lib.rs", "rs/moq-net/tests/loom.rs", "Cargo.lock"):
                with self.subTest(action=action, path=path):
                    self.assertIn("rs loom-check", self.selected(path, action=action))
            for path in ("rs/moq-tokio/src/lib.rs", "rs/moq-stats/benches/producer_tick.rs"):
                with self.subTest(action=action, path=path):
                    self.assertNotIn("rs loom-check", self.selected(path, action=action))
        self.assertNotIn("rs loom-check", self.selected("rs/moq-net/src/lib.rs", action="test"))

    def test_device_features_join_the_one_test_build(self):
        for path, features in (("rs/moq-audio/src/lib.rs", DEVICE_FEATURES),
                               ("rs/moq-cli/src/main.rs", "moq-cli/play moq-cli/openh264")):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertEqual(len(calls), 1, calls)
                self.assertTrue(calls[0].endswith(f" --features {features}"), calls)
        # moq-tokio's WebRTC module is off by default too.
        calls = self.selected("rs/moq-tokio/src/webrtc.rs", action="test")
        self.assertEqual(len(calls), 1, calls)
        self.assertIn(" moq-tokio/webrtc", calls[0])
        # A dependency of the media crates still runs both their device tests.
        calls = self.selected("rs/moq-audio/src/lib.rs", action="test")
        self.assertIn("--package moq-video --package moq-audio", calls[0])

    def test_net_session_tests_follow_the_library(self):
        for path in ("rs/moq-net/src/lib.rs", "rs/moq-json/src/lib.rs",
                     "rs/moq-net-tests/tests/integration/main.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertTrue(any("/rs/moq-net-tests#" in call for call in calls), calls)

    def test_demo_crate_follows_its_dependencies(self):
        for path in ("demo/carla/bridge/src/lib.rs", "rs/moq-video/src/lib.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertTrue(any("/demo/carla/bridge#moq-carla-bridge" in call for call in calls), calls)
        self.assertEqual(self.selected("demo/carla/web/src/player.ts"), [])

    def test_out_of_crate_target_selects_its_crate(self):
        # moq-cli builds the Rust interop clients as examples from outside its crate.
        for path in ("test/interop/clients/rust/tail.rs", "test/interop/clients/rust/added.rs"):
            for action in ("lint", "fix"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertTrue(any(call.startswith(f"rs {action} ") and "/rs/moq-cli#" in call
                                        for call in calls), calls)
        self.assertEqual(self.selected("test/interop/compat/container.rs"), [])

    def test_relay_nixos_module_selects_the_relay(self):
        # moq-relay's tests read the module.
        calls = self.selected("nix/modules/moq-relay.nix", action="test")
        self.assertTrue(any("/rs/moq-relay#" in call for call in calls), calls)
        scope = re.search(r"^\s*\[rs\]='([^']*)'", (ROOT / "sh/dispatch.sh").read_text(), re.M).group(1)
        self.assertRegex("nix/modules/moq-relay.nix", scope)

    def test_dispatch_scope_covers_out_of_crate_targets(self):
        scope = re.search(r"^\s*\[rs\]='([^']*)'", (ROOT / "sh/dispatch.sh").read_text(), re.M).group(1)
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, text=True))
        root = Path(metadata["workspace_root"])
        directories = set()
        for package in metadata["packages"]:
            crate = Path(package["manifest_path"]).parent
            for target in package["targets"]:
                source = Path(os.path.normpath(target["src_path"]))
                if not source.is_relative_to(crate):
                    directories.add(source.parent.relative_to(root).as_posix())
        self.assertIn("test/interop/clients/rust", directories)
        for directory in sorted(directories):
            with self.subTest(directory=directory):
                self.assertRegex(f"{directory}/added.rs", scope)

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
    def test_feature_shapes_isolate_selected_crates(self):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            for package in ("feature-owner", "feature-consumer"):
                (scratch / package / "src").mkdir(parents=True)
            manifest = scratch / "Cargo.toml"
            manifest.write_text(
                '[workspace]\nresolver = "2"\nmembers = ["feature-owner", "feature-consumer"]\n'
            )
            (scratch / "feature-owner/Cargo.toml").write_text(
                '[package]\nname = "feature-owner"\nversion = "0.0.0"\nedition = "2024"\n'
                '[features]\nenabled = []\n'
            )
            (scratch / "feature-owner/src/lib.rs").write_text(
                'fn needs_feature() -> bool { true }\n'
                '#[cfg(feature = "enabled")]\npub fn enabled() -> bool { needs_feature() }\n'
            )
            (scratch / "feature-consumer/Cargo.toml").write_text(
                '[package]\nname = "feature-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
                '[dependencies]\nfeature-owner = { path = "../feature-owner", features = ["enabled"] }\n'
            )
            (scratch / "feature-consumer/src/lib.rs").write_text(
                'pub fn enabled() -> bool { feature_owner::enabled() }\n'
            )
            env = dict(os.environ, LOCAL_SUPERVISED="1", BASH_ENV=os.devnull,
                       CARGO_TARGET_DIR=str(scratch / "target"), CARGO_NET_OFFLINE="true",
                       CARGO_TERM_COLOR="never")
            subprocess.run(["cargo", "generate-lockfile", "--manifest-path", str(manifest)],
                           cwd=ROOT, env=env, check=True, capture_output=True, text=True)
            unified = subprocess.run(
                ["cargo", "clippy", "--locked", "--tests", "--no-default-features",
                 "--workspace", "--manifest-path", str(manifest), "--", "-D", "warnings"],
                cwd=ROOT, env=env, capture_output=True, text=True,
            )
            self.assertEqual(unified.returncode, 0, unified.stdout + unified.stderr)
            metadata = json.loads(subprocess.check_output(
                ["cargo", "metadata", "--format-version", "1", "--no-deps",
                 "--manifest-path", str(manifest)], cwd=ROOT, env=env, text=True,
            ))
            selected = [arg for package in metadata["workspace_members"] for arg in ("--package", package)]
            for flags, succeeds in ((["--workspace", "--exclude", "feature-owner"], True), (selected, False)):
                with self.subTest(flags=flags):
                    result = subprocess.run(
                        ["just", "rs", "feature-shapes", "--manifest-path", str(manifest), *flags],
                        cwd=ROOT, env=env, capture_output=True, text=True,
                    )
                    output = result.stdout + result.stderr
                    if succeeds:
                        self.assertEqual(result.returncode, 0, output)
                    else:
                        self.assertNotEqual(result.returncode, 0, output)
                        self.assertIn("needs_feature", output)
                        self.assertIn("dead_code", output)

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
                env = dict(os.environ, LOCAL_SUPERVISED="1", BASH_ENV=os.devnull, TMPDIR="/tmp",
                           NEXTEST_PROFILE="default")
                for recipe in ("test", "feature-shapes"):
                    with self.subTest(recipe=recipe):
                        result = subprocess.run(
                            ["just", "rs", recipe, "--manifest-path", str(scratch / "Cargo.toml")],
                            cwd=ROOT, env=env, capture_output=True, text=True,
                        )
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class FeatureTest(unittest.TestCase):
    def trace(self, owner, *args, net_features="", expected_returncode=0):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            cargo = scratch / "cargo"
            cargo.write_text(CARGO_STUB)
            cargo.chmod(0o755)
            env = dict(os.environ, LOCAL_SUPERVISED="1", PATH=f"{scratch}:{os.environ['PATH']}",
                       FEATURE_CALLS=str(calls), FEATURE_NET_FEATURES=net_features, BASH_ENV=os.devnull)
            result = subprocess.run(["bash", f"sh/rs/{owner}-features.sh", *args],
                                    cwd=ROOT, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, expected_returncode, result.stdout + result.stderr)
            return calls.read_text().splitlines()

    def test_net_keeps_model_and_browser_contracts(self):
        boundary = self.trace("net")
        exhaustive = self.trace("net", "--all")
        for calls in (boundary, exhaustive):
            self.assertTrue(any(" --lib " in call for call in calls if call.startswith("clippy ")))
            self.assertFalse(any(" --all-targets " in call for call in calls))
            self.assertTrue(any(" --target wasm32-unknown-unknown " in call for call in calls))
            self.assertTrue(any(call.startswith("nextest run ") for call in calls))
            self.assertTrue(any(call.startswith("doc ") for call in calls))

    def test_net_rejects_async_from_test_dependencies(self):
        for feature in ("async", "default", "fuzz"):
            with self.subTest(feature=feature):
                calls = self.trace("net", net_features=f'moq-net feature "{feature}"',
                                   expected_returncode=1)
                self.assertTrue(all(call.startswith("tree ") for call in calls), calls)

    def test_tokio_keeps_extremes_and_crypto_contracts(self):
        boundary = self.trace("tokio")
        exhaustive = self.trace("tokio", "--all")
        self.assertEqual(sum(call.startswith("clippy ") for call in boundary), 3)
        self.assertEqual(sum(call.startswith("clippy ") for call in exhaustive), 12)
        self.assertTrue(any(" --tests " in call for call in boundary))
        self.assertFalse(any(" --all-targets " in call for call in boundary))
        self.assertTrue(any(" --all-targets " in call for call in exhaustive))
        self.assertEqual([call for call in boundary if not call.startswith("clippy ")],
                         [call for call in exhaustive if not call.startswith("clippy ")])

    def test_media_keeps_dependency_contracts_and_vpx_tests(self):
        boundary = self.trace("media")
        exhaustive = self.trace("media", "--all")
        self.assertEqual(sum(call.startswith("check ") for call in boundary), 4)
        self.assertEqual(sum(call.startswith("check ") for call in exhaustive), 10)
        self.assertFalse(any(" --all-targets " in call for call in boundary))
        self.assertTrue(any(" --all-targets " in call for call in exhaustive))
        self.assertEqual([call for call in boundary if not call.startswith("check ")],
                         [call for call in exhaustive if not call.startswith("check ")])


CARGO_STUB = r"""#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FEATURE_CALLS"
if [[ "$*" == 'check --locked -p moq-tokio --no-default-features --features quic' ]]; then
 echo 'a rustls QUIC backend requires a crypto provider: enable either the `aws-lc-rs` or `ring` feature' >&2
 exit 1
fi
if [[ $1 == tree ]]; then
 if [[ "$*" == *'-i moq-net'* ]]; then
  printf '%s\n' "${FEATURE_NET_FEATURES:-}"
  exit 0
 fi
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
