#!/usr/bin/env python3
"""Check the binary size cap against real diffs in a scratch repository."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CAP = 500 * 1024


class LargeFilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        (self.repo / "sh").mkdir(parents=True)
        for script in ("large-files.sh", "changed.sh"):
            shutil.copy(ROOT / "sh" / script, self.repo / "sh" / script)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for command in ("bash", "git", "sort", "wc", "mktemp", "rm", "cp"):
            (self.bin / command).symlink_to(shutil.which(command))
        self.env = dict(os.environ, PATH=str(self.bin), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.env.pop("GITHUB_BASE_REF", None)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "commit.gpgsign", "false")
        self.binary("fixture.bin", CAP + 1000)
        self.allow("fixture.bin some reason")
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.repo, env=self.env, text=True, capture_output=True, check=True
        ).stdout.strip()

    def binary(self, path, size):
        (self.repo / path).write_bytes(b"\0" * size)

    def allow(self, *lines):
        (self.repo / "sh/large-files.allow").write_text("# comment\n\n" + "".join(f"{line}\n" for line in lines))

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "change")

    def check(self, base="origin/main"):
        return subprocess.run(
            ["bash", "sh/large-files.sh", base], cwd=self.repo, env=self.env, text=True, capture_output=True
        )

    def expect_pass(self, base="origin/main"):
        result = self.check(base)
        self.assertEqual(result.returncode, 0, result.stderr)

    def expect_fail(self, message, base="origin/main"):
        result = self.check(base)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(message, result.stderr)

    def test_added_binary_over_the_cap_fails(self):
        self.binary("image.png", CAP + 1)
        self.commit()
        self.expect_fail(f"image.png ({CAP + 1} bytes)")

    def test_untracked_binary_over_the_cap_fails(self):
        self.binary("image.png", CAP + 1)
        self.expect_fail("image.png")
        self.assertEqual(self.git("ls-files", "image.png"), "")

    def test_binary_at_the_cap_and_large_text_pass(self):
        self.binary("image.png", CAP)
        (self.repo / "generated.rs").write_text("a\n" * CAP)
        self.commit()
        self.expect_pass()

    def test_allowlisted_binary_passes(self):
        self.binary("image.png", CAP + 1)
        self.allow("fixture.bin some reason", "image.png another reason")
        self.commit()
        self.expect_pass()

    def test_growing_an_oversized_binary_fails_and_shrinking_passes(self):
        self.binary("legacy.bin", CAP + 1000)
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.binary("legacy.bin", CAP + 500)
        self.commit()
        self.expect_pass()
        self.binary("legacy.bin", CAP + 2000)
        self.commit()
        self.expect_fail("legacy.bin")

    def test_all_checks_every_binary(self):
        self.binary("legacy.bin", CAP + 1000)
        self.commit()
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.expect_pass()
        self.expect_fail("legacy.bin", base="--all")
        self.git("rm", "-q", "legacy.bin")
        self.expect_pass(base="--all")

    def test_allowlist_entries_need_a_reason_and_an_oversized_file(self):
        for line, message in (
            ("fixture.bin", "needs a reason"),
            ("missing.bin some reason", "missing.bin is not a file over the cap"),
        ):
            with self.subTest(line=line):
                self.allow(line)
                self.expect_fail(message)
        self.allow("fixture.bin some reason")
        self.binary("fixture.bin", CAP)
        self.expect_fail("fixture.bin is not a file over the cap")


if __name__ == "__main__":
    unittest.main()
