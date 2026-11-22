"""Verify Rust selection and test target boundaries."""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FUZZ_CRATES = ("moq-net", "moq-quic", "moq-mux", "moq-rtmp", "moq-sock", "moq-uring")
FUZZ_EXCLUDES = " ".join(f"--exclude {crate}-fuzz" for crate in FUZZ_CRATES)
DEVICE_FEATURES = "moq-video/capture moq-audio/capture moq-audio/playback moq-cli/play moq-cli/openh264"


def target_features():
    """Every `required-features` of a bench, example, or tested target, as `crate/feature`, for the workspace."""
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
            if {"bench", "example"} & set(target["kind"]) or target["test"]
            for feature in target.get("required-features", [])
        }
    )


def dispatch_scope():
    """The rs module's impact pattern, as sh/dispatch.sh registers it."""
    return re.search(r"^module rs '([^']*)'", (ROOT / "sh/dispatch.sh").read_text(), re.M).group(1)


def rust_reads():
    """Each `(crate directory, glob, file:line)` a Rust source reads outside its crate, from sh/rs/reads.sh."""
    listed = subprocess.run(["bash", "sh/rs/reads.sh"], cwd=ROOT, check=True, capture_output=True, text=True)
    return [tuple(line.split(" ", 2)) for line in listed.stdout.splitlines()]


def read_probes(glob):
    """Paths a read covers: the file it names, or a new file in each place a wildcard or directory leaves open."""
    if "*" not in glob:
        return [f"{glob}/added" if (ROOT / glob).is_dir() else glob]
    return [glob.replace("**/", nested).replace("**", "added").replace("*", "added") for nested in ("", "added/")]


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
        features = DEVICE_FEATURES.split()
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
        self.assertNotIn("rs capture", selected)

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

    def test_gate_proves_send_depth_for_net(self):
        # moq-net and its dependencies can deepen the session driver's Send proof.
        # send-depth runs in parallel with the stable lint steps in its own target dir.
        for path in ("rs/moq-net/src/lib.rs", "rs/kio/src/lib.rs", "Cargo.lock"):
            with self.subTest(path=path):
                calls = [call for call in self.selected(path) if call != "rs c-tests --syntax"]
                self.assertEqual(calls.count("rs send-depth"), 1, calls)
                stable = [call for call in calls if call != "rs send-depth"]
                self.assertEqual(len(stable), 3, stable)
                self.assertTrue(stable[0].startswith("rs lint "), stable)
                self.assertEqual(stable[1], "rs media-defaults", stable)
                self.assertEqual(stable[2], "rs no-async", stable)
        # The script that runs the proof selects moq-net, so an edit to it is proved too.
        self.assertIn("rs send-depth", self.selected("sh/rs/send-depth.sh"))
        # Only the lint phase runs it, so the gate compiles it once.
        self.assertNotIn("rs send-depth", self.selected("rs/moq-net/src/lib.rs", action="test"))
        self.assertNotIn("rs send-depth", self.selected("rs/moq-auth/src/lib.rs"))
        # moq-tokio spawns the driver, so its own changes are proved too.
        self.assertIn("rs send-depth", self.selected("rs/moq-tokio/src/client.rs"))

    def test_gate_compiles_c_callers_against_libmoq(self):
        # libmoq, the C callers' own inputs, and workspace-wide changes.
        for path in (
            "rs/libmoq/src/lib.rs",
            "rs/libmoq/build.rs",
            "rs/libmoq/c-tests/abort-unwanted.c",
            "sh/rs/c-tests.sh",
            "doc/lib/c/index.md",
            "doc/lib/samples.sh",
            "Cargo.lock",
        ):
            with self.subTest(path=path):
                self.assertRegex(path, dispatch_scope())
                self.assertEqual(self.selected(path).count("rs c-tests --syntax"), 1)
        # Upstream crate edits cannot alter rs/libmoq/src (where cbindgen reads moq.h),
        # so they skip the extra libmoq header check while still testing libmoq.
        self.assertNotIn("rs c-tests --syntax", self.selected("rs/moq-net/src/lib.rs"))
        self.assertTrue(any("/rs/libmoq#" in call for call in self.selected("rs/moq-net/src/lib.rs", action="test")))
        # Only the lint phase runs it, and only when libmoq is seeded.
        self.assertNotIn("rs c-tests --syntax", self.selected("rs/libmoq/src/lib.rs", action="test"))
        self.assertNotIn("rs c-tests --syntax", self.selected("rs/moq-auth/src/lib.rs"))

    def test_media_defaults_are_isolated_from_device_features(self):
        for path in ("rs/moq-audio/src/lib.rs", "rs/moq-video/src/lib.rs", "Cargo.lock"):
            for github_actions in (False, True):
                with self.subTest(path=path, github_actions=github_actions):
                    calls = self.selected(path, github_actions=github_actions)
                    self.assertIn("rs media-defaults", calls)
                    self.assertEqual(calls.count("rs media-defaults"), 1, calls)
        # A crate with no media dependents needs no extra compile.
        for action in ("lint", "test"):
            self.assertNotIn("rs media-defaults", self.selected("rs/moq-auth/src/lib.rs", action=action))
        # The device-feature test build stays a single build.
        self.assertNotIn("rs media-defaults", self.selected("rs/moq-audio/src/lib.rs", action="test"))

    def test_gate_builds_net_without_async(self):
        # Every other build unifies `async` back in, so the gate compiles it alone.
        for path in ("rs/moq-net/src/lib.rs", "Cargo.lock"):
            for action in ("check", "check-test", "lint"):
                with self.subTest(path=path, action=action):
                    calls = self.selected(path, action=action)
                    self.assertEqual(calls.count("rs no-async"), 1, calls)
        # A crate with no moq-net dependents needs no extra compile.
        for action in ("check", "check-test", "lint"):
            self.assertNotIn("rs no-async", self.selected("rs/moq-auth/src/lib.rs", action=action))
        # The test and fix lanes build the default features only.
        for action in ("test", "fix"):
            self.assertNotIn("rs no-async", self.selected("rs/moq-net/src/lib.rs", action=action))

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
        # A dependency of the media crates still runs both their device tests.
        calls = self.selected("rs/moq-audio/src/lib.rs", action="test")
        self.assertIn("--package moq-video --package moq-audio", calls[0])

    def test_io_uring_changes_test_the_relay(self):
        for path in ("rs/moq-uring/src/lib.rs", "rs/moq-relay/src/lib.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertEqual(len(calls), 1, calls)
                self.assertIn("/rs/moq-relay#", calls[0])

    def test_net_session_tests_follow_the_library(self):
        for path in ("rs/moq-net/src/lib.rs", "rs/moq-json/src/lib.rs", "rs/moq-net-tests/tests/integration/main.rs"):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertTrue(any("/rs/moq-net-tests#" in call for call in calls), calls)

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

    def test_a_read_selects_its_reader(self):
        for path, crate in (
            # A new relay config, the shape of moq-dev/moq#8459.
            ("test/added/relay.toml", "moq-relay"),
            ("nix/modules/moq-relay.nix", "moq-relay"),
            # A format interpolation stands for one segment.
            ("doc/concept/audio-jitter/added.json", "moq-audio"),
            ("test/interop/added.ts", "moq-net"),
            ("test/max-age/client.ts", "moq-tokio"),
            # A bench module another crate's bench includes, beside a bench-only change.
            ("rs/moq-net/benches/session/added.rs", "moq-relay"),
        ):
            with self.subTest(path=path):
                calls = self.selected(path, action="test")
                self.assertTrue(any(f"/rs/{crate}#" in call for call in calls), calls)
        for path in ("test/interop/clients/js/added.ts", "doc/concept/audio-jitter/clock/reference.ts"):
            with self.subTest(path=path):
                self.assertEqual(self.selected(path), [])

    def test_dispatch_scope_covers_rust_reads(self):
        # `just gh check` runs this on every diff, so a new read outside rs/
        # fails before its file's next change skips the test that reads it.
        scope = dispatch_scope()
        reads = rust_reads()
        for crate, glob, source in reads:
            if glob.startswith("rs/"):
                continue
            # A `../` string that is no path, such as a path-traversal test input
            # in a file that mentions CARGO_MANIFEST_DIR, also lands here.
            hint = "; if that literal is no path, keep it out of files that mention CARGO_MANIFEST_DIR"
            with self.subTest(crate=crate, glob=glob, source=source):
                self.assertTrue(glob, f"{crate} reads the whole repository at {source}; name the files it reads{hint}")
                for probe in read_probes(glob):
                    self.assertRegex(
                        probe, scope, f"{crate} reads {glob} at {source}, which sh/dispatch.sh's rs scope misses{hint}"
                    )
        # The lister still finds the read behind moq-dev/moq#8459.
        self.assertIn(("rs/moq-relay", "test/**/*.toml"), [(crate, glob) for crate, glob, _ in reads])

    def test_reads_handles_nul_bytes_and_colon_lines(self):
        # Literal NUL bytes, colon-prefixed lines, and missing trailing newlines
        # must not shift file framing or line numbers in sh/rs/reads.sh.
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
            (repo / "rs/a/src").mkdir(parents=True)
            (repo / "rs/a/Cargo.toml").write_text('[package]\nname = "a"\n')
            (repo / "rs/a/src/lib.rs").write_bytes(
                b'const A: &str = "\x00"; const B: &str = include_str!("../../shared.txt");\n'
                b":not_a_header\n"
                b"\n"
                b' const D: &str = include_str!("../../tail.txt");'
            )
            (repo / "rs/b/src").mkdir(parents=True)
            (repo / "rs/b/Cargo.toml").write_text('[package]\nname = "b"\n')
            (repo / "rs/b/src/caf\u00e9.rs").write_bytes(b'const C: &str = include_str!("../../other.txt");')

            result = subprocess.run(
                ["bash", str(ROOT / "sh/rs/reads.sh")],
                cwd=repo,
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                result.stdout.splitlines(),
                [
                    "rs/a rs/shared.txt rs/a/src/lib.rs:1",
                    "rs/a rs/tail.txt rs/a/src/lib.rs:4",
                    "rs/b rs/other.txt rs/b/src/caf\u00e9.rs:1",
                ],
            )

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

    def selected_crates(self, *paths):
        """The crates the test build holds for a diff, by directory name."""
        (call,) = [call for call in self.selected(*paths, action="test") if call.startswith("rs test ")]
        return set(re.findall(r"/rs/([^/#]+)#", call))

    def test_a_target_change_selects_only_its_crate(self):
        # No crate depends on another's tests or examples, so their changes stop there.
        for path, crate in (
            ("rs/moq-net/tests/added.rs", "moq-net"),
            ("rs/moq-net/examples/added.rs", "moq-net"),
            # A read from a target, here moq-tokio's integration tests, is a target change too.
            ("rs/moq-net/tests/range-table.json", "moq-tokio"),
        ):
            with self.subTest(path=path):
                crates = self.selected_crates(path)
                self.assertIn(crate, crates)
                self.assertNotIn("moq-relay", crates)
        # A library that reads its own tests/ still reaches its dependents.
        self.assertIn("moq-relay", self.selected_crates("rs/moq-json/tests/vectors.json"))

    def test_a_dev_dependency_stops_at_its_dependent(self):
        # hang's tests run on moq-tokio, but hang's library and its dependents do not.
        crates = self.selected_crates("rs/moq-tokio/src/lib.rs")
        self.assertIn("hang", crates)
        self.assertIn("moq-relay", crates)
        self.assertNotIn("moq-mux", crates)
        self.assertIn("moq-mux", self.selected_crates("rs/hang/src/lib.rs"))

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

    def test_a_binary_alone_enables_its_required_features(self):
        # moq-boy's only target is its binary, so without openh264 the build
        # holds nothing and its tests never run.
        calls = self.selected("rs/moq-boy/src/main.rs", action="test")
        (call,) = [call for call in calls if call.startswith("rs test ")]
        self.assertIn("moq-boy/openh264", call.split(" --features ", 1)[1].split())


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
            'count_file="$TEST_METADATA.count"\nlock_dir="$TEST_METADATA.lock"\n'
            # Pipeline stages (e.g. reads.sh | jq) invoke this wrapper concurrently;
            # serialize counter updates with an atomic mkdir lock before exec.
            'while ! mkdir "$lock_dir" 2>/dev/null; do sleep 0.005; done\n'
            "count=0\n"
            'if [[ -f "$count_file" ]]; then read -r count <"$count_file"; fi\n'
            'count=$((count + 1))\nprintf "%s\\n" "$count" >"$count_file"\n'
            'rmdir "$lock_dir"\n'
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
            "package_id": "path+file:///rs/libmoq#1.0.0",
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


def _iter_cases(suite):
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            yield from _iter_cases(item)
        else:
            yield item


def _run_parallel():
    suite = unittest.defaultTestLoader.loadTestsFromModule(sys.modules[__name__])
    cases = list(_iter_cases(suite))
    workers = min(os.cpu_count() or 4, 8, max(len(cases), 1))

    def run_one(case):
        result = unittest.TestResult()
        case.run(result)
        return case, result

    failures = []
    errors = []
    unexpected = []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for _case, result in pool.map(run_one, cases):
            failures.extend(result.failures)
            errors.extend(result.errors)
            unexpected.extend(result.unexpectedSuccesses)

    for test, tb in errors:
        print(f"ERROR: {test}\n{tb}", file=sys.stderr)
    for test, tb in failures:
        print(f"FAIL: {test}\n{tb}", file=sys.stderr)
    for test in unexpected:
        print(f"UNEXPECTED SUCCESS: {test}", file=sys.stderr)
    print(
        f"Ran {len(cases)} tests ({len(failures)} failures, {len(errors)} errors, {len(unexpected)} unexpected)",
        file=sys.stderr,
    )
    sys.exit(1 if failures or errors or unexpected else 0)


if __name__ == "__main__":
    if len(sys.argv) == 1:
        _run_parallel()
    else:
        unittest.main()
