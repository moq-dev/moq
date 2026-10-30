"""The interop report keeps unreached cases red, lays out publisher by subscriber grids, and refuses malformed records."""

from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("report", HERE / "report.py")
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)
NOW = datetime(2026, 10, 20, 8, tzinfo=timezone.utc)
LINKS = report.Links(
    "https://example.test/run", "https://example.test/artifact", "https://example.test/commit/0123456789abcdef"
)


class ReportTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.results = self.root / "runs" / "results.jsonl"
        self.run = self.root / "runs" / "interop-abcd"

    def harness(self, *args):
        """Record through the real harness_result, as interop.sh does."""
        script = f'source {HERE.parent / "lib" / "harness.sh"}; HARNESS_RUN={self.run}; harness_result "$@"'
        env = dict(os.environ, HARNESS_RESULTS=str(self.results))
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c", script, "harness", *args], env=env, capture_output=True, text=True
        )

    def record(self, *args):
        result = self.harness(*args)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_planned_case_never_reached_stays_red(self):
        for subscriber in ("rust", "go"):
            self.record(
                "not run",
                "suite=interop",
                "case=media",
                "publisher=rust",
                f"subscriber={subscriber}",
                "transport=auto",
                "version=auto",
            )
        self.record(
            "pass",
            "suite=interop",
            "case=media",
            "publisher=rust",
            "subscriber=rust",
            "transport=auto",
            "version=auto",
            "seconds=3",
            f"log={self.run}/rust-rust.log",
        )
        records = report.load(self.results)
        self.assertEqual([(r["subscriber"], r["outcome"]) for r in records], [("rust", "pass"), ("go", "not run")])
        self.assertEqual(records[0]["log"], "interop-abcd/rust-rust.log")
        self.assertEqual(records[0]["seconds"], 3)
        page = report.render(records, report.steps('{"Interop (full matrix)": "success"}'), LINKS, NOW)
        self.assertIn('<td class="pass" title="log: interop-abcd/rust-rust.log">pass 3s</td>', page)
        self.assertIn(
            '<td class="not-run"><a href="https://example.test/run">not run</a> '
            '<a class="logs" href="https://example.test/artifact">logs</a></td>',
            page,
        )
        self.assertIn('<span class="badge fail">fail</span> MoQ interop', page)
        self.assertIn("<title>MoQ interop</title>", page)

    def test_grids_split_by_case_transport_and_version(self):
        for version in ("moq-lite-05", "moq-lite-06"):
            for pub, sub in (("rust", "js-native-node"), ("js-native-node", "rust")):
                self.record(
                    "pass",
                    "suite=interop",
                    "case=drop",
                    f"publisher={pub}",
                    f"subscriber={sub}",
                    "transport=auto",
                    f"version={version}",
                    "seconds=1",
                )
        self.record("fail", "suite=interop", "case=relay close", "publisher=rust", "detail=idled out")
        page = report.render(report.load(self.results), {}, LINKS, NOW)
        self.assertEqual(page.count('<table class="grid">'), 2)
        self.assertIn("<h3>drop <small>transport auto, version moq-lite-05</small></h3>", page)
        # A diagonal pairing leaves the other cells empty rather than inventing outcomes.
        self.assertEqual(page.count('<td class="none"></td>'), 4)
        # A case without a pairing is listed with its detail.
        self.assertIn("<td>relay close</td>", page)
        self.assertIn("<td>idled out</td>", page)

    def test_a_failed_step_fails_the_page_even_when_every_case_passed(self):
        self.record("pass", "suite=media", "case=chromium: control: frozen video", "publisher=js", "subscriber=js")
        records = report.load(self.results)
        self.assertTrue(report.passed(records, report.steps('{"Media output and lifecycle": "success"}')))
        suites = report.steps(
            '{"Media output and lifecycle": "success", "Portrait transcode": "failure", '
            '"Released wire compatibility": "skipped"}'
        )
        self.assertFalse(report.passed(records, suites))
        page = report.render(records, suites, LINKS, NOW)
        self.assertIn('<tr><td>Portrait transcode</td><td class="fail">fail</td></tr>', page)
        self.assertIn('<tr><td>Released wire compatibility</td><td class="skip">skip</td></tr>', page)

    def test_a_run_where_nothing_ran_is_not_green(self):
        skipped = report.steps('{"Rust cache": "failure", "Interop (full matrix)": "skipped"}')
        self.assertFalse(report.passed([], skipped))
        self.assertFalse(report.passed([], report.steps('{"Interop (full matrix)": "skipped"}')))
        self.assertFalse(report.passed([], {}))
        # Every step green but no case recorded: the records were lost, not passed.
        self.assertFalse(report.passed([], report.steps('{"Interop (full matrix)": "success"}')))

    def test_without_results_the_steps_still_render(self):
        records = report.load(self.results)
        self.assertEqual(records, [])
        page = report.render(records, report.steps('{"Playwright Chromium": "failure"}'), LINKS, NOW)
        self.assertIn('<td class="fail">fail</td>', page)

    def test_the_harness_refuses_malformed_records(self):
        for args in (
            ("passed", "suite=s", "case=c"),
            ("pass", "suite=s"),
            ("pass", "suite=s", "case=c", "bogus=1"),
            ("pass", "suite=s", "case=c", "outcome=fail"),
            ("pass", "suite=s", "case=c", "seconds=soon"),
        ):
            self.assertNotEqual(self.harness(*args).returncode, 0, args)
        self.assertFalse(self.results.exists())
        # Without HARNESS_RESULTS the harness records nothing, for any input.
        script = f"source {HERE.parent / 'lib' / 'harness.sh'}; harness_result bogus"
        env = {k: v for k, v in os.environ.items() if k != "HARNESS_RESULTS"}
        subprocess.run(["bash", "-euo", "pipefail", "-c", script], env=env, check=True)

    def test_the_report_refuses_malformed_records_and_steps(self):
        self.results.parent.mkdir(parents=True)
        good = {field: "" for field in report.FIELDS} | {"suite": "s", "case": "c", "outcome": "pass", "seconds": None}
        for line in (
            "{",
            json.dumps(good | {"outcome": "passed"}),
            json.dumps(good | {"suite": ""}),
            json.dumps({k: v for k, v in good.items() if k != "log"}),
        ):
            self.results.write_text(line + "\n")
            with self.assertRaises(ValueError):
                report.load(self.results)
        for text in ("[]", '{"x": "neutral"}'):
            with self.assertRaises(ValueError):
                report.steps(text)

    def test_cli_writes_the_page_and_the_records(self):
        self.record("pass", "suite=interop", "case=CLI FETCH", "publisher=rust", "subscriber=rust")
        out = self.root / "site"
        subprocess.run(
            [
                "python3",
                str(HERE / "report.py"),
                str(self.results),
                str(out),
                "--steps",
                '{"Interop": "success"}',
                "--run-url",
                LINKS.run,
            ],
            check=True,
        )
        self.assertIn("<td>CLI FETCH</td>", (out / "index.html").read_text())
        self.assertEqual((out / "results.jsonl").read_text(), self.results.read_text())


if __name__ == "__main__":
    unittest.main()
