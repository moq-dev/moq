#!/usr/bin/env python3
"""Exercise sampler-owned generator lifetime with deterministic fake counters."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


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
        frames_sent=t, frames_recv=t, groups_expected=1, groups_present=1,
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


unittest.main()
