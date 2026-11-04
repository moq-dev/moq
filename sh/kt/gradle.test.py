"""Kotlin runners select executables even when the shell exports Gradle functions."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASH = shutil.which("bash")
RUNNERS = ("check", "package", "bench")


class GradleTest(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".tmp"
        scratch.mkdir(exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.calls = self.root / "calls.jsonl"
        for name in ("bash", "dirname", "mkdir", "rm", "cp"):
            (self.tools / name).symlink_to(shutil.which(name))
        (self.tools / "python3").symlink_to(sys.executable)
        self.write("tools/java", "exit 0\n")
        for name in (f"{runner}.sh" for runner in RUNNERS):
            target = self.root / "sh/kt" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / "sh/kt" / name, target)
        for name in ("format.sh", "generate.sh"):
            self.write(f"sh/kt/{name}", "exit 0\n")
        self.write("doc/lib/samples.sh", "exit 0\n")
        (self.root / "sh/kt/check-poms.py").write_text("")
        (self.root / "kt/moq/src/jvmAndAndroidTest/kotlin/dev/moq/docs").mkdir(parents=True)
        source = self.root / "bindings/uniffi/moq/moq.kt"
        source.parent.mkdir(parents=True)
        source.write_text("")
        self.gradle("gradle")
        self.gradle("custom-gradle")

    def write(self, name, source):
        target = self.root / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(f"#!{BASH}\n{source}")
        target.chmod(0o755)

    def gradle(self, name):
        target = self.tools / name
        target.write_text(
            f"#!{sys.executable}\n"
            "import json, os, sys\n"
            'with open(os.environ["GRADLE_CALLS"], "a") as output:\n'
            "    output.write(json.dumps({\n"
            '        "executable": sys.argv[0], "args": sys.argv[1:],\n'
            '        "runner_keys": [key for key in os.environ\n'
            '                        if key.startswith(("ACTIONS_", "GITHUB_", "RUNNER_"))],\n'
            '    }) + "\\n")\n'
            'sys.exit(int(os.environ.get("GRADLE_STATUS", "0")))\n'
        )
        target.chmod(0o755)

    def run_script(self, script, **changes):
        self.calls.unlink(missing_ok=True)
        # An imported function must never satisfy executable discovery. FUNCNEST
        # turns a recursive wrapper into a deterministic failure in the fixture.
        env = {
            "PATH": str(self.tools),
            "HOME": str(self.root),
            "LOCAL_SUPERVISED": "1",
            "BASH_ENV": os.devnull,
            "BASH_FUNC_gradle%%": "() { echo 'inherited function called' >&2; return 99; }",
            "FUNCNEST": "10",
            "GRADLE_CALLS": str(self.calls),
            "ACTIONS_TEST": "fixture",
            "GITHUB_TEST": "fixture",
            "RUNNER_TEST": "fixture",
            **changes,
        }
        args = [BASH, f"sh/kt/{script}.sh"]
        if script == "package":
            args += ["--version", "0.0.0-dev", "--lib-dir", "libs", "--bindings-dir", "bindings", "--output", "dist"]
        return subprocess.run(args, cwd=self.root, env=env, text=True, capture_output=True)

    def recorded(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_path_executable_wins_over_exported_function(self):
        first = {"check": ":moq:jvmTest", "package": ":moq-ffi:assemble", "bench": "dev.moq.FfiBench"}
        for script in RUNNERS:
            for override in ({}, {"GRADLE_CMD": "gradle"}):
                with self.subTest(script=script, override=override):
                    result = self.run_script(script, **override)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    calls = self.recorded()
                    self.assertEqual(len(calls), 2 if script == "check" else 1)
                    self.assertTrue(all(Path(call["executable"]) == self.tools / "gradle" for call in calls))
                    self.assertIn(first[script], calls[0]["args"])
                    if script == "check":
                        self.assertTrue(all(call["runner_keys"] == [] for call in calls))
                        self.assertIn(":moq:generatePomFileForJvmPublication", calls[1]["args"])

    def test_explicit_executable_is_preserved(self):
        for script in RUNNERS:
            for override in ("custom-gradle", "tools/custom-gradle", str(self.tools / "custom-gradle")):
                with self.subTest(script=script, override=override):
                    result = self.run_script(script, GRADLE_CMD=override)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(all(Path(call["executable"]).name == "custom-gradle" for call in self.recorded()))

    def test_function_without_executable_is_refused(self):
        (self.tools / "gradle").unlink()
        for script in RUNNERS:
            with self.subTest(script=script):
                result = self.run_script(script)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("Gradle executable 'gradle' not found", result.stderr)
                self.assertNotIn("inherited function called", result.stderr)
                self.assertFalse(self.calls.exists())

    def test_missing_override_is_named(self):
        for script in RUNNERS:
            with self.subTest(script=script):
                result = self.run_script(script, GRADLE_CMD="tools/missing-gradle")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("Gradle executable 'tools/missing-gradle' not found", result.stderr)
                self.assertFalse(self.calls.exists())

    def test_gradle_failure_stops_the_runner(self):
        for script in RUNNERS:
            with self.subTest(script=script):
                result = self.run_script(script, GRADLE_STATUS="7")
                self.assertEqual(result.returncode, 7, result.stderr)
                self.assertEqual(len(self.recorded()), 1)
                self.assertNotIn("kt bench: wrote", result.stdout)


if __name__ == "__main__":
    unittest.main()
