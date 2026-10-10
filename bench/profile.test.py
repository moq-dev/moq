import importlib.util
import unittest
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
        for samples in [[], [self.sample([])]]:
            with self.assertRaises(ValueError):
                profile.reduce(samples, {'1': 1}, {'1': 2})


if __name__ == '__main__':
    unittest.main()
