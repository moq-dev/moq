"""Rust test runs enforce lints and preserve runner options."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class TestLints(unittest.TestCase):
    def test_fail_fast_environment_reaches_nextest(self):
        temporary = ROOT / ".tmp"
        temporary.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            root = Path(directory)
            (root / "rs").mkdir()
            (root / "sh" / "rs").mkdir(parents=True)
            (root / "src").mkdir()
            shutil.copy(ROOT / "sh" / "local.sh", root / "sh" / "local.sh")
            shutil.copy(ROOT / "sh" / "local.ts", root / "sh" / "local.ts")
            (root / "justfile").write_text("mod rs\n")
            shutil.copy(ROOT / "rs" / "justfile", root / "rs" / "justfile")
            shutil.copy(ROOT / "sh" / "rs" / "nextest.sh", root / "sh" / "rs" / "nextest.sh")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "fail-fast-probe"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n'
            )
            (root / "src" / "lib.rs").write_text(
                '#[test]\nfn first() { panic!("first failure"); }\n#[test]\nfn second() { panic!("second failure"); }\n'
            )
            env = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"), NEXTEST_PROFILE="default")
            env.pop("NEXTEST_NO_FAIL_FAST", None)
            subprocess.run(
                ["cargo", "generate-lockfile", "--offline"], cwd=root, env=env, check=True, capture_output=True
            )
            for command in (["just", "rs", "test"], ["bash", "sh/rs/nextest.sh", "cargo", "nextest", "run"]):
                for value, options, count in (
                    (None, [], 1),
                    ("", [], 1),
                    ("1", [], 2),
                    ("1", ["--no-fail-fast"], 2),
                    ("1", ["--nff"], 2),
                    ("1", ["--fail-fast"], 1),
                    ("1", ["--ff"], 1),
                    ("1", ["--max-fail", "1"], 1),
                    ("1", ["--max-fail=1"], 1),
                ):
                    with self.subTest(command=command, value=value, options=options):
                        run_env = env.copy()
                        if value is not None:
                            run_env["NEXTEST_NO_FAIL_FAST"] = value
                        result = subprocess.run(
                            [
                                *command,
                                *options,
                                "--test-threads",
                                "1",
                                "--retries",
                                "0",
                                "--",
                                "--exact",
                                "first",
                                "second",
                            ],
                            cwd=root,
                            env=run_env,
                            text=True,
                            capture_output=True,
                        )
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(f"{count} failed", result.stderr)
                        self.assertEqual("tests were not run due to test failure" in result.stderr, count == 1)
                result = subprocess.run(
                    [*command, "--no-run"],
                    cwd=root,
                    env=dict(env, NEXTEST_NO_FAIL_FAST="1"),
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_clippy_warning_fails_test_build(self):
        temporary = ROOT / ".tmp"
        temporary.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            root = Path(directory)
            (root / "rs").mkdir()
            (root / "sh" / "rs").mkdir(parents=True)
            (root / "src").mkdir()
            shutil.copy(ROOT / "sh" / "local.sh", root / "sh" / "local.sh")
            (root / "justfile").write_text("mod rs\n")
            shutil.copy(ROOT / "rs" / "justfile", root / "rs" / "justfile")
            shutil.copy(ROOT / "sh" / "rs" / "nextest.sh", root / "sh" / "rs" / "nextest.sh")
            (root / "Cargo.toml").write_text(
                '[package]\nname = "test-lint-gate"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n'
            )
            (root / "src" / "lib.rs").write_text(
                "pub fn empty(values: &[u8]) -> bool { values.len() == 0 }\n"
                "#[test]\nfn runs() { assert!(empty(&[])); }\n"
            )
            toolchain = Path(shutil.which("clippy-driver")).parent
            env = dict(
                os.environ,
                LOCAL_SUPERVISED="1",
                CARGO_TARGET_DIR=str(root / "target"),
                PATH=f"{toolchain}:{os.environ['PATH']}",
                NEXTEST_PROFILE="default",
            )
            subprocess.run(
                ["cargo", "generate-lockfile", "--offline"], cwd=root, env=env, check=True, capture_output=True
            )
            result = subprocess.run(["just", "rs", "test"], cwd=root, env=env, text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("clippy::len_zero", result.stderr)
            source = root / "src" / "lib.rs"
            source.write_text(source.read_text().replace("values.len() == 0", "values.is_empty()"))
            result = subprocess.run(["just", "rs", "test"], cwd=root, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
