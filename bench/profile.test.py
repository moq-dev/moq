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
    @staticmethod
    def records(tid='1', stack=None):
        return [(tid, 10, 'PERF_RECORD_SWITCH IN', []),
                (tid, 20, 'context-switches', stack or ['moq_net::serve']),
                (tid, 21, 'PERF_RECORD_SWITCH OUT', []),
                (tid, 40, 'PERF_RECORD_SWITCH IN', [])]

    def test_recorded_cpu_ignores_outer_schedstat_window(self):
        result, _ = profile.reduce({'1': 'moq-quic-0'}, self.records(), {'1': 0}, {'1': 999})
        self.assertEqual(result['version'], 3)
        self.assertEqual(result['window_ns'], 30)
        # CPU ends at the sampled switch-out, where the wait begins, so the two tile the window.
        self.assertEqual(result['workers']['1']['cpu_ns'], 10)
        self.assertEqual(result['workers']['1']['other_off_cpu_ns'], 20)
        # A worker with recorded switches never uses schedstat, even if it did not advance.
        unchanged, _ = profile.reduce({'1': 'moq-quic-0'}, self.records(), {'1': 7}, {'1': 7})
        self.assertEqual(result, unchanged)

    def test_first_out_and_last_in_clip_to_shared_window(self):
        records = self.records('2') + [
            ('1', 15, 'context-switches', ['moq_net::serve']),
            ('1', 16, 'PERF_RECORD_SWITCH OUT preempt', []),
            ('1', 30, 'PERF_RECORD_SWITCH IN', [])]
        result, _ = profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 500})
        self.assertEqual(result['workers']['1']['cpu_ns'], 15)  # [10,15] + [30,40]
        self.assertEqual(result['workers']['1']['other_off_cpu_ns'], 15)

    def test_worker_running_only_before_first_sample(self):
        # Switch records begin before sampled stacks. The worker then sleeps to the end.
        records = [('1', 10, 'PERF_RECORD_SWITCH IN', []),
                   ('1', 20, 'context-switches', ['moq_net::serve']),
                   ('1', 21, 'PERF_RECORD_SWITCH OUT', []),
                   ('2', 50, 'PERF_RECORD_SWITCH IN', [])]
        result, _ = profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 1000})
        self.assertEqual(result['workers']['1']['cpu_ns'], 10)
        self.assertEqual(result['workers']['1']['other_off_cpu_ns'], 30)

    def test_recorded_waits_by_class_over_cpu(self):
        records = profile.parse(FIXTURE)
        start = min(row[1] for row in records) - 3000
        end = max(row[1] for row in records) + 3000
        names = {IDLE: 'moq-quic-0', MUTEX: 'moq-quic-1', ALLOCATOR: 'moq-quic-2'}
        # The historical stack fixture omits OUT sideband records; restore them beside samples.
        records += [(tid, at, 'PERF_RECORD_SWITCH OUT', []) for tid, at, event, _ in records if event == profile.EVENT]
        records += [('outside', start, 'PERF_RECORD_SWITCH IN', []), ('outside', end, 'PERF_RECORD_SWITCH OUT', [])]
        result, folded = profile.reduce(names, records, dict.fromkeys(names, 0), dict.fromkeys(names, 999))
        rows = result['workers']
        self.assertEqual(rows[MUTEX]['mutex_wait_ns'], 6315)
        self.assertAlmostEqual(rows[MUTEX]['mutex_wait_per_cpu'], 6315 / rows[MUTEX]['cpu_ns'])
        self.assertEqual(rows[ALLOCATOR]['other_futex_wait_ns'], 13709)
        self.assertEqual(rows[IDLE]['other_off_cpu_ns'], 21661)
        [(stack, weight)] = folded.items()
        self.assertTrue(stack.endswith('lock_contended;std::sys::pal::unix::futex::futex_wait;syscall'))
        self.assertEqual(weight, 6)

    def test_silent_worker_refused(self):
        with self.assertRaisesRegex(ValueError, 'ambiguous'):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, self.records(),
                           {'1': 0, '2': 0}, {'1': 1, '2': 1})

    def test_switches_without_samples_refused(self):
        records = [row for row in self.records() if row[2] != profile.EVENT]
        with self.assertRaisesRegex(ValueError, 'no sampled switch-out'):
            profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 1})

    def test_sleeping_worker_reported_idle(self):
        result, _ = profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, self.records(),
                                   {'1': 0, '2': 7}, {'1': 1, '2': 7})
        self.assertEqual(list(result['workers']), ['1'])
        self.assertEqual(result['idle'], ['moq-quic-1'])
        with self.assertRaisesRegex(ValueError, 'no worker ran'):
            profile.reduce({'2': 'moq-quic-1'}, self.records(), {'2': 7}, {'2': 7})

    def test_unresolved_stacks_refused_per_worker(self):
        records = self.records() + self.records('2', ['[unknown]', 'syscall'])
        with self.assertRaisesRegex(ValueError, 'no resolved MoQ frames'):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, records,
                           {'1': 0, '2': 0}, {'1': 1, '2': 1})

    def test_duplicate_same_switch_is_idempotent(self):
        records = self.records()
        expected, _ = profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 1})
        # Repeating either state at the same instant adds no interval.
        duplicate = records + [row for row in records if row[2].startswith('PERF_RECORD_SWITCH ')]
        result, _ = profile.reduce({'1': 'moq-quic-0'}, duplicate, {'1': 0}, {'1': 1})
        self.assertEqual(result, expected)

    def test_invalid_switch_stream_refused(self):
        records = self.records() + [('1', 15, 'PERF_RECORD_SWITCH IN', [])]
        with self.assertRaisesRegex(ValueError, 'nonalternating'):
            profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 1})
        for records in [[], [('1', 10, 'PERF_RECORD_SWITCH IN', [])]]:
            with self.assertRaisesRegex(ValueError, 'no recorded switch window'):
                profile.reduce({'1': 'moq-quic-0'}, records, {'1': 0}, {'1': 1})


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
