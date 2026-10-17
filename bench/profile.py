#!/usr/bin/env python3
"""Capture a relay profile: std mutex waits or CPU stacks with perf, or jemalloc heap snapshots."""
import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import time
import urllib.request
from collections import Counter
from pathlib import Path

EVENT = 'context-switches'
WORKER = re.compile(r"moq-(?:uring|quic)-[0-9]+")
# The blocking call itself: std's futex wrapper or glibc's (allocator, pthread).
BLOCKED = re.compile(r"futex_wait|futex_abstimed_wait|lll_lock_wait")
# kio::Lock is a std Mutex; condvar, allocator and parking waits are separate.
# An inlined frame carries only its short name.
MUTEX = re.compile(r"(?:\bMutex>?::|^)lock_contended$")
HEADER = re.compile(r"\s*(\d+)\s+(\d+)\.(\d+):\s+(.*)")
FRAME = re.compile(r"\s+[0-9a-f]+\s+(.*?)\s+\((.*)\)$")
RESOLVED = re.compile(r'\bmoq_|\bkio::')
CPU_EVENT = 'cpu-clock'
# Off the 1 kHz tick, so sampling does not run in lockstep with periodic timers.
FREQUENCY = 999
# Both perf modes sample in kernel context (paranoid <= 1): a switch-out happens there, and
# without kernel samples, time in sendmsg and recvmsg would vanish from CPU stacks. Their
# per-thread ring buffers exceed the locked-memory limit unless paranoid is -1.
SETUP = 'sudo sysctl -w kernel.perf_event_paranoid=-1'
SNAPSHOTS = ['before', 'steady-start', 'steady-end', 'teardown']


def require(mode):
    """Refuse a capture this host cannot take, naming the setup that would allow it."""
    if mode == 'heap':
        if not shutil.which('jeprof'):
            raise ValueError('heap profiles need jeprof; run through nix develop')
        return
    paranoid = int(Path('/proc/sys/kernel/perf_event_paranoid').read_text())
    if paranoid != -1:
        raise ValueError(f'perf needs kernel samples and unlimited buffers (perf_event_paranoid={paranoid}); run:\n{SETUP}')


def parse(text, kernel=False):
    """Split `perf script` output into (tid, nanoseconds, event, innermost-first frames) records.

    Kernel frames are dropped; with `kernel`, a sample taken in the kernel keeps a `[kernel]` leaf.
    """
    records, frames = [], None
    for line in text.splitlines():
        if match := HEADER.match(line):
            tid, seconds, fraction, event = match.groups()
            frames = []
            nanoseconds = int(seconds) * 1_000_000_000 + int(fraction.ljust(9, '0'))
            event = event.strip().rstrip(':')
            # A dropped switch would silently shorten or lose a wait.
            if event.startswith('PERF_RECORD_LOST'):
                raise ValueError(f'perf dropped events ({event}); the recording is incomplete')
            records.append((tid, nanoseconds, event, frames))
        elif frames is not None and (match := FRAME.match(line)):
            symbol, dso = match.groups()
            # Kernel frames all show the scheduler; the user stack names the wait.
            if not dso.startswith('['):
                frames.append(re.sub(r'\+0x[0-9a-f]+$', '', symbol))
            elif kernel and not frames and dso.startswith('[kernel'):
                frames.append('[kernel]')
    return records


def waits(records, end):
    """Pair each switch-out stack with the thread's next switch-in for an exact off-CPU duration."""
    pending, result = {}, []
    for tid, at, event, frames in records:
        if event == EVENT:
            pending[tid] = (at, frames)
        elif event.startswith('PERF_RECORD_SWITCH IN') and tid in pending:
            start, stack = pending.pop(tid)
            result.append((tid, at - start, stack))
    # A wait still open when recording stops counts up to the end of the window.
    result.extend((tid, end - start, stack) for tid, (start, stack) in pending.items())
    return result


def classify(stack):
    """An off-CPU interval is a mutex wait only when blocked in the futex under lock_contended."""
    if not any(BLOCKED.search(frame) for frame in stack):
        return 'other'
    return 'mutex' if any(MUTEX.search(frame) for frame in stack) else 'futex'


def reduce(names, records, before, after):
    """Sum waits and on-CPU intervals within the same recorded switch window."""
    records = sorted((row for row in records if row[2] == EVENT or row[2].startswith('PERF_RECORD_SWITCH ')),
                     key=lambda row: row[1])
    if not records or records[0][1] == records[-1][1]:
        raise ValueError('no recorded switch window')
    start, end = records[0][1], records[-1][1]
    intervals = waits(records, end)
    workers, idle, folded = {}, [], Counter()
    for tid, name in names.items():
        own = [(at, event) for owner, at, event, _ in records if owner == tid]
        switches = [(at, event.startswith('PERF_RECORD_SWITCH IN')) for at, event in own if event != EVENT]
        if not switches:
            # Schedstat spans perf startup too, so an advance cannot prove CPU in this window.
            if after[tid] == before[tid]:
                idle.append(name)
                continue
            raise ValueError(f'worker {name} ({tid}) has no switch records; CPU in the window is ambiguous')
        mine = [(wait, stack) for owner, wait, stack in intervals if owner == tid]
        if not mine:
            raise ValueError(f'worker {name} ({tid}) has switch records but no sampled switch-out')
        # The first switch-out means the worker was running at the window start.
        running, last, sampled, cpu = not switches[0][1], start, None, 0
        for at, event in own:
            if event == EVENT:
                # waits() starts the off-CPU interval at the sample, which precedes the sideband OUT.
                if running:
                    sampled = at
                continue
            entering = event.startswith('PERF_RECORD_SWITCH IN')
            if entering == running:
                # An identical observation contributes no new interval. Distinct timestamps still refuse.
                if at == last:
                    continue
                raise ValueError(f'worker {name} ({tid}) has nonalternating switch records')
            if running:
                cpu += (at if sampled is None else sampled) - last
            running, last, sampled = entering, at, None
        if running:
            cpu += end - last
        if not any(RESOLVED.search(frame) for _, stack in mine for frame in stack):
            raise ValueError(f'worker {name} ({tid}) has no resolved MoQ frames; build with the profiling profile and frame pointers')
        if cpu <= 0:
            raise ValueError(f'worker {name} ({tid}) has no recorded CPU interval')
        total = Counter()
        for wait, stack in mine:
            kind = classify(stack)
            total[kind] += wait
            if kind == 'mutex':
                folded[';'.join(reversed(stack))] += wait // 1000
        workers[tid] = {'comm': name, 'switches': len(mine), 'cpu_ns': cpu, 'mutex_wait_ns': total['mutex'],
                        'other_futex_wait_ns': total['futex'], 'other_off_cpu_ns': total['other'],
                        'mutex_wait_per_cpu': total['mutex'] / cpu}
    if not workers:
        raise ValueError('no worker ran during the window')
    return {'version': 3, 'window_ns': end - start, 'workers': workers, 'idle': sorted(idle),
            'scope': 'exact off-CPU time blocked in a futex under std Mutex::lock_contended; spinning is excluded'}, folded


def schedstat(pid, tids):
    return {tid: int((Path('/proc') / str(pid) / 'task' / tid / 'schedstat').read_text().split()[0]) for tid in tids}


def perf_script(data, *args):
    # Symbols come from the local binaries; never wait on a debuginfod server.
    return subprocess.run(['perf', 'script', '-i', str(data), *args, '--show-lost-events', '--inline', '--ns',
                           '-F', 'tid,time,event,ip,sym,dso'], capture_output=True, text=True, check=True,
                          env=dict(os.environ, DEBUGINFOD_URLS='')).stdout


def lock(pid, delay, duration, directory):
    """Record one steady window of worker switch-outs, then symbolize after the relay is released."""
    require('lock')
    directory.mkdir(parents=True, exist_ok=True)
    tasks = Path('/proc') / str(pid) / 'task'
    names = {task.name: name for task in tasks.iterdir() if WORKER.fullmatch(name := (task / 'comm').read_text().strip())}
    if not names:
        raise ValueError(f'PID {pid} has no moq-uring-N or moq-quic-N workers')
    time.sleep(delay)
    data = directory / 'perf.data'
    before = schedstat(pid, names)
    # A period of 1 samples every switch-out. The sample fires in the outgoing thread, so -p
    # never sees a worker's switch-in; --switch-events records it. glibc's syscall wrapper
    # keeps no frame pointer, so frame-pointer unwinding drops its caller, lock_contended;
    # DWARF unwinding keeps it. Switch timestamps bound CPU and open waits alike.
    # The default 512 KiB buffer drops samples at 16 workers; 2 MiB does not.
    subprocess.run(['perf', 'record', '--quiet', '--no-buildid-cache', '-e', EVENT, '-c', '1', '--call-graph', 'dwarf', '--switch-events',
                    '-k', 'monotonic', '-m', '2M', '-p', str(pid), '-o', str(data), '--', 'sleep', str(duration)], check=True)
    after = schedstat(pid, names)
    result, folded = reduce(names, parse(perf_script(data, '--show-switch-events')), before, after)
    (directory / 'lock.json').write_text(json.dumps(result, indent=2) + '\n')
    (directory / 'mutex.folded').write_text(''.join(f'{stack} {count}\n' for stack, count in folded.most_common()))


def threads(pid):
    names = {}
    for task in (Path('/proc') / str(pid) / 'task').iterdir():
        try:
            names[task.name] = (task / 'comm').read_text().strip()
        except FileNotFoundError:
            pass
    return names


def fold(names, records, seconds):
    """Count CPU samples per relay thread and fold their stacks under the thread's role."""
    samples = [(tid, frames) for tid, _, event, frames in records if event.startswith(CPU_EVENT)]
    if not samples:
        raise ValueError('perf recorded no CPU samples for the relay')
    if not any(RESOLVED.search(frame) for _, frames in samples for frame in frames):
        raise ValueError('no CPU sample has resolved MoQ frames; build with the profiling profile and frame pointers')
    threads, folded = Counter(), Counter()
    for tid, frames in samples:
        name = names.get(tid, f'tid-{tid}')
        threads[name] += 1
        # Workers share a role, so moq-quic-0..N fold into one moq-quic tower.
        folded[';'.join([re.sub(r'-[0-9]+$', '', name), *reversed(frames)])] += 1
    return {'version': 1, 'event': CPU_EVENT, 'frequency_hz': FREQUENCY, 'window_s': seconds, 'samples': len(samples),
            'sampled_cores': len(samples) / FREQUENCY / seconds, 'threads': dict(threads.most_common()),
            'scope': 'every relay thread, user and kernel time; kernel time is a [kernel] leaf under its user stack'}, folded


def cpu(pid, delay, duration, directory):
    """Sample every relay thread's CPU stacks for one steady window."""
    require('cpu')
    directory.mkdir(parents=True, exist_ok=True)
    time.sleep(delay)
    data = directory / 'perf.data'
    names = threads(pid)
    # Frame pointers, unlike lock waits' DWARF stacks, keep a 999 Hz capture small enough not
    # to drop samples. A frameless libc leaf (memcpy, sendmsg) hides only its direct caller.
    subprocess.run(['perf', 'record', '--quiet', '--no-buildid-cache', '-e', CPU_EVENT, '-F', str(FREQUENCY),
                    '--call-graph', 'fp', '-p', str(pid), '-o', str(data), '--', 'sleep', str(duration)], check=True)
    # Name threads spawned during the window too; one that already exited keeps its TID.
    names.update(threads(pid))
    result, folded = fold(names, parse(perf_script(data), kernel=True), duration)
    result['pid'] = pid
    (directory / 'cpu.json').write_text(json.dumps(result, indent=2) + '\n')
    (directory / 'cpu.folded').write_text(''.join(f'{stack} {count}\n' for stack, count in folded.most_common()))


def catches_usr1(pid):
    """Whether the relay installed its SIGUSR1 handler; the default action would kill it."""
    status = (Path('/proc') / str(pid) / 'status').read_text()
    caught = int(re.search(r'^SigCgt:\s*([0-9a-f]+)$', status, re.M).group(1), 16)
    return bool(caught >> (signal.SIGUSR1 - 1) & 1)


def holds(pid, path):
    """Whether the process still has PATH open."""
    for fd in (Path('/proc') / str(pid) / 'fd').iterdir():
        try:
            if os.readlink(fd) == str(path):
                return True
        except FileNotFoundError:
            pass
    return False


def poll(condition, timeout, failure):
    deadline = time.monotonic() + timeout
    while not condition():
        if time.monotonic() > deadline:
            raise ValueError(failure)
        time.sleep(0.01)


def sessions(url):
    with urllib.request.urlopen(url, timeout=5) as response:
        return len(json.load(response)['sessions'])


def snapshot(pid, directory, labels, delay, interval, settled=None):
    """Signal one jemalloc heap dump per label and keep each once the relay has closed it.

    With `settled`, a relay's /sessions URL, wait until the relay has dropped every session first.
    """
    time.sleep(delay)
    if settled:
        # Past the relay's 10 s default idle timeout, a session no close ended.
        poll(lambda: sessions(settled) == 0, 15, f'the relay still holds sessions after the load ended ({settled})')
    for index, label in enumerate(labels):
        if index:
            time.sleep(interval)
        poll(lambda: catches_usr1(pid), 5, f'PID {pid} never installed its SIGUSR1 handler; build with moq-relay/jemalloc')
        existing = set(directory.glob('jeprof.*.heap'))
        os.kill(pid, signal.SIGUSR1)
        dumped = []
        poll(lambda: dumped.extend(set(directory.glob('jeprof.*.heap')) - existing) or dumped, 10,
             f'no heap dump appeared in {directory}; the relay needs MALLOC_CONF=prof:true')
        poll(lambda: not holds(pid, dumped[0].resolve()), 10, f'the relay never finished writing {dumped[0]}')
        at = time.monotonic_ns()
        dumped[0].rename(directory / f'{label}.heap')
        with (directory / 'snapshots.tsv').open('a') as log:
            log.write(f'{label}\t{at}\n')


def jeprof(binary, dump, *args):
    return subprocess.run(['jeprof', *args, str(binary), str(dump)], capture_output=True, text=True, check=True).stdout


def collapsed(binary, dump, *args):
    """Folded stacks, without those a --base cancelled out or freed."""
    output = jeprof(binary, dump, '--collapsed', *args)
    return ''.join(line for line in output.splitlines(keepends=True) if int(line.rsplit(' ', 1)[1]) > 0)


def total(binary, dump, *args):
    """jeprof's total for one sample type, scaled up from jemalloc's sampled allocations."""
    output = jeprof(binary, dump, '--text', '--show_bytes', *args)
    if not (match := re.search(r'^Total: (\d+)', output, re.M)):
        raise ValueError(f'jeprof printed no total for {dump}:\n{output}')
    return int(match.group(1))


def heap(binary, directory):
    """Report retained bytes at each snapshot and allocation churn across the steady window."""
    times = dict(line.split('\t') for line in (directory / 'snapshots.tsv').read_text().splitlines())
    if missing := [label for label in SNAPSHOTS if label not in times]:
        raise ValueError(f'missing heap snapshots: {", ".join(missing)}')
    dump = {label: directory / f'{label}.heap' for label in SNAPSHOTS}
    seconds = (int(times['steady-end']) - int(times['steady-start'])) / 1e9
    base = f'--base={dump["steady-start"]}'
    churn = collapsed(binary, dump['steady-end'], '--alloc_space', base)
    if not RESOLVED.search(churn):
        raise ValueError('no allocation stack has resolved MoQ frames; build with the profiling profile and frame pointers')
    (directory / 'churn.folded').write_text(churn)
    (directory / 'retained.folded').write_text(collapsed(binary, dump['steady-end'], '--inuse_space'))
    # What the load left behind once its sessions closed: a leak candidate when it grows.
    (directory / 'teardown.folded').write_text(
        collapsed(binary, dump['teardown'], '--inuse_space', f'--base={dump["before"]}'))
    result = {'version': 1, 'window_s': seconds,
              'retained_bytes': {label: total(binary, dump[label], '--inuse_space') for label in SNAPSHOTS},
              'churn_bytes_per_s': total(binary, dump['steady-end'], '--alloc_space', base) / seconds,
              'churn_allocs_per_s': total(binary, dump['steady-end'], '--alloc_objects', base) / seconds,
              'scope': "jemalloc's sampled estimate (lg_prof_sample); churn spans the steady window"}
    (directory / 'heap.json').write_text(json.dumps(result, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    check = commands.add_parser('check', help='refuse a capture this host cannot take')
    check.add_argument('mode', choices=['lock', 'cpu', 'heap'])
    for name, help in [('lock', 'record worker off-CPU stacks of PID into DIRECTORY'),
                       ('cpu', 'sample CPU stacks of every PID thread into DIRECTORY')]:
        capture = commands.add_parser(name, help=help)
        capture.add_argument('pid', type=int)
        capture.add_argument('directory', type=Path)
        capture.add_argument('--delay', type=float, default=0)
        capture.add_argument('--duration', type=float, required=True)
    dumps = commands.add_parser('snapshot', help='dump the heap of PID, started with prof_prefix DIRECTORY/jeprof, per label')
    dumps.add_argument('pid', type=int)
    dumps.add_argument('directory', type=Path)
    dumps.add_argument('labels', nargs='+', choices=SNAPSHOTS)
    dumps.add_argument('--delay', type=float, default=0)
    dumps.add_argument('--interval', type=float, default=0)
    dumps.add_argument('--settled', help="the relay's /sessions URL, to wait until it holds no session")
    report = commands.add_parser('heap', help='report the snapshots in DIRECTORY against BINARY')
    report.add_argument('binary', type=Path)
    report.add_argument('directory', type=Path)
    args = parser.parse_args()
    try:
        if args.command == 'check':
            require(args.mode)
        elif args.command == 'lock':
            lock(args.pid, args.delay, args.duration, args.directory)
        elif args.command == 'cpu':
            cpu(args.pid, args.delay, args.duration, args.directory)
        elif args.command == 'snapshot':
            snapshot(args.pid, args.directory, args.labels, args.delay, args.interval, args.settled)
        else:
            heap(args.binary, args.directory)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'profile: {error}\n')


if __name__ == '__main__':
    main()
