"""Interpreter exit while the Rust runtime thread is mid-callback."""

import os
import pathlib
import subprocess
import sys


def test_exit_inside_callback():
    script = pathlib.Path(__file__).with_name("exit_inside_callback.py")
    # RUST_BACKTRACE slows the abort down enough for a clean exit code to race it,
    # so judge stderr as well.
    env = {**os.environ, "RUST_BACKTRACE": "0"}
    result = subprocess.run([sys.executable, str(script)], capture_output=True, text=True, env=env, timeout=60)

    assert result.returncode == 0, result.stderr
    assert "panic" not in result.stderr, result.stderr
    assert "received hello" in result.stdout
