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
        for script in ("dispatch.sh", "changed.sh"):
            shutil.copy(ROOT / "sh" / script, self.repo / "sh" / script)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for command in ("bash", "git", "sort", "mktemp", "rm", "grep"):
            (self.bin / command).symlink_to(shutil.which(command))
        self.env = dict(os.environ, PATH=str(self.bin), GIT_CONFIG_NOSYSTEM="1",
                        GIT_CONFIG_GLOBAL=os.devnull, MOQ_STRICT="1")
        for key in ("MOQ_DEV_SHELL", "MOQ_ALLOW_HOST", "GITHUB_BASE_REF"):
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

    def run_query(self, job="test", base="origin/main"):
        return subprocess.run(["bash", "sh/dispatch.sh", f"ci-{job}", base, "--query"],
                              cwd=self.repo, env=self.env, text=True, capture_output=True)

    def expect(self, needed, job="test", base="origin/main", rust=None):
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
        self.expect(False, rust=False)
        self.expect(True, "check", rust=False)

    def test_rust_cache_follows_modules_that_compile_rust(self):
        # Path, then Check's Rust, then Test's selection and Rust.
        for path, check, test, test_rust in (("rs/moq-net/src/lib.rs", True, True, True),
                                             ("py/moq-rs/example.py", True, True, True),
                                             ("js/net/src/index.ts", True, True, False),
                                             ("kt/example.kt", True, False, False),
                                             ("infra/moq-sh/install.sh", False, False, False),
                                             (".github/workflows/check.yml", False, False, False)):
            with self.subTest(path=path):
                self.change(path)
                self.expect(True, "check", rust=check)
                self.expect(test, rust=test_rust)
                (self.repo / path).unlink()
        self.expect(True, "check", "--all", rust=True)
        self.expect(True, base="--all", rust=True)

    def test_empty_diff_keeps_repository_lints(self):
        self.expect(False)
        self.expect(True, "check")

    def test_selected_tests_do_not_depend_on_installed_tools(self):
        for path in ("js/net/src/index.ts", "rs/moq-net/src/lib.rs", "py/moq-rs/example.py",
                     "doc/concept/example.md", "rs/moq-wasm/src/lib.rs", "test/example.ts"):
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
        self.expect(False, base="")
        self.change("rs/moq-net/src/lib.rs")
        self.git("add", ".")
        self.git("commit", "-qm", "queued source")
        group_base = self.git("rev-parse", "HEAD")
        self.change("quest/m1/example.md")
        self.expect(False, base=group_base)
        self.expect(True, base="origin/release")

    def test_invalid_base_fails_instead_of_reporting_no_work(self):
        result = self.run_query(base="missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("needed=", result.stdout)
        self.assertIn("cannot resolve merge-base", result.stderr)

    def test_query_and_execution_agree_on_selected_modules(self):
        for tool in ("bun", "python3", "cargo", "wasm-bindgen", "jq", "uv"):
            (self.bin / tool).symlink_to(shutil.which("true"))
        just = self.bin / "just"
        just.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$*" >>"$CALLS"\n')
        just.chmod(0o755)
        self.env.update(MOQ_DEV_SHELL="1", CALLS=str(self.root / "calls"))
        for path, call in (("js/net/src/index.ts", "js test"),
                           ("rs/moq-net/src/lib.rs", "rs test-changed"),
                           ("py/moq-rs/example.py", "py test")):
            with self.subTest(path=path):
                self.change(path)
                self.expect(True)
                subprocess.run(["bash", "sh/dispatch.sh", "ci-test", "origin/main"],
                               cwd=self.repo, env=self.env, capture_output=True, check=True)
                self.assertTrue((self.root / "calls").read_text().strip().startswith(call))
                (self.root / "calls").unlink()
                (self.repo / path).unlink()

    def test_execution_still_requires_dev_shell(self):
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci-test", "origin/main"],
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
        for suite in ("rs", "swift", "cpp", "obs_compile", "needed"):
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

    def test_ci_check_leaves_default_rust_lints_to_test(self):
        for tool in ("bun", "python3", "cargo", "wasm-bindgen", "jq", "shellcheck",
                     "shfmt", "taplo", "nixfmt", "actionlint", "quest", "nix"):
            path = self.bin / tool
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        just = self.bin / "just"
        just.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$*" >>"$CALLS"\n')
        just.chmod(0o755)
        self.env.update(MOQ_DEV_SHELL="1", CALLS=str(self.root / "calls"))
        self.change("rs/moq-clock/src/lib.rs")
        self.env["PATH"] += f":{os.environ['PATH']}"
        for script in ("markdown.sh", "shell.sh"):
            path = self.repo / "sh" / script
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        result = subprocess.run(["bash", "sh/dispatch.sh", "ci-check", "origin/main"],
                                cwd=self.repo, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertTrue(any(call.startswith("rs lint-changed ") for call in calls), calls)
        self.assertFalse(any(call.startswith("rs check-changed ") for call in calls), calls)


def benchmark():
    # Sweep path count and both job selections. No timing threshold: hosted setup
    # and checkout costs are separate, and the query must never run those actions.
    for count in (1, 100, 1000):
        fixture = QueryTests()
        fixture.setUp()
        try:
            for index in range(count):
                fixture.change(f"quest/m1/example-{index}.md")
            for job in ("check", "test"):
                samples = []
                for _ in range(10):
                    started = time.perf_counter()
                    fixture.expect(job == "check", job)
                    samples.append((time.perf_counter() - started) * 1000)
                print(f"{job}: {count} paths, median {statistics.median(samples):.1f} ms")
        finally:
            fixture.doCleanups()


if __name__ == "__main__":
    if sys.argv[1:] == ["--bench"]:
        benchmark()
    else:
        unittest.main()
