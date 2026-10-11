import importlib.util
import unittest
import json
import subprocess
import tempfile
from unittest.mock import patch
from pathlib import Path

spec = importlib.util.spec_from_file_location('profile', Path(__file__).with_name('profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class Waits(unittest.TestCase):
    def sample(self, stack, wait=300):
        return {'tid': 1, 'comm': 'moq-quic-0', 'wait_ns': wait, 'stack': stack}

    def test_cpu_denominator_can_exceed_one(self):
        row = profile.reduce([self.sample(['std::sys::sync::mutex::futex::Mutex::lock_contended', 'moq_net::serve'])],
                             {'1': 100}, {'1': 200})['workers']['1']
        self.assertEqual(row['mutex_wait_per_cpu'], 3)
        self.assertEqual(row['cpu_ns'], 100)

    def test_condvar_and_allocator_waits_are_separate(self):
        row = profile.reduce([self.sample(['std::sys::sync::condvar::futex::Condvar::wait']),
                              self.sample(['jemalloc::background_thread'])], {'1': 10}, {'1': 20})['workers']['1']
        self.assertEqual(row['mutex_wait_ns'], 0)
        self.assertEqual(row['other_futex_wait_ns'], 600)

    def test_missing_or_reset_cpu_refused(self):
        for before, after in [({}, {'1': 2}), ({'1': 2}, {'1': 1}), ({'1': 2}, {'1': 2})]:
            with self.assertRaises(ValueError):
                profile.reduce([self.sample(['moq_net::serve'])], before, after)

    def test_empty_and_unsymbolized_refused(self):
        for samples in [[], [self.sample([])], [self.sample(['0x7f123abc'])], [self.sample(['[unknown]'])]]:
            with self.assertRaises(ValueError):
                profile.reduce(samples, {'1': 1}, {'1': 2})


class Probes(unittest.TestCase):
    def setUp(self):
        kernel = patch.object(profile.platform, "platform", return_value="test-kernel")
        kernel.start()
        self.addCleanup(kernel.stop)

    def test_lock_probe_loads_futex_and_stack_programs(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(profile.platform, 'system', return_value='Linux'), patch.object(profile.shutil, 'which', return_value='/bpftrace'), patch.object(profile.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')) as run:
            profile.probe('lock', Path(directory))
            program = run.call_args.args[0][2]
            self.assertIn('sys_enter_futex', program)
            self.assertIn('sys_exit_futex', program)
            self.assertIn('ustack', program)
            self.assertNotIn('BEGIN', program)
            self.assertEqual(run.call_args.args[0][-2:], ['-c', 'true'])

    def test_unavailable_tool_retains_refusal(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(profile.platform, 'system', return_value='Linux'), patch.object(profile.shutil, 'which', return_value=None):
            with self.assertRaises(ValueError):
                profile.probe('lock', Path(directory))
            evidence = json.loads((Path(directory) / 'probe.json').read_text())
            self.assertEqual(evidence['status'], 'unavailable')
            self.assertTrue((Path(directory) / 'probe.stderr').read_text())

    def test_spawn_failure_retains_refusal(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(profile.platform, 'system', return_value='Linux'), patch.object(profile.shutil, 'which', return_value='/bpftrace'), patch.object(profile.subprocess, 'run', side_effect=PermissionError('denied')):
            with self.assertRaises(OSError):
                profile.probe('lock', Path(directory))
            evidence = json.loads((Path(directory) / 'probe.json').read_text())
            self.assertEqual(evidence['status'], 'spawn failed')
            self.assertIn('denied', (Path(directory) / 'probe.stderr').read_text())


if __name__ == '__main__':
    unittest.main()
