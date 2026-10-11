#!/usr/bin/env python3
"""Probe capture privileges and reduce explicitly classified worker futex waits."""
import argparse
import json
import platform
import re
import shutil
import subprocess
from pathlib import Path

# Condvar, allocator and runtime parking futexes are not std mutex acquisitions.
MUTEX = re.compile(r"std::sys::(?:sync|pal::unix)::mutex::futex::Mutex::lock_contended")
SYMBOL = re.compile(r"[A-Za-z_][A-Za-z_0-9]*::|(?:^|\s)[A-Za-z_][A-Za-z_0-9]*\+")
LOCK_PROBE = """
tracepoint:syscalls:sys_enter_futex /pid == cpid/ {
    @started[tid] = nsecs;
    @stack[tid] = ustack;
}
tracepoint:syscalls:sys_exit_futex /pid == cpid && @started[tid]/ {
    @waits[@stack[tid]] = sum(nsecs - @started[tid]);
    delete(@started, tid);
    delete(@stack, tid);
}
"""
WORKER = re.compile(r"moq-(?:uring|quic)-[0-9]+$")


def reduce(samples, before, after):
    """Normalize mutex waits by thread CPU counters, never elapsed time."""
    result = {}
    for sample in samples:
        tid, name = str(sample['tid']), sample['comm']
        if not WORKER.fullmatch(name):
            continue
        wait = sample['wait_ns']
        if not isinstance(wait, int) or isinstance(wait, bool) or wait < 0:
            raise ValueError('wait_ns must be a nonnegative integer')
        stack = sample['stack']
        if not isinstance(stack, list) or not stack or not all(isinstance(frame, str) for frame in stack):
            raise ValueError('a nonempty symbolized user stack is required')
        if not any(SYMBOL.search(frame) for frame in stack):
            raise ValueError('user stack contains no resolved symbols')
        row = result.setdefault(tid, {'comm': name, 'mutex_wait_ns': 0, 'other_futex_wait_ns': 0, 'stacks': []})
        if row['comm'] != name:
            raise ValueError('thread identity changed inside the capture')
        if any(MUTEX.search(frame) for frame in stack):
            row['mutex_wait_ns'] += wait
            row['stacks'].append({'wait_ns': wait, 'stack': stack})
        else:
            row['other_futex_wait_ns'] += wait
    if not result:
        raise ValueError('capture has no symbolized relay-worker futex samples')
    for tid, row in result.items():
        if tid not in before or tid not in after:
            raise ValueError(f'missing CPU counter for worker {tid}')
        if any(not isinstance(value, int) or isinstance(value, bool) or value < 0 for value in (before[tid], after[tid])):
            raise ValueError(f'invalid CPU counter for worker {tid}')
        cpu = after[tid] - before[tid]
        if cpu <= 0:
            raise ValueError(f'worker {tid} has no advancing CPU counter')
        row['cpu_ns'] = cpu
        row['mutex_wait_per_cpu'] = row['mutex_wait_ns'] / cpu
    return {'version': 1, 'workers': result,
            'scope': 'completed futex waits with std mutex lock_contended frames; spinning is excluded'}


def thread_cpu(pid):
    """Read Linux per-thread scheduled CPU nanoseconds for the matching workers."""
    if platform.system() != 'Linux':
        raise ValueError('thread CPU capture requires Linux /proc schedstat')
    result = {}
    for task in (Path('/proc') / str(pid) / 'task').iterdir():
        if WORKER.fullmatch((task / 'comm').read_text().strip()):
            result[task.name] = int((task / 'schedstat').read_text().split()[0])
    if not result:
        raise ValueError('PID has no moq-uring-N or moq-quic-N workers')
    return result


def probe(mode, directory):
    """Retain both successful and refused capability probes before any build."""
    directory.mkdir(parents=True, exist_ok=True)
    evidence = {'version': 1, 'mode': mode, 'command': None,
                'kernel': platform.platform(), 'exit_code': None,
                'status': 'unavailable', 'capture': 'not attempted; capability probe only'}
    stdout, stderr = '', ''
    try:
        if platform.system() != 'Linux':
            raise ValueError('this draft probes Linux only; macOS samply capture remains unimplemented')
        tool = {'lock': 'bpftrace', 'cpu': 'perf'}[mode]
        executable = shutil.which(tool)
        if not executable:
            raise ValueError(f'{tool} is unavailable; enter the Nix dev shell')
        command = ([executable, '-e', LOCK_PROBE, '-c', 'true'] if mode == 'lock' else
                   [executable, 'stat', '-e', 'cpu-clock', '--', 'true'])
        evidence['command'] = command
        evidence['status'] = 'spawn failed'
        # The traced child starts after attachment and its exit ends the probe.
        # BEGIN exits before attachment and cannot prove these capabilities.
        # Both probes exit by construction. No deadline hides slow setup or
        # substitutes for the child process's real result.
        completed = subprocess.run(command, capture_output=True, text=True)
        stdout, stderr = completed.stdout, completed.stderr
        evidence.update(exit_code=completed.returncode,
                        status='passed' if completed.returncode == 0 else 'refused')
        if completed.returncode:
            requirement = 'tracefs, BPF and user-stack permissions' if mode == 'lock' else 'perf_event permissions'
            raise ValueError(f'{tool} capture refused: requires {requirement}; see {directory / "probe.stderr"}')
    except (ValueError, OSError) as error:
        if not stderr:
            stderr = str(error) + '\n'
        raise
    finally:
        (directory / 'probe.stdout').write_text(stdout)
        (directory / 'probe.stderr').write_text(stderr)
        (directory / 'probe.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print('Capability probe passed. No profile was captured.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    capability = commands.add_parser('probe')
    capability.add_argument('mode', choices=('lock', 'cpu'))
    capability.add_argument('directory', type=Path)
    cpu = commands.add_parser('thread-cpu')
    cpu.add_argument('pid', type=int)
    cpu.add_argument('output', type=Path)
    merge = commands.add_parser('reduce')
    for name in ('samples', 'before', 'after', 'output'):
        merge.add_argument(name, type=Path)
    args = parser.parse_args()
    try:
        if args.command == 'probe':
            probe(args.mode, args.directory)
        elif args.command == 'thread-cpu':
            args.output.write_text(json.dumps(thread_cpu(args.pid), indent=2) + '\n')
        else:
            result = reduce(*(json.loads(getattr(args, name).read_text()) for name in ('samples', 'before', 'after')))
            args.output.write_text(json.dumps(result, indent=2) + '\n')
    except (ValueError, OSError) as error:
        parser.exit(1, f'profile: {error}\n')


if __name__ == '__main__':
    main()
