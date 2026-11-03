"""Admission must preserve failures and own the entire admitted command tree."""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = ROOT / "sh/local.sh"
SUPERVISOR = ROOT / "sh/local.ts"
BUN = shutil.which("bun")
BASH = shutil.which("bash")


class LocalGate(unittest.TestCase):
    def env(self, **changes):
        env = dict(os.environ)
        for key in (
            "QUEST_GATE_ADMITTED",
            "LOCAL_SUPERVISED",
            "GITHUB_ACTIONS",
            "NEXTEST_TEST_THREADS",
            "MOQ_QUEST",
            "ALLOW_HOST_TOOLCHAIN",
            "BASH_ENV",
        ):
            env.pop(key, None)
        env.update(changes)
        return env

    def run_command(self, script, **changes):
        return subprocess.run(
            [BASH, str(LAUNCHER), sys.executable, "-c", script], env=self.env(**changes), text=True, capture_output=True
        )

    def test_hosted_ci_bypasses_admission_and_worker_defaults(self):
        result = self.run_command(
            'import os; print(os.getenv("NEXTEST_TEST_THREADS")); exit(7)', GITHUB_ACTIONS="true", PATH=""
        )
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(result.stdout.strip(), "None")

    def test_nested_admission_preserves_failures_and_worker_override(self):
        result = self.run_command(
            'import os; print(os.environ["NEXTEST_TEST_THREADS"]); exit(9)',
            LOCAL_SUPERVISED="1",
            NEXTEST_TEST_THREADS="2",
            PATH="",
        )
        self.assertEqual(result.returncode, 9, result.stderr)
        self.assertEqual(result.stdout.strip(), "2")
        result = self.run_command('import os; print(os.environ["NEXTEST_TEST_THREADS"])', LOCAL_SUPERVISED="1", PATH="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLessEqual(int(result.stdout), 4)
        self.assertGreater(int(result.stdout), 0)

    def test_admission_precedes_launch_and_nested_calls_skip_it(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "bun").symlink_to(BUN)
            (Path(directory) / "bash").symlink_to(BASH)
            quest = Path(directory) / "pinned-quest"
            quest.write_text(
                f"#!{sys.executable}\n"
                "import os, sys\n"
                'assert sys.argv[1] == "gate"\n'
                'print("admitted", flush=True)\n'
                'os.environ["QUEST_GATE_ADMITTED"] = "1"\n'
                "os.execvp(sys.argv[2], sys.argv[2:])\n"
            )
            quest.chmod(0o755)
            script = (
                'import subprocess, sys; print("outer", flush=True); '
                f"sys.exit(subprocess.call([{BASH!r}, {str(LAUNCHER)!r}, "
                'sys.executable, "-c", "print(\'inner\'); exit(11)"]))'
            )
            result = self.run_command(script, PATH=directory, MOQ_QUEST=str(quest))
            self.assertEqual(result.returncode, 11, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ["admitted", "outer", "inner"])

    def test_pinned_review_preserves_arguments_cwd_and_failure_without_host_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            quest = root / "pinned-quest"
            quest.write_text(
                f"#!{sys.executable}\n"
                "import os, sys\n"
                'assert sys.argv[1:] == ["review", "--base", "origin/main", "--responses", "a file.json"]\n'
                f"assert os.getcwd() == {directory!r}\n"
                'print("pinned build failed", file=sys.stderr)\n'
                "sys.exit(7)\n"
            )
            quest.chmod(0o755)
            host = root / "quest"
            host.write_text(f"#!{sys.executable}\nraise RuntimeError('host quest must not run')\n")
            host.chmod(0o755)
            result = subprocess.run(
                [BASH, str(ROOT / "sh/quest.sh"), "review", "--base", "origin/main", "--responses", "a file.json"],
                cwd=directory,
                env=self.env(PATH=directory, MOQ_QUEST=str(quest)),
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(result.stderr.strip(), "pinned build failed")

    def test_outside_shell_resolves_the_checkout_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            nix = root / "nix"
            nix.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                f'assert sys.argv[1:] == ["run", "--no-update-lock-file", "{ROOT}#quest", "--", "review"]\n'
                'print("pinned input failed", file=sys.stderr)\n'
                "sys.exit(7)\n"
            )
            nix.chmod(0o755)
            result = subprocess.run(
                [BASH, str(ROOT / "sh/quest.sh"), "review"],
                env=self.env(PATH=directory),
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(result.stderr.strip(), "pinned input failed")

    def test_explicit_host_toolchain_reports_its_unverified_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bun").symlink_to(BUN)
            quest = root / "quest"
            quest.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                'if sys.argv[1:] == ["--version"]: print("quest host-test"); sys.exit(0)\n'
                'assert sys.argv[1:] == ["check"]\n'
                "sys.exit(9)\n"
            )
            quest.chmod(0o755)
            result = subprocess.run(
                [BASH, str(ROOT / "sh/quest.sh"), "check"],
                env=self.env(PATH=directory, ALLOW_HOST_TOOLCHAIN="1"),
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 9, result.stderr)
            self.assertIn(f"quest host-test at {quest}", result.stderr)
            pin = json.loads((ROOT / "flake.lock").read_text())["nodes"]["quest"]["locked"]["rev"]
            self.assertIn(f"repository pin: {pin} (build identity unverified)", result.stderr)

    def test_external_admission_keeps_descendant_supervision(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "bun").symlink_to(BUN)
            result = self.run_command(
                'import os; print(os.environ["LOCAL_SUPERVISED"])', QUEST_GATE_ADMITTED="1", PATH=directory
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "1")

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_cancellation_reaches_descendants_and_waits_for_cleanup(self):
        # The grandchild inherits stdout; EOF proves no descendant holds it open.
        script = """import signal, subprocess, sys
child = subprocess.Popen([sys.executable, "-c", "import signal; print('ready', flush=True); signal.pause()"])
signal.pause()
"""
        process = subprocess.Popen(
            [BUN, "--no-orphans", str(SUPERVISOR), sys.executable, "-c", script],
            env=self.env(QUEST_GATE_ADMITTED="1"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertEqual(process.stdout.readline().strip(), "ready")
            process.send_signal(signal.SIGTERM)
            output, error = process.communicate()
            self.assertEqual(process.returncode, 128 + signal.SIGTERM, error)
            self.assertEqual(output.strip(), "")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    @unittest.skipUnless(sys.platform == "linux", "adopting orphans requires Linux")
    def test_exit_reaches_descendants_orphaned_by_their_parent(self):
        # The child exits first, so its grandchild reparents before the supervisor exits.
        script = """import subprocess, sys
subprocess.Popen([sys.executable, "-c", "import signal; signal.pause()"])
print("ready", flush=True)
sys.exit(5)
"""
        process = subprocess.Popen(
            [BUN, "--no-orphans", str(SUPERVISOR), sys.executable, "-c", script],
            env=self.env(QUEST_GATE_ADMITTED="1"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            output, error = process.communicate(timeout=30)
            self.assertEqual(process.returncode, 5, error)
            self.assertEqual(output.strip(), "ready")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()


if __name__ == "__main__":
    unittest.main()
