"""Run a command against a private PulseAudio server with one null sink, `moq-null`.

    python3 pulse.py --dir <run dir> -- <command> [args...]

The server, its socket, and its configuration live under --dir, and only the
command's process tree sees them: the desktop sound server and its settings are
untouched, and no physical device is opened. ALSA's default PCM routes to the
server, so the production ALSA/cpal backend plays into the null sink, which
clocks it in real time. Exits with the command's status.
"""

import argparse
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import threading
import time


def run():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dir", required=True, type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("no command to run")

    root = args.dir
    # One-letter names: a Unix socket path must fit sockaddr_un's 108 bytes, and run
    # directories under a CI runner's temp root already spend most of them.
    runtime = root / "p"
    socket = runtime / "s"
    if len(os.fsencode(socket.absolute())) >= 108:
        parser.error(f"socket path {socket.absolute()} is too long for a Unix socket; use a shorter --dir")
    runtime.mkdir(mode=0o700, parents=True)
    config_home = root / "config"
    config_home.mkdir(mode=0o700)
    config = root / "alsa.conf"
    config.write_text(f'pcm.!default {{ type pulse server "unix:{socket}" }}\n')
    env = os.environ | {
        "XDG_RUNTIME_DIR": str(runtime),
        "XDG_CONFIG_HOME": str(config_home),
        "PULSE_COOKIE": str(runtime / "cookie"),
        "PULSE_RUNTIME_PATH": str(runtime),
        "PULSE_SERVER": f"unix:{socket}",
        "ALSA_CONFIG_PATH": str(config),
        "DISPLAY": "",
        "WAYLAND_DISPLAY": "",
    }

    # By default the null sink renders 2 s ahead and sleeps through it, and a new stream does not
    # rewind that, so a stream's first callback waits up to 2 s: Firefox reports its AudioContext
    # suspended until then. `norewinds` caps the block at 50 ms, which starts streams in tens of
    # milliseconds, as a real device does.
    pulse = subprocess.Popen(
        [
            "pulseaudio", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1",
            "--log-level=info", "--log-target=stderr", "-n",
            f"--load=module-native-protocol-unix socket={socket} auth-anonymous=1",
            "--load=module-null-sink sink_name=moq-null rate=48000 channels=2 norewinds=1",
        ], env=env, stderr=subprocess.PIPE,
    )
    try:
        assert pulse.stderr is not None
        log = (root / "pulse.log").open("wb")
        with selectors.DefaultSelector() as ready:
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
        # The rest of its log is drained into the file, so a full pipe never stalls it.
        forward = threading.Thread(target=shutil.copyfileobj, args=(pulse.stderr, log), daemon=True)
        forward.start()
        try:
            return subprocess.run(command, env=env).returncode
        finally:
            pulse.terminate()
            try:
                pulse.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass  # the outer finally kills it; the command's status stands
            forward.join(timeout=5)
    finally:
        if pulse.poll() is None:
            pulse.kill()
            pulse.wait()


if __name__ == "__main__":
    sys.exit(run())
