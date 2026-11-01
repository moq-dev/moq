#!/usr/bin/env python3
"""Exercise pre-setup selection with real diffs and no development toolchain."""

import os
from pathlib import Path
import shutil
import subprocess
import statistics
import sys
import time
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


class QueryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        (self.repo / "sh").mkdir(parents=True)
        for script in ("dispatch.sh", "changed.sh", "local.sh"):
            shutil.copy(ROOT / "sh" / script, self.repo / "sh" / script)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for command in ("bash", "git", "sort", "mktemp", "rm", "grep"):
            (self.bin / command).symlink_to(shutil.which(command))
        self.env = dict(os.environ, LOCAL_SUPERVISED="1", PATH=str(self.bin), GIT_CONFIG_NOSYSTEM="1",
                        GIT_CONFIG_GLOBAL=os.devnull, CI_STRICT="1")
        # The runner's own Actions variables would move the run's error
        # annotations to stdout and append to the real job summary, a gate's
        # CHECK_MODULES split would skip the modules these tests expect, and the
        # land gate's CHECK_FAIL_FAST would cancel the lanes they expect to finish.
        for key in ("IN_MOQ_DEV_SHELL", "ALLOW_HOST_TOOLCHAIN", "CHECK_JOBS", "CHECK_MODULES",
                    "CHECK_FAIL_FAST", "GITHUB_BASE_REF", "GITHUB_ACTIONS", "GITHUB_STEP_SUMMARY"):
            self.env.pop(key, None)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "commit.gpgsign", "false")
        self.git("add", ".")
        self.git("commit", "-qm", "base")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.git("update-ref", "refs/remotes/origin/release", "HEAD")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def change(self, path):
        file = self.repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(file.read_text() + "\n" if file.exists() else "changed\n")

    def run_query(self, job="ci", base="origin/main"):
        return subprocess.run(["bash", "sh/dispatch.sh", job if job == "ci" else f"ci-{job}", base, "--query"],
                              cwd=self.repo, env=self.env, text=True, capture_output=True)

    def expect(self, needed, job="ci", base="origin/main", rust=None):
        result = self.run_query(job, base)
        self.assertEqual(result.returncode, 0, result.stderr)
        output = dict(line.split("=") for line in result.stdout.splitlines())
        self.assertEqual(set(output), {"needed", "rust"})
        self.assertEqual(output["needed"], str(needed).lower())
        if rust is not None:
            self.assertEqual(output["rust"], str(rust).lower())
        self.assertNotIn("missing", result.stderr)
        self.assertNotIn("skipping", result.stderr)

    def test_quest_only_has_no_tests_but_keeps_repository_lints(self):
        self.change("quest/m1/example.md")
        self.expect(True, rust=False)

    def test_rust_cache_follows_modules_that_compile_rust(self):
        for path, rust in (("rs/moq-net/src/lib.rs", True), ("py/moq-rs/example.py", True),
                           ("js/net/src/index.ts", False), ("kt/example.kt", True),
                           ("infra/moq-sh/install.sh", False), (".github/workflows/check.yml", False)):
            with self.subTest(path=path):
                self.change(path)
                self.expect(True, rust=rust)
                (self.repo / path).unlink()
        self.expect(True, base="--all", rust=True)

    def test_cargo_configuration_selects_rust(self):
        for path in (".cargo/config.toml", ".cargo/config"):
            with self.subTest(path=path):
                self.change(path)
                self.expect(True, rust=True)
                calls = self.run_ci()
                self.assertTrue(any(call.startswith("rs lint-changed ") for call in calls), calls)
                self.assertTrue(any(call.startswith("rs test-changed ") for call in calls), calls)
                (self.repo / path).unlink()

    def test_nixpkgs_updates_check_browser_pins(self):
        self.change("flake.lock")
        self.expect(True)

    def test_empty_diff_keeps_repository_lints(self):
        self.expect(True)

    def test_selected_tests_do_not_depend_on_installed_tools(self):
        for path in ("js/net/src/index.ts", "rs/moq-net/src/lib.rs", "py/moq-rs/example.py",
                     "doc/concept/example.md", "test/example.ts"):
            with self.subTest(path=path):
                self.change(path)
                self.expect(True)
                (self.repo / path).unlink()

    def test_orchestration_and_all_select_work(self):
        self.expect(True, base="--all")
        self.change("justfile")
        self.expect(True)

    def test_working_tree_changes_and_deletions_count(self):
        self.change("rs/moq-net/src/lib.rs")
        self.git("add", ".")
        self.git("commit", "-qm", "source")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        (self.repo / "rs/moq-net/src/lib.rs").unlink()
        self.expect(True)

    def test_release_pr_base_and_merge_group_base(self):
        self.env["GITHUB_BASE_REF"] = "release"
        self.expect(True, base="", rust=False)
        # The entry queued ahead changes Rust and has not merged; this one changes a quest.
        self.change("rs/moq-net/src/lib.rs")
        self.git("add", ".")
        self.git("commit", "-qm", "queued source")
        self.change("quest/m1/example.md")
        # Check's merge group base: a passing head can merge the entry ahead, so it selects that too.
        self.expect(True, base="origin/main", rust=True)
        self.expect(True, base="origin/release")

    def test_invalid_base_fails_instead_of_reporting_no_work(self):
        result = self.run_query(base="missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("needed=", result.stdout)
        self.assertIn("cannot resolve merge-base", result.stderr)

    def test_execution_still_requires_dev_shell(self):
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"],
                                cwd=self.repo, env=self.env, text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run inside the Nix dev shell", result.stderr)

    def platform(self, base="origin/main"):
        result = self.run_query("platform", base)
        self.assertEqual(result.returncode, 0, result.stderr)
        return dict(line.split("=") for line in result.stdout.splitlines())

    def test_platform_skips_quest_and_js(self):
        for path in ("quest/m1/example.md", "js/net/src/example.ts"):
            with self.subTest(path=path):
                self.change(path)
                self.assertEqual(self.platform()["needed"], "false")
                (self.repo / path).unlink()

    def test_platform_rust_change_rebuilds_external_consumers(self):
        self.change("rs/moq-net/src/lib.rs")
        result = self.platform()
        for suite in ("rs", "swift", "c", "cpp", "obs_compile", "needed"):
            self.assertEqual(result[suite], "true", suite)
        self.assertEqual(result["moq_sh"], "false")

    def test_platform_selects_only_the_touched_suite(self):
        self.change("doc/lib/swift/index.md")
        result = self.platform()
        self.assertEqual((result["swift"], result["rs"]), ("true", "false"))
        (self.repo / "doc/lib/swift/index.md").unlink()
        self.change("infra/moq-sh/install.sh")
        result = self.platform()
        self.assertEqual((result["moq_sh"], result["rs"]), ("true", "false"))

    def test_platform_shared_configuration_selects_everything(self):
        for path in (".cargo/config.toml", ".github/workflows/platform.yml", "rs/justfile"):
            with self.subTest(path=path):
                self.change(path)
                self.assertEqual(set(self.platform().values()), {"true"})
                (self.repo / path).unlink()
        self.assertEqual(set(self.platform("--all").values()), {"true"})

    def test_platform_is_only_a_query(self):
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci-platform", "origin/main"],
                                cwd=self.repo, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")

    def stub_ci(self, just=""):
        for tool in ("bun", "python3", "cargo", "jq", "shellcheck",
                     "shfmt", "taplo", "nixfmt", "actionlint", "quest", "nix",
                     "c++", "clang-format", "cmake", "dart", "dash", "gersemi", "go",
                     "gradle", "java", "kramdown-rfc", "pkg-config", "uniffi-bindgen-cpp",
                     "clippy-driver", "dpkg-deb", "nfpm", "rpm",
                     "uniffi-bindgen-go", "uniffi_bindgen_dart", "uv", "xml2rfc"):
            path = self.bin / tool
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        path = self.bin / "just"
        path.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$*" >>"$CALLS"\n' + just)
        path.chmod(0o755)
        nix = self.bin / "nix"
        nix.write_text('#!/usr/bin/env bash\nprintf "nix %s\\n" "$*" >>"$CALLS"\n')
        nix.chmod(0o755)
        self.env.update(IN_MOQ_DEV_SHELL="1", CALLS=str(self.root / "calls"))
        (self.root / "calls").unlink(missing_ok=True)
        self.env["PATH"] = f"{self.bin}:{os.environ['PATH']}"
        for script in ("markdown.sh", "shell.sh", "large-files.sh"):
            path = self.repo / "sh" / script
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)

    def run_ci(self, base="origin/main"):
        self.stub_ci()
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", base],
                                cwd=self.repo, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return (self.root / "calls").read_text().splitlines()

    def test_failure_keeps_running_lanes_and_starts_remaining_modules(self):
        # The failed lane finishes before go releases the two parked lanes.
        # No timing assumption can make cancellation look like completion.
        blocked = {}
        for name, call in (("rs", '"rs test-changed "*'), ("cpp", '"cpp check"')):
            started = self.root / f"{name}.started"
            released = self.root / f"{name}.released"
            os.mkfifo(started)
            os.mkfifo(released)
            blocked[name] = (call, started, released)
        stub = "".join(f'    {call}) echo >"{started}"; read -r <"{released}" ;;\n'
                       for call, started, released in blocked.values())
        reads = "; ".join(f'read -r <"{started}"' for _, started, _ in blocked.values())
        releases = "; ".join(f'echo >"{released}"' for _, _, released in blocked.values())
        self.stub_ci(f'case "$*" in\n{stub}    "gh test") {reads}; exit 3 ;;\n'
                     f'    "go check") {releases} ;;\nesac\n')
        self.change("rs/moq-ffi/src/lib.rs")
        self.change("js/net/src/index.ts")
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("ci: test gh failed", result.stderr)
        self.assertRegex(result.stderr, r"\d+  test gh \(failed\)")
        self.assertNotIn("(cancelled)", result.stderr)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertIn("go check", calls)
        self.assertIn("obs compile", calls)
        self.assertIn("dart check", calls)

    def test_fail_fast_cancels_running_lanes_and_starts_no_more(self):
        # The land gate opts in: the parked lanes never finish on their own, so
        # only the cancellation can end the run.
        blocked = {}
        for name, call in (("rs", '"rs test-changed "*'), ("cpp", '"cpp check"')):
            started = self.root / f"{name}.started"
            released = self.root / f"{name}.released"
            os.mkfifo(started)
            os.mkfifo(released)
            blocked[name] = (call, started, released)
        stub = "".join(f'    {call}) echo >"{started}"; read -r <"{released}" ;;\n'
                       for call, started, released in blocked.values())
        reads = "; ".join(f'read -r <"{started}"' for _, started, _ in blocked.values())
        self.stub_ci(f'case "$*" in\n{stub}    "gh test") {reads}; exit 3 ;;\nesac\n')
        self.change("rs/moq-ffi/src/lib.rs")
        self.change("js/net/src/index.ts")
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"], cwd=self.repo,
                                env=dict(self.env, CHECK_FAIL_FAST="1"), text=True, capture_output=True,
                                timeout=60)
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("ci: test gh failed", result.stderr)
        self.assertRegex(result.stderr, r"\d+  test rs \(cancelled\)")
        self.assertRegex(result.stderr, r"\d+  check cpp \(cancelled\)")
        self.assertNotIn("ci: test rs failed", result.stderr)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertNotIn("go check", calls)
        self.assertNotIn("obs compile", calls)

    def test_fail_fast_stops_a_serial_run_at_the_first_failure(self):
        self.stub_ci('case "$*" in "js check-changed "*) exit 3 ;; "gh check") exit 4 ;; esac\n')
        self.change("js/net/src/index.ts")
        self.change("dart/example.dart")
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"], cwd=self.repo,
                                env=dict(self.env, CHECK_JOBS="1", CHECK_FAIL_FAST="1"), text=True,
                                capture_output=True)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        failures = [line for line in result.stderr.splitlines() if line.startswith("ci: ") and line.endswith(" failed")]
        self.assertEqual(len(failures), 1, result.stderr)

    def test_failure_continues_independent_modules_in_the_same_lane(self):
        for jobs in ("1", "3"):
            with self.subTest(jobs=jobs):
                self.stub_ci('case "$*" in "js check-changed "*) exit 3 ;; "gh check") exit 4 ;; esac\n')
                self.change("js/net/src/index.ts")
                self.change("dart/example.dart")
                result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"], cwd=self.repo,
                                        env=dict(self.env, CHECK_JOBS=jobs), text=True, capture_output=True)
                self.assertEqual(result.returncode, 3, result.stderr)
                self.assertIn("ci: check js failed", result.stderr)
                self.assertIn("ci: check gh failed", result.stderr)
                calls = (self.root / "calls").read_text().splitlines()
                self.assertIn("gh check", calls)
                self.assertIn("dart check", calls)
                self.assertFalse(any(call.startswith("js test-changed ") for call in calls))
                self.assertNotIn("gh test", calls)

    @unittest.skipUnless(sys.platform == "linux" and os.uname().machine == "x86_64", "Linux VM checks only")
    def test_local_flake_namespace_refusal_is_reported_as_skipped(self):
        self.stub_ci()
        self.change("flake.nix")
        nix = self.bin / "nix"
        nix.write_text(nix.read_text() + 'case "$*" in "build "*) echo MOQ_USERNS_UNAVAILABLE >&2; exit 1 ;; esac\n')
        self.env.pop("CI_STRICT")
        nonces = []
        for _ in range(2):
            (self.root / "calls").unlink(missing_ok=True)
            result = subprocess.run(["bash", "sh/dispatch.sh", "check", "origin/main"], cwd=self.repo,
                                    env=self.env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("local Nix builder cannot create user namespaces", result.stdout + result.stderr)
            self.assertRegex(result.stderr, r"\d+  check flake \(skipped\)")
            calls = (self.root / "calls").read_text().splitlines()
            self.assertNotIn("nix flake check --print-build-logs", calls)
            self.assertIn("gh test", calls)
            probe, = [call for call in calls if call.startswith("nix build ")]
            self.assertIn("--builders  --max-jobs 1", probe)
            self.assertIn("--no-link", probe)
            nonces.append(probe.split("--argstr nonce ")[1].split()[0])
        self.assertNotEqual(*nonces)

    @unittest.skipUnless(sys.platform == "linux" and os.uname().machine == "x86_64", "Linux VM checks only")
    def test_local_capable_builder_runs_the_full_flake(self):
        self.stub_ci()
        self.change("flake.nix")
        self.env.pop("CI_STRICT")
        result = subprocess.run(["bash", "sh/dispatch.sh", "check", "origin/main"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("(skipped)", result.stderr)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertIn("nix flake check --print-build-logs", calls)
        self.assertEqual(sum(call.startswith("nix build ") for call in calls), 1)

    @unittest.skipUnless(sys.platform == "linux" and os.uname().machine == "x86_64", "Linux VM checks only")
    def test_local_flake_check_exiting_77_fails_instead_of_skipping(self):
        self.stub_ci()
        self.change("flake.nix")
        nix = self.bin / "nix"
        nix.write_text(nix.read_text() + 'case "$*" in "flake check --print-build-logs") exit 77 ;; esac\n')
        self.env.pop("CI_STRICT")
        result = subprocess.run(["bash", "sh/dispatch.sh", "check", "origin/main"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn("(skipped)", result.stderr)
        self.assertIn("check: check flake failed", result.stderr)

    @unittest.skipUnless(sys.platform == "linux" and os.uname().machine == "x86_64", "Linux VM checks only")
    def test_local_flake_probe_errors_are_failures_without_cancelling_other_modules(self):
        self.stub_ci()
        self.change("flake.nix")
        nix = self.bin / "nix"
        nix.write_text(nix.read_text() + 'case "$*" in "build "*) echo "evaluation failed" >&2; exit 1 ;; esac\n')
        self.env.pop("CI_STRICT")
        result = subprocess.run(["bash", "sh/dispatch.sh", "check", "origin/main"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("check: check flake failed", result.stderr)
        self.assertNotIn("(skipped)", result.stderr)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertNotIn("nix flake check --print-build-logs", calls)
        self.assertIn("gh test", calls)

    def test_strict_ci_runs_the_full_flake_without_a_probe(self):
        self.change("flake.nix")
        calls = self.run_ci()
        self.assertIn("nix flake check --print-build-logs", calls)
        self.assertFalse(any(call.startswith("nix build ") for call in calls))
        nix = self.bin / "nix"
        nix.write_text(nix.read_text() + 'exit 77\n')
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "origin/main"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertNotIn("(skipped)", result.stderr)
        self.assertIn("ci: check flake failed", result.stderr)

    def test_terminating_the_run_stops_the_running_phase(self):
        started = self.root / "started"
        os.mkfifo(started)
        pid = self.root / "pid"
        self.stub_ci(f'''case "$*" in
    "rs lint-changed "*) echo $$ >"{pid}"; echo >"{started}"; exec sleep 600 ;;
esac
''')
        self.change("rs/moq-clock/src/lib.rs")
        for jobs in ("1", "3"):
            with self.subTest(jobs=jobs):
                run = subprocess.Popen(["bash", "sh/dispatch.sh", "ci", "origin/main"],
                                       cwd=self.repo, env=dict(self.env, CHECK_JOBS=jobs),
                                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                started.read_text()
                # A job timeout can kill the run without a signal it traps, so
                # a buffered lane names its running phase before the phase ends.
                os.set_blocking(run.stderr.fileno(), False)
                live = (run.stderr.read() or b"").decode()
                if jobs != "1":
                    self.assertIn("ci: check rs started", live)
                run.terminate()
                os.set_blocking(run.stderr.fileno(), True)
                stderr = live + run.communicate(timeout=60)[1].decode()
                self.assertEqual(run.returncode, 143, stderr)
                self.assertRegex(stderr, r"\d+  check rs \(cancelled\)")
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(pid.read_text()), 0)

    def test_ci_runs_one_lint_preamble_and_one_test_pass(self):
        self.change("rs/moq-clock/src/lib.rs")
        calls = self.run_ci()
        self.assertTrue(any(call.startswith("rs lint-changed ") for call in calls), calls)
        self.assertFalse(any(call.startswith("rs check-changed ") for call in calls), calls)
        self.assertEqual(sum(call.startswith("rs lint-changed ") for call in calls), 1)
        self.assertEqual(sum(call.startswith("rs test-changed ") for call in calls), 1)
        self.assertLess(next(i for i, call in enumerate(calls) if call.startswith("rs lint-changed ")),
                        next(i for i, call in enumerate(calls) if call.startswith("rs test-changed ")))

    def test_rust_lane_runs_only_the_lint_and_the_test_build(self):
        self.change("rs/moq-clock/src/lib.rs")
        calls = [call.split()[1] for call in self.run_ci() if call.startswith("rs ")]
        self.assertEqual(calls, ["lint-changed", "test-changed"])

    def test_module_filter_splits_the_gate(self):
        modules = {"rs": ["rs lint-changed --all", "rs test-changed --all"], "-rs": []}
        for filter, expected in modules.items():
            with self.subTest(filter=filter):
                self.env["CHECK_MODULES"] = filter
                calls = self.run_ci("--all")
                self.assertEqual([call for call in calls if call.startswith("rs ")], expected)
                # Every other module runs in the job that does not name one.
                self.assertEqual("gh check" in calls, filter.startswith("-"))
        self.env["CHECK_MODULES"] = "rs_shapes"
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci", "--all"], cwd=self.repo,
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("CHECK_MODULES names 'rs_shapes'", result.stderr)

    def test_flake_checks_follow_nix_inputs_instead_of_rust_source(self):
        for path, expected in ((".cargo/config.toml", True),
                               ("rs/moq-net/src/lib.rs", False),
                               ("rs/moq-net/Cargo.toml", True),
                               ("rs/moq-c/native-libs/linux.txt", True),
                               ("rs/moq-video/src/frame/nv12_resize.ptx", True),
                               ("Cargo.toml", True), ("Cargo.lock", True),
                               ("rust-toolchain.toml", True),
                               ("flake.nix", True), ("flake.lock", True),
                               ("nix/runner/hog.py", True),
                               ("nix/runner/migrate.sh", True),
                               ("infra/example.nix", True),
                               ("quest/m1/example.md", False)):
            with self.subTest(path=path):
                self.change(path)
                calls = self.run_ci()
                (self.repo / path).unlink()
                self.assertEqual("nix flake check --print-build-logs" in calls, expected)
        self.assertIn("nix flake check --print-build-logs", self.run_ci("--all"))


def benchmark():
    # Sweep path count and both job selections. No timing threshold: hosted setup
    # and checkout costs are separate, and the query must never run those actions.
    for count in (1, 100, 1000):
        fixture = QueryTests()
        fixture.setUp()
        try:
            for index in range(count):
                fixture.change(f"quest/m1/example-{index}.md")
            for job in ("ci",):
                samples = []
                for _ in range(10):
                    started = time.perf_counter()
                    fixture.expect(True, job)
                    samples.append((time.perf_counter() - started) * 1000)
                print(f"{job}: {count} paths, median {statistics.median(samples):.1f} ms")
        finally:
            fixture.doCleanups()


if __name__ == "__main__":
    if sys.argv[1:] == ["--bench"]:
        benchmark()
    else:
        unittest.main()
