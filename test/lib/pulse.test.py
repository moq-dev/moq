"""Private audio servers start together and play on their own sinks under one session bus."""

import array
import json
import math
import os
import signal
import subprocess
import sys
import tempfile
import unittest
import wave
from contextlib import ExitStack
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def play(tone):
    assert os.environ["DBUS_SESSION_BUS_ADDRESS"]
    info = json.loads(subprocess.check_output(["pactl", "--format=json", "info"]))
    assert info["server_string"] == os.environ["PULSE_SERVER"]
    assert info["default_sink_name"] == "moq-null"
    # Hold both daemons alive before either playback can finish and release its D-Bus name.
    print("ready", flush=True)
    assert sys.stdin.readline() == "play\n"
    subprocess.run(["paplay", "--device=moq-null", str(tone)], check=True)


def parallel(root):
    with ExitStack() as children:
        runs = [
            children.enter_context(
                subprocess.Popen(
                    [
                        sys.executable,
                        str(HERE / "pulse.py"),
                        "--dir",
                        str(root / name),
                        "--",
                        sys.executable,
                        str(Path(__file__).resolve()),
                        "--play",
                        str(root / "tone.wav"),
                    ],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                )
            )
            for name in ("one", "two")
        ]
        for child in runs:
            assert child.stdout.readline() == b"ready\n", "private server exited before playback"
        for child in runs:
            child.stdin.write(b"play\n")
            child.stdin.flush()
        for child in runs:
            output, _ = child.communicate()
            assert child.returncode == 0, output.decode()


@unittest.skipUnless(sys.platform == "linux", "private PulseAudio is Linux-only")
class Pulse(unittest.TestCase):
    def test_parallel_playback(self):
        scratch = ROOT / ".tmp"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch, prefix="pulse-") as directory:
            root = Path(directory)
            bus_config = root / "bus.conf"
            bus_config.write_text(
                f"<busconfig><type>session</type><listen>unix:path={root}/b</listen><auth>EXTERNAL</auth>"
                '<policy context="default"><allow own="*"/><allow send_destination="*"/>'
                '<allow receive_sender="*"/></policy></busconfig>'
            )
            with wave.open(str(root / "tone.wav"), "wb") as tone:
                tone.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
                samples = array.array("h", (int(8000 * math.sin(2 * math.pi * 440 * i / 48000)) for i in range(12000)))
                if sys.byteorder != "little":
                    samples.byteswap()
                tone.writeframes(samples.tobytes())
            # The shared bus is private to this test, never the desktop's session bus.
            with subprocess.Popen(
                [
                    "dbus-run-session",
                    f"--config-file={bus_config}",
                    "--",
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "--parallel",
                    str(root),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            ) as session:
                try:
                    output, _ = session.communicate(timeout=30)
                finally:
                    # Reap the whole test tree even if startup or a playback client hangs.
                    try:
                        os.killpg(session.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    session.wait()
            logs = "\n".join(f"{log}:\n{log.read_text()}" for log in root.glob("*/pulse.log"))
            self.assertEqual(session.returncode, 0, output.decode() + logs)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--play"]:
        play(Path(sys.argv[2]))
    elif sys.argv[1:2] == ["--parallel"]:
        parallel(Path(sys.argv[2]))
    else:
        unittest.main()
