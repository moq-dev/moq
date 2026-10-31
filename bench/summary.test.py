#!/usr/bin/env python3
"""Regressions for CPU/load window alignment."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('summary', Path(__file__).with_name('summary.py'))
summary_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary_module)


class Windows(unittest.TestCase):
    def test_host_uses_load_window(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host = root / 'host.jsonl'
            records = [dict(timestamp_ms=t, pid=7, cpu_user=t / 1000, cpu_system=0,
                            ctx_voluntary=t, ctx_involuntary=0, rss_bytes=1, threads=1,
                            per_thread=[dict(tid=8, name='moq-worker-0', cpu_user=t / 1000, cpu_system=0, ctx_voluntary=t, ctx_involuntary=0)]) for t in (0, 1000, 2000, 3000)]
            host.write_text(''.join(json.dumps(row) + '\n' for row in records))
            summary = root / 'summary.json'
            summary.write_text(json.dumps(dict(throughput_window_start_ms=500,
                                               throughput_window_end_ms=2500, recv_fps=2000, recv_mbps=500)))
            subprocess.run(['bash', '-c', 'source bench/relay.sh; add_host_summary "$1" "$2" 7',
                            'test', str(host), str(summary)], check=True)
            result = json.loads(summary.read_text())
            self.assertEqual(result['host_window_start_ms'], 500)
            self.assertEqual(result['host_window_end_ms'], 2500)
            self.assertEqual(result['cpu_cores'], 1)
            self.assertEqual(result['cpu_us_per_frame'], 500)
            self.assertEqual(result['cpu_cores_per_gbps'], 2)
            self.assertEqual(result['thread_cpu'], [dict(tid=8, name='moq-worker-0', cpu_cores=1)])

    def test_refuses_unbracketed_window_and_backward_counters(self):
        rows = [dict(timestamp_ms=t, pid=1, cpu_user=t, cpu_system=0,
                     ctx_voluntary=0, ctx_involuntary=0) for t in (100, 200)]
        with self.assertRaises(ValueError):
            summary_module.at(rows, 99)
        with self.assertRaises(ValueError):
            summary_module.at(rows, 201)
        rows[0]['cpu_user'] = float('nan')
        with self.assertRaises(ValueError):
            summary_module.processes(rows)
        rows[0]['cpu_user'] = 100
        rows[1]['cpu_user'] = 0
        with self.assertRaises(ValueError):
            summary_module.processes(rows)


unittest.main()
