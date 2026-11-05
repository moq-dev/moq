#!/usr/bin/env python3
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

import local

LOCAL = Path(__file__).with_name("local.py")


@unittest.skipUnless(sys.platform == "linux", "workers watch their coordinator through a pidfd on Linux only")
class Started(unittest.TestCase):
    def started(self, coordinator):
        """Run a worker's --started wrapper the way a gate does: not the coordinator's child, and sent no signal."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        pidfile = Path(directory.name) / "pid"
        stamp = Path(directory.name) / "stamp"
        wrapper = subprocess.Popen(
            [
                sys.executable,
                LOCAL,
                "--started",
                stamp,
                coordinator,
                "sh",
                "-c",
                'echo $$ > "$0"; exec sleep 60',
                pidfile,
            ],
            stderr=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(lambda: wrapper.poll() is None and (wrapper.kill(), wrapper.wait()))
        return wrapper, pidfile

    def test_worker_stops_when_its_coordinator_is_killed(self):
        coordinator = subprocess.Popen(["sleep", "60"])
        self.addCleanup(lambda: coordinator.poll() is None and (coordinator.kill(), coordinator.wait()))
        wrapper, pidfile = self.started(local.identity(coordinator.pid))
        deadline = time.monotonic() + 10
        while not pidfile.exists() or not pidfile.read_text().strip():
            self.assertLess(time.monotonic(), deadline, "the worker never started")
            time.sleep(0.05)
        worker = int(pidfile.read_text())
        coordinator.send_signal(signal.SIGKILL)
        coordinator.wait()
        wrapper.wait(timeout=10)
        self.assertNotEqual(wrapper.returncode, 0)
        with self.assertRaises(ProcessLookupError):
            os.kill(worker, 0)

    def test_worker_refuses_once_its_coordinator_is_gone(self):
        # A start time the PID's process doesn't have, as when the coordinator exited and its PID was reused.
        wrapper, pidfile = self.started(f"{os.getpid()}:0")
        _, err = wrapper.communicate(timeout=10)
        self.assertNotEqual(wrapper.returncode, 0)
        self.assertIn("coordinator holding the benchmark lock exited", err)
        self.assertFalse(pidfile.exists())


if __name__ == "__main__":
    unittest.main()
