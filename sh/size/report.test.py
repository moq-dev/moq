#!/usr/bin/env python3
import importlib.util
import json
import subprocess
import sys
import tempfile
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

    def test_refuses_invalid_revision_metadata(self):
        for revision in [None, 123, "main", "a" * 39, "a" * 40 + ")"]:
            current = row("2026-10-10") | {"revision": revision}
            with self.subTest(revision=revision), self.assertRaisesRegex(ValueError, "commit SHA"):
                report.compare(current, [])

    def test_failed_growth_advances_daily_history_but_keeps_monthly_anchor(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            history = root / "previous.json"
            history.write_text(json.dumps([row("2026-09-10"), row("2026-10-09")]))

            def run(current):
                (root / "sizes.json").write_text(json.dumps(current))
                result = subprocess.run(
                    [sys.executable, str(Path(report.__file__)), str(root), "--history", str(history)],
                    capture_output=True,
                    text=True,
                )
                retained = json.loads((root / "history.json").read_text())
                history.write_text(json.dumps(retained))
                return result, retained

            previous = row("2026-10-10", 106) | {"revision": "a" * 40}
            failed, retained = run(previous)
            self.assertEqual(failed.returncode, 1, failed.stderr)
            self.assertEqual(retained[-1], previous)

            # A stable next report compares against the failed point, not the last success.
            current = row("2026-10-11", 106) | {"revision": "b" * 40}
            passed, _ = run(current)
            self.assertEqual(passed.returncode, 0, passed.stderr)
            self.assertIn(report.source(current), passed.stdout)

            # The thirty-day guard still catches drift despite the advanced daily baseline.
            monthly, _ = run(row("2026-10-12", 111))
            self.assertEqual(monthly.returncode, 1, monthly.stderr)
            self.assertIn("30 days: 2026-09-10", monthly.stdout)
            self.assertNotIn("(previous:", monthly.stdout)

    def test_failure_names_the_measured_baseline_revision(self):
        baseline = row("2026-10-09") | {"revision": "a" * 40}
        failures = report.compare(row("2026-10-10", 106), [baseline])
        self.assertIn(report.source(baseline), failures[0])


unittest.main()
