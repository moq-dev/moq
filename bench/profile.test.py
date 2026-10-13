import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('profile', Path(__file__).with_name('profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

FIXTURE = Path(__file__).with_name('profile.test.txt').read_text()
IDLE, MUTEX, ALLOCATOR = '2152705', '2152709', '2152707'


class Parse(unittest.TestCase):
    def test_recorded_switches_pair_into_exact_waits(self):
        records = profile.parse(FIXTURE)
        self.assertEqual([event for _, _, event, _ in records], ['context-switches', 'PERF_RECORD_SWITCH IN'] * 3)
        # Kernel frames are dropped; inlined user frames are kept innermost first.
        self.assertFalse(any('__schedule' in frame for _, _, _, frames in records for frame in frames))
        stacks = {tid: (wait, stack) for tid, wait, stack in profile.waits(records)}
        self.assertEqual(stacks[IDLE][0], 21661)
        self.assertEqual(stacks[MUTEX][0], 6315)
        self.assertEqual(stacks[ALLOCATOR][0], 13709)
        # DWARF unwinding keeps the caller of glibc's frameless syscall wrapper.
        self.assertEqual(stacks[MUTEX][1][:3], ['syscall', 'std::sys::pal::unix::futex::futex_wait',
                                                '<std::sys::sync::mutex::futex::Mutex>::lock_contended'])

    def test_open_wait_runs_to_the_last_event(self):
        records = [('1', 10, 'context-switches', ['a']), ('2', 50, 'PERF_RECORD_SWITCH IN', [])]
        self.assertEqual(profile.waits(records), [('1', 40, ['a'])])


class Classify(unittest.TestCase):
    def test_recorded_stacks(self):
        stacks = {tid: stack for tid, _, stack in profile.waits(profile.parse(FIXTURE))}
        self.assertEqual(profile.classify(stacks[MUTEX]), 'mutex')
        self.assertEqual(profile.classify(stacks[ALLOCATOR]), 'futex')
        self.assertEqual(profile.classify(stacks[IDLE]), 'other')

    def test_spinning_is_not_a_wait(self):
        self.assertEqual(profile.classify(['<std::sys::sync::mutex::futex::Mutex>::lock_contended', 'moq_net::serve']), 'other')


class Reduce(unittest.TestCase):
    def test_recorded_waits_by_class_over_cpu(self):
        intervals = profile.waits(profile.parse(FIXTURE))
        names = {IDLE: 'moq-quic-0', MUTEX: 'moq-quic-1', ALLOCATOR: 'moq-quic-2'}
        before = dict.fromkeys(names, 0)
        result, folded = profile.reduce(names, intervals, before, dict.fromkeys(names, 3000), 1)
        rows = result['workers']
        self.assertEqual(rows[MUTEX]['mutex_wait_ns'], 6315)
        self.assertAlmostEqual(rows[MUTEX]['mutex_wait_per_cpu'], 6315 / 3000)
        self.assertEqual(rows[ALLOCATOR]['other_futex_wait_ns'], 13709)
        self.assertEqual(rows[IDLE]['other_off_cpu_ns'], 21661)
        [(stack, weight)] = folded.items()
        self.assertTrue(stack.endswith('lock_contended;std::sys::pal::unix::futex::futex_wait;syscall'))
        self.assertEqual(weight, 6)

    def test_silent_or_idle_worker_refused(self):
        intervals = [('1', 5, ['moq_net::serve'])]
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, intervals, {'1': 0, '2': 0}, {'1': 1, '2': 1}, 1)
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0'}, intervals, {'1': 5}, {'1': 5}, 1)

    def test_unresolved_stacks_refused_per_worker(self):
        # One resolved worker must not vouch for another whose unwinding failed.
        intervals = [('1', 5, ['moq_net::serve']), ('2', 5, ['[unknown]', 'syscall'])]
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, intervals, {'1': 0, '2': 0}, {'1': 1, '2': 1}, 1)


class Require(unittest.TestCase):
    def check(self, paranoid):
        with patch.object(profile.Path, 'read_text', return_value=paranoid + '\n'):
            profile.require()

    def test_kernel_sampling_refusal_names_the_command(self):
        # Ubuntu's default is 4; 2 already forbids kernel samples.
        for paranoid in ['4', '2']:
            with self.assertRaises(ValueError) as refused:
                self.check(paranoid)
            self.assertIn('sudo sysctl -w kernel.perf_event_paranoid=1', str(refused.exception))
        for paranoid in ['1', '0', '-1']:
            self.check(paranoid)


if __name__ == '__main__':
    unittest.main()
