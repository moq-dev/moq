import importlib.util
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('profile', Path(__file__).with_name('profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

BLOCKED = ['syscall', 'std::sys::pal::unix::futex::futex_wait', '<std::sys::sync::mutex::futex::Mutex>::lock_contended',
           'lock<kio::State<moq_net::model::track::TrackState>>', '<moq_net::model::track::GroupExpiry>::is_expired']
INLINED = ['syscall', 'std::sys::pal::unix::futex::futex_wait', 'lock_contended', 'moq_net::serve']
SPINNING = ['<std::sys::sync::mutex::futex::Mutex>::lock_contended', 'moq_net::serve']
CONDVAR = ['syscall', 'std::sys::pal::unix::futex::futex_wait',
           '<std::sys::sync::condvar::futex::Condvar>::wait', 'moq_net::serve']
IDLE = ['epoll_wait', '<mio::poll::Poll>::poll', 'moq_net::serve']


class Parse(unittest.TestCase):
    def test_threads_modules_and_addresses(self):
        text = '''PID 7 - process
TID 7:
#0  0x00007ffff7d2966d - /lib/libc.so.6
#1  0x00005555567d3259 - /bin/moq-relay
TID 8:
#0  0x00007ffff7d2966d - /lib/libc.so.6
'''
        self.assertEqual(profile.parse(text), {
            '7': [('/lib/libc.so.6', 0x7ffff7d2966d), ('/bin/moq-relay', 0x5555567d3259)],
            '8': [('/lib/libc.so.6', 0x7ffff7d2966d)]})

    def test_inlined_frames_and_caller_offsets(self):
        output = '''0x0000000000001000
inner
/src/a.rs:1
outer
/src/b.rs:2
0x0000000000002000
??
??:0
'''
        done = subprocess.CompletedProcess([], 0, output, '')
        with patch.object(profile.subprocess, 'run', return_value=done) as run:
            stacks = profile.symbolize([{'1': [('/bin/relay', 0x2100), ('/bin/relay', 0x1101)]}], {'/bin/relay': 0x100})
        # A caller frame holds a return address, so it resolves one byte earlier.
        self.assertEqual(run.call_args.kwargs['input'], '0x1000\n0x2000\n')
        self.assertEqual(stacks, [{'1': ['??', 'inner', 'outer']}])


class Classify(unittest.TestCase):
    def test_only_blocked_lock_contended_is_a_mutex_wait(self):
        self.assertEqual(profile.classify(BLOCKED), 'mutex')
        self.assertEqual(profile.classify(INLINED), 'mutex')
        self.assertEqual(profile.classify(SPINNING), 'other')
        self.assertEqual(profile.classify(CONDVAR), 'futex')
        self.assertEqual(profile.classify(IDLE), 'other')


class Reduce(unittest.TestCase):
    def test_wait_is_window_share_and_can_exceed_cpu(self):
        samples = [{'1': BLOCKED}, {'1': BLOCKED}, {'1': BLOCKED}, {'1': CONDVAR}]
        result, folded = profile.reduce({'1': 'moq-quic-0'}, samples, {'1': 0}, {'1': 100}, 400)
        row = result['workers']['1']
        self.assertEqual((row['samples'], row['mutex_samples'], row['other_futex_samples']), (4, 3, 1))
        self.assertEqual(row['mutex_wait_ns'], 300)
        self.assertEqual(row['mutex_wait_per_cpu'], 3)
        self.assertEqual(folded, {';'.join(reversed(BLOCKED)): 3})

    def test_unsampled_or_idle_cpu_refused(self):
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0', '2': 'moq-quic-1'}, [{'1': IDLE}], {'1': 0, '2': 0}, {'1': 1, '2': 1}, 1)
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0'}, [{'1': IDLE}], {'1': 5}, {'1': 5}, 1)

    def test_unresolved_stacks_refused(self):
        with self.assertRaises(ValueError):
            profile.reduce({'1': 'moq-quic-0'}, [{'1': ['??', 'syscall']}], {'1': 0}, {'1': 1}, 1)


if __name__ == '__main__':
    unittest.main()
