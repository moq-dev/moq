#!/usr/bin/env python3
"""The upstream registry grows without changing our matrix selection."""

import importlib.util
import json
import subprocess
import tempfile
from unittest.mock import patch
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('runner', Path(__file__).with_name('runner.py'))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def implementation(**roles):
    return {'draft_versions': ['draft-18'], 'roles': {
        role: {'docker': {'image': image}} for role, image in roles.items()}}


class Runner(unittest.TestCase):
    def setUp(self):
        self.registry = {'implementations': {
            'moq-dev-rs': implementation(client='ghcr.io/ours/rs', relay='moq-local'),
            'moq-dev-js': implementation(client='ghcr.io/ours/js'),
            'new': implementation(client='docker.io/new/client', relay='docker.io/new/relay'),
            'adapter': implementation(relay='local-adapter'),
        }}
        self.registry['implementations']['adapter']['roles']['relay']['docker']['build'] = {'context': '.'}

    def test_new_published_implementations_join_both_directions_once(self):
        plans, inventory = runner.matrices(self.registry)
        pairs = [runner.expected_pairs(plan) for plan in plans]
        self.assertEqual(pairs[0], {('moq-dev-rs', 'moq-dev-rs'), ('moq-dev-js', 'moq-dev-rs'),
                                    ('new', 'moq-dev-rs')})
        self.assertEqual(pairs[1], {('moq-dev-rs', 'new'), ('moq-dev-js', 'new')})
        self.assertFalse(pairs[0] & pairs[1])
        self.assertEqual(next(entry['status'] for entry in inventory
                              if entry['implementation'] == 'adapter'), 'not run: local build only')
        self.assertEqual(self.registry['implementations']['moq-dev-rs']['roles']['relay']['docker']['image'],
                         'moq-local')

    def test_published_image_with_source_recipe_is_still_selected(self):
        self.registry['implementations']['new']['roles']['relay']['docker']['build'] = {'context': '.'}
        plans, _ = runner.matrices(self.registry)
        self.assertIn(('moq-dev-rs', 'new'), runner.expected_pairs(plans[1]))

    def test_missing_skipped_duplicate_or_unexpected_cells_fail(self):
        cell = {'client': 'a', 'relay': 'b', 'mode': 'docker', 'status': 'fail'}
        runner.validate_summary({'runs': [cell]}, {('a', 'b')})
        for cells in ([], [cell, cell], [dict(cell, status='skip')], [dict(cell, relay='c')]):
            with self.subTest(cells=cells), self.assertRaises(ValueError):
                runner.validate_summary({'runs': cells}, {('a', 'b')})

    def run_matrices(self, status):
        """Run both matrices against a fake runner that gives each pair `status(client, relay)`."""
        with tempfile.TemporaryDirectory() as temporary:
            upstream = Path(temporary)
            registry = upstream / 'implementations.json'
            original = json.dumps(self.registry)
            registry.write_text(original)
            matrices = []

            def command(args, **kwargs):
                results = upstream / 'results'
                if args[:2] == ['bash', 'run-interop-tests.sh']:
                    pairs = runner.expected_pairs(json.loads(registry.read_text()))
                    result = results / str(len(matrices))
                    result.mkdir(parents=True)
                    runs = [{'client': client, 'relay': relay, 'mode': 'docker',
                             'status': status(client, relay)} for client, relay in pairs]
                    (result / 'summary.json').write_text(json.dumps({'runs': runs}))
                    matrices.append(pairs)
                    failed = any(run['status'] != 'pass' for run in runs)
                    return subprocess.CompletedProcess(args, int(failed))
                if args[:2] == ['bash', 'generate-report.sh']:
                    (results / 'index.html').write_text('<body></body>')
                    (results / 'nightly' / 'report.html').write_text('')
                return subprocess.CompletedProcess(args, 0)

            failure = None
            with patch.object(runner.subprocess, 'run', side_effect=command):
                try:
                    runner.run(upstream, upstream / 'report')
                except SystemExit as error:
                    failure = error
            self.assertEqual(len(matrices), 2)
            self.assertEqual(registry.read_text(), original)
            runs = json.loads((upstream / 'report' / 'nightly' / 'summary.json').read_text())['runs']
            self.assertEqual(len(runs), sum(len(pairs) for pairs in matrices))
            self.assertIn('local-adapter', (upstream / 'report' / 'index.html').read_text())
            return failure

    def test_failed_required_pair_reports_every_matrix_then_fails(self):
        required = ('moq-dev-js', 'moq-dev-rs')
        failure = self.run_matrices(lambda client, relay: 'timeout' if (client, relay) == required else 'pass')
        self.assertIn(str(required), str(failure))

    def test_failed_optional_pairs_are_reported_but_pass(self):
        def status(client, relay):
            return 'pass' if (client, relay) in runner.REQUIRED else 'fail'
        with patch('builtins.print') as report:
            failure = self.run_matrices(status)
        self.assertIsNone(failure)
        self.assertIn("('new', 'moq-dev-rs')", report.call_args.args[0])

    def test_required_pair_the_registry_drops_fails(self):
        self.registry['implementations']['moq-dev-js']['draft_versions'] = ['draft-22']
        self.registry['implementations']['new']['draft_versions'] = ['draft-18', 'draft-22']
        self.assertIn("('moq-dev-js', 'moq-dev-rs')", str(self.run_matrices(lambda client, relay: 'pass')))

    def test_drafts_without_overlap_have_no_pair(self):
        self.registry['implementations']['new']['draft_versions'] = ['draft-22']
        plans, _ = runner.matrices(self.registry)
        self.assertNotIn(('new', 'moq-dev-rs'), runner.expected_pairs(plans[0]))
        self.assertFalse(runner.expected_pairs(plans[1]))


if __name__ == '__main__':
    unittest.main()
