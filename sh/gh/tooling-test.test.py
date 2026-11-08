"""Slow fixture exclusions preserve default unittest discovery and fail on stale names."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class Selection(unittest.TestCase):
    def test_new_classes_and_methods_run_unless_explicitly_excluded(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
            script = Path(directory) / "fixture.py"
            script.write_text(
                "import unittest\n"
                "class Slow(unittest.TestCase):\n"
                "    def test_build(self): self.fail('slow fixture ran')\n"
                "class Mixed(unittest.TestCase):\n"
                "    def test_build(self): self.fail('slow method ran')\n"
                "    def test_new(self): print('new method ran')\n"
                "class New(unittest.TestCase):\n"
                "    def test_new(self): print('new class ran')\n"
                "unittest.main()\n"
            )
            command = [sys.executable, str(ROOT / "sh/gh/tooling-test.py"), str(script)]
            selected = subprocess.run(
                [*command, "--exclude", "Slow", "--exclude", "Mixed.test_build"], capture_output=True, text=True
            )
            self.assertEqual(selected.returncode, 0, selected.stdout + selected.stderr)
            self.assertIn("new method ran", selected.stdout)
            self.assertIn("new class ran", selected.stdout)
            self.assertIn("Ran 2 tests", selected.stderr)
            for exclusions in ([], ["--exclude", "Unknown"]):
                result = subprocess.run([*command, *exclusions], capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
            stale = subprocess.run(
                [*command, "--exclude", "Slow", "--exclude", "Mixed", "--exclude", "Unknown"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(stale.returncode, 2)
            self.assertIn("unknown exclusions: Unknown", stale.stderr)


if __name__ == "__main__":
    unittest.main()
