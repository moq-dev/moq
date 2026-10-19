"""Observe real native playback on the private null sink that test/lib/pulse.py runs."""

import argparse
import array
import json
import math
import os
from pathlib import Path
import selectors
import signal
import subprocess
import time


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--moq", required=True)
    args = parser.parse_args()
    # pulse.py has pointed this process tree at a private server; nothing here reaches the host's.
    if not os.environ.get("PULSE_SERVER"):
        raise RuntimeError("run under test/lib/pulse.py")
    root = Path(os.environ["MOQ_TEST_RUN"])
    children = []
    try:
        with (root / "player.log").open("wb") as player_log, (root / "monitor.log").open("wb") as monitor_log:
            monitor = subprocess.Popen(
                ["parec", "--raw", "--format=float32le", "--rate=48000", "--channels=2",
                 "--latency-msec=20", "--device=moq-null.monitor"],
                stdout=subprocess.PIPE, stderr=monitor_log,
            )
            children.append(monitor)
            player = subprocess.Popen(
                [args.moq, "--connect", args.url, "--broadcast", "tone.hang", "play", "--no-video"],
                stdout=player_log, stderr=player_log,
            )
            children.append(player)
            assert monitor.stdout is not None
            capture = bytearray()
            pending = bytearray()
            started = False
            deadline = time.monotonic() + 30
            with selectors.DefaultSelector() as output:
                output.register(monitor.stdout, selectors.EVENT_READ)
                # A second of observed PCM proves the real speaker callback is running.
                while len(capture) < 48000 * 2 * 4:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("no audible native playback on the private device")
                    if player.poll() is not None:
                        raise RuntimeError(f"native player exited: {player.returncode}; see player.log")
                    if not output.select(max(0, min(1, deadline - time.monotonic()))):
                        if time.monotonic() >= deadline:
                            raise RuntimeError("no audible native playback on the private device")
                        continue
                    chunk = os.read(monitor.stdout.fileno(), 65536)
                    if not chunk:
                        raise RuntimeError("private device monitor exited")
                    pending.extend(chunk)
                    end = len(pending) // 8 * 8
                    aligned = bytes(pending[:end])
                    del pending[:end]
                    samples = array.array("f", aligned)
                    if not all(math.isfinite(sample) for sample in samples):
                        raise RuntimeError("native playback emitted non-finite PCM")
                    started |= any(sample != 0 for sample in samples)
                    if started:
                        capture.extend(aligned)
            (root / "output.f32le").write_bytes(capture)
            samples = array.array("f", capture)
            print(json.dumps({"frames": len(samples) // 2,
                              "rms": math.sqrt(sum(x * x for x in samples) / len(samples)),
                              "backend": "private PulseAudio null sink via production ALSA/cpal"}))
            player.send_signal(signal.SIGINT)
            if player.wait(timeout=10) != 0:
                raise RuntimeError("native player did not shut down cleanly on Ctrl-C")
    finally:
        for child in reversed(children):
            if child.poll() is None:
                child.terminate()
            try:
                child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.communicate()


if __name__ == "__main__":
    run()
