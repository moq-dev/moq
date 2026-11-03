"""Admission must preserve failures and own the entire admitted command tree."""

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
        for key in ("QUEST_GATE_ADMITTED", "LOCAL_SUPERVISED", "GITHUB_ACTIONS", "NEXTEST_TEST_THREADS"):
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
            quest = Path(directory) / "quest"
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
            result = self.run_command(script, PATH=directory)
            self.assertEqual(result.returncode, 11, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ["admitted", "outer", "inner"])

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
