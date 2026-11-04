#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import lock

LOCK = Path(__file__).with_name("lock.py")


class Lock(unittest.TestCase):
    def wait_behind(self, pid):
        """Lock a file for pid, hand the lock to a process that outlives this call, and start a waiter on it."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "lock"
        fd = os.open(path, os.O_RDWR | os.O_CREAT)
        lock.acquire(fd, pid, ["holder", "two words"])
        keeper = subprocess.Popen(["cat"], stdin=subprocess.PIPE, pass_fds=[fd])
        os.close(fd)
        waiter = subprocess.Popen(
            ["bash", "-c", 'exec 9<>"$0"; exec "$1" "$2" "$0" 9 $$ waiter', path, sys.executable, LOCK],
            stderr=subprocess.PIPE,
            text=True,
        )
        line = waiter.stderr.readline()
        self.assertIsNone(waiter.poll(), "a held lock must make the waiter wait")
        keeper.stdin.close()
        keeper.wait()
        _, rest = waiter.communicate()
        self.assertEqual(waiter.returncode, 0, rest)
        record = json.loads(path.read_text())
        self.assertEqual((record["pid"], record["command"]), (waiter.pid, ["waiter"]))
        return line

    def test_waiter_names_the_live_holder(self):
        line = self.wait_behind(os.getpid())
        self.assertIn(f"PID {os.getpid()}, started ", line)
        self.assertIn(f"in {os.getcwd()}: holder 'two words'", line)

    def test_lock_outlives_an_exited_holder_while_its_process_keeps_it(self):
        exited = subprocess.Popen(["true"])
        exited.wait()
        line = self.wait_behind(exited.pid)
        self.assertIn(f"PID {exited.pid} (exited; a process it started still holds the lock)", line)

    def test_unreadable_record(self):
        self.assertEqual(lock.describe(b""), "an unrecorded holder")
        self.assertEqual(lock.describe(b'{"pid": 1}'), "an unrecorded holder")
        record = {"pid": os.getpid(), "started": "now", "cwd": "/", "command": None}
        self.assertEqual(lock.describe(json.dumps(record).encode()), "an unrecorded holder")

    def test_refuses_a_link_in_place_of_the_lock(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        target = Path(directory.name) / "target"
        target.write_text("keep")
        link = Path(directory.name) / "lock"
        link.symlink_to(target)
        result = subprocess.run(
            ["bash", "-c", 'exec 9<>"$0"; exec "$1" "$2" "$0" 9 $$ waiter', link, sys.executable, LOCK],
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a regular file of its own", result.stderr)
        self.assertEqual(target.read_text(), "keep")


if __name__ == "__main__":
    unittest.main()
