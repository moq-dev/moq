#!/usr/bin/env python3
"""Exercise the tracked-text gate in a real Git repository."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
EM_DASH = "\u2014"


class TextTests(unittest.TestCase):
    def setUp(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / ".tmp")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "sh").mkdir()
        shutil.copy(ROOT / "sh/text.sh", self.repo / "sh/text.sh")
        self.env = dict(os.environ, LC_ALL="C", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "commit.gpgsign", "false")
        self.git("add", ".")
        self.git("commit", "-qm", "base")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env,
                              text=True, capture_output=True, check=True)

    def write(self, name, data):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data if isinstance(data, bytes) else data.encode())

    def check(self, cwd=None):
        return subprocess.run(["bash", str(self.repo / "sh/text.sh")], cwd=cwd or self.repo,
                              env=self.env, text=True, capture_output=True)

    def test_plain_punctuation_and_other_unicode_pass(self):
        self.write("text.md", "a - b, c: café (d).\n")
        self.git("add", ".")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_reports_committed_staged_and_worktree_text(self):
        self.write("old file.md", f"first line\nold {EM_DASH} text\n")
        self.git("add", ".")
        self.git("commit", "-qm", "old text")
        self.write("staged.ts", f"// staged {EM_DASH} text\n")
        self.git("add", ".")
        self.write("sh/text.sh", (self.repo / "sh/text.sh").read_text() + f"# worktree {EM_DASH} text\n")
        result = self.check(cwd=self.repo / "sh")
        self.assertEqual(result.returncode, 1, result.stderr)
        for location in ("old file.md:2:", "staged.ts:1:", "sh/text.sh:"):
            self.assertIn(location, result.stdout)
        self.assertIn("em dashes are forbidden", result.stderr)

    def test_binary_lockfiles_and_untracked_files_are_excluded(self):
        self.write("binary.bin", b"\0" + EM_DASH.encode())
        for name in ("Cargo.lock", "nested/bun.lock", "nested/pubspec.lock", "flake.lock"):
            self.write(name, EM_DASH)
        self.git("add", ".")
        self.write("untracked.md", EM_DASH)
        self.write(".gitignore", "ignored.md\n")
        self.write("ignored.md", EM_DASH)
        self.git("add", ".gitignore")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_hidden_generated_and_extensionless_text_are_checked(self):
        for name in (".github/workflow.yml", "AGENTS.md", "generated/output.ts", "clock.ts", "LICENSE"):
            with self.subTest(name=name):
                self.write(name, EM_DASH)
                self.git("add", name)
                result = self.check()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(f"{name}:1:", result.stdout)
                self.git("rm", "-f", name)

    def test_git_errors_fail(self):
        self.git("config", "grep.patternType", "invalid")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr)

    @unittest.skipIf(os.geteuid() == 0, "root reads files regardless of mode")
    def test_unreadable_tracked_file_fails(self):
        self.write("unreadable.md", "plain text\n")
        self.git("add", ".")
        path = self.repo / "unreadable.md"
        path.chmod(0)
        self.addCleanup(path.chmod, 0o644)
        result = self.check()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("unreadable.md", result.stderr)
        self.assertIn("could not scan", result.stderr)


if __name__ == "__main__":
    unittest.main()
