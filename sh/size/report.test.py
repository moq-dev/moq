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

    def test_new_artifact_starts_its_series(self):
        current = row("2026-10-10")
        current["bytes"]["release/moq-c-generated-x/moq-c-x.tar.gz"] = 1000
        history = [row("2026-09-10"), row("2026-10-09", 90)]
        failures = report.compare(current, history)
        self.assertEqual(len(failures), 1)
        self.assertIn("artifact: 100 > 90", failures[0])
        # Once retained, the added series is compared like any other.
        grown = row("2026-10-11")
        grown["bytes"]["release/moq-c-generated-x/moq-c-x.tar.gz"] = 1100
        self.assertEqual(len(report.compare(grown, [*history, current])), 1)

    def test_refuses_missing_and_renamed_artifacts(self):
        older = row("2026-10-08")
        older["bytes"]["retired"] = 100
        for history in [[older, row("2026-10-09")], [row("2026-10-09"), older]]:
            with self.subTest(history=history), self.assertRaisesRegex(ValueError, "missing or renamed: retired"):
                report.compare(row("2026-10-10"), history)
        renamed = {"version": 1, "date": "2026-10-10", "bytes": {"renamed": 100}}
        with self.assertRaisesRegex(ValueError, "missing or renamed: artifact"):
            report.compare(renamed, [row("2026-10-09")])

    def test_retains_last_same_date_anchor(self):
        first, second = row("2026-10-10"), row("2026-10-10")
        second["bytes"]["added"] = 100
        current = row("2026-11-09")
        self.assertEqual(report.retain(current, [first, second]), [second, current])
        # compare() picks the same row, so the added series keeps its 30-day check.
        later = row("2026-11-10")
        later["bytes"]["added"] = 111
        failures = report.compare(later, [second, current])
        self.assertTrue(any("added" in failure and "30 days" in failure for failure in failures))

    def test_validates_every_history_row(self):
        cases = [
            {"version": 2, "date": "2026-09-01", "bytes": {"artifact": 100}},
            {"version": 1, "date": "2026-09-01", "bytes": {"other": 100}},
            row("2026-09-01", 0),
            row("2026-10-11"),
        ]
        for invalid in cases:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                report.compare(row("2026-10-10"), [invalid, row("2026-10-09")])

    def test_validates_first_report_date(self):
        with self.assertRaises(ValueError):
            report.compare(row("invalid"), [])


unittest.main()
