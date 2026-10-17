#!/usr/bin/env python3
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location('ffi', Path(__file__).with_name('ffi.py'))
ffi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ffi)


def write(directory, runner, entries):
    (Path(directory) / f'{runner}.json').write_text(
        json.dumps([{'name': n, 'unit': u, 'value': v} for n, u, v in entries])
    )


class FfiTest(unittest.TestCase):
    def runners(self, directory):
        write(directory, 'rust-ffi', [('throughput/a', 'MB/s', 100.0), ('cpu/a', 'ms/MB', 2.0)])
        write(directory, 'rust-net', [('throughput/a', 'MB/s', 200.0)])
        write(directory, 'python', [('throughput/a', 'MB/s', 25.0), ('cpu/a', 'ms/MB', 8.0)])
        return ffi.load(directory)

    def test_ratio_is_worse_above_one(self):
        with tempfile.TemporaryDirectory() as directory:
            report = {e['name']: e['value'] for e in ffi.report(self.runners(directory))}
        self.assertEqual(report['python/throughput/a vs rust-ffi'], 4.0)
        self.assertEqual(report['python/cpu/a vs rust-ffi'], 4.0)
        self.assertEqual(report['rust-net/throughput/a vs rust-ffi'], 0.5)
        self.assertNotIn('rust-ffi/throughput/a vs rust-ffi', report)

    def test_table_orders_baselines_first(self):
        with tempfile.TemporaryDirectory() as directory:
            table = ffi.table(self.runners(directory))
        self.assertTrue(table.splitlines()[0].startswith('| benchmark | unit | rust-net | rust-ffi | python |'))
        self.assertIn('| throughput/a | MB/s | 200 (0.50x) | 100 | 25 (4.00x) |', table)

    def test_missing_baseline_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            write(directory, 'python', [('throughput/a', 'MB/s', 25.0)])
            with self.assertRaises(ValueError):
                ffi.load(directory)

    def test_skips_its_own_report(self):
        with tempfile.TemporaryDirectory() as directory:
            self.runners(directory)
            (Path(directory) / 'report.json').write_text('[]')
            self.assertNotIn('report', ffi.load(directory))

    def test_a_skipped_runner_cannot_silently_disappear(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # Some platform checks return success when their toolchain is absent.
            # The orchestrator must refuse that success without benchmark output.
            stub = root / 'just'
            stub.write_text(
                '#!/bin/sh\nmkdir -p "$MOQ_BENCH_DIR"\n'
                'printf "[]" > "$MOQ_BENCH_DIR/rust-ffi.json"\n'
            )
            stub.chmod(0o755)
            env = {**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}', 'MOQ_BENCH_DIR': str(root / 'results')}
            result = subprocess.run(
                ['bash', str(Path(__file__).with_name('ffi.sh')), 'cpp'],
                env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('did not produce cpp.json', result.stderr)


if __name__ == '__main__':
    unittest.main()
