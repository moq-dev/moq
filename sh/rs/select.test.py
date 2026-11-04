"""Verify Rust selection and test target boundaries."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FUZZ_EXCLUDES = " ".join(f"--exclude {crate}-fuzz" for crate in ("moq-net", "moq-quic", "moq-mux", "moq-rtmp"))
DEVICE_FEATURES = "moq-video/capture moq-audio/capture moq-audio/playback moq-cli/play moq-cli/openh264"
LINUX = os.uname().sysname == "Linux"


def target_features():
    """Every bench and example `required-features`, as `crate/feature`, for the workspace."""
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    return sorted(
        {
            f"{package['name']}/{feature}"
            for package in metadata["packages"]
            if not package["name"].endswith("-fuzz")
            for target in package["targets"]
            if {"bench", "example"} & set(target["kind"])
            for feature in target.get("required-features", [])
        }
    )


def dispatch_scope():
    """The rs module's impact pattern, as sh/dispatch.sh registers it."""
    return re.search(r"^module rs '([^']*)'", (ROOT / "sh/dispatch.sh").read_text(), re.M).group(1)


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
            env = dict(
                os.environ,
                LOCAL_SUPERVISED="1",
                PATH=f"{scratch}:{os.environ['PATH']}",
                SELECT_CALLS=str(calls),
                BASH_ENV=os.devnull,
                GITHUB_ACTIONS=str(github_actions).lower(),
            )
            subprocess.run(
                ["bash", "sh/rs/select.sh", action, str(changed)],
                cwd=ROOT,
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )
            return calls.read_text().splitlines() if calls.exists() else []

    def test_nextest_launcher_selects_workspace(self):
        features = [*DEVICE_FEATURES.split(), "moq-tokio/webrtc", *(["moq-relay/io-uring"] if LINUX else [])]
        features += [feature for feature in target_features() if feature not in features]
        for github_actions in (False, True):
            for action in ("check", "check-test", "lint", "test"):
                with self.subTest(action=action, github_actions=github_actions):
                    suffix = f" --benches --examples --features {' '.join(features)}" if action == "test" else ""
                    suffix += " --timings" if github_actions and action == "test" else ""
                    self.assertIn(
                        f"rs {action} --workspace {FUZZ_EXCLUDES}{suffix}",
                        self.selected("sh/rs/nextest.sh", action=action, github_actions=github_actions),
                    )
        selected = self.selected("sh/rs/nextest.sh")
        # The gate's lint runs no extra target or feature builds.
        for recipe in ("uring-check", "capture"):
            self.assertNotIn(f"rs {recipe}", selected)

    def test_cargo_configuration_selects_workspace(self):
        for path in (".cargo/config.toml", ".cargo/config"):
            for action in ("lint", "test"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertTrue(any(call.startswith(f"rs {action} --workspace ") for call in calls), calls)

    def test_workspace_clippy_configuration_selects_workspace(self):
        # The root clippy.toml is every crate's lint policy, alone or beside one crate's change.
        for paths in (("clippy.toml",), ("clippy.toml", "rs/moq-stats/src/lib.rs")):
            with self.subTest(paths=paths):
                calls = self.selected(*paths)
                self.assertTrue(any(call.startswith("rs lint --workspace ") for call in calls), calls)
        scope = dispatch_scope()
        self.assertRegex("clippy.toml", scope)

    def test_gate_runs_no_feature_matrix(self):
        # `just rs features` owns the isolated feature builds, outside the gate.
        for path in (
            "rs/kio/src/lib.rs",
            "rs/moq-tokio/src/lib.rs",
            "rs/moq-quic/src/lib.rs",
            "rs/moq-video/src/lib.rs",
            "Cargo.lock",
        ):
            for action in ("check", "check-test", "lint"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertFalse(
                        any(call.endswith("-features") or "feature-shapes" in call for call in calls), calls
                    )
        for owner in ("tokio", "media", "net"):
            self.assertEqual(self.selected(f"sh/rs/{owner}-features.sh"), [])

    def test_gate_leaves_send_depth_to_nightly(self):
        # Nightly runs `rs send-depth` on the pinned toolchain.
        for path in ("rs/moq-net/src/lib.rs", "rs/kio/src/lib.rs", "Cargo.lock"):
            with self.subTest(path=path):
                calls = self.selected(path)
                self.assertEqual(len(calls), 2, calls)
                self.assertTrue(calls[0].startswith("rs lint "), calls)
                self.assertEqual(calls[1], "rs media-defaults", calls)
        self.assertEqual(self.selected("sh/rs/send-depth.sh"), [])

    def test_media_defaults_are_isolated_from_device_features(self):
        for path in ("rs/moq-audio/src/lib.rs", "rs/moq-video/src/lib.rs", "Cargo.lock"):
            for github_actions in (False, True):
                with self.subTest(path=path, github_actions=github_actions):
                    calls = self.selected(path, github_actions=github_actions)
                    self.assertEqual(calls[-1], "rs media-defaults", calls)
                    self.assertEqual(calls.count("rs media-defaults"), 1, calls)
        # A crate with no media dependents needs no extra compile.
        for action in ("lint", "test"):
            self.assertNotIn("rs media-defaults", self.selected("rs/moq-auth/src/lib.rs", action=action))
        # The device-feature test build stays a single build.
        self.assertNotIn("rs media-defaults", self.selected("rs/moq-audio/src/lib.rs", action="test"))

    def test_device_features_join_the_one_test_build(self):
        for path, features in (
            ("rs/moq-audio/src/lib.rs", DEVICE_FEATURES),
            ("rs/moq-cli/src/main.rs", "moq-cli/play moq-cli/openh264"),
        ):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertEqual(len(calls), 1, calls)
                # The targets' required features follow the device ones.
                self.assertIn(f" --features {features}", calls[0])
        # moq-tokio's WebRTC module is off by default too.
        calls = self.selected("rs/moq-tokio/src/webrtc.rs", action="test")
        self.assertEqual(len(calls), 1, calls)
        self.assertIn(" moq-tokio/webrtc", calls[0])
        # A dependency of the media crates still runs both their device tests.
        calls = self.selected("rs/moq-audio/src/lib.rs", action="test")
        self.assertIn("--package moq-video --package moq-audio", calls[0])

    def test_io_uring_joins_the_linux_test_build(self):
        for path in ("rs/moq-uring/src/lib.rs", "rs/moq-relay/src/lib.rs", "rs/moq-net/src/lib.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertEqual(len(calls), 1, calls)
                self.assertEqual(" moq-relay/io-uring" in calls[0], LINUX, calls)
                if path == "rs/moq-uring/src/lib.rs":
                    self.assertIn("/rs/moq-uring#", calls[0])
        self.assertNotIn("moq-relay/io-uring", self.selected("rs/moq-cli/src/main.rs", action="test")[0])

    def test_net_session_tests_follow_the_library(self):
        for path in ("rs/moq-net/src/lib.rs", "rs/moq-json/src/lib.rs", "rs/moq-net-tests/tests/integration/main.rs"):
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
                    self.assertTrue(
                        any(call.startswith(f"rs {action} ") and "/rs/moq-cli#" in call for call in calls), calls
                    )
        self.assertEqual(self.selected("test/interop/compat/container.rs"), [])

    def test_relay_nixos_module_selects_the_relay(self):
        # moq-relay's tests read the module.
        calls = self.selected("nix/modules/moq-relay.nix", action="test")
        self.assertTrue(any("/rs/moq-relay#" in call for call in calls), calls)
        scope = dispatch_scope()
        self.assertRegex("nix/modules/moq-relay.nix", scope)

    def test_dispatch_scope_covers_out_of_crate_targets(self):
        scope = dispatch_scope()
        metadata = json.loads(
            subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, text=True)
        )
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

    def test_a_bench_alone_enables_its_required_features(self):
        # Alone in the selection, no dependent turns these on, and cargo would
        # skip the targets without a word.
        for bench, feature in (
            ("rs/moq-stats/benches/producer_tick.rs", "moq-stats/bench"),
            ("rs/moq-net/benches/codec.rs", "moq-net/fuzz"),
            ("rs/moq-relay/benches/session.rs", "moq-relay/test-support"),
            ("rs/moq-quic/benches/wire.rs", "moq-quic/fuzz"),
        ):
            with self.subTest(bench=bench):
                (call,) = [call for call in self.selected(bench, action="test") if call.startswith("rs test ")]
                self.assertIn(" --benches --examples ", call)
                self.assertIn(feature, call.split(" --features ", 1)[1].split())


class MetadataTest(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".tmp"
        scratch.mkdir(exist_ok=True)
        directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(directory.cleanup)
        self.scratch = Path(directory.name)
        self.calls = self.scratch / "calls"
        self.fixture = self.scratch / "metadata"
        self.changed = self.scratch / "changed"
        self.changed.write_text("rs/seed/src/lib.rs\n")
        self.env = dict(
            os.environ,
            LOCAL_SUPERVISED="1",
            BASH_ENV=os.devnull,
            GITHUB_ACTIONS="false",
            PATH=f"{self.scratch}:{os.environ['PATH']}",
            SELECT_CALLS=str(self.calls),
            TEST_METADATA=str(self.fixture),
            CC=str(self.scratch / "cc"),
            TMPDIR=str(self.scratch),
        )
        for name, source in (
            ("cargo", '#!/bin/sh\ncat "$TEST_METADATA"\n'),
            ("just", '#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\n'),
            # Stop after artifact selection, before compiling or linking anything.
            ("cc", '#!/bin/sh\nprintf "%s\\n" "$*" >> "$SELECT_CALLS"\nexit 77\n'),
        ):
            tool = self.scratch / name
            tool.write_text(source)
            tool.chmod(0o755)

    def metadata(self):
        return {
            "workspace_root": "/",
            "packages": [
                {
                    "name": name,
                    "id": f"path+file:///rs/{name}#1.0.0",
                    "manifest_path": f"/rs/{name}/Cargo.toml",
                    "targets": [{"src_path": f"/rs/{name}/src/lib.rs"}],
                    "dependencies": [{"name": dep} for dep in deps],
                }
                for name, deps in (("seed", []), ("dependent", ["seed"]))
            ],
        }

    def run_selection(self, script="select", fixture=None):
        self.fixture.write_text(json.dumps(fixture) if fixture is not None else "{")
        self.calls.unlink(missing_ok=True)
        args = ["test", str(self.changed)] if script == "select" else []
        return subprocess.run(
            ["bash", f"sh/rs/{script}.sh", *args], cwd=ROOT, env=self.env, capture_output=True, text=True
        )

    def assert_failed_selection(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("jq:", result.stderr)
        self.assertFalse(self.calls.exists(), self.calls.read_text() if self.calls.exists() else "")
        self.assertNotIn("skipping", result.stdout)

    def test_valid_metadata_selects_dependents_without_errors(self):
        result = self.run_selection(fixture=self.metadata())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(
            self.calls.read_text().splitlines(),
            [
                "rs test --package path+file:///rs/dependent#1.0.0 --package path+file:///rs/seed#1.0.0"
                " --no-tests=pass --benches --examples"
            ],
        )

    def test_malformed_metadata_stops_before_running_tests(self):
        for field in ("json", "manifest_path", "workspace_root", "targets", "dependencies"):
            with self.subTest(field=field):
                metadata = self.metadata()
                if field == "workspace_root":
                    del metadata[field]
                elif field != "json":
                    # Keep the first package valid so a stream can emit partial output.
                    del metadata["packages"][1][field]
                self.assert_failed_selection(self.run_selection(fixture=None if field == "json" else metadata))

    def test_directory_mismatch_still_selects_the_workspace(self):
        metadata = self.metadata()
        metadata["packages"][1]["name"] = "renamed"
        result = self.run_selection(fixture=metadata)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("a crate directory no longer matches", result.stderr)
        self.assertIn(f"rs test --workspace {FUZZ_EXCLUDES}", self.calls.read_text())

    def test_every_jq_failure_stops_selection_even_after_partial_output(self):
        jq = self.scratch / "jq"
        jq.write_text(
            "#!/usr/bin/env bash\nset -eu\n"
            'count_file="$TEST_METADATA.count"\ncount=0\n'
            'if [[ -f "$count_file" ]]; then read -r count <"$count_file"; fi\n'
            'count=$((count + 1))\nprintf "%s\\n" "$count" >"$count_file"\n'
            'if [[ "$count" == "$FAIL_JQ" ]]; then\n'
            '    echo "partial output"\n    echo "jq: injected failure" >&2\n    exit 7\nfi\n'
            f'exec "{shutil.which("jq")}" "$@"\n'
        )
        jq.chmod(0o755)
        self.env["FAIL_JQ"] = "0"
        result = self.run_selection(fixture=self.metadata())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        stages = int(self.fixture.with_suffix(".count").read_text())
        for stage in range(1, stages + 1):
            with self.subTest(stage=stage):
                self.fixture.with_suffix(".count").unlink(missing_ok=True)
                self.env["FAIL_JQ"] = str(stage)
                result = self.run_selection(fixture=self.metadata())
                self.assert_failed_selection(result)
                self.assertEqual(result.returncode, 7, result.stdout + result.stderr)

    def test_c_artifact_and_header_selection_rejects_partial_json_streams(self):
        include = self.scratch / "include"
        include.mkdir()
        (include / "moq.h").touch()
        artifact = {"reason": "compiler-artifact", "filenames": [str(self.scratch / "libmoq.a")]}
        header = {
            "reason": "build-script-executed",
            "package_id": "path+file:///rs/moq-c#1.0.0",
            "out_dir": str(self.scratch),
        }
        # Cargo emits a stream, not an array. Both selectors must reject an
        # error even when another record supplied the artifact they wanted.
        for malformed in (None, {"reason": "compiler-artifact"}, {"reason": "build-script-executed"}):
            with self.subTest(malformed=malformed):
                stream = "\n".join(json.dumps(record) for record in (artifact, header))
                stream += "\n" + ("{" if malformed is None else json.dumps(malformed))
                self.fixture.write_text(stream)
                result = subprocess.run(
                    ["bash", "sh/rs/c-tests.sh"], cwd=ROOT, env=self.env, capture_output=True, text=True
                )
                self.assert_failed_selection(result)
        self.fixture.write_text("\n".join(json.dumps(record) for record in (artifact, header)))
        result = subprocess.run(["bash", "sh/rs/c-tests.sh"], cwd=ROOT, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 77, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertIn(f"-I{include}", self.calls.read_text())


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
                    "#[test] fn runs() { assert_eq!(1 + 1, 2); }\n" + ("fn main() {}\n" if source == "main.rs" else "")
                )
                (scratch / "benches/unexecuted.rs").write_text(
                    'compile_error!("nextest must not compile a benchmark it never runs");\nfn main() {}\n'
                )
                env = dict(
                    os.environ, LOCAL_SUPERVISED="1", BASH_ENV=os.devnull, TMPDIR="/tmp", NEXTEST_PROFILE="default"
                )
                result = subprocess.run(
                    ["just", "rs", "test", "--manifest-path", str(scratch / "Cargo.toml")],
                    cwd=ROOT,
                    env=env,
                    capture_output=True,
                    text=True,
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
            env = dict(
                os.environ,
                LOCAL_SUPERVISED="1",
                PATH=f"{scratch}:{os.environ['PATH']}",
                FEATURE_CALLS=str(calls),
                FEATURE_NET_FEATURES=net_features,
                BASH_ENV=os.devnull,
            )
            result = subprocess.run(
                ["bash", f"sh/rs/{owner}-features.sh", *args], cwd=ROOT, env=env, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, expected_returncode, result.stdout + result.stderr)
            return calls.read_text().splitlines()

    def test_net_keeps_model_contracts(self):
        boundary = self.trace("net")
        exhaustive = self.trace("net", "--all")
        for calls in (boundary, exhaustive):
            self.assertTrue(any(" --lib " in call for call in calls if call.startswith("clippy ")))
            self.assertFalse(any(" --all-targets " in call for call in calls))
            self.assertTrue(any(call.startswith("nextest run ") for call in calls))
            self.assertTrue(any(call.startswith("doc ") for call in calls))

    def test_net_rejects_async_from_test_dependencies(self):
        for feature in ("async", "default", "fuzz"):
            with self.subTest(feature=feature):
                calls = self.trace("net", net_features=f'moq-net feature "{feature}"', expected_returncode=1)
                self.assertTrue(all(call.startswith("tree ") for call in calls), calls)

    def test_tokio_keeps_extremes_and_crypto_contracts(self):
        boundary = self.trace("tokio")
        exhaustive = self.trace("tokio", "--all")
        self.assertEqual(sum(call.startswith("clippy ") for call in boundary), 3)
        self.assertEqual(sum(call.startswith("clippy ") for call in exhaustive), 12)
        self.assertTrue(any(" --tests " in call for call in boundary))
        self.assertFalse(any(" --all-targets " in call for call in boundary))
        self.assertTrue(any(" --all-targets " in call for call in exhaustive))
        self.assertEqual(
            [call for call in boundary if not call.startswith("clippy ")],
            [call for call in exhaustive if not call.startswith("clippy ")],
        )

    def test_media_keeps_dependency_contracts_and_vpx_tests(self):
        boundary = self.trace("media")
        exhaustive = self.trace("media", "--all")
        self.assertEqual(sum(call.startswith("check ") for call in boundary), 4)
        self.assertEqual(sum(call.startswith("check ") for call in exhaustive), 10)
        self.assertFalse(any(" --all-targets " in call for call in boundary))
        self.assertTrue(any(" --all-targets " in call for call in exhaustive))
        self.assertEqual(
            [call for call in boundary if not call.startswith("check ")],
            [call for call in exhaustive if not call.startswith("check ")],
        )


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
