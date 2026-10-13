"""Observe real native playback without connecting to the desktop sound server."""

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
    root = Path(os.environ["MOQ_TEST_RUN"])
    runtime = root / "pulse"
    runtime.mkdir(mode=0o700)
    socket = runtime / "native"
    config_home = root / "config"
    config_home.mkdir(mode=0o700)
    # Only this process tree sees these settings. No host configuration or device is used.
    env = os.environ | {
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_CONFIG_HOME": str(config_home),
        "PULSE_COOKIE": str(runtime / "cookie"),
        "PULSE_RUNTIME_PATH": str(runtime),
        "PULSE_SERVER": f"unix:{socket}",
        "DISPLAY": "",
        "WAYLAND_DISPLAY": "",
    }
    config = root / "alsa.conf"
    config.write_text(f'pcm.!default {{ type pulse server "unix:{socket}" }}\n')
    env["ALSA_CONFIG_PATH"] = str(config)
    children = []
    try:
        pulse = subprocess.Popen(
            [
                "pulseaudio", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1",
                "--log-level=info", "--log-target=stderr", "-n",
                f"--load=module-native-protocol-unix socket={socket} auth-anonymous=1",
                "--load=module-null-sink sink_name=moq-null rate=48000 channels=2",
            ], env=env, stderr=subprocess.PIPE,
        )
        children.append(pulse)
        assert pulse.stderr is not None
        with selectors.DefaultSelector() as ready, (root / "pulse.log").open("wb") as log:
            ready.register(pulse.stderr, selectors.EVENT_READ)
            deadline = time.monotonic() + 30
            pending = b""
            while b"Daemon startup complete" not in pending:
                if not ready.select(max(0, deadline - time.monotonic())):
                    raise RuntimeError("private audio server did not start")
                chunk = os.read(pulse.stderr.fileno(), 65536)
                if not chunk:
                    raise RuntimeError("private audio server exited during startup")
                log.write(chunk)
                pending = (pending + chunk)[-65536:]

        with (root / "player.log").open("wb") as player_log, (root / "monitor.log").open("wb") as monitor_log:
            monitor = subprocess.Popen(
                ["parec", "--raw", "--format=float32le", "--rate=48000", "--channels=2",
                 "--latency-msec=20", "--device=moq-null.monitor"],
                env=env, stdout=subprocess.PIPE, stderr=monitor_log,
            )
            children.append(monitor)
            player = subprocess.Popen(
                [args.moq, "--connect", args.url, "--broadcast", "tone.hang", "play", "--no-video"],
                env=env, stdout=player_log, stderr=player_log,
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
