#!/usr/bin/env python3
"""Check the MIGRATE.md guard against real diffs in a scratch repository."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GUIDE = "# Migrating\n\n## 0.1 -> 0.2\n\nOld note.\n"


class MigrateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "sh").mkdir()
        (self.repo / "rs/pkg").mkdir(parents=True)
        for script in ("migrate.sh", "changed.sh"):
            shutil.copy(ROOT / "sh" / script, self.repo / "sh" / script)
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.env.pop("GITHUB_BASE_REF", None)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "commit.gpgsign", "false")
        self.write("MIGRATE.md", GUIDE)
        self.write("rs/pkg/MIGRATE.md", GUIDE)
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.git("switch", "-qc", "feature")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=self.env, capture_output=True, check=True)

    def write(self, path, text):
        (self.repo / path).write_text(text)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "change")

    def check(self):
        return subprocess.run(
            ["bash", "sh/migrate.sh", "origin/main"], cwd=self.repo, env=self.env, text=True, capture_output=True
        )

    def test_unrelated_change_passes(self):
        self.write("README.md", "hi\n")
        self.commit()
        self.assertEqual(self.check().returncode, 0)

    def test_note_outside_the_cut_fails(self):
        self.write("rs/pkg/MIGRATE.md", GUIDE + "\n### New break\n")
        self.write("MIGRATE.md", GUIDE.replace("Old", "Edited"))
        self.commit()
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("       MIGRATE.md\n", result.stderr)
        self.assertIn("rs/pkg/MIGRATE.md", result.stderr)
        self.assertIn("Impact section", result.stderr)

    def test_deleted_guide_passes(self):
        self.git("rm", "-q", "rs/pkg/MIGRATE.md")
        self.commit()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_uncommitted_edit_fails(self):
        self.write("rs/pkg/MIGRATE.md", GUIDE + "\nMore.\n")
        self.assertNotEqual(self.check().returncode, 0)

    def test_cut_adding_a_version_section_passes(self):
        self.write("rs/pkg/MIGRATE.md", GUIDE.replace("## 0.1", "## 0.2 -> 0.3\n\nNew note.\n\n## 0.1"))
        self.commit()
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
