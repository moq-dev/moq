#!/usr/bin/env python3
"""A TLC run's metadir sweep removes only the metadirs no live run holds."""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("tlc", ROOT / "sh/tlc.py")
tlc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tlc)


class Claim(unittest.TestCase):
    def setUp(self):
        scratch = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, scratch)
        self.base = scratch / "tlc"

    def test_keeps_a_live_run_and_removes_a_dead_one(self):
        live, fd = tlc.claim(self.base)
        # A run that died, however it died, left its metadir and an unheld lock.
        dead, dead_fd = tlc.claim(self.base)
        os.close(dead_fd)
        # A metadir from before locks, holding none.
        bare = self.base / "bare"
        bare.mkdir()

        mine, mine_fd = tlc.claim(self.base)
        self.addCleanup(os.close, fd)
        self.addCleanup(os.close, mine_fd)
        self.assertTrue(live.is_dir())
        self.assertTrue(mine.is_dir())
        self.assertFalse(dead.exists())
        self.assertFalse(bare.exists())

    def test_a_child_holding_the_lock_keeps_the_run_alive(self):
        meta, fd = tlc.claim(self.base)
        child = subprocess.Popen(
            [sys.executable, "-c", "import sys; sys.stdin.read()"], stdin=subprocess.PIPE, pass_fds=(fd,)
        )
        os.close(fd)
        try:
            tlc.claim(self.base)
            self.assertTrue(meta.is_dir(), "the child still holds the lock")
        finally:
            child.communicate(b"")
        tlc.claim(self.base)
        self.assertFalse(meta.exists(), "the child exited, so nothing holds it")


if __name__ == "__main__":
    unittest.main()
