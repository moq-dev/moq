"""Nightly groups report a failure without hiding independent later benchmarks."""

import json
import os
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[2]


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
            )
            env.pop("BENCH_BASE", None)
            env.update(environment)
            result = subprocess.run(
                ["bash", ROOT / "sh/gh/nightly.sh", group], cwd=cwd, env=env, capture_output=True, text=True
            )
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
        for group in ("js", "rust-test", "bindings"):
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
        for runner in ("rust-ffi", "rust-net", "go", "python", "kotlin", "cpp", "dart"):
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
        workflow = (ROOT / ".github/workflows/nightly.yml").read_text()
        jobs = re.split(r"^  (?=[\w-]+:\n)", workflow.split("\njobs:\n", 1)[1], flags=re.M)
        for job in jobs:
            if "nix develop" in job:
                self.assertIn("uses: ./.github/actions/nix", job, job.splitlines()[0])

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
                "run --locked -q -p moq-net --features fuzz --example fuzz-seeds -- rs/moq-net/fuzz/seeds",
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
