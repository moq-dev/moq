"""Nightly reports accept measurements, refuse malformed data, flag regressions, and share one history branch."""
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
DAY = 86400
START = datetime(2026, 10, 1, tzinfo=timezone.utc).timestamp()
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

    def test_js_byte_measurements_do_not_become_dimensions(self):
        source = self.root / 'compression.csv'
        source.write_text('bytes,us,size_bytes\n4,1.5,64\nmeasurement,size_bytes\nbundle,512\n')
        entries = report.javascript_file(source)
        self.assertEqual([(entry['name'], entry['unit'], entry['value']) for entry in entries], [
            ('js/compression/bytes=4/us', 'us', 1.5),
            ('js/compression/bytes=4/size_bytes', 'bytes', 64),
            ('js/compression/measurement=bundle/size_bytes', 'bytes', 512),
        ])
        source.write_text('bytes,us,size_bytes\n4,2.5,128\nmeasurement,size_bytes\nbundle,1024\n')
        self.assertEqual([entry['name'] for entry in report.javascript_file(source)],
                         [entry['name'] for entry in entries])

    def test_criterion_requires_fresh_measured_estimates(self):
        run = self.root / 'runs' / 'one'
        run.mkdir(parents=True)
        (self.root / 'criterion-runs.txt').write_text(f'{run}\n')
        (run / 'metadata.json').write_text(json.dumps({'mode': '--criterion', 'revision': 'abc',
            'environment': {'BENCH_LABEL': 'net-cpu', 'SESSION_CPU': '1'}}))
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

    def paired_runs(self, comparisons):
        """Write measured Criterion runs, each with {label: (base, paired.json)}."""
        runs = []
        for label, (base, comparison) in comparisons.items():
            run = self.root / 'runs' / label
            run.mkdir(parents=True)
            (run / 'metadata.json').write_text(json.dumps({'mode': '--criterion', 'revision': 'c2', 'base': base,
                                                           'environment': {'BENCH_LABEL': label}}))
            if comparison is not None:
                (run / 'paired.json').write_text(json.dumps(comparison))
            runs.append(str(run))
        (self.root / 'criterion-runs.txt').write_text('\n'.join(runs) + '\n')

    def comparison(self, cases, unavailable=()):
        return {'commit': 'c2', 'base': 'c1', 'threshold_pct': 10, 'rounds': 3,
                'unavailable': [{'target': target, 'reason': 'base build failed'} for target in unavailable],
                'cases': [{'target': 'kio/lock', 'id': name, 'base_ns': 100, 'current_ns': 150, 'delta_pct': 50,
                           'verdict': verdict, **({'rounds': {'delta_pct': 40, 'paired_deltas_pct': [30, 40, 50],
                                                              'status': 'consistent sign'}}
                                                  if verdict == 'regression' else {})}
                          for name, verdict in cases.items()]}

    def test_paired_runs_combine_under_trend_names_and_gate_regressions(self):
        self.paired_runs({'kio': ('c1', self.comparison({'lock/4': 'regression', 'lock/1': 'unchanged'})),
                          'gst': ('c1', self.comparison({'catalog': 'faster'}, ['moq-gst/catalog']))})
        combined = report.paired(self.root)
        self.assertEqual([case['name'] for case in combined['cases']], ['rust/kio/lock/4', 'rust/kio/lock/1',
                                                                         'rust/gst/catalog'])
        self.assertEqual(combined['unavailable'], [{'name': 'rust/gst/moq-gst/catalog', 'reason': 'base build failed'}])
        (self.root / 'paired.json').write_text(json.dumps(combined))
        self.assertEqual(report.gate(self.root), 1)
        combined['cases'][0]['verdict'], combined['unavailable'] = 'noise', []
        (self.root / 'paired.json').write_text(json.dumps(combined))
        self.assertEqual(report.gate(self.root), 0)
        (self.root / 'paired.json').unlink()
        with self.assertRaises(ValueError):
            report.gate(self.root)

    def test_unpaired_runs_have_no_comparison_and_bases_must_agree(self):
        self.paired_runs({'kio': ('', None)})
        self.assertIsNone(report.paired(self.root))
        self.temp.cleanup()
        self.root.mkdir()
        self.paired_runs({'kio': ('c1', self.comparison({})), 'gst': ('', None)})
        with self.assertRaises(ValueError):
            report.paired(self.root)

    def test_paired_comparison_publishes_beside_the_trend(self):
        self.paired_runs({'kio': ('c1', self.comparison({'lock/4': 'regression', 'lock/1': 'unchanged'},
                                                        ['kio/other']))})
        source = self.root / 'inputs' / 'bench-rust'
        source.mkdir(parents=True)
        (source / 'report.json').write_text(json.dumps([{'name': 'rust/kio/lock/4', 'unit': 'ns', 'value': 150}]))
        (source / 'outcome.json').write_text('{"status":"success"}')
        (source / 'paired.json').write_text(json.dumps(report.paired(self.root)))
        reports = self.root / 'reports'
        report.prepare(self.root / 'inputs', reports)
        self.history('costs', [('c2', 1, {'bench-rust/rust/kio/lock/4': 150})])
        report.index(self.root, 'c2', reports / 'paired.json')
        page = (self.root / 'bench' / 'index.html').read_text()
        self.assertIn('Confirmed regressions (1)', page)
        self.assertIn('Unchanged (1)', page)
        self.assertIn('bench-rust/rust/kio/lock/4', page)
        self.assertIn('+30.0% +40.0% +50.0%', page)
        self.assertIn('bench-rust/rust/kio/kio/other: base build failed', page)
        # The trend table still reads only its own rows.
        self.assertEqual(self.rows('c2')[1], {'bench-rust/rust/kio/lock/4': 'muted'})
        # A night without a finished comparison replaces the published one.
        (source / 'outcome.json').write_text('{"status":"failure"}')
        report.prepare(self.root / 'inputs', reports)
        report.index(self.root, 'c3', reports / 'paired.json')
        self.assertIn('No paired comparison finished', (self.root / 'bench' / 'index.html').read_text())
        self.assertEqual(json.loads((self.root / 'bench' / 'paired.json').read_text()), {'sources': {}})

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
        # With every source failed, no point is recorded but the table still rebuilds from history.
        (source / 'outcome.json').write_text('{"status":"failure"}')
        report.prepare(self.root, output)
        self.assertEqual(json.loads((output / 'costs.json').read_text()), [])

    def history(self, kind, runs):
        """Store runs of (commit, day, {name: value}) the way github-action-benchmark does."""
        unit = 'MB/s' if kind == 'rates' else 'ns'
        entries = [{'commit': {'id': commit, 'timestamp': f'2026-10-{day:02}T08:00:00Z'}, 'date': (START + day * DAY) * 1000,
                    'benches': [{'name': name, 'unit': unit, 'value': number} for name, number in values.items()]}
                   for commit, day, values in runs]
        path = self.root / 'bench' / kind / 'data.js'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(report.HISTORY_PREFIX + json.dumps({'entries': {kind: entries}}, indent=2))

    def rows(self, commit=None):
        page = report.regressions(self.root, commit, now=START + 31 * DAY)
        return page, dict(re.findall(r'<tr><td>([^<]*)<td>[^<]*<td>[^<]*<td class="(\w*)">', page))

    def test_flags_beyond_each_series_noise(self):
        steady = [100, 101, 99, 100, 102, 98, 100]
        noisy = [100, 140, 70, 120, 80, 130, 60]
        runs = [(f'c{day}', day, {'bench-a/step': number, 'bench-a/noisy': loud, 'bench-a/zero': 0,
                                  'bench-a/flat': 0, 'bench-a/drift': number})
                for day, (number, loud) in enumerate(zip(steady, noisy), 1)]
        runs.append(('c8', 8, {'bench-a/step': 120, 'bench-a/noisy': 150, 'bench-a/zero': 1, 'bench-a/flat': 0,
                               'bench-a/drift': 105, 'bench-a/short': 1}))
        self.history('costs', runs)
        self.history('rates', [(commit, day, {'bench-a/rate': 50 if commit == 'c8' else 100})
                               for commit, day, _ in runs])
        page, rows = self.rows()
        self.assertEqual(rows, {'bench-a/step': 'worse', 'bench-a/rate': 'worse', 'bench-a/zero': 'worse',
                                'bench-a/noisy': '', 'bench-a/flat': '', 'bench-a/drift': '',
                                'bench-a/short': 'muted'})
        # Worst first: the rate halved, the step rose a fifth, and a zero baseline sorts above both.
        self.assertLess(page.index('bench-a/zero'), page.index('bench-a/rate'))
        self.assertLess(page.index('bench-a/rate'), page.index('bench-a/step'))
        self.assertIn('Regressions (3)', page)
        self.assertIn('1 of 8 points', page)
        self.assertIn('+20.0%', page)

    def test_reruns_collapse_and_failed_sources_go_stale(self):
        runs = [(f'c{day}', day, {'bench-a/x': 100, 'bench-a/retired': 100, 'bench-b/y': 100}) for day in range(1, 8)]
        # A re-run records the same commit again; the earlier, regressed point must not count.
        runs += [('c8', 8, {'bench-a/x': 200, 'bench-a/retired': 100, 'bench-b/y': 200}),
                 ('c8', 9, {'bench-a/x': 100, 'bench-a/retired': 100, 'bench-b/y': 200})]
        self.history('costs', runs)
        page, rows = self.rows()
        self.assertEqual(rows, {'bench-a/x': '', 'bench-a/retired': '', 'bench-b/y': 'worse'})
        # bench-b failed on the next nightly, so its regression is not tonight's result,
        # and bench-a no longer measures the retired series.
        self.history('costs', runs + [('c9', 10, {'bench-a/x': 100})])
        page, rows = self.rows()
        self.assertEqual(rows, {'bench-a/x': '', 'bench-b/y': 'muted'})
        self.assertIn('stale: last measured 2026-10-10 at c8, 22 days ago', page)
        self.assertIn('Regressions (0)', page)
        # When every source fails, the nightly commit has no point at all.
        page, rows = self.rows('c10')
        self.assertEqual(rows, {'bench-a/x': 'muted', 'bench-b/y': 'muted'})

    def test_table_replaces_charts_and_keeps_history(self):
        with self.assertRaises(ValueError):
            report.index(self.root)
        self.history('costs', [('c1', 1, {'bench-a/<x>': 1})])
        chart = self.root / 'bench' / 'costs'
        (chart / 'index.html').write_text('chart')
        history = (chart / 'data.js').read_text()
        report.index(self.root)
        self.assertIn('bench-a/&lt;x&gt;', (chart.parent / 'index.html').read_text())
        self.assertIn('url=../', (chart / 'index.html').read_text())
        relays = self.root / 'relays'
        relays.mkdir()
        (relays / 'index.html').write_text('<title>Relay smoke</title>')
        report.index(self.root)
        landing = (self.root / 'index.html').read_text()
        self.assertIn('<a href="bench/">MoQ benchmarks</a>', landing)
        self.assertIn('<a href="relays/">Relay smoke</a>', landing)
        self.assertEqual((chart / 'data.js').read_text(), history)
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
        just.write_text(f'#!/bin/sh\nshift 2\nexec python3 {ROOT}/sh/gh/bench-report.py "$@"\n')
        just.chmod(0o755)
        env = dict(os.environ, PATH=f'{commands}:{os.environ["PATH"]}')
        publisher = ROOT / 'sh/gh/bench-publish.sh'
        paired = self.root / 'paired.json'
        paired.write_text('{"sources": {}}')
        for iteration in (1, 2):
            subprocess.run(['bash', str(publisher), 'init'], cwd=checkout, env=env, check=True,
                           capture_output=True)
            history = self.root / f'history-{iteration}'
            git('worktree', 'add', str(history), 'gh-pages', cwd=checkout)
            stored = report.HISTORY_PREFIX + json.dumps({'entries': {'costs': [{
                'commit': {'id': f'c{iteration}', 'timestamp': '2026-10-20T08:00:00Z'}, 'date': iteration,
                'benches': [{'name': 'bench-a/x', 'unit': 'ns', 'value': iteration}]}]}})
            chart = history / 'bench' / 'costs'
            chart.mkdir(parents=True, exist_ok=True)
            (chart / 'index.html').write_text('chart')
            (chart / 'data.js').write_text(stored)
            git('add', '.', cwd=history)
            git('commit', '-m', 'Measured result', cwd=history)
            git('worktree', 'remove', str(history), cwd=checkout)
            site = self.root / f'site-{iteration}'
            subprocess.run(['bash', str(publisher), 'finish', str(site), str(paired)], cwd=checkout, env=env,
                           check=True, capture_output=True)
            self.assertFalse((site / 'source').exists())
            self.assertEqual((site / 'bench/costs/data.js').read_text(), stored)
            # The source checkout's commit has no point, so the only source is stale.
            self.assertIn('stale: last measured', (site / 'bench/index.html').read_text())
            git('worktree', 'remove', str(site), cwd=checkout)
            git('branch', '-D', 'gh-pages', cwd=checkout)
        self.assertEqual(git('--git-dir', str(remote), 'show', 'gh-pages:bench/costs/data.js').stdout, stored)
        self.assertIn('url=../', git('--git-dir', str(remote), 'show', 'gh-pages:bench/costs/index.html').stdout)


if __name__ == '__main__':
    unittest.main()
