"""Observe native replacement and fallback on the private sink from test/lib/pulse.py."""

import argparse
import array
from contextlib import ExitStack
from datetime import datetime
import json
import math
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "interop"))
from tone import MIN_RMS, MIN_SHARE, WINDOW_MS, goertzel_share

RATE = 48000
CHANNELS = 2
WINDOW_BYTES = RATE * CHANNELS * 4 * WINDOW_MS // 1000
VERSIONS = ("moq-lite-07-wip", "moq-lite-06", "moq-transport-22")
DELAY_MS = 50
MONITOR_LATENCY_MS = 20
# How long the player may take to sound a replacement once it is connected, or a fallback once
# the replacement exits. Routing: the relay's and the player's origins may each hold a route
# change for moq-net's 300 ms `origin::DEFAULT_UPDATE_HOLD`, and the announcement plus the
# catalog and track subscriptions take five loopback round trips at QUIC's 1 ms timer
# granularity. Presentation: one 20 ms Opus frame and the player's delay. Observation: the
# monitor's latency and two judge windows, one of which may straddle the switch.
SWITCH_S = (2 * 300 + 5 + 20 + DELAY_MS + MONITOR_LATENCY_MS + 2 * WINDOW_MS) / 1000
# Cold start and setup are not part of the switch; this only stops a hang.
SETUP_S = 30
# A tracing line's leading UTC timestamp, past any color codes, on a line logging the connection.
CONNECTED = re.compile(rb"^(?:\x1b\[[0-9;]*m)*(\S+Z)\b.*\bconnected\b", re.MULTILINE)


def stop(child):
    if child.poll() is None:
        child.terminate()
    try:
        child.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        child.kill()
        child.communicate()


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--moq", required=True)
    args = parser.parse_args()
    # pulse.py gives this process tree its own server, without touching the host's.
    if not os.environ.get("PULSE_SERVER"):
        raise RuntimeError("run under test/lib/pulse.py")
    root = Path(os.environ["HARNESS_RUN_DIR"])
    with ExitStack() as resources:
        children = []
        resources.callback(lambda: [stop(child) for child in reversed(children)])

        def spawn(name, command, **kwargs):
            log = resources.enter_context((root / f"{name}.log").open("wb"))
            # Connection timestamps and device openings are part of the measurement.
            if command[0] == args.moq:
                kwargs["env"] = dict(os.environ, RUST_LOG="info")
            child = subprocess.Popen(command, stderr=log, **kwargs)
            children.append(child)
            return child

        monitor = spawn("monitor", [
            "parec", "--raw", "--format=float32le", f"--rate={RATE}", f"--channels={CHANNELS}",
            f"--latency-msec={MONITOR_LATENCY_MS}", "--device=moq-null.monitor",
        ], stdout=subprocess.PIPE)
        assert monitor.stdout is not None
        output = resources.enter_context(selectors.DefaultSelector())
        output.register(monitor.stdout, selectors.EVENT_READ)
        pending = bytearray()

        def drain(deadline, live):
            """Read the monitor until a chunk or the deadline, so the judge never lags the device."""
            for child, what in live:
                if child.poll() is not None:
                    raise RuntimeError(f"{what} exited: {child.returncode}")
            if output.select(max(0, min(0.1, deadline - time.monotonic()))):
                chunk = os.read(monitor.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError("private device monitor exited")
                pending.extend(chunk)

        def flush(live):
            """Discard what the monitor captured before now, so judge windows start here."""
            while output.select(0):
                drain(time.monotonic(), live)
            pending.clear()

        def publisher(name, hz, live):
            """Start a tone publisher; once it is connected with media to send, return its
            processes and the monotonic time its log stamped the connection."""
            deadline = time.monotonic() + SETUP_S
            encoded = spawn(name + "-encoder", [
                "ffmpeg", "-hide_banner", "-v", "error", "-re", "-f", "lavfi",
                "-i", f"sine=frequency={hz}:sample_rate={RATE}", "-ac", str(CHANNELS),
                "-c:a", "libopus", "-b:a", "128k", "-f", "mp4", "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer", "-frag_duration", "1000", "-",
            ], stdout=subprocess.PIPE)
            assert encoded.stdout is not None
            started = [(encoded, f"{name} encoder")]
            # Encoder startup is preparation, not routing: wait for its first output.
            with selectors.DefaultSelector() as encoder:
                encoder.register(encoded.stdout, selectors.EVENT_READ)
                while not encoder.select(0):
                    if time.monotonic() >= deadline:
                        raise RuntimeError(f"{name}: encoder produced nothing")
                    drain(deadline, live + started)
            log = root / f"{name}-publisher.log"
            imported = spawn(name + "-publisher", [
                args.moq, "--connect", args.url, "--connect-version", VERSIONS[0],
                "--broadcast", "tone.hang", "import", "fmp4",
            ], stdin=encoded.stdout, stdout=subprocess.DEVNULL)
            encoded.stdout.close()
            encoded.stdout = None
            started.append((imported, f"{name} publisher"))
            while not (connected := CONNECTED.search(log.read_bytes())):
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"{name}: publisher did not connect; see {log.name}")
                drain(deadline, live + started)
            # The log's own stamp, so the driver's polling never extends the switch budget.
            stamp = datetime.fromisoformat(connected[1].decode()).timestamp()
            return started, time.monotonic() - (time.time() - stamp)

        def hear(hz, phase, seconds, live):
            """Wait for a judge window of the tone, failing after `seconds`."""
            deadline = time.monotonic() + seconds
            rejected = []
            while time.monotonic() < deadline:
                drain(deadline, live)
                while len(pending) >= WINDOW_BYTES:
                    pcm = bytes(pending[:WINDOW_BYTES])
                    del pending[:WINDOW_BYTES]
                    samples = array.array("f", pcm)
                    if not all(math.isfinite(sample) for sample in samples):
                        raise RuntimeError("native playback emitted non-finite PCM")
                    shares = [goertzel_share(samples[channel::CHANNELS], RATE, hz)
                              for channel in range(CHANNELS)]
                    rms = math.sqrt(sum(x * x for x in samples) / len(samples))
                    if min(shares) >= MIN_SHARE and rms >= MIN_RMS:
                        (root / f"{phase}.f32le").write_bytes(pcm)
                        return {"toneHz": hz, "shares": shares, "rms": rms}
                    rejected.append({"share": round(min(shares), 3), "rms": round(rms, 3)})
            raise RuntimeError(f"{phase}: no {hz}Hz decoded within {seconds * 1000:.0f}ms; "
                               f"rejected windows {rejected}; see the player log")

        for version in VERSIONS:
            first, _ = publisher(version + "-first", 440, [])
            player = spawn(version + "-player", [
                args.moq, "--connect", args.url, "--connect-version", version,
                "--broadcast", "tone.hang", "play", "--video", "off", "--delay", f"{DELAY_MS}ms",
            ], stdout=subprocess.DEVNULL)
            live = first + [(player, f"{version} player")]
            hear(440, f"{version}-initial", SETUP_S, live)

            second, started = publisher(version + "-replacement", 880, live)
            flush(live + second)
            replaced = hear(880, f"{version}-replacement", started + SWITCH_S - time.monotonic(), live + second)
            replacement_ms = (time.monotonic() - started) * 1000

            # Closing only B must reveal the still-live A, without restarting the player.
            started = time.monotonic()
            second[1][0].terminate()
            flush(live)
            fallback = hear(440, f"{version}-fallback", started + SWITCH_S - time.monotonic(), live)
            fallback_ms = (time.monotonic() - started) * 1000
            for child, _ in reversed(second):
                stop(child)
            opens = (root / f"{version}-player.log").read_bytes().count(b"opened audio output")
            print(json.dumps({"version": version, "replacementMs": replacement_ms,
                              "fallbackMs": fallback_ms, "boundMs": SWITCH_S * 1000,
                              "outputOpens": opens,
                              "replacement": replaced, "fallback": fallback,
                              "backend": "private PulseAudio null sink via production ALSA/cpal"}), flush=True)
            player.send_signal(signal.SIGINT)
            if player.wait(timeout=10) != 0:
                raise RuntimeError(f"{version}: player did not shut down cleanly on Ctrl-C")
            for child, _ in reversed(first):
                stop(child)


if __name__ == "__main__":
    run()
