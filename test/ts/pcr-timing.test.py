import contextlib
import importlib.util
import io
import json
import pathlib
import random
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
            first_null=20,
            new_base=set(),
            pcr_pid=256,
            other_pcr_pids=set(),
        )
        args = types.SimpleNamespace(mux_rate=3_760_000, schedule_pct_min=99, schedule_tolerance_pct=1)
        result = timing.check_schedule(scan, args)
        self.assertFalse(result[2], result)
        self.assertEqual(result[4]["count"], 5)


class Rate(unittest.TestCase):
    ARGS = types.SimpleNamespace(
        live=True,
        seconds=120,
        live_cover_pct=50,
        live_min_pcr=20,
        release_ms=10,
        release_pct_max=1,
        drift_ms=1000,
        rate_ppm=30,
    )

    def stalled(self, scale):
        """116 s of 25 ms PCRs, one released 737 ms late at 75 s and the next 28 each 23.8 ms
        early, leaving a ~70 ms standing lag: the shape of the 2026-10-11 local failure."""
        lag, pcr = 0.0, []
        for i in range(4648):
            lag += 0.737 if i == 3000 else -0.0238 if 3000 < i <= 3028 else 0.0
            pcr.append((i * 166, round(i * 0.025 * 27_000_000), 1000.0 + i * 0.025 * scale + lag, 256))
        return types.SimpleNamespace(pcr=pcr, new_base=set())

    def test_caught_up_stall_is_bounded_lag_not_rate(self):
        scan = self.stalled(1.0)
        release = timing.check_release(scan, self.ARGS)
        self.assertTrue(release[2], release)
        self.assertLess(abs(release[4]["tail_drift_rate_ms_per_s"]), 0.01)
        rate = timing.check_rate(scan, self.ARGS)
        self.assertTrue(rate[2], rate)
        self.assertEqual(rate[4]["lag_steps"], 1)

    def test_read_delay_wander_is_not_rate(self):
        # Reads only ever land late: up to 4 ms through the start of the graded window, under
        # 0.5 ms either side, on a clock at exactly the media rate.
        rng = random.Random(7)
        scan = types.SimpleNamespace(
            new_base=set(),
            pcr=[
                (
                    i * 166,
                    round(i * 0.025 * 27_000_000),
                    1000.0 + i * 0.025 + rng.uniform(0, 0.004 if 1600 <= i < 2600 else 0.0005),
                    256,
                )
                for i in range(4648)
            ],
        )
        rate = timing.check_rate(scan, self.ARGS)
        self.assertTrue(rate[2], rate)
        self.assertLess(abs(rate[4]["ppm"][0]), 3.0)

    def test_off_rate_clock_fails_through_a_stall(self):
        rate = timing.check_rate(self.stalled(1.001), self.ARGS)
        self.assertFalse(rate[2], rate)
        self.assertAlmostEqual(rate[4]["ppm"][0], -999.0, delta=1.0)


class Evidence(unittest.TestCase):
    def test_live_report_replays_both_clock_rate_verdicts(self):
        for scale in [1.0, 1.001]:
            with self.subTest(scale=scale), tempfile.TemporaryDirectory() as directory:
                scan = timing.Scan()
                scan.pcr = [
                    (i * 266, round(i * 0.04 * 27_000_000), 1000.0 + i * 0.04 * scale, 256) for i in range(3001)
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
                    pcr=saved["pcr_samples"],
                    new_base=set(saved["new_base"]),
                    wraps=saved["wraps"],
                )
                args = types.SimpleNamespace(**saved["parameters"])
                results = {row["name"]: row for row in saved["checks"]}
                for check in [timing.check_rate, timing.check_release, timing.check_value_interval]:
                    name, severity, ok, headline, detail = check(replay, args)
                    self.assertEqual(
                        results[name],
                        dict(
                            name=name,
                            severity=severity,
                            ok=ok,
                            headline=headline,
                            detail=detail,
                        ),
                    )
                self.assertEqual(saved["clock"], "time.monotonic")
                self.assertEqual(saved["new_base"], [1500])
                self.assertEqual(results["pcr-rate"]["ok"], scale == 1.0)
                self.assertTrue(results["pcr-release-timing"]["ok"])
                self.assertEqual(status, 0 if scale == 1.0 else 1)


if __name__ == "__main__":
    unittest.main()
