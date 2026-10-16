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

EVENT = 'context-switches'
WORKER = re.compile(r"moq-(?:uring|quic)-[0-9]+")
# The blocking call itself: std's futex wrapper or glibc's (allocator, pthread).
BLOCKED = re.compile(r"futex_wait|futex_abstimed_wait|lll_lock_wait")
# kio::Lock is a std Mutex; condvar, allocator and parking waits are separate.
# An inlined frame carries only its short name.
MUTEX = re.compile(r"(?:\bMutex>?::|^)lock_contended$")
HEADER = re.compile(r"\s*(\d+)\s+(\d+)\.(\d+):\s+(.*)")
FRAME = re.compile(r"\s+[0-9a-f]+\s+(.*?)\s+\((.*)\)$")
# A switch-out is sampled in kernel context (paranoid <= 1), and the ring buffer that keeps
# 16 workers from dropping samples exceeds the locked-memory limit unless paranoid is -1.
SETUP = 'sudo sysctl -w kernel.perf_event_paranoid=-1'


def require():
    """Refuse to record unless perf may sample the relay in kernel context with a large buffer."""
    paranoid = int(Path('/proc/sys/kernel/perf_event_paranoid').read_text())
    if paranoid != -1:
        raise ValueError(f'perf needs kernel samples and unlimited buffers (perf_event_paranoid={paranoid}); run:\n{SETUP}')


def parse(text):
    """Split `perf script` output into (tid, nanoseconds, event, innermost-first frames) records."""
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
        if not any(re.search(r'\bmoq_|\bkio::', frame) for _, stack in mine for frame in stack):
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
    before = schedstat(pid, names)
    # A period of 1 samples every switch-out. The sample fires in the outgoing thread, so -p
    # never sees a worker's switch-in; --switch-events records it. glibc's syscall wrapper
    # keeps no frame pointer, so frame-pointer unwinding drops its caller, lock_contended;
    # DWARF unwinding keeps it. Switch timestamps bound CPU and open waits alike.
    # The default 512 KiB buffer drops samples at 16 workers; 2 MiB does not.
    subprocess.run(['perf', 'record', '--quiet', '--no-buildid-cache', '-e', EVENT, '-c', '1', '--call-graph', 'dwarf', '--switch-events',
                    '-k', 'monotonic', '-m', '2M', '-p', str(pid), '-o', str(data), '--', 'sleep', str(duration)], check=True)
    after = schedstat(pid, names)
    # Symbols come from the local binaries; never wait on a debuginfod server.
    script = subprocess.run(['perf', 'script', '-i', str(data), '--show-switch-events', '--show-lost-events', '--inline', '--ns',
                             '-F', 'tid,time,event,ip,sym,dso'], capture_output=True, text=True, check=True,
                            env=dict(os.environ, DEBUGINFOD_URLS=''))
    result, folded = reduce(names, parse(script.stdout), before, after)
    (directory / 'lock.json').write_text(json.dumps(result, indent=2) + '\n')
    (directory / 'mutex.folded').write_text(''.join(f'{stack} {count}\n' for stack, count in folded.most_common()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('check', help='refuse unless perf can sample the relay in kernel context')
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
