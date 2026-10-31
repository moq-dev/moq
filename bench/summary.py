#!/usr/bin/env python3
"""Align cumulative CPU samples to the measured load window."""
import argparse
import bisect
import json
import math
from pathlib import Path

COUNTERS = ('cpu_user', 'cpu_system', 'ctx_voluntary', 'ctx_involuntary')


def records(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines()]


def processes(rows):
    result = {}
    for row in rows:
        result.setdefault(row['pid'], []).append(row)
    for samples in result.values():
        for row in samples:
            for key in ('timestamp_ms', *COUNTERS):
                value = row[key]
                if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value) or value < 0:
                    raise ValueError(f'invalid host counter: {key}')
        if len(samples) < 2:
            raise ValueError('fewer than two host samples')
        for a, b in zip(samples, samples[1:]):
            if b['timestamp_ms'] <= a['timestamp_ms']:
                raise ValueError('host timestamps did not advance')
            for key in COUNTERS:
                if not math.isfinite(b[key]) or b[key] < a[key]:
                    raise ValueError(f'host counter moved backwards: {key}')
    if not result:
        raise ValueError('no host samples')
    return result


def at(rows, timestamp):
    times = [row['timestamp_ms'] for row in rows]
    index = bisect.bisect_left(times, timestamp)
    if index == len(rows) or timestamp < times[0]:
        raise ValueError('host samples do not bracket the load window')
    if times[index] == timestamp:
        return rows[index]
    before, after = rows[index - 1], rows[index]
    fraction = (timestamp - before['timestamp_ms']) / (after['timestamp_ms'] - before['timestamp_ms'])
    result = {key: before[key] + fraction * (after[key] - before[key]) for key in COUNTERS}
    first = {row['tid']: row for row in before.get('per_thread', [])}
    last = {row['tid']: row for row in after.get('per_thread', [])}
    result['per_thread'] = [dict(last[tid], **{
        key: first[tid][key] + fraction * (last[tid][key] - first[tid][key]) for key in COUNTERS
    }) for tid in first.keys() & last.keys()]
    return result


def cpu(rows, start, end):
    if end <= start:
        raise ValueError('load window did not advance')
    first, last = at(rows, start), at(rows, end)
    seconds = (end - start) / 1000
    rates = {key: (last[key] - first[key]) / seconds for key in COUNTERS}
    window = [row for row in rows if start <= row['timestamp_ms'] <= end]
    if not window:
        raise ValueError('no host samples inside the load window')
    workers = []
    initial = {row['tid']: row for row in first.get('per_thread', [])}
    final = {row['tid']: row for row in last.get('per_thread', [])}
    for tid in sorted(initial.keys() & final.keys()):
        user = final[tid]['cpu_user'] - initial[tid]['cpu_user']
        system = final[tid]['cpu_system'] - initial[tid]['cpu_system']
        if user < 0 or system < 0:
            raise ValueError('thread CPU counter moved backwards')
        workers.append(dict(tid=tid, name=final[tid]['name'], cpu_cores=(user + system) / seconds))
    return dict(cpu_user_cores=rates['cpu_user'], cpu_system_cores=rates['cpu_system'],
                cpu_cores=rates['cpu_user'] + rates['cpu_system'],
                ctx_voluntary_s=rates['ctx_voluntary'], ctx_involuntary_s=rates['ctx_involuntary'],
                rss_bytes=max(row['rss_bytes'] for row in window), threads=max(row['threads'] for row in window),
                thread_cpu=workers, thread_scope='threads present at both interpolated window boundaries')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    align = sub.add_parser('align')
    align.add_argument('host', type=Path)
    align.add_argument('load', type=Path)
    align.add_argument('output', type=Path)
    host = sub.add_parser('host')
    host.add_argument('host', type=Path)
    host.add_argument('summary', type=Path)
    host.add_argument('relay', type=int)
    host.add_argument('generator', type=int, nargs='?')
    args = parser.parse_args()
    groups = processes(records(args.host))
    if args.command == 'align':
        start = max(rows[0]['timestamp_ms'] for rows in groups.values())
        end = min(rows[-1]['timestamp_ms'] for rows in groups.values())
        rows = [row for row in records(args.load) if start <= row['timestamp_ms'] <= end]
        if len(rows) < 2:
            raise ValueError('host/load windows do not overlap')
        args.output.write_text(''.join(json.dumps(row) + '\n' for row in rows))
        return
    result = json.loads(args.summary.read_text())
    start, end = result['throughput_window_start_ms'], result['throughput_window_end_ms']
    result.update(cpu(groups[args.relay], start, end))
    # The relay's cost per unit of delivered work: per frame for small messages, per Gbps for bytes.
    result.update(cpu_us_per_frame=result['cpu_cores'] * 1e6 / result['recv_fps'],
                  cpu_cores_per_gbps=result['cpu_cores'] * 1000 / result['recv_mbps'])
    result.update(host_window_start_ms=start, host_window_end_ms=end,
                  cpu_scope='cumulative counters linearly interpolated at load window boundaries')
    if args.generator is not None:
        result['generator'] = cpu(groups[args.generator], start, end)
    args.summary.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
