#!/usr/bin/env python3
"""Record relay worker off-CPU stacks with perf and attribute std mutex waits."""
import argparse
import json
import os
import re
import subprocess
import time
from collections import Counter
from pathlib import Path

EVENT = 'sched:sched_switch'
WORKER = re.compile(r"moq-(?:uring|quic)-[0-9]+")
# The blocking call itself: std's futex wrapper or glibc's (allocator, pthread).
BLOCKED = re.compile(r"futex_wait|futex_abstimed_wait|lll_lock_wait")
# kio::Lock is a std Mutex; condvar, allocator and parking waits are separate.
# An inlined frame carries only its short name.
MUTEX = re.compile(r"(?:\bMutex>?::|^)lock_contended$")
HEADER = re.compile(r"\s*(\d+)\s+(\d+)\.(\d+):\s+(.*)")
FRAME = re.compile(r"\s+[0-9a-f]+\s+(.*?)\s+\((.*)\)$")
# Without these, perf refuses the tracepoint or records unresolvable stacks.
SETUP = """sudo sysctl -w kernel.perf_event_paranoid=-1 kernel.kptr_restrict=0
sudo mount -o remount,mode=755 /sys/kernel/tracing"""


def require():
    """Refuse to record unless the kernel exposes the tracepoint and stacks to this user."""
    settings = {name: Path('/proc/sys/kernel', name).read_text().strip() for name in ('perf_event_paranoid', 'kptr_restrict')}
    tracing = os.access(f'/sys/kernel/tracing/events/{EVENT.replace(":", "/")}/format', os.R_OK)
    if settings != {'perf_event_paranoid': '-1', 'kptr_restrict': '0'} or not tracing:
        state = ', '.join(f'{name}={value}' for name, value in settings.items())
        raise ValueError(f'perf needs {EVENT} and kernel stacks ({state}, tracefs readable={tracing}); run:\n{SETUP}')


def parse(text):
    """Split `perf script` output into (tid, nanoseconds, event, innermost-first frames) records."""
    records, frames = [], None
    for line in text.splitlines():
        if match := HEADER.match(line):
            tid, seconds, fraction, event = match.groups()
            frames = []
            nanoseconds = int(seconds) * 1_000_000_000 + int(fraction.ljust(9, '0'))
            records.append((tid, nanoseconds, event.strip().rstrip(':'), frames))
        elif frames is not None and (match := FRAME.match(line)):
            symbol, dso = match.groups()
            # Kernel frames all show the scheduler; the user stack names the wait.
            if not dso.startswith('['):
                frames.append(re.sub(r'\+0x[0-9a-f]+$', '', symbol))
    return records


def waits(records):
    """Pair each switch-out stack with the thread's next switch-in for an exact off-CPU duration."""
    end = max((record[1] for record in records), default=0)
    pending, result = {}, []
    for tid, at, event, frames in records:
        if event == EVENT:
            pending[tid] = (at, frames)
        elif event.startswith('PERF_RECORD_SWITCH IN') and tid in pending:
            start, stack = pending.pop(tid)
            result.append((tid, at - start, stack))
    # A wait still open when recording stops counts up to the last event.
    result.extend((tid, end - start, stack) for tid, (start, stack) in pending.items())
    return result


def classify(stack):
    """An off-CPU interval is a mutex wait only when blocked in the futex under lock_contended."""
    if not any(BLOCKED.search(frame) for frame in stack):
        return 'other'
    return 'mutex' if any(MUTEX.search(frame) for frame in stack) else 'futex'


def reduce(names, intervals, before, after, window_ns):
    """Sum each worker's off-CPU time by class, as a share of its CPU time."""
    workers, folded = {}, Counter()
    for tid, name in names.items():
        mine = [(wait, stack) for owner, wait, stack in intervals if owner == tid]
        if not mine:
            raise ValueError(f'worker {name} ({tid}) never switched out')
        if not any(re.search(r'\bmoq_|\bkio::', frame) for _, stack in mine for frame in stack):
            raise ValueError(f'worker {name} ({tid}) has no resolved MoQ frames; build with the profiling profile')
        cpu = after[tid] - before[tid]
        if cpu <= 0:
            raise ValueError(f'worker {name} ({tid}) has no advancing CPU counter')
        total = Counter()
        for wait, stack in mine:
            kind = classify(stack)
            total[kind] += wait
            if kind == 'mutex':
                folded[';'.join(reversed(stack))] += wait // 1000
        workers[tid] = {'comm': name, 'switches': len(mine), 'cpu_ns': cpu, 'mutex_wait_ns': total['mutex'],
                        'other_futex_wait_ns': total['futex'], 'other_off_cpu_ns': total['other'],
                        'mutex_wait_per_cpu': total['mutex'] / cpu}
    return {'version': 2, 'window_ns': window_ns, 'workers': workers,
            'scope': 'exact off-CPU time blocked in a futex under std Mutex::lock_contended; spinning is excluded'}, folded


def schedstat(pid, tids):
    return {tid: int((Path('/proc') / str(pid) / 'task' / tid / 'schedstat').read_text().split()[0]) for tid in tids}


def record(pid, delay, duration, directory):
    """Record one steady window of worker switch-outs, then symbolize after the relay is released."""
    require()
    directory.mkdir(parents=True, exist_ok=True)
    tasks = Path('/proc') / str(pid) / 'task'
    names = {task.name: name for task in tasks.iterdir() if WORKER.fullmatch(name := (task / 'comm').read_text().strip())}
    if not names:
        raise ValueError(f'PID {pid} has no moq-uring-N or moq-quic-N workers')
    time.sleep(delay)
    data = directory / 'perf.data'
    before, start = schedstat(pid, names), time.monotonic_ns()
    # The tracepoint fires in the outgoing thread, so -p never sees a worker's switch-in;
    # --switch-events records it. glibc's syscall wrapper keeps no frame pointer, so
    # frame-pointer unwinding drops its caller, lock_contended; DWARF unwinding keeps it.
    subprocess.run(['perf', 'record', '--quiet', '--no-buildid-cache', '-e', EVENT, '--call-graph', 'dwarf', '--switch-events',
                    '-p', str(pid), '-o', str(data), '--', 'sleep', str(duration)], check=True)
    after, window = schedstat(pid, names), time.monotonic_ns() - start
    # Symbols come from the local binaries; never wait on a debuginfod server.
    script = subprocess.run(['perf', 'script', '-i', str(data), '--show-switch-events', '--inline', '--ns',
                             '-F', 'tid,time,event,ip,sym,dso'], capture_output=True, text=True, check=True,
                            env=dict(os.environ, DEBUGINFOD_URLS=''))
    result, folded = reduce(names, waits(parse(script.stdout)), before, after, window)
    (directory / 'lock.json').write_text(json.dumps(result, indent=2) + '\n')
    (directory / 'mutex.folded').write_text(''.join(f'{stack} {count}\n' for stack, count in folded.most_common()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('check', help='refuse unless perf can record the tracepoint with kernel stacks')
    capture = commands.add_parser('record', help='record worker off-CPU stacks of PID into DIRECTORY')
    capture.add_argument('pid', type=int)
    capture.add_argument('directory', type=Path)
    capture.add_argument('--delay', type=float, default=0)
    capture.add_argument('--duration', type=float, required=True)
    args = parser.parse_args()
    try:
        if args.command == 'check':
            require()
        else:
            record(args.pid, args.delay, args.duration, args.directory)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'profile: {error}\n')


if __name__ == '__main__':
    main()
