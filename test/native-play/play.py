"""Observe native replacement and fallback on the private sink from test/lib/pulse.py."""

import argparse
import array
from contextlib import ExitStack
import json
import math
import os
from pathlib import Path
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
    root = Path(os.environ["MOQ_TEST_RUN"])
    with ExitStack() as resources:
        children = []
        resources.callback(lambda: [stop(child) for child in reversed(children)])

        def spawn(name, command, **kwargs):
            log = resources.enter_context((root / f"{name}.log").open("wb"))
            child = subprocess.Popen(command, stderr=log, **kwargs)
            children.append(child)
            return child

        def publisher(name, hz):
            encoded = spawn(name + "-encoder", [
                "ffmpeg", "-hide_banner", "-v", "error", "-re", "-f", "lavfi",
                "-i", f"sine=frequency={hz}:sample_rate={RATE}", "-ac", str(CHANNELS),
                "-c:a", "libopus", "-b:a", "128k", "-f", "mp4", "-movflags",
                "cmaf+separate_moof+delay_moov+skip_trailer", "-frag_duration", "1000", "-",
            ], stdout=subprocess.PIPE)
            assert encoded.stdout is not None
            imported = spawn(name + "-publisher", [
                args.moq, "--connect", args.url, "--connect-version", VERSIONS[0],
                "--broadcast", "tone.hang", "import", "fmp4",
            ], stdin=encoded.stdout, stdout=subprocess.DEVNULL)
            encoded.stdout.close()
            encoded.stdout = None
            return encoded, imported

        monitor = spawn("monitor", [
            "parec", "--raw", "--format=float32le", f"--rate={RATE}",
            f"--channels={CHANNELS}", "--latency-msec=20", "--device=moq-null.monitor",
        ], stdout=subprocess.PIPE)
        assert monitor.stdout is not None
        output = resources.enter_context(selectors.DefaultSelector())
        output.register(monitor.stdout, selectors.EVENT_READ)

        for version in VERSIONS:
            first = publisher(version + "-first", 440)
            player = spawn(version + "-player", [
                args.moq, "--connect", args.url, "--connect-version", version,
                "--broadcast", "tone.hang", "play", "--no-video", "--delay", "50ms",
            ], stdout=subprocess.DEVNULL)
            pending = bytearray()

            def hear(hz, phase):
                # The existing tone judge's window proves decoded content identity. The
                # deadline is a hang guard, not a route-switch performance allowance.
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    if player.poll() is not None:
                        raise RuntimeError(f"{version}: player exited during {phase}: {player.returncode}")
                    if any(child.poll() is not None for child in first):
                        raise RuntimeError(f"{version}: incumbent stopped during {phase}")
                    if not output.select(max(0, min(1, deadline - time.monotonic()))):
                        continue
                    chunk = os.read(monitor.stdout.fileno(), 65536)
                    if not chunk:
                        raise RuntimeError("private device monitor exited")
                    pending.extend(chunk)
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
                            (root / f"{version}-{phase}.f32le").write_bytes(pcm)
                            return {"toneHz": hz, "shares": shares,
                                    "rms": rms}
                raise RuntimeError(f"{version}: no {hz}Hz decoded during {phase}; see player log")

            hear(440, "initial")
            started = time.monotonic()
            second = publisher(version + "-replacement", 880)
            replaced = hear(880, "replacement")
            replacement_ms = (time.monotonic() - started) * 1000
            # Closing only B must reveal the still-live A, without restarting the player.
            started = time.monotonic()
            stop(second[1])
            stop(second[0])
            fallback = hear(440, "fallback")
            fallback_ms = (time.monotonic() - started) * 1000
            print(json.dumps({"version": version, "replacementMs": replacement_ms,
                              "fallbackMs": fallback_ms, "judgeWindowMs": WINDOW_MS,
                              "playbackDelayMs": 50, "replacement": replaced, "fallback": fallback,
                              "backend": "private PulseAudio null sink via production ALSA/cpal"}), flush=True)
            player.send_signal(signal.SIGINT)
            if player.wait(timeout=10) != 0:
                raise RuntimeError(f"{version}: player did not shut down cleanly on Ctrl-C")
            for child in reversed(first):
                stop(child)


if __name__ == "__main__":
    run()
