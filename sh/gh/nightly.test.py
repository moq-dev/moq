"""Nightly groups report a failure without hiding independent later benchmarks."""

import json
import os
import re
import shlex
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[2]


class FeaturesTest(unittest.TestCase):
    def run_features(self, failures=(), nested=False, leaks=(), recipe="features"):
        runner = shutil.which("just")
        (ROOT / ".tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            for command in ("just", "cargo"):
                stub = scratch / command
                stub.write_text(
                    textwrap.dedent("""\
                    #!/usr/bin/env python3
                    import json, os, subprocess, sys
                    from pathlib import Path
                    args = sys.argv[1:]
                    call = [os.path.basename(sys.argv[0]), *[
                        'mir=<output>' if arg.startswith('mir=') else arg for arg in args
                    ]]
                    with open(os.environ['FEATURE_CALLS'], 'a') as output:
                        output.write(json.dumps(call) + '\\n')
                    if call in json.loads(os.environ['FEATURE_FAILURES']):
                        sys.exit(1)
                    if call[0] == 'just':
                        if os.environ['FEATURE_NESTED'] == '1' and args[1] in (
                            'net-features', 'quic-features', 'crypto-key', 'tokio-features', 'media-features',
                            'coroutines',
                        ):
                            sys.exit(subprocess.call([os.environ['FEATURE_REAL_JUST'], *args]))
                        sys.exit(0)
                    if args[0] == 'tree':
                        for crate in json.loads(os.environ['FEATURE_LEAKS']):
                            print(crate + ' v1.0.0')
                        features = args[args.index('--features') + 1].split(',') if '--features' in args else []
                        for feature, crates in {
                            'openh264': ['openh264', 'openh264-sys2'], 'nvidia': ['moq-nvenc'],
                            'render': ['wgpu'], 'vpx': ['libvpx-native-sys'], 'capture': ['cpal'],
                        }.items():
                            if feature in features:
                                for crate in crates:
                                    print(crate + ' v1.0.0')
                    elif args[0] == 'rustc':
                        for arg in args:
                            if arg.startswith('mir='):
                                Path(arg[4:]).write_text('')
                    """)
                )
                stub.chmod(0o755)
            result = subprocess.run(
                [runner, "rs", recipe],
                cwd=ROOT,
                env=dict(
                    os.environ,
                    PATH=f"{scratch}:{os.environ['PATH']}",
                    FEATURE_CALLS=str(calls),
                    FEATURE_FAILURES=json.dumps(failures),
                    FEATURE_REAL_JUST=runner,
                    FEATURE_NESTED=str(int(nested)),
                    FEATURE_LEAKS=json.dumps(leaks),
                    LOCAL_SUPERVISED="1",
                    GITHUB_ACTIONS="false",
                    BASH_ENV="/dev/null",
                ),
                capture_output=True,
                text=True,
            )
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_independent_checks_run_after_multiple_failures(self):
        passed, expected = self.run_features()
        self.assertEqual(passed.returncode, 0, passed.stderr)
        # Early, middle, and final failures must all survive subsequent successes.
        for failures in ([expected[0], expected[len(expected) // 2]], [expected[-1]]):
            with self.subTest(failures=failures):
                failed, calls = self.run_features(failures)
                self.assertNotEqual(failed.returncode, 0)
                self.assertEqual(calls, expected)
                summary = [line for line in failed.stderr.splitlines() if line.startswith("checks: failed: ")]
                self.assertEqual(len(summary), len(failures))
                for failure in failures:
                    self.assertTrue(any(" ".join(failure) in line for line in summary))

    def test_platform_tests_run_after_a_failure(self):
        for recipe in ("macos-test", "windows-test"):
            with self.subTest(recipe=recipe):
                passed, expected = self.run_features(recipe=recipe)
                self.assertEqual(passed.returncode, 0, passed.stderr)
                failed, calls = self.run_features([expected[0]], recipe=recipe)
                self.assertNotEqual(failed.returncode, 0)
                self.assertEqual(calls, expected)
                self.assertIn("checks: failed: " + " ".join(expected[0]), failed.stderr)

    def test_nested_feature_checks_continue_after_compile_failures(self):
        passed, expected = self.run_features(nested=True)
        self.assertEqual(passed.returncode, 0, passed.stderr)
        failures = []
        for package in ("kio", "moq-net", "moq-quic", "moq-crypto", "moq-tokio", "moq-video"):
            failures.append(
                next(call for call in expected if call[0] == "cargo" and call[1] != "tree" and package in call)
            )
        failed, calls = self.run_features(failures, nested=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(calls, expected)
        summary = "\n".join(line for line in failed.stderr.splitlines() if line.startswith("checks: failed: "))
        for package in ("kio", "moq-net", "moq-quic", "moq-crypto", "moq-tokio", "moq-video"):
            self.assertIn(package, summary)
        self.assertIn("checks: just rs coroutines", failed.stdout)

    def test_dependency_contract_failures_do_not_hide_checks(self):
        passed, expected = self.run_features(nested=True)
        self.assertEqual(passed.returncode, 0, passed.stderr)
        failed, calls = self.run_features(nested=True, leaks=("cpal",))
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(calls, expected)
        self.assertIn("moq-video minimal unexpectedly contains cpal", failed.stderr)


RUST_TEST_GROUPS = ("rust-test", "rust-test-apps")


class NightlyTest(unittest.TestCase):
    def run_group(self, group, fail="", cwd=ROOT, **environment):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            for command in ("just", "cargo"):
                stub = scratch / command
                stub.write_text(
                    "#!/usr/bin/env bash\n"
                    'printf "%s %s\n" "${0##*/}" "$*" >> "$NIGHTLY_CALLS"\n'
                    'printf "%s\n" "${BENCH_BASE:-}" >> "$NIGHTLY_CALLS.base"\n'
                    # Nextest's bench profile writes a JUnit report with a case per filtered test,
                    # unless the test withholds the report or one test.
                    'if [[ ${NEXTEST_PROFILE:-} == bench && "$*" != "$NIGHTLY_NO_JUNIT" ]]; then\n'
                    '    mkdir -p "$CARGO_TARGET_DIR/nextest/bench"\n'
                    "    names=$(grep -oE 'test\\(=?[[:alnum:]_:]+\\)' <<<\"$*\" | sed -E 's/^test\\(=?//; s/\\)$//')\n"
                    "    for name in $names; do\n"
                    '        [[ $name == "${NIGHTLY_UNMATCHED:-}" ]] ||\n'
                    '            printf \'<testcase name="mod::%s" classname="crate"/>\' "$name"\n'
                    '    done > "$CARGO_TARGET_DIR/nextest/bench/junit.xml"\n'
                    '    printf "<testsuites/>" >> "$CARGO_TARGET_DIR/nextest/bench/junit.xml"\n'
                    "fi\n"
                    '[[ "$*" != "$NIGHTLY_FAIL" ]]\n'
                )
                stub.chmod(0o755)
            # Answers `gh run list` by applying its --jq filter to NIGHTLY_RUNS.
            gh = scratch / "gh"
            gh.write_text(
                "#!/usr/bin/env bash\nwhile [[ $1 != --jq ]]; do shift; done\n"
                'printf "%s" "$NIGHTLY_RUNS" | jq -r "$2"\n'
            )
            gh.chmod(0o755)
            env = dict(
                os.environ,
                PATH=f"{scratch}:{os.environ['PATH']}",
                BENCH_RESULTS=str(scratch / "results"),
                NIGHTLY_CALLS=str(calls),
                NIGHTLY_FAIL=fail,
                BASH_ENV="/dev/null",
                CARGO_TARGET_DIR=str(scratch / "target"),
            )
            for name in ("BENCH_BASE", "NEXTEST_PROFILE", "NIGHTLY_NO_JUNIT", "NIGHTLY_UNMATCHED"):
                env.pop(name, None)
            env.update(environment)
            result = subprocess.run(
                ["bash", ROOT / "sh/gh/nightly.sh", group], cwd=cwd, env=env, capture_output=True, text=True
            )
            raw = scratch / "results" / "raw"
            self.raw = sorted(path.name for path in raw.iterdir()) if raw.exists() else []
            self.bases = set(Path(f"{calls}.base").read_text().splitlines()) if calls.exists() else set()
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_paired_base_is_the_previous_scheduled_nightly(self):
        # Nightly checks out shallowly, so the test builds the history it resolves against.
        with tempfile.TemporaryDirectory() as repo:
            git = [
                "git",
                "-C",
                repo,
                "-c",
                "user.name=nightly",
                "-c",
                "user.email=nightly@example.com",
                "-c",
                "commit.gpgsign=false",
            ]
            subprocess.run([*git, "init", "-q"], check=True)
            for message in ("parent", "head"):
                subprocess.run([*git, "commit", "-q", "--allow-empty", "-m", message], check=True)
            head, parent = (
                subprocess.run([*git, "rev-parse", revision], capture_output=True, text=True, check=True).stdout.strip()
                for revision in ("HEAD", "HEAD~1")
            )
            # This run is the newest scheduled one; the previous run's commit is the base.
            runs = json.dumps([{"databaseId": 2, "headSha": parent}, {"databaseId": 1, "headSha": head}])
            result, _ = self.run_group(
                "rust-uring", cwd=repo, BENCH_BASE="previous-nightly", GITHUB_RUN_ID="2", NIGHTLY_RUNS=runs
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.bases, {head})
            # A named revision reaches the runner as its commit.
            result, _ = self.run_group("rust-uring", cwd=repo, BENCH_BASE="HEAD~1")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.bases, {parent})
            result, calls = self.run_group(
                "rust-uring",
                cwd=repo,
                BENCH_BASE="previous-nightly",
                GITHUB_RUN_ID="1",
                NIGHTLY_RUNS=json.dumps([{"databaseId": 1, "headSha": head}]),
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("no previous scheduled Nightly", result.stderr)
            self.assertEqual(calls, [])

    def test_paired_base_applies_only_to_rust_bench_groups(self):
        for group in ("js", "rust-test", "rust-test-apps", "bindings"):
            result, calls = self.run_group(group, BENCH_BASE="HEAD")
            self.assertEqual(result.returncode, 2, group)
            self.assertEqual(calls, [])

    def test_failed_js_benchmark_does_not_hide_later_work(self):
        result, calls = self.run_group("js", "js bench reader")
        self.assertEqual(result.returncode, 1)
        self.assertIn("just js bench frames", calls)
        self.assertEqual(calls[-1], "just js clock-bench")
        self.assertIn("failed: just js bench reader", result.stderr)

    def test_binding_runner_fails_without_its_report(self):
        # The stubs write no reports, so every runner fails the group, a crash included.
        result, calls = self.run_group("bindings", "py bench")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(calls[-1], "just dart bench")
        self.assertIn("failed: just py bench", result.stderr)
        for runner in ("rust-ffi", "rust-net", "rust-background", "go", "python", "kotlin", "cpp", "dart"):
            self.assertIn(f"/{runner}.json", result.stderr)

    def test_bench_targets_share_one_invocation(self):
        result, calls = self.run_group("rust-bench")
        self.assertEqual(result.returncode, 0, result.stderr)
        release = [call for call in calls if call.startswith("just gh criterion")]
        self.assertEqual(len(release), 4)  # moq-net, its CPU clock, kio, then GStreamer.
        # The CPU clock reuses moq-net's build instead of rebuilding it without fuzz.
        self.assertTrue(
            any(
                "-p moq-net --features fuzz --bench session -- session_delivery_group_cost_cpu" in call
                for call in release
            )
        )
        self.assertEqual(release[-1], "just gh criterion -p moq-gst --bench catalog")
        self.assertTrue(calls[-1].startswith("just gh benchmark-report rust "))
        result, calls = self.run_group("rust-shared")
        self.assertEqual(result.returncode, 0, result.stderr)
        shared = [call for call in calls if call.startswith("just gh criterion")]
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

    def test_every_cargo_bench_target_runs_nightly(self):
        covered = set()
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        for group in ("rust-bench", "rust-shared", "rust-relay", "rust-extra", "rust-uring"):
            self.assertIn(f"recipe: gh nightly {group}\n", workflow)
            result, calls = self.run_group(group)
            self.assertEqual(result.returncode, 0, result.stderr)
            for call in calls:
                args = shlex.split(call)
                if args[:3] != ["just", "gh", "criterion"]:
                    continue
                packages = [args[i + 1] for i, arg in enumerate(args) if arg == "-p"]
                packages += [Path(args[i + 1]).parent.name for i, arg in enumerate(args) if arg == "--manifest-path"]
                targets = [args[i + 1] for i, arg in enumerate(args) if arg == "--bench"]
                names = set()
                for package in packages:
                    manifest = ROOT / "rs" / package / "Cargo.toml"
                    for bench in tomllib.loads(manifest.read_text()).get("bench", []):
                        names.add(bench["name"])
                        if "*" in targets or bench["name"] in targets:
                            covered.add((package, bench["name"]))
                # Cargo rejects the whole invocation when any named target is missing.
                self.assertEqual(set(targets) - names - {"*"}, set(), call)
        declared = {
            (manifest.parent.name, bench["name"])
            for manifest in (ROOT / "rs").glob("*/Cargo.toml")
            for bench in tomllib.loads(manifest.read_text()).get("bench", [])
        }
        self.assertEqual(declared - covered, set(), "Cargo bench targets missing from Nightly")

    def test_every_js_bench_runs_nightly(self):
        recipes = {}
        for block in re.split(r"\n(?=\S)", (ROOT / "js/justfile").read_text()):
            if match := re.match(r"([\w-]+)( \w+)?:\n", block):
                recipes[match[1]] = re.findall(r"^    bun (.+\.ts)$", block, re.M)
        result, calls = self.run_group("js")
        self.assertEqual(result.returncode, 0, result.stderr)
        # The browser benches run in their own Nightly job.
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        calls += re.findall(r"just js \S+-bench\b", workflow)
        entries = set()
        for call in calls:
            args = shlex.split(call)
            if args[:2] != ["just", "js"]:
                continue
            for path in recipes[args[2]]:
                entries.add(ROOT / "js" / path.replace("{{ name }}", args[-1]))
        # The browser smoke lane runs the in-memory fixtures in Chromium.
        self.assertIn("recipe: bench-browser --smoke", workflow)
        smoke = re.search(
            r"^for benchmark in (.+); do\n\s+bun micro\.ts ", (ROOT / "bench/browser/run.sh").read_text(), re.M
        )
        self.assertIsNotNone(smoke)
        # Each smoke name runs the entry micro.ts's table names for it.
        micro = (ROOT / "bench/browser/micro.ts").read_text()
        fixtures = dict(re.findall(r'^\t"?([\w-]+)"?: \{ entry: "([^"]+)"', micro, re.M))
        for name in smoke[1].split():
            self.assertIn(name, fixtures, f"browser smoke bench {name}")
            entries.add(ROOT / fixtures[name])
        # Bundled helpers only run through the entry that names them.
        helpers = {
            path for entry in entries for path in entry.parent.glob("*.ts") if f"/{path.name}" in entry.read_text()
        }
        declared = {path for path in (ROOT / "js").glob("*/bench/*.ts") if not path.name.endswith(".test.ts")}
        missing = {str(path.relative_to(ROOT)) for path in declared - entries - helpers}
        self.assertEqual(missing, set(), "JS benches missing from Nightly")

    def test_dev_shell_jobs_trust_the_nix_cache(self):
        # Without the flake's substituter, the dev shell builds uncached tools from
        # source and outlasts short jobs: the benchmark site timed out before publishing.
        # A self-hosted runner VM's own Nix already names the cache, as Queue relies on.
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        jobs = re.split(r"^  (?=[\w-]+:\n)", workflow.split("\njobs:\n", 1)[1], flags=re.M)
        for job in jobs:
            if "nix develop" in job and "runs-on: [self-hosted" not in job:
                self.assertIn("uses: ./.github/actions/nix", job, job.splitlines()[0])

    def test_every_tla_leg_path_exists(self):
        # A leg naming a deleted config checks nothing and fails Nightly; an
        # exclusion naming one silently stops excluding.
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        job = re.search(r"^  tla:\n(.*?)^  \S", workflow, re.M | re.S)
        self.assertIsNotNone(job)
        paths = [p for line in re.findall(r"^ +paths: (.+)$", job[1], re.M) for p in line.strip('"').split()]
        self.assertTrue(paths)
        missing = [p for p in paths if not (ROOT / p.removeprefix(":!")).exists()]
        self.assertEqual(missing, [], "TLA+ matrix paths that no longer exist")

    def test_benchmark_tests_stay_out_of_cargo_bench(self):
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        for group in RUST_TEST_GROUPS:
            self.assertIn(f"recipe: gh nightly {group}\n", workflow)
            result, calls = self.run_group(group)
            self.assertEqual(result.returncode, 0, result.stderr)
            *runs, publish = calls
            # One release build per group, keeping its JUnit report, so the benchmarks
            # share the build and the report the publish step reads.
            self.assertEqual(runs, [call for call in runs if call.startswith("just rs bench-tests ")])
            self.assertEqual(len(runs), 1)
            self.assertEqual(self.raw, ["nextest-1.xml"])
            self.assertTrue(publish.startswith("just gh benchmark-report rust-test "), publish)

    def test_benchmark_test_without_its_report_fails(self):
        _, (run, _) = self.run_group("rust-test")
        result, calls = self.run_group("rust-test", NIGHTLY_NO_JUNIT=run.removeprefix("just "))
        self.assertEqual(result.returncode, 1)
        self.assertIn("no JUnit report from: just rs bench-tests", result.stderr)
        self.assertFalse(any(call.startswith("just gh") for call in calls))

    def test_benchmark_filter_that_runs_nothing_fails(self):
        # A test behind a feature the build lacks matches nothing, and nextest still passes.
        result, calls = self.run_group("rust-test", NIGHTLY_UNMATCHED="benchmark_feed_fetch_wakes")
        self.assertEqual(result.returncode, 1)
        self.assertIn("no test ran for benchmark_feed_fetch_wakes", result.stderr)
        self.assertFalse(any(call.startswith("just gh") for call in calls))

    def test_every_nightly_benchmark_test_exists(self):
        # A filter naming a removed test matches nothing; this catches it without a build.
        named = set()
        for group in RUST_TEST_GROUPS:
            _, calls = self.run_group(group)
            for call in calls:
                named |= set(re.findall(r"test\(=?(?:\w+::)*(\w+)\)", call))
        self.assertTrue(named)
        sources = "".join(path.read_text() for path in (ROOT / "rs").glob("*/**/*.rs") if "/target/" not in str(path))
        # A filter can name one test or a module containing several benchmark cases.
        missing = {name for name in named if not re.search(rf"\b(?:fn|mod) {name}\b", sources)}
        self.assertEqual(missing, set(), "Nightly benchmark tests that no longer exist")

    def test_failed_release_build_does_not_hide_later_work(self):
        _, calls = self.run_group("rust-bench")
        build = next(
            call.removeprefix("just ")
            for call in calls
            if call.startswith("just gh criterion") and "-p moq-net" in call
        )
        result, calls = self.run_group("rust-bench", build)
        self.assertEqual(result.returncode, 1)
        # A failed group publishes no report, but every later target still runs.
        self.assertEqual(calls[-1], "just gh criterion -p moq-gst --bench catalog")

    def test_unknown_group_fails_before_running_anything(self):
        result, calls = self.run_group("unknown")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, [])


class FuzzTest(unittest.TestCase):
    def run_fuzz(self, *args):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            script = root / "sh/rs/fuzz.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "sh/rs/fuzz.sh", script)
            target = root / "rs/moq-net/fuzz/fuzz_targets/lite.rs"
            target.parent.mkdir(parents=True)
            target.touch()
            corpus = root / "rs/moq-net/fuzz/corpus/lite"
            corpus.mkdir(parents=True)
            (corpus / "discovery").write_bytes(b"from the previous run")
            seeds = root / "rs/moq-net/fuzz/seeds/lite"
            seeds.mkdir(parents=True)
            (seeds / "generated").write_bytes(b"generated seed")
            binary = root / "bin"
            binary.mkdir()
            rustup = binary / "rustup"
            rustup.write_text(
                "#!/bin/sh\n"
                'if [ "$*" = "run nightly rustc --print sysroot" ]; then printf "%s\\n" "$FUZZ_SYSROOT"; fi\n'
            )
            rustup.chmod(0o755)
            cargo = binary / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >>"$FUZZ_CALLS"\n')
            cargo.chmod(0o755)
            (binary / "cargo-fuzz").symlink_to(cargo)
            calls = root / "calls"
            env = dict(os.environ, PATH=f"{binary}:{os.environ['PATH']}", FUZZ_SYSROOT=str(root), FUZZ_CALLS=str(calls))
            result = subprocess.run(["bash", str(script), *args], cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual((corpus / "discovery").read_bytes(), b"from the previous run")
            self.assertEqual((seeds / "generated").read_bytes(), b"generated seed")
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_run_keeps_discoveries_writable_and_seeds_separate(self):
        result, calls = self.run_fuzz("lite", "--", "-runs=2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            calls,
            [
                "run --locked -q --manifest-path rs/moq-net/Cargo.toml --features fuzz --example fuzz-seeds -- "
                "rs/moq-net/fuzz/seeds",
                "fuzz run --fuzz-dir rs/moq-net/fuzz lite rs/moq-net/fuzz/corpus/lite rs/moq-net/fuzz/seeds/lite "
                "-- -runs=2",
            ],
        )

    def test_minimization_only_rewrites_the_evolved_corpus(self):
        result, calls = self.run_fuzz("--minimize", "lite")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, ["fuzz cmin --fuzz-dir rs/moq-net/fuzz lite rs/moq-net/fuzz/corpus/lite"])

    def test_unknown_target_fails_before_running_cargo(self):
        result, calls = self.run_fuzz("missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
