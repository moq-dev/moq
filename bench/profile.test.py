import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('profile', Path(__file__).with_name('profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

FIXTURE = Path(__file__).with_name('profile.test.txt').read_text()
IDLE, MUTEX, ALLOCATOR = '2152705', '2152709', '2152707'
# Every fixture wait closes, so the window end only has to follow the last event.
END = 271707 * 1_000_000_000


class Parse(unittest.TestCase):
    def test_recorded_switches_pair_into_exact_waits(self):
        records = profile.parse(FIXTURE)
        self.assertEqual([event for _, _, event, _ in records], ['context-switches', 'PERF_RECORD_SWITCH IN'] * 3)
        # Kernel frames are dropped; inlined user frames are kept innermost first.
        self.assertFalse(any('__schedule' in frame for _, _, _, frames in records for frame in frames))
        stacks = {tid: (wait, stack) for tid, wait, stack in profile.waits(records, END)}
        self.assertEqual(stacks[IDLE][0], 21661)
        self.assertEqual(stacks[MUTEX][0], 6315)
        self.assertEqual(stacks[ALLOCATOR][0], 13709)
        # DWARF unwinding keeps the caller of glibc's frameless syscall wrapper.
        self.assertEqual(stacks[MUTEX][1][:3], ['syscall', 'std::sys::pal::unix::futex::futex_wait',
                                                '<std::sys::sync::mutex::futex::Mutex>::lock_contended'])

    def test_open_wait_runs_to_the_window_end(self):
        # Even when the switch-out is the last recorded event.
        records = [('2', 5, 'PERF_RECORD_SWITCH IN', []), ('1', 10, 'context-switches', ['a'])]
        self.assertEqual(profile.waits(records, 50), [('1', 40, ['a'])])

    def test_lost_events_refused(self):
        with self.assertRaises(ValueError):
            profile.parse(FIXTURE + '2152705 271706.914950000: PERF_RECORD_LOST lost 3\n')


class Classify(unittest.TestCase):
    def test_recorded_stacks(self):
        stacks = {tid: stack for tid, _, stack in profile.waits(profile.parse(FIXTURE), END)}
        self.assertEqual(profile.classify(stacks[MUTEX]), 'mutex')
        self.assertEqual(profile.classify(stacks[ALLOCATOR]), 'futex')
        self.assertEqual(profile.classify(stacks[IDLE]), 'other')

    def test_spinning_is_not_a_wait(self):
        self.assertEqual(profile.classify(['<std::sys::sync::mutex::futex::Mutex>::lock_contended', 'moq_net::serve']), 'other')


class Reduce(unittest.TestCase):
    def test_recorded_waits_by_class_over_cpu(self):
        intervals = profile.waits(profile.parse(FIXTURE), END)
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

    def test_silent_worker_refused(self):
        intervals = [('1', 5, ['moq_net::serve'])]
        # Worker 2 ran but recorded no switch-out, so the capture missed it.
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, intervals, {'1': 0, '2': 0}, {'1': 1, '2': 1}, 1)
        # Switched out without a CPU advance: the counter is broken.
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0'}, intervals, {'1': 5}, {'1': 5}, 1)

    def test_sleeping_worker_reported_idle(self):
        # Light load at 16 workers leaves some without connections, asleep for the whole window.
        intervals = [('1', 5, ['moq_net::serve'])]
        result, _ = profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, intervals, {'1': 0, '2': 7}, {'1': 1, '2': 7}, 1)
        self.assertEqual(list(result['workers']), ['1'])
        self.assertEqual(result['idle'], ['moq-quic-1'])
        with self.assertRaises(ValueError):
            profile.reduce({'2': 'moq-quic-1'}, [], {'2': 7}, {'2': 7}, 1)

    def test_unresolved_stacks_refused_per_worker(self):
        # One resolved worker must not vouch for another whose unwinding failed.
        intervals = [('1', 5, ['moq_net::serve']), ('2', 5, ['[unknown]', 'syscall'])]
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, intervals, {'1': 0, '2': 0}, {'1': 1, '2': 1}, 1)


class Require(unittest.TestCase):
    def check(self, paranoid):
        with patch.object(profile.Path, 'read_text', return_value=paranoid + '\n'):
            profile.require()

    def test_refusal_names_the_command(self):
        # Ubuntu's default is 4; 2 forbids kernel samples; 1 and 0 cap the locked buffer.
        for paranoid in ['4', '2', '1', '0']:
            with self.assertRaises(ValueError) as refused:
                self.check(paranoid)
            self.assertIn('sudo sysctl -w kernel.perf_event_paranoid=-1', str(refused.exception))
        self.check('-1')


if __name__ == '__main__':
    unittest.main()
