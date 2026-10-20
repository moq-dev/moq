"""Nightly charts accept measurements, refuse malformed data, and share one history branch."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('report', Path(__file__).with_name('bench-report.py'))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_values_refuse_smoke_and_malformed_reports(self):
        for entries in ([], {}, [None], [{'name': '', 'unit': 'ns', 'value': 1}]):
            with self.assertRaises(ValueError):
                report.checked(entries)
        for number in (float('nan'), float('inf'), -1, None, True, '2'):
            with self.assertRaises(ValueError):
                report.value(number)
        for entries in ([{'name': 'x', 'unit': 'unknown', 'value': 1}],
                        [{'name': 'x', 'unit': 'ns', 'value': 1}] * 2):
            with self.assertRaises(ValueError):
                report.checked(entries)

    def test_js_multiple_schemas_and_interests(self):
        source = self.root / 'requests.csv'
        source.write_text('bun install v1.3\npaths,active_us,settled_us\n4,1.25,2.5\noperation,ns_per_op\nclose,0\n')
        entries = report.javascript_file(source)
        self.assertEqual([entry['value'] for entry in entries], [1.25, 2.5, 0])
        self.assertEqual(entries[0]['name'], 'js/requests/paths=4/active_us')
        source.write_text('broadcasts=32 tracks=8 acquire+release=1.250 us\n')
        self.assertEqual(report.javascript_file(source)[0]['value'], 1.25)
        for text in ('Testing case\nSuccess\n', 'paths,time_us\n4\n', 'paths,time_us\n4,nan\n'):
            source.write_text(text)
            with self.assertRaises(ValueError):
                report.javascript_file(source)

    def test_criterion_requires_fresh_measured_estimates(self):
        run = self.root / 'runs' / 'one'
        run.mkdir(parents=True)
        (self.root / 'criterion-runs.txt').write_text(f'{run}\n')
        (run / 'metadata.json').write_text(json.dumps({'mode': '--criterion', 'revision': 'abc',
            'environment': {'MOQ_BENCH_LABEL': 'net-cpu', 'SESSION_CPU': '1'}}))
        with self.assertRaises(ValueError):
            report.rust(self.root)
        case = run / 'criterion' / 'delivery' / 'new'
        case.mkdir(parents=True)
        (case / 'benchmark.json').write_text(json.dumps({'full_id': 'delivery/4/8'}))
        (case / 'estimates.json').write_text(json.dumps({'mean': {'point_estimate': 20,
            'confidence_interval': {'lower_bound': 18, 'upper_bound': 22}}}))
        entry = report.rust(self.root)[0]
        self.assertEqual(entry['name'], 'rust/net-cpu/delivery/4/8')
        self.assertEqual(entry['value'], 20)
        self.assertIn('abc', entry['extra'])

    def test_only_successful_sources_are_published(self):
        source = self.root / 'bench-ffi'
        source.mkdir()
        entries = [{'name': 'go/round', 'unit': 'MB/s', 'value': 4},
                   {'name': 'go/round vs rust-ffi', 'unit': 'x', 'value': 2}]
        (source / 'report.json').write_text(json.dumps(entries))
        (source / 'outcome.json').write_text('{"status":"success"}')
        # Another nightly artifact with the same prefix and its own report schema.
        unrelated = self.root / 'bench-browser'
        unrelated.mkdir()
        (unrelated / 'report.json').write_text('{"samples": []}')
        failed = self.root / 'bench-rust'
        failed.mkdir()
        (failed / 'outcome.json').write_text('{"status":"failure"}')
        (failed / 'report.json').write_text('not a usable report')
        output = self.root / 'output'
        report.prepare(self.root, output)
        self.assertEqual(json.loads((output / 'rates.json').read_text())[0]['value'], 4)
        self.assertEqual(json.loads((output / 'costs.json').read_text())[0]['value'], 2)
        (source / 'report.json').unlink()
        with self.assertRaises(ValueError):
            report.prepare(self.root, output)

    def test_landing_page_keeps_existing_charts_and_history(self):
        chart = self.root / 'bench' / 'costs'
        chart.mkdir(parents=True)
        (chart / 'index.html').write_text('chart')
        (chart / 'data.js').write_text('history')
        report.index(self.root)
        self.assertIn('costs/', (chart.parent / 'index.html').read_text())
        relays = self.root / 'relays'
        relays.mkdir()
        (relays / 'index.html').write_text('<title>Relay smoke</title>')
        report.index(self.root)
        landing = (self.root / 'index.html').read_text()
        self.assertIn('<a href="bench/">MoQ benchmarks</a>', landing)
        self.assertIn('<a href="relays/">Relay smoke</a>', landing)
        self.assertEqual((chart / 'data.js').read_text(), 'history')
        self.assertTrue((self.root / '.nojekyll').exists())

    def test_first_and_subsequent_history_publications(self):
        remote, checkout = self.root / 'remote.git', self.root / 'checkout'
        def git(*args, cwd=None):
            return subprocess.run(['git', *args], cwd=cwd, check=True, capture_output=True, text=True)
        git('init', '--bare', str(remote))
        git('clone', str(remote), str(checkout))
        git('config', 'user.name', 'test', cwd=checkout)
        git('config', 'user.email', 'test@example.com', cwd=checkout)
        (checkout / 'source').write_text('source tree')
        git('add', '.', cwd=checkout)
        git('commit', '-m', 'Source', cwd=checkout)
        commands = self.root / 'commands'
        commands.mkdir()
        just = commands / 'just'
        just.write_text(f'#!/bin/sh\nexec python3 {ROOT}/sh/gh/bench-report.py "$3" "$4"\n')
        just.chmod(0o755)
        env = dict(os.environ, PATH=f'{commands}:{os.environ["PATH"]}')
        publisher = ROOT / 'sh/gh/bench-publish.sh'
        for iteration in (1, 2):
            subprocess.run(['bash', str(publisher), 'init'], cwd=checkout, env=env, check=True,
                           capture_output=True)
            history = self.root / f'history-{iteration}'
            git('worktree', 'add', str(history), 'gh-pages', cwd=checkout)
            chart = history / 'bench' / 'costs'
            chart.mkdir(parents=True, exist_ok=True)
            (chart / 'index.html').write_text('chart')
            (chart / 'data.js').write_text(f'run {iteration}')
            git('add', '.', cwd=history)
            git('commit', '-m', 'Measured result', cwd=history)
            git('worktree', 'remove', str(history), cwd=checkout)
            site = self.root / f'site-{iteration}'
            subprocess.run(['bash', str(publisher), 'finish', str(site)], cwd=checkout, env=env,
                           check=True, capture_output=True)
            self.assertFalse((site / 'source').exists())
            self.assertEqual((site / 'bench/costs/data.js').read_text(), f'run {iteration}')
            git('worktree', 'remove', str(site), cwd=checkout)
            git('branch', '-D', 'gh-pages', cwd=checkout)
        self.assertEqual(git('--git-dir', str(remote), 'show', 'gh-pages:bench/costs/data.js').stdout, 'run 2')


if __name__ == '__main__':
    unittest.main()
