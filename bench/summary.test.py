#!/usr/bin/env python3
"""Regressions for CPU/load window alignment."""

import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("summary", Path(__file__).with_name("summary.py"))
summary_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary_module)


class Windows(unittest.TestCase):
    def test_host_uses_load_window(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            host = root / "host.jsonl"
            records = [
                dict(
                    timestamp_ms=t,
                    pid=7,
                    cpu_user=t / 1000,
                    cpu_system=0,
                    ctx_voluntary=t,
                    ctx_involuntary=0,
                    rss_bytes=1,
                    threads=1,
                    per_thread=[
                        dict(
                            tid=8,
                            name="moq-worker-0",
                            cpu_user=t / 1000,
                            cpu_system=0,
                            ctx_voluntary=t,
                            ctx_involuntary=0,
                        )
                    ],
                )
                for t in (0, 1000, 2000, 3000)
            ]
            host.write_text("".join(json.dumps(row) + "\n" for row in records))
            summary = root / "summary.json"
            summary.write_text(
                json.dumps(
                    dict(throughput_window_start_ms=500, throughput_window_end_ms=2500, recv_fps=2000, recv_mbps=500)
                )
            )
            subprocess.run(
                ["bash", "-c", 'source bench/relay.sh; add_host_summary "$1" "$2" 7', "test", str(host), str(summary)],
                check=True,
            )
            result = json.loads(summary.read_text())
            self.assertEqual(result["host_window_start_ms"], 500)
            self.assertEqual(result["host_window_end_ms"], 2500)
            self.assertEqual(result["cpu_cores"], 1)
            self.assertEqual(result["cpu_us_per_frame"], 500)
            self.assertEqual(result["cpu_cores_per_gbps"], 2)
            self.assertEqual(result["thread_cpu"], [dict(tid=8, name="moq-worker-0", cpu_cores=1)])

    def test_udp_counts_whole_drops_across_the_bracketing_samples(self):
        rows = [
            dict(
                timestamp_ms=t,
                pid=1,
                cpu_user=t / 1000,
                cpu_system=0,
                ctx_voluntary=0,
                ctx_involuntary=0,
                rss_bytes=1,
                threads=1,
                udp_sockets=2,
                udp_rcvbuf_min=rcvbuf,
                udp_sndbuf_min=1,
                udp_rx_queue_bytes=queued,
                udp_drops=drops,
                udp_rcvbuf_errors=drops + 5,
                udp_sndbuf_errors=0,
                udp_in_errors=drops + 5,
            )
            for t, drops, queued, rcvbuf in ((0, 1, 0, 9), (1000, 4, 70, 8), (2000, 10, 30, 8), (3000, 11, 0, 8))
        ]
        result = summary_module.cpu(rows, 500, 1500)
        # The samples at 0 and 2000 bracket the window: 10 - 1, not an interpolated 4.5.
        self.assertEqual(result["udp_drops"], 9)
        self.assertEqual(result["udp_rcvbuf_errors"], 9)
        self.assertEqual(result["udp_rx_queue_peak_bytes"], 70)
        self.assertEqual(result["udp_rcvbuf_min"], 8)
        # A sampler without UDP accounting adds nothing.
        plain = [{key: value for key, value in row.items() if not key.startswith("udp_")} for row in rows]
        self.assertNotIn("udp_drops", summary_module.cpu(plain, 500, 1500))
        rows[2]["udp_drops"] = 0
        with self.assertRaises(ValueError):
            summary_module.cpu(rows, 500, 1500)

    def test_refuses_unbracketed_window_and_backward_counters(self):
        rows = [
            dict(timestamp_ms=t, pid=1, cpu_user=t, cpu_system=0, ctx_voluntary=0, ctx_involuntary=0)
            for t in (100, 200)
        ]
        with self.assertRaises(ValueError):
            summary_module.at(rows, 99)
        with self.assertRaises(ValueError):
            summary_module.at(rows, 201)
        rows[0]["cpu_user"] = float("nan")
        with self.assertRaises(ValueError):
            summary_module.processes(rows)
        rows[0]["cpu_user"] = 100
        rows[1]["cpu_user"] = 0
        with self.assertRaises(ValueError):
            summary_module.processes(rows)


unittest.main()
