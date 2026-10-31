#!/usr/bin/env python3
"""Exercise sampler-owned generator lifetime with deterministic fake counters."""
import os
import random
import signal
import socket
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class Loss(unittest.TestCase):
    def test_confirmed_loss_survives_reordering_recovery(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stats, summary = root / 'stats.jsonl', root / 'summary.json'
            rows = [dict(timestamp_ms=t, bytes_sent=t, bytes_recv=t,
                         frames_sent=t, frames_recv=t, groups_expected=100,
                         groups_present=present, groups_lost=lost,
                         latency_p50_ms=1, latency_p99_ms=2)
                    for t, present, lost in [(1000, 90, 2), (6000, 99, 3)]]
            stats.write_text(''.join(json.dumps(row) + '\n' for row in rows))
            result = subprocess.run(['bash', '-c',
                'source "$1/bench/relay.sh"; summarize_load "$2" "$3" fixture',
                'test', str(ROOT), str(stats), str(summary)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(summary.read_text())
            self.assertEqual(report['loss_pct'], 3)


class Lifecycle(unittest.TestCase):
    def test_sampler_stops_generator_after_collecting_both_processes(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            os.mkfifo(run / 'ready')
            fake = run / 'process.py'
            fake.write_text('''#!/usr/bin/env python3
import argparse, json, os, pathlib, signal, sys
p = argparse.ArgumentParser()
p.add_argument('--pid', action='append', type=int, default=[])
p.add_argument('--output', type=pathlib.Path, required=True)
a, rest = p.parse_known_args()
if not a.pid:
    assert '--duration' not in rest
    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
    a.output.write_text(''.join(json.dumps(dict(timestamp_ms=t, bytes_sent=t*10, bytes_recv=t*10,
        frames_sent=t, frames_recv=t, groups_expected=1, groups_present=1, groups_lost=0,
        latency_p50_ms=1, latency_p99_ms=1))+'\\n' for t in range(500,10000,500)))
    with open(os.environ['READY'], 'w') as ready:
        ready.write('ready')
    while True: signal.pause()
else:
    assert '--threads' in rest
    with open(os.environ['READY']) as ready: assert ready.read() == 'ready'
    a.output.write_text(''.join(json.dumps(dict(timestamp_ms=t, pid=pid, cpu_user=t/1000,
        cpu_system=0, ctx_voluntary=t, ctx_involuntary=0, rss_bytes=1, threads=1,
        per_thread=[]))+'\\n' for t in range(0,10500,500) for pid in a.pid))
''')
            fake.chmod(0o755)
            script = '''set -euo pipefail
ROOT=$1 RUN=$2 LOAD_BIN=$3 HOST_BIN=$3
RELAY_PID= HOST_PID= LOAD_PID=
trap 'for pid in "$LOAD_PID" "$HOST_PID"; do if [[ -n $pid ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi; done' EXIT
source "$ROOT/bench/relay.sh"
start_relay() { RELAY_PID=$$; RELAY_PORT=4500; }
stop_relay() { RELAY_PID=; }
run_workload fixture ignored video
'''
            result = subprocess.run(['bash', '-c', script, 'test', str(ROOT), str(run), str(fake)],
                                    env=dict(os.environ, READY=str(run / 'ready')), capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            import json
            result = json.loads((run / 'relay/fixture/video/summary.json').read_text())
            self.assertEqual(result['cpu_cores'], 1)
            self.assertEqual(result['generator']['cpu_cores'], 1)
            self.assertEqual(result['host_window_start_ms'], result['throughput_window_start_ms'])


@unittest.skipUnless(sys.platform == 'linux', 'relay process metrics require Linux')
class PortOwnership(unittest.TestCase):
    def run_shell(self, script, directory, relay, **env):
        process = subprocess.Popen(['bash', '-c', script, 'test', str(ROOT), str(directory), str(relay)],
                                   env=dict(os.environ, **env), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=15)
            return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
        finally:
            if process.poll() is None:
                # This session contains only the fixture shell and its own children.
                os.killpg(process.pid, signal.SIGTERM)
                process.wait()

    def test_a_foreign_listener_never_proves_relay_readiness(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            holder = run / 'hold.py'
            holder.write_text('''#!/usr/bin/env python3
import signal, socket, sys
prefix, port = sys.argv[1], int(sys.argv[2])
try:
    tcp = socket.socket()
    tcp.bind(('127.0.0.1', port))
    tcp.listen()
    udp = socket.socket(type=socket.SOCK_DGRAM)
    udp.bind(('127.0.0.1', port))
except OSError as error:
    # Another owner already holds the port, which is just as foreign: report it and release the relay.
    with open(prefix + '.ready', 'w') as ready: ready.write(f'busy: {error}\\n')
    with open(prefix + '.release', 'w') as release: release.write('release\\n')
    sys.exit()
with open(prefix + '.ready', 'w') as ready: ready.write('ready\\n')
connection, _ = tcp.accept()
connection.close()
with open(prefix + '.release', 'w') as release: release.write('release\\n')
while True: signal.pause()
''')
            relay = run / 'relay.py'
            relay.write_text('''#!/usr/bin/env python3
import sys
# Stay alive until the harness probes the foreign listener, making the race deterministic.
with open(sys.argv[1] + '.release') as release: release.read()
sys.exit('Address already in use: fixture collision')
''')
            relay.chmod(0o755)
            script = '''set -euo pipefail
ROOT=$1 RUN=$2 RELAY_PID=
HOLDERS=()
trap 'for pid in "${HOLDERS[@]}" "$RELAY_PID"; do if [[ -n $pid ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi; done' EXIT
source "$ROOT/bench/relay.sh"
RANDOM=$SEED
relay_config() {
    : > "$1"
    mkfifo "$1.ready" "$1.release"
    python3 "$RUN/hold.py" "$1" "$2" &
    HOLDERS+=("$!")
    IFS= read -r ready < "$1.ready"
    printf '%s\\n' "$ready" >> "$RUN/holds"
    # Reseeding redraws this port, so once a holder owns one every later attempt collides with it.
    [[ $ready == ready ]] || SEED=$((SEED + 1))
    RANDOM=$SEED
}
start_relay "$3" "$RUN"
'''
            result = self.run_shell(script, run, relay, SEED=str(random.randrange(32768)))
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn('relay failed to start', result.stderr)
            self.assertIn('Address already in use: fixture collision', result.stderr)
            holds = (run / 'holds').read_text().splitlines()
            self.assertIn('ready', holds)
            self.assertTrue(any(hold.startswith('busy') for hold in holds), holds)

    def test_startup_requires_tcp_and_udp_owned_by_the_relay(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            relay = run / 'relay.py'
            relay.write_text('''#!/usr/bin/env python3
import signal, socket, sys, tomllib
config = tomllib.loads(open(sys.argv[1]).read())
port = int(config['listen']['bind'].rsplit(':', 1)[1])
tcp = socket.socket()
tcp.bind(('127.0.0.1', port))
tcp.listen()
udp = socket.socket(type=socket.SOCK_DGRAM)
udp.bind(('127.0.0.1', port))
while True: signal.pause()
''')
            relay.chmod(0o755)
            script = '''set -euo pipefail
ROOT=$1 RUN=$2 RELAY_PID=
trap 'if [[ -n $RELAY_PID ]]; then kill "$RELAY_PID" 2>/dev/null || true; wait "$RELAY_PID" 2>/dev/null || true; fi' EXIT
source "$ROOT/bench/relay.sh"
start_relay "$3" "$RUN"
relay_owns_port "$RELAY_PID" "$RELAY_PORT"
stop_relay
'''
            result = self.run_shell(script, run, relay)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_tcp_without_udp_is_not_owned_as_a_relay_port(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as tcp:
            tcp.bind(('127.0.0.1', 0))
            tcp.listen()
            script = '''set -euo pipefail
ROOT=$1 RUN=$2
source "$ROOT/bench/relay.sh"
relay_owns_port "$OWNER_PID" "$PORT"
'''
            result = self.run_shell(script, Path(directory), 'unused',
                                    OWNER_PID=str(os.getpid()), PORT=str(tcp.getsockname()[1]))
            self.assertEqual(result.returncode, 1, result.stderr)

    def test_a_stream_transport_needs_its_own_listener(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as tcp, \
                socket.socket(type=socket.SOCK_DGRAM) as udp:
            tcp.bind(('127.0.0.1', 0))
            tcp.listen()
            port = tcp.getsockname()[1]
            udp.bind(('127.0.0.1', port))
            script = '''set -euo pipefail
ROOT=$1 RUN=$2
source "$ROOT/bench/relay.sh"
TRANSPORT=ws relay_owns_port "$OWNER_PID" "$PORT"
! TRANSPORT=tls relay_owns_port "$OWNER_PID" "$PORT"
'''
            result = self.run_shell(script, Path(directory), 'unused', OWNER_PID=str(os.getpid()), PORT=str(port))
            self.assertEqual(result.returncode, 0, result.stderr)


unittest.main()
