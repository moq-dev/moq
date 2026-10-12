#!/usr/bin/env python3
"""Sample relay worker stacks with eu-stack and attribute std mutex waits."""
import argparse
import ctypes
import json
import os
import re
import subprocess
import time
from collections import Counter, defaultdict
from pathlib import Path

PR_SET_PTRACER = 0x59616D61
# Each sample stops the relay while it unwinds; 20 Hz bounds that, and the control run measures it.
INTERVAL_NS = 50_000_000
WORKER = re.compile(r"moq-(?:uring|quic)-[0-9]+")
# The blocking call itself: std's futex wrapper or glibc's (allocator, pthread).
BLOCKED = re.compile(r"futex_wait|futex_abstimed_wait|lll_lock_wait")
# kio::Lock is a std Mutex; condvar, allocator and parking waits are separate.
# An inlined frame carries only its short name.
MUTEX = re.compile(r"(?:\bMutex>?::|^)lock_contended$")
FRAME = re.compile(r"#\d+\s+0x([0-9a-f]+)\s+-\s+(\S+)")


def parse(text):
    """Split `eu-stack -q -m` output into innermost-first (module, address) lists keyed by TID."""
    stacks, frames = {}, None
    for line in text.splitlines():
        if line.startswith('TID '):
            frames = stacks.setdefault(line[4:].rstrip(':'), [])
        elif frames is not None and (match := FRAME.match(line.strip())):
            frames.append((match.group(2), int(match.group(1), 16)))
    return stacks


def bases(pid):
    """Load address of each mapped file, so addresses become file-relative for addr2line."""
    result = {}
    for line in (Path('/proc') / str(pid) / 'maps').read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) == 6 and fields[2] == '00000000' and fields[5].startswith('/'):
            result.setdefault(fields[5], int(fields[0].split('-')[0], 16))
    return result


def addr2line(module, offsets):
    """Resolve file offsets to innermost-first function names, inlined frames included."""
    done = subprocess.run(['addr2line', '-a', '-f', '-i', '-C', '-e', module],
                          input=''.join(f'{offset:#x}\n' for offset in offsets),
                          capture_output=True, text=True, check=True)
    # Each address line is followed by (function, file:line) pairs, innermost first.
    result, names, position = {}, [], 0
    for line in done.stdout.splitlines():
        if line.startswith('0x'):
            names, position = result.setdefault(int(line, 16), []), 0
            continue
        if position % 2 == 0:
            names.append(line)
        position += 1
    return result


def symbolize(samples, loaded):
    """Replace (module, address) frames with names; caller frames use their call instruction."""
    wanted = defaultdict(set)
    for sample in samples:
        for stack in sample.values():
            for depth, (module, address) in enumerate(stack):
                if module in loaded:
                    wanted[module].add(address - loaded[module] - (depth > 0))
    names = {module: addr2line(module, sorted(offsets)) for module, offsets in wanted.items()}
    return [{tid: [name for depth, (module, address) in enumerate(stack)
                   for name in names.get(module, {}).get(address - loaded.get(module, 0) - (depth > 0), ['??'])]
             for tid, stack in sample.items()} for sample in samples]


def classify(stack):
    """A sample is a mutex wait only while blocked in the futex under lock_contended."""
    if not any(BLOCKED.search(frame) for frame in stack):
        return 'other'
    return 'mutex' if any(MUTEX.search(frame) for frame in stack) else 'futex'


def reduce(names, samples, before, after, window_ns):
    """Turn per-sample stacks into each worker's wait share of wall and CPU time."""
    if not any(re.search(r'\bmoq_|\bkio::', frame) for sample in samples for tid in names for frame in sample.get(tid, [])):
        raise ValueError('worker stacks contain no resolved MoQ frames; build with the profiling profile')
    workers, folded = {}, Counter()
    for tid, name in names.items():
        stacks = [sample[tid] for sample in samples if tid in sample]
        if not stacks:
            raise ValueError(f'worker {name} ({tid}) was never sampled')
        cpu = after[tid] - before[tid]
        if cpu <= 0:
            raise ValueError(f'worker {name} ({tid}) has no advancing CPU counter')
        counts = Counter(classify(stack) for stack in stacks)
        wait = window_ns * counts['mutex'] // len(stacks)
        workers[tid] = {'comm': name, 'samples': len(stacks), 'mutex_samples': counts['mutex'],
                        'other_futex_samples': counts['futex'], 'cpu_ns': cpu,
                        'mutex_wait_ns': wait, 'mutex_wait_per_cpu': wait / cpu}
        folded.update(';'.join(reversed(stack)) for stack in stacks if classify(stack) == 'mutex')
    return {'version': 1, 'window_ns': window_ns, 'samples': len(samples), 'workers': workers,
            'scope': 'wall-clock stack samples blocked in a futex under std Mutex::lock_contended; spinning is excluded'}, folded


def schedstat(pid, tids):
    return {tid: int((Path('/proc') / str(pid) / 'task' / tid / 'schedstat').read_text().split()[0]) for tid in tids}


def sample(pid, delay, duration, directory):
    """Capture one steady window, then symbolize once after the relay is released."""
    directory.mkdir(parents=True, exist_ok=True)
    tasks = Path('/proc') / str(pid) / 'task'
    names = {task.name: name for task in tasks.iterdir() if WORKER.fullmatch(name := (task / 'comm').read_text().strip())}
    if not names:
        raise ValueError(f'PID {pid} has no moq-uring-N or moq-quic-N workers')
    loaded = bases(pid)
    time.sleep(delay)
    before, start = schedstat(pid, names), time.monotonic_ns()
    raw = []
    with open(directory / 'eu-stack.log', 'w') as log:
        while (now := time.monotonic_ns()) - start < duration * 1_000_000_000:
            # Exit 1 marks a partial backtrace, such as a frame without CFI; 2 is no frames at all.
            done = subprocess.run(['eu-stack', '-q', '-m', '-p', str(pid)], capture_output=True, text=True)
            log.write(done.stderr)
            if done.returncode > 1:
                raise ValueError(f'eu-stack failed ({done.returncode}): {done.stderr.strip()}')
            raw.append(parse(done.stdout))
            time.sleep(max(0, now + INTERVAL_NS - time.monotonic_ns()) / 1e9)
    after, window = schedstat(pid, names), time.monotonic_ns() - start
    result, folded = reduce(names, symbolize(raw, loaded), before, after, window)
    (directory / 'lock.json').write_text(json.dumps(result, indent=2) + '\n')
    (directory / 'mutex.folded').write_text(''.join(f'{stack} {count}\n' for stack, count in folded.most_common()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    traceable = commands.add_parser('traceable', help='let TRACER and its descendants ptrace COMMAND, then exec it')
    traceable.add_argument('tracer', type=int)
    traceable.add_argument('argv', nargs=argparse.REMAINDER)
    lock = commands.add_parser('lock', help='sample worker stacks of PID into DIRECTORY')
    lock.add_argument('pid', type=int)
    lock.add_argument('directory', type=Path)
    lock.add_argument('--delay', type=float, default=0)
    lock.add_argument('--duration', type=float, required=True)
    args = parser.parse_args()
    try:
        if args.command == 'traceable':
            # Yama's default ptrace_scope=1 only lets ancestors attach, and eu-stack is not one.
            if ctypes.CDLL(None, use_errno=True).prctl(PR_SET_PTRACER, args.tracer, 0, 0, 0):
                raise OSError(ctypes.get_errno(), 'PR_SET_PTRACER failed')
            argv = args.argv[1:] if args.argv[:1] == ['--'] else args.argv
            os.execvp(argv[0], argv)
        sample(args.pid, args.delay, args.duration, args.directory)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f'profile: {error}\n')


if __name__ == '__main__':
    main()
