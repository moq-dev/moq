import importlib.util
import json
import os
import signal
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('profile', Path(__file__).with_name('profile.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)

spec = importlib.util.spec_from_file_location('runtime', Path(__file__).with_name('runtime.py'))
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class Runtime(unittest.TestCase):
    def test_workers_must_match_the_backend_and_count(self):
        with tempfile.TemporaryDirectory() as directory:
            tasks = Path(directory)
            for backend, prefix in [('tokio-workers', 'moq-quic'), ('io-uring-workers', 'moq-uring')]:
                with self.subTest(backend=backend):
                    for path in tasks.glob('*/comm'):
                        path.unlink()
                    names = [f'{prefix}-0', f'{prefix}-1', 'tokio-runtime-w', 'moq-relay']
                    for index, name in enumerate(names):
                        task = tasks / str(index)
                        task.mkdir(exist_ok=True)
                        (task / 'comm').write_text(name + '\n')
                    self.assertEqual(runtime.verify(tasks, backend, 2)['workers'], names[:2])
                    other = 'io-uring-workers' if backend == 'tokio-workers' else 'tokio-workers'
                    for selected, count in [(other, 2), (backend, 1), (backend, 3)]:
                        with self.assertRaises(ValueError):
                            runtime.verify(tasks, selected, count)
                    (tasks / '2/comm').write_text('moq-quic-2' if prefix == 'moq-uring' else 'moq-uring-2')
                    with self.assertRaises(ValueError):
                        runtime.verify(tasks, backend, 2)

    def test_profile_rejects_invalid_selection_before_building(self):
        root = Path(__file__).resolve().parent.parent
        for environment, diagnostic in [
            ({'BENCH_PROFILE_RUNTIME': 'tokio-shared'}, 'profile runtime'),
            ({'BENCH_PROFILE_WORKERS': '0'}, 'profile workers'),
            ({'BENCH_PROFILE_WORKERS': '1 257'}, 'profile workers'),
            ({'BENCH_PROFILE_WORKERS': '1 nope'}, 'profile workers'),
            ({'BENCH_PROFILE_WORKERS': '01'}, 'profile workers'),
            ({'BENCH_PROFILE_WORKERS': '999999999999999999999999'}, 'profile workers'),
        ]:
            with self.subTest(environment=environment):
                result = subprocess.run(['bash', str(root / 'bench/run.sh'), '--profile'],
                                        env=dict(os.environ, BENCH_PROFILE_MODES='cpu', **environment),
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(diagnostic, result.stderr)


FIXTURE = Path(__file__).with_name('profile.test.txt').read_text()
IDLE, MUTEX, ALLOCATOR = '2152705', '2152709', '2152707'
# Every fixture wait closes, so the window end only has to follow the last event.
END = 271707 * 1_000_000_000


class Orchestration(unittest.TestCase):
    def test_old_base_uses_current_harness_and_generator(self):
        root = Path(__file__).resolve().parent.parent
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory) / 'repo'
            bench = checkout / 'bench'
            bench.mkdir(parents=True)
            (bench / 'workloads').mkdir()
            (bench / 'run.sh').write_text('exit 87\n')
            environment = dict(os.environ, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
                               GIT_AUTHOR_NAME='Test', GIT_AUTHOR_EMAIL='test@example.com',
                               GIT_COMMITTER_NAME='Test', GIT_COMMITTER_EMAIL='test@example.com')
            for command in [['init', '-q'], ['add', '.'], ['commit', '-qm', 'old harness']]:
                subprocess.run(['git', *command], cwd=checkout, env=environment, check=True,
                               capture_output=True)
            (bench / 'run.sh').write_text((root / 'bench/run.sh').read_text())
            (bench / 'metadata.py').write_text('')
            (bench / 'profile.py').write_text('')
            (bench / 'relay.sh').write_text('''PROFILE=
run_workload() {
    printf '%s\\t%s\\t%s\\t%s\\t%s\\n' "$1" "$4" "$5" "$LOAD_BIN" "$HOST_BIN" >> "$RUN/calls.tsv"
    local directory=$RUN/relay/$1/$3
    mkdir -p "$directory/cpu"
    printf '{"recv_fps":100,"cpu_cores":1}\\n' > "$directory/summary.json"
    printf '{"samples":999,"sampled_cores":1}\\n' > "$directory/cpu/cpu.json"
}
''')
            commands = Path(directory) / 'bin'
            commands.mkdir()
            cargo = commands / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
output = Path(os.environ['CARGO_TARGET_DIR']) / 'profiling'
output.mkdir(parents=True, exist_ok=True)
check = output / 'moq-bench-uring-check'
check.write_text('#!/bin/sh\\nexit 0\\n')
check.chmod(0o755)
''')
            cargo.chmod(0o755)
            artifacts = Path(directory) / 'evidence'
            environment.update(PATH=str(commands) + os.pathsep + os.environ['PATH'],
                               BENCH_ARTIFACTS=str(artifacts), BENCH_PROFILE_MODES='cpu',
                               BENCH_PROFILE_WORKLOAD='fixture', BENCH_PROFILE_WORKERS='2',
                               BENCH_PROFILE_RUNTIME='io-uring-workers', BENCH_BASE='HEAD',
                               CARGO_TARGET_DIR=str(Path(directory) / 'target'))
            result = subprocess.run(['bash', 'bench/run.sh', '--profile'], cwd=checkout,
                                    env=environment, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            run, = artifacts.glob('run.*')
            calls = [line.split('\t') for line in (run / 'calls.tsv').read_text().splitlines()]
            self.assertEqual([call[0] for call in calls],
                             ['base/control-cpu-2', 'base/cpu-2',
                              'current/control-cpu-2', 'current/cpu-2'])
            for _, backend, count, generator, sampler in calls:
                self.assertEqual((backend, count), ('io-uring-workers', '2'))
                self.assertIn('/current/profile/profiling/', generator)
                self.assertIn('/current/profile/profiling/', sampler)
            self.assertFalse((run / 'base').exists(), 'temporary base worktree must be removed')


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


CPU_FIXTURE = '''\
11 100.000000001: cpu-clock:
	    ffffffff81000000 [unknown] ([kernel.kallsyms])
	    ffffffff81000100 [unknown] ([kernel.kallsyms])
	    7ffff7d2c07a sendmsg (/nix/store/glibc/lib/libc.so.6)
	    5555569d8ae4 <moq_quic::Endpoint>::send (/moq/moq-relay)
	    5555569d8608 std::thread::run (/moq/moq-relay)
12 100.001000001: cpu-clock:
	    5555569d8ae4 <moq_net::Session>::poll (/moq/moq-relay)
	    5555569d8608 std::thread::run (/moq/moq-relay)
13 100.002000001: cpu-clock:
	    5555569d8608 std::thread::run (/moq/moq-relay)
'''


class Cpu(unittest.TestCase):
    def test_kernel_time_is_a_leaf_under_its_user_stack(self):
        [(_, _, event, frames), *_] = profile.parse(CPU_FIXTURE, kernel=True)
        self.assertEqual(event, 'cpu-clock')
        self.assertEqual(frames[:2], ['[kernel]', 'sendmsg'])
        # Lock waits keep dropping kernel frames outright.
        self.assertEqual(profile.parse(CPU_FIXTURE)[0][3][0], 'sendmsg')

    def test_workers_fold_into_their_role(self):
        names = {'11': 'moq-quic-0', '12': 'moq-quic-1'}
        result, folded = profile.fold(names, profile.parse(CPU_FIXTURE, kernel=True), 0.5)
        self.assertEqual(result['samples'], 3)
        self.assertEqual(result['threads'], {'moq-quic-0': 1, 'moq-quic-1': 1, 'tid-13': 1})
        self.assertAlmostEqual(result['sampled_cores'], 3 / 999 / 0.5)
        self.assertEqual(folded['moq-quic;std::thread::run;<moq_quic::Endpoint>::send;sendmsg;[kernel]'], 1)
        self.assertEqual(folded['moq-quic;std::thread::run;<moq_net::Session>::poll'], 1)

    def test_empty_or_unresolved_refused(self):
        with self.assertRaisesRegex(ValueError, 'no CPU samples'):
            profile.fold({}, [], 1)
        unresolved = [('1', 0, 'cpu-clock', ['[kernel]', 'syscall'])]
        with self.assertRaisesRegex(ValueError, 'no CPU sample has resolved MoQ frames'):
            profile.fold({}, unresolved, 1)

    def test_lost_samples_refused(self):
        with self.assertRaises(ValueError):
            profile.parse(CPU_FIXTURE + '11 100.003000000: PERF_RECORD_LOST_SAMPLES lost 3\n', kernel=True)


class Heap(unittest.TestCase):
    def test_usr1_handler_detected(self):
        previous = signal.signal(signal.SIGUSR1, signal.SIG_DFL)
        try:
            self.assertFalse(profile.catches_usr1(os.getpid()))
            signal.signal(signal.SIGUSR1, lambda *_: None)
            self.assertTrue(profile.catches_usr1(os.getpid()))
        finally:
            signal.signal(signal.SIGUSR1, previous)

    def test_report_scales_churn_by_the_steady_window(self):
        def jeprof(binary, dump, *args):
            if '--collapsed' in args:
                return 'moq_relay::main;<moq_net::Session>::run;alloc 4096\nmoq_relay::main;freed 0\n'
            totals = {'--inuse_space': {'before.heap': 1 << 20, 'steady-end.heap': 3 << 20},
                      '--alloc_space': {'steady-end.heap': 20 << 20}, '--alloc_objects': {'steady-end.heap': 1000}}
            kind = next(arg for arg in args if arg in totals)
            return f'Total: {totals[kind].get(dump.name, 2 << 20)} B\n'

        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            (directory / 'snapshots.tsv').write_text(
                'before\t0\nsteady-start\t1000000000\nsteady-end\t6000000000\nteardown\t8000000000\n')
            with patch.object(profile, 'jeprof', side_effect=jeprof) as calls:
                profile.heap(Path('relay'), directory)
            result = json.loads((directory / 'heap.json').read_text())
            self.assertEqual(result['window_s'], 5)
            self.assertEqual(result['retained_bytes'], {'before': 1 << 20, 'steady-start': 2 << 20,
                                                        'steady-end': 3 << 20, 'teardown': 2 << 20})
            self.assertEqual(result['churn_bytes_per_s'], 4 << 20)
            self.assertEqual(result['churn_allocs_per_s'], 200)
            # Churn is the steady-end accumulation net of steady-start's.
            churn = [call.args for call in calls.call_args_list if '--alloc_space' in call.args]
            self.assertTrue(all(f'--base={directory / "steady-start.heap"}' in args for args in churn))
            # A stack the base cancelled out is dropped from the flame graph.
            self.assertEqual((directory / 'churn.folded').read_text(), 'moq_relay::main;<moq_net::Session>::run;alloc 4096\n')

    def test_teardown_waits_for_the_relay_to_drop_every_session(self):
        clock = iter(range(0, 100, 1))
        with patch.object(profile.time, 'monotonic', side_effect=lambda: next(clock)), \
                patch.object(profile.time, 'sleep'), \
                patch.object(profile, 'sessions', return_value=1), \
                patch.object(profile.os, 'kill') as kill:
            with self.assertRaisesRegex(ValueError, 'still holds sessions'):
                profile.snapshot(1, Path('.'), ['teardown'], 0, 0, settled='http://relay/sessions')
            # A teardown snapshot of live sessions would read as a leak; none is taken.
            kill.assert_not_called()

    def test_missing_snapshot_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / 'snapshots.tsv').write_text('before\t0\n')
            with self.assertRaisesRegex(ValueError, 'missing heap snapshots: steady-start, steady-end, teardown'):
                profile.heap(Path('relay'), Path(directory))


class Require(unittest.TestCase):
    def check(self, mode, paranoid):
        with patch.object(profile.Path, 'read_text', return_value=paranoid + '\n'):
            profile.require(mode)

    def test_refusal_names_the_command(self):
        # Ubuntu's default is 4; 2 forbids kernel samples; 1 and 0 cap the locked buffer.
        for mode in ['lock', 'cpu']:
            for paranoid in ['4', '2', '1', '0']:
                with self.assertRaises(ValueError) as refused:
                    self.check(mode, paranoid)
                self.assertIn('sudo sysctl -w kernel.perf_event_paranoid=-1', str(refused.exception))
            self.check(mode, '-1')

    def test_heap_needs_jeprof(self):
        with patch.object(profile.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'jeprof'):
                profile.require('heap')


if __name__ == '__main__':
    unittest.main()
