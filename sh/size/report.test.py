#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location("report", Path(__file__).with_name("report.py"))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


def row(date, size=100):
    return {"version": 1, "date": date, "bytes": {"artifact": size}}


class Trends(unittest.TestCase):
    def test_first_report(self):
        self.assertEqual(report.compare(row("2026-10-10"), []), [])

    def test_daily_threshold(self):
        self.assertEqual(report.compare(row("2026-10-10", 105), [row("2026-10-09")]), [])
        self.assertEqual(len(report.compare(row("2026-10-10", 106), [row("2026-10-09")])), 1)

    def test_slow_drift(self):
        failures = report.compare(row("2026-10-10", 111), [row("2026-09-10"), row("2026-10-09", 109)])
        self.assertEqual(len(failures), 1)
        self.assertIn("30 days", failures[0])

    def test_refuses_missing_and_invalid_artifacts(self):
        for invalid in [0, -1, None, True, float("nan")]:
            with self.assertRaises(ValueError):
                report.compare(row("2026-10-10", invalid), [])
        with self.assertRaises(ValueError):
            report.compare(row("2026-10-10"), [{"date": "2026-10-09", "bytes": {}}])


unittest.main()
