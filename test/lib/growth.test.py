"""The growth judge fails a process whose memory climbs after warm-up and passes one that swings."""

import importlib.util
import io
import json
import random
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("growth", HERE / "growth.py")
growth = importlib.util.module_from_spec(spec)
spec.loader.exec_module(growth)

MIB = 1 << 20


def records(label, values, interval=5, counter=None):
    out = []
    for i, value in enumerate(values):
        record = {"label": label, "t": i * interval, "rss": value, "allocated": value, "resident": value}
        record["rchar"] = record["wchar"] = i * interval * 100_000 if counter is None else counter(i)
        out.append(record)
    return out


class Judge(unittest.TestCase):
    def judge(self, samples, *args):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "samples.jsonl"
            path.write_text("".join(json.dumps(r) + "\n" for r in samples))
            report = Path(tmp) / "report.json"
            err = io.StringIO()
            with redirect_stdout(io.StringIO()), redirect_stderr(err):
                status = growth.main(["judge", str(path), "--report", str(report), *args])
            return status, err.getvalue(), json.loads(report.read_text())

    def test_swinging_heap_passes(self):
        # In-flight data swinging by 8 MiB around a flat level, with outliers, is no slope.
        rng = random.Random(7)
        values = [40 * MIB + rng.choice([0, 8 * MIB]) + rng.randrange(MIB) for _ in range(120)]
        status, err, _ = self.judge(records("relay", values), "--max", "allocated=1024")
        self.assertEqual(status, 0, err)

    def test_slow_leak_under_swings_fails_naming_the_process(self):
        # 4 KiB/s under the same swings, the shape a per-group leak takes at 60 groups/s.
        rng = random.Random(7)
        values = [40 * MIB + i * 5 * 4096 + rng.choice([0, 8 * MIB]) + rng.randrange(MIB) for i in range(120)]
        samples = records("relay", values) + records("export", [30 * MIB] * 120)
        status, err, report = self.judge(samples, "--max", "allocated=1024")
        self.assertEqual(status, 1)
        self.assertIn("relay allocated grew", err)
        self.assertNotIn("export", err)
        self.assertAlmostEqual(report["processes"]["relay"]["allocated"]["slope"], 4096, delta=1024)

    def test_warmup_growth_is_ignored(self):
        values = [i * MIB for i in range(20)] + [20 * MIB] * 100
        status, err, _ = self.judge(records("import", values), "--warmup", "100", "--max", "rss=1024")
        self.assertEqual(status, 0, err)

    def test_stalled_chain_fails(self):
        # A flat heap proves nothing once the media stops moving.
        samples = records("export", [30 * MIB] * 40, counter=lambda i: min(i, 10) * 500_000)
        status, err, _ = self.judge(samples, "--warmup", "60", "--max", "allocated=1024", "--flow", "export=wchar")
        self.assertEqual(status, 1)
        self.assertIn("the chain stalled", err)

    def test_stall_late_in_the_run_fails(self):
        # Media moving for most of the window, then only the sampler's own trickle.
        samples = records("export", [30 * MIB] * 40, counter=lambda i: min(i, 30) * 500_000 + i * 4096)
        status, err, _ = self.judge(
            samples, "--warmup", "60", "--max", "allocated=1024", "--flow", "export=wchar", "--min-flow", "32768"
        )
        self.assertEqual(status, 1)
        self.assertIn("the chain stalled", err)

    def test_too_few_samples_after_warmup_is_an_error(self):
        with self.assertRaises(SystemExit):
            self.judge(records("relay", [MIB] * 12), "--warmup", "30", "--max", "allocated=1")


class HeapLines(unittest.TestCase):
    def test_reads_colored_tracing_fields(self):
        log = (
            "\x1b[2m2026-11-25T10:00:00Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mmoq_tokio::jemalloc\x1b[0m\x1b[2m:\x1b[0m "
            "jemalloc heap \x1b[3mallocated\x1b[0m\x1b[2m=\x1b[0m123 \x1b[3mresident\x1b[0m\x1b[2m=\x1b[0m456\n"
            "unrelated line\n"
            "jemalloc heap allocated=7 resident=8\n"
        )
        self.assertEqual(growth.heap_lines(log), [(123, 456), (7, 8)])

    def test_incremental_log_reading_handles_partial_lines(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "relay.log"
            log.write_text("boot\n")
            proc = growth.Process(f"relay=1={log}")
            self.assertEqual(proc._read_lines(), "boot\n")
            with log.open("a") as f:
                f.write("jemalloc heap alloc")
            self.assertEqual(proc._read_lines(), "")
            with log.open("a") as f:
                f.write("ated=100 resident=200\n")
            self.assertEqual(growth.heap_lines(proc._read_lines()), [(100, 200)])
            # Nothing new appended: nothing re-read.
            self.assertEqual(proc._read_lines(), "")


if __name__ == "__main__":
    sys.exit(unittest.main())
