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

    def test_complete_summary_does_not_mask_ambiguous_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            upstream = Path(temporary)
            registry = upstream / 'implementations.json'
            original = json.dumps(self.registry)
            registry.write_text(original)

            def command(args, **kwargs):
                if args[:2] == ['bash', 'run-interop-tests.sh']:
                    pairs = runner.expected_pairs(json.loads(registry.read_text()))
                    result = upstream / 'results' / 'one'
                    result.mkdir(parents=True)
                    (result / 'summary.json').write_text(json.dumps({'runs': [
                        {'client': client, 'relay': relay, 'mode': 'docker', 'status': 'fail'}
                        for client, relay in pairs]}))
                    return subprocess.CompletedProcess(args, 1)
                return subprocess.CompletedProcess(args, 0)

            with patch.object(runner.subprocess, 'run', side_effect=command):
                with self.assertRaises(subprocess.CalledProcessError):
                    runner.run(upstream, upstream / 'report')
            self.assertEqual(registry.read_text(), original)
            self.assertFalse((upstream / 'report').exists())

    def test_drafts_without_overlap_have_no_pair(self):
        self.registry['implementations']['new']['draft_versions'] = ['draft-22']
        plans, _ = runner.matrices(self.registry)
        self.assertNotIn(('new', 'moq-dev-rs'), runner.expected_pairs(plans[0]))
        self.assertFalse(runner.expected_pairs(plans[1]))


if __name__ == '__main__':
    unittest.main()
