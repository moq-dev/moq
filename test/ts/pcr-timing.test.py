import contextlib
import importlib.util
import io
import json
import pathlib
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("pcr_timing", pathlib.Path(__file__).with_name("pcr-timing.py"))
timing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timing)


class Schedule(unittest.TestCase):
    def test_unpadded_start_is_graded(self):
        scan = types.SimpleNamespace(
            pcr=[(index, i * 1_080_000) for i, index in enumerate([0, 10, 20, 120, 220, 320])],
            first_null=20, new_base=set(), pcr_pid=256, other_pcr_pids=set(),
        )
        args = types.SimpleNamespace(mux_rate=3_760_000, schedule_pct_min=99, schedule_tolerance_pct=1)
        result = timing.check_schedule(scan, args)
        self.assertFalse(result[2], result)
        self.assertEqual(result[4]["count"], 5)


class Evidence(unittest.TestCase):
    def test_live_report_replays_both_clock_rate_verdicts(self):
        for scale in [1.0, 1.001]:
            with self.subTest(scale=scale), tempfile.TemporaryDirectory() as directory:
                scan = timing.Scan()
                scan.pcr = [
                    (i * 266, round(i * 0.04 * 27_000_000), 1000.0 + i * 0.04 * scale, 256)
                    for i in range(3001)
                ]
                # A flagged clock jump must remain excluded when the report is replayed.
                for i in range(1500, len(scan.pcr)):
                    packet, ticks, arrival, pid = scan.pcr[i]
                    scan.pcr[i] = (packet, ticks + 27_000_000, arrival, pid)
                scan.new_base = {1500}
                scan.packets = scan.pcr[-1][0] + 1
                report = pathlib.Path(directory) / "timing.json"
                argv = ["pcr-timing.py", "--live", "--seconds", "120", "--report-json", str(report)]
                with patch.object(sys, "argv", argv), patch.object(timing, "scan_live", return_value=scan):
                    with contextlib.redirect_stdout(io.StringIO()):
                        status = timing.main()
                saved = json.loads(report.read_text())
                replay = types.SimpleNamespace(
                    pcr=saved["pcr_samples"], new_base=set(saved["new_base"]), wraps=saved["wraps"],
                )
                args = types.SimpleNamespace(**saved["parameters"])
                results = {row["name"]: row for row in saved["checks"]}
                for check in [timing.check_rate, timing.check_release, timing.check_value_interval]:
                    name, severity, ok, headline, detail = check(replay, args)
                    self.assertEqual(results[name], dict(
                        name=name, severity=severity, ok=ok, headline=headline, detail=detail,
                    ))
                self.assertEqual(saved["clock"], "time.monotonic")
                self.assertEqual(saved["new_base"], [1500])
                self.assertEqual(results["pcr-rate"]["ok"], scale == 1.0)
                self.assertTrue(results["pcr-release-timing"]["ok"])
                self.assertEqual(status, 0 if scale == 1.0 else 1)


if __name__ == "__main__":
    unittest.main()
