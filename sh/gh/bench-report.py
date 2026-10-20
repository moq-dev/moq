#!/usr/bin/env python3
"""Convert measured nightly results to github-action-benchmark's JSON format."""

import argparse
import csv
import html
import json
import math
import os
from pathlib import Path
import re
from urllib.parse import quote


BIGGER = {'MB/s', 'MiB/s', 'B/s', 'ops/s', 'frames/s'}
SMALLER = {'ns', 'us', 'µs', 'ms', 's', 'ms/MB', 'x', 'bytes', 'B', 'MiB'}


def value(number):
    if isinstance(number, bool) or not isinstance(number, (int, float)) or not math.isfinite(number) or number < 0:
        raise ValueError(f'invalid measurement: {number!r}')
    return number


def checked(entries):
    if not isinstance(entries, list) or not entries:
        raise ValueError('a benchmark report must contain measured entries')
    names = set()
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get('name'), str) or not entry['name']:
            raise ValueError(f'invalid benchmark entry: {entry!r}')
        if entry['name'] in names:
            raise ValueError(f'duplicate benchmark: {entry["name"]}')
        names.add(entry['name'])
        if entry.get('unit') not in BIGGER | SMALLER:
            raise ValueError(f'unsupported measurement unit: {entry.get("unit")!r}')
        value(entry.get('value'))
    return entries


def write(path, entries):
    checked(entries)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(entries, indent=2) + '\n')


def rust(directory):
    manifest = directory / 'criterion-runs.txt'
    runs = [Path(line) for line in manifest.read_text().splitlines() if line]
    if not runs:
        raise ValueError(f'no measured Criterion runs in {manifest}')
    entries = []
    for run in runs:
        metadata = json.loads((run / 'metadata.json').read_text())
        if metadata['mode'] != '--criterion':
            raise ValueError(f'not a measured Criterion run: {run}')
        label = metadata['environment'].get('MOQ_BENCH_LABEL')
        if not isinstance(label, str) or not label:
            raise ValueError(f'missing Criterion source label: {run}')
        estimates = sorted((run / 'criterion').glob('**/new/estimates.json'))
        if not estimates:
            raise ValueError(f'Criterion produced no measured estimates: {run}')
        for path in estimates:
            benchmark = json.loads(path.with_name('benchmark.json').read_text())
            estimate = json.loads(path.read_text())['mean']
            interval = estimate['confidence_interval']
            low, high = value(interval['lower_bound']), value(interval['upper_bound'])
            entries.append({
                'name': f'rust/{label}/{benchmark["full_id"]}', 'unit': 'ns',
                'value': value(estimate['point_estimate']), 'range': f'{low:.4g}..{high:.4g}',
                # Criterion does not record a target's measurement, so the clock is left to the README.
                'extra': f'Mean nanoseconds per iteration; {metadata["revision"]}',
            })
    return checked(entries)


def timing_unit(column):
    for unit in ('ns', 'us', 'ms'):
        if column == unit or column.endswith(f'_{unit}') or column.startswith(f'{unit}_per_'):
            return unit
    return None


def javascript_file(path):
    entries, headers = [], None
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        # The interests sweep reports dimensions and elapsed time in one line.
        interests = re.fullmatch(r'broadcasts=(\d+) tracks=(\d+) acquire\+release=([\d.eE+-]+) us', line)
        if interests:
            broadcasts, tracks, elapsed = interests.groups()
            entries.append({'name': f'js/{path.stem}/broadcasts={broadcasts}/tracks={tracks}/acquire+release',
                            'unit': 'us', 'value': value(float(elapsed))})
            continue
        row = [field.strip() for field in next(csv.reader([line]))]
        if len(row) > 1 and any(timing_unit(field) for field in row):
            if len(set(row)) != len(row):
                raise ValueError(f'duplicate CSV headers: {path}:{line_number}')
            headers = row
            continue
        if not headers or not line.strip():
            continue
        if len(row) != len(headers):
            raise ValueError(f'malformed benchmark CSV: {path}:{line_number}')
        dimensions = '/'.join(f'{key}={quote(field, safe="-_.~")}' for key, field in zip(headers, row)
                              if timing_unit(key) is None)
        for key, field in zip(headers, row):
            if unit := timing_unit(key):
                entries.append({'name': f'js/{path.stem}/{dimensions}/{key}',
                                'unit': unit, 'value': value(float(field))})
    if not entries:
        raise ValueError(f'JS benchmark produced no measurements: {path}')
    return checked(entries)


def javascript(directory):
    entries = [entry for path in sorted((directory / 'raw').glob('*.csv')) for entry in javascript_file(path)]
    return checked(entries)


def prepare(directory, output):
    costs, rates = [], []
    for source in sorted(directory.glob('bench-*')):
        # Other nightly artifacts share the prefix; only a recorded outcome marks a chart source.
        status = source / 'outcome.json'
        if not status.exists():
            continue
        outcome = json.loads(status.read_text())['status']
        if outcome not in {'success', 'failure', 'cancelled', 'skipped'}:
            raise ValueError(f'invalid benchmark outcome: {source}')
        if outcome != 'success':
            print(f'{source.name}: {outcome}; no history point')
            continue
        report = source / 'report.json'
        if not report.exists():
            raise ValueError(f'successful benchmark has no report: {source}')
        entries = checked(json.loads(report.read_text()))
        for entry in entries:
            target = rates if entry['unit'] in BIGGER else costs
            target.append({**entry, 'name': f'{source.name}/{entry["name"]}'})
        print(f'{source.name}: {len(entries)} measured entries')
    if not costs and not rates:
        raise ValueError('no completed benchmark reports to publish')
    output.mkdir(parents=True, exist_ok=True)
    for name, entries in [('costs', costs), ('rates', rates)]:
        (output / f'{name}.json').write_text(json.dumps(entries, indent=2) + '\n')
        if entries:
            checked(entries)
        if target := os.environ.get('GITHUB_OUTPUT'):
            with open(target, 'a') as stream:
                stream.write(f'{name}={str(bool(entries)).lower()}\n')


def index(directory):
    destination = directory / 'bench'
    links = []
    for name, label in [('costs', 'Time, CPU, size and Rust ratios'), ('rates', 'Delivered throughput')]:
        if (destination / name / 'index.html').exists():
            links.append(f'<li><a href="{name}/">{html.escape(label)}</a></li>')
    if not links:
        raise ValueError('no generated benchmark charts to deploy')
    (destination / 'index.html').write_text('''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>MoQ benchmarks</title>
<style>body{font:18px system-ui;max-width:48rem;margin:4rem auto;padding:0 1rem;line-height:1.6}a{color:#066fbb}</style>
<h1>MoQ nightly benchmarks</h1>
<p>Hosted runners vary. These charts are informational. Binding ratios compare each wrapper
with Rust on the same host: above 1x means slower or more CPU per delivered byte.</p>
<ul>''' + ''.join(links) + '''</ul>
<p>Each series keeps its source and payload shape. A failed suite contributes no point.
<a href="https://github.com/moq-dev/moq/blob/main/bench/README.md">Metrics and local reproduction</a></p>
</html>
''')
    # gh-pages hosts every nightly report, each in its own directory with its own writer.
    reports = []
    for page in sorted(directory.glob('*/index.html')):
        title = re.search(r'<title>(.*?)</title>', page.read_text(), re.S)
        label = title.group(1).strip() if title else page.parent.name
        reports.append(f'<li><a href="{page.parent.name}/">{html.escape(label)}</a></li>')
    (directory / 'index.html').write_text('''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>MoQ nightly reports</title>
<style>body{font:18px system-ui;max-width:48rem;margin:4rem auto;padding:0 1rem;line-height:1.6}a{color:#066fbb}</style>
<h1>MoQ nightly reports</h1>
<ul>''' + ''.join(reports) + '''</ul>
<p><a href="https://github.com/moq-dev/moq/actions/workflows/nightly.yml">Nightly runs</a></p>
</html>
''')
    (directory / '.nojekyll').touch()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for command in ('rust', 'js', 'prepare'):
        sub = commands.add_parser(command)
        sub.add_argument('directory', type=Path)
        sub.add_argument('output', type=Path)
    sub = commands.add_parser('status')
    sub.add_argument('status', choices=('success', 'failure', 'cancelled', 'skipped'))
    sub.add_argument('directory', type=Path)
    commands.add_parser('index').add_argument('directory', type=Path)
    args = parser.parse_args()
    if args.command == 'rust':
        write(args.output, rust(args.directory))
    elif args.command == 'js':
        write(args.output, javascript(args.directory))
    elif args.command == 'prepare':
        prepare(args.directory, args.output)
    elif args.command == 'status':
        args.directory.mkdir(parents=True, exist_ok=True)
        (args.directory / 'outcome.json').write_text(json.dumps({'status': args.status}) + '\n')
    else:
        index(args.directory)


if __name__ == '__main__':
    main()
