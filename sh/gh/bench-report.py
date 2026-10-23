#!/usr/bin/env python3
"""Convert measured nightly results to github-action-benchmark's JSON format and render its history."""

import argparse
import csv
from datetime import datetime, timezone
import html
import json
import math
import os
from pathlib import Path
import re
import statistics
import time
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


def paired(directory):
    """Combine each run's paired comparison under its trend series names; None when no run was paired."""
    runs = [Path(line) for line in (directory / 'criterion-runs.txt').read_text().splitlines() if line]
    metadata = [json.loads((run / 'metadata.json').read_text()) for run in runs]
    bases = {data['base'] for data in metadata}
    if bases == {''}:
        return None
    if len(bases) != 1:
        raise ValueError(f'Criterion runs disagree on their paired base: {sorted(bases)}')
    combined = {'unavailable': [], 'cases': []}
    for run, data in zip(runs, metadata):
        comparison = json.loads((run / 'paired.json').read_text())
        for key in ('commit', 'base', 'threshold_pct', 'rounds'):
            if combined.setdefault(key, comparison[key]) != comparison[key]:
                raise ValueError(f'paired run disagrees on {key}: {run}')
        label = data['environment']['MOQ_BENCH_LABEL']
        combined['unavailable'] += [{'name': f'rust/{label}/{entry["target"]}', 'reason': entry['reason']}
                                    for entry in comparison['unavailable']]
        for case in comparison['cases']:
            case = dict(case)
            combined['cases'].append({'name': f'rust/{label}/{case.pop("id")}', **case})
    return combined


def gate(directory):
    """Print confirmed paired regressions and comparisons that could not run; nonzero when any exist."""
    path = directory / 'paired.json'
    if not path.exists():
        raise ValueError(f'no paired comparison in {directory}')
    comparison = json.loads(path.read_text())
    regressions = [case for case in comparison['cases'] if case['verdict'] == 'regression']
    lines = [f'Paired comparison of {comparison["commit"][:9]} against {comparison["base"][:9]}: '
             f'{len(regressions)} confirmed regressions, {len(comparison["unavailable"])} targets not compared', '']
    for case in regressions:
        rounds = ', '.join(f'{delta:+.1f}%' for delta in case['rounds']['paired_deltas_pct'])
        lines.append(f'- `{case["name"]}`: {case["rounds"]["delta_pct"]:+.1f}% (rounds {rounds})')
    lines += [f'- `{entry["name"]}` not compared: {entry["reason"]}' for entry in comparison['unavailable']]
    text = '\n'.join(lines) + '\n'
    print(text, end='')
    if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(summary, 'a') as stream:
            stream.write(text)
    return 1 if regressions or comparison['unavailable'] else 0


def measurement_unit(column):
    if column == 'size_bytes':
        return 'bytes'
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
        if len(row) > 1 and any(measurement_unit(field) for field in row):
            if len(set(row)) != len(row):
                raise ValueError(f'duplicate CSV headers: {path}:{line_number}')
            headers = row
            continue
        if not headers or not line.strip():
            continue
        if len(row) != len(headers):
            raise ValueError(f'malformed benchmark CSV: {path}:{line_number}')
        dimensions = '/'.join(f'{key}={quote(field, safe="-_.~")}' for key, field in zip(headers, row)
                              if measurement_unit(key) is None)
        for key, field in zip(headers, row):
            if unit := measurement_unit(key):
                entries.append({'name': f'js/{path.stem}/{dimensions}/{key}',
                                'unit': unit, 'value': value(float(field))})
    if not entries:
        raise ValueError(f'JS benchmark produced no measurements: {path}')
    return checked(entries)


def javascript(directory):
    entries = [entry for path in sorted((directory / 'raw').glob('*.csv')) for entry in javascript_file(path)]
    return checked(entries)


def prepare(directory, output):
    costs, rates, comparisons = [], [], {}
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
        if (source / 'paired.json').exists():
            comparisons[source.name] = json.loads((source / 'paired.json').read_text())
    if not costs and not rates:
        # The table still rebuilds from stored history and marks every source stale.
        print('no completed benchmark reports; no history point')
    output.mkdir(parents=True, exist_ok=True)
    # Tonight's paired comparisons replace the published ones, even when none finished.
    (output / 'paired.json').write_text(json.dumps({'sources': comparisons}, indent=2) + '\n')
    for name, entries in [('costs', costs), ('rates', rates)]:
        (output / f'{name}.json').write_text(json.dumps(entries, indent=2) + '\n')
        if entries:
            checked(entries)
        if target := os.environ.get('GITHUB_OUTPUT'):
            with open(target, 'a') as stream:
                stream.write(f'{name}={str(bool(entries)).lower()}\n')


HISTORY_PREFIX = 'window.BENCHMARK_DATA = '
BASELINE = 7
TREND = 30


def history(directory):
    """Read github-action-benchmark's stored runs into one point per series and commit."""
    series = {}
    for kind in ('costs', 'rates'):
        path = directory / 'bench' / kind / 'data.js'
        if not path.exists():
            continue
        text = path.read_text()
        if not text.startswith(HISTORY_PREFIX):
            raise ValueError(f'unrecognized benchmark history: {path}')
        for runs in json.loads(text[len(HISTORY_PREFIX):])['entries'].values():
            for run in runs:
                commit = run['commit']
                when = datetime.fromisoformat(commit['timestamp'].replace('Z', '+00:00'))
                for bench in run['benches']:
                    point = {'commit': commit['id'], 'order': (when, run['date']), 'date': run['date'],
                             'unit': bench['unit'], 'value': value(bench['value'])}
                    points = series.setdefault(bench['name'], {})
                    # Re-running the site job records the same commit again; the latest run wins.
                    if commit['id'] not in points or points[commit['id']]['date'] < run['date']:
                        points[commit['id']] = point
    return {name: sorted(points.values(), key=lambda point: point['order']) for name, points in series.items()}


def assess(points):
    """Compare the newest point with the median of the previous BASELINE, scaled by their own spread."""
    unit = points[-1]['unit']
    # A series that changed its unit starts its history over.
    values = [point['value'] for point in points if point['unit'] == unit]
    row = {'unit': unit, 'latest': values[-1], 'values': values[-TREND:], 'count': len(values)}
    previous = values[-BASELINE - 1:-1]
    if len(previous) < BASELINE:
        return row
    baseline = statistics.median(previous)
    spread = statistics.median(abs(number - baseline) for number in previous)
    delta = values[-1] - baseline
    row.update(baseline=baseline, delta=delta,
               relative=abs(delta) / baseline if baseline else math.inf if delta else 0.0)
    if abs(delta) > max(0.1 * baseline, 3 * spread):
        row['flag'] = 'better' if (delta > 0) == (unit in BIGGER) else 'worse'
    return row


def sparkline(values):
    low, high = min(values), max(values)
    points = ' '.join(f'{index * 3},{15 - round(14 * (number - low) / (high - low)) if high > low else 8}'
                      for index, number in enumerate(values))
    return f'<svg width="87" height="16"><polyline points="{points}"/></svg>'


def number(amount, unit):
    text = f'{amount:,.0f}' if abs(amount) >= 1000 else f'{amount:.4g}'
    return f'{text} {html.escape(unit)}'


def table(rows):
    if not rows:
        return ''
    body = []
    for row in rows:
        unit = row['unit']
        if 'baseline' not in row:
            change, baseline = f'{row["count"]} of {BASELINE + 1} points', ''
        else:
            baseline = number(row['baseline'], unit)
            change = (f'{100 * row["delta"] / row["baseline"]:+.1f}%' if row['baseline']
                      else ('+' if row['delta'] > 0 else '') + number(row['delta'], unit))
        status = row.get('flag', 'muted' if row['stale'] or 'baseline' not in row else '')
        body.append(f'<tr><td>{html.escape(row["name"])}<td>{number(row["latest"], unit)}<td>{baseline}'
                    f'<td class="{status}">{change}<td>{sparkline(row["values"])}')
    return ('<table><thead><tr><th>Series<th>Latest<th>Baseline<th>Change<th>Trend</thead><tbody>'
            + '\n'.join(body) + '</tbody></table>')


def paired_table(cases):
    body = []
    for case in sorted(cases, key=lambda case: -abs(case.get('delta_pct') or 0)):
        sides = [number(case[side], 'ns') if case[side] is not None else '' for side in ('base_ns', 'current_ns')]
        status = {'regression': 'worse', 'faster': 'better'}.get(case['verdict'], '')
        change = f'{case["delta_pct"]:+.1f}%' if 'delta_pct' in case else case['verdict']
        rounds = ' '.join(f'{delta:+.1f}%' for delta in case['rounds']['paired_deltas_pct']) if 'rounds' in case else ''
        body.append(f'<tr class="paired"><td>{html.escape(case["name"])}<td>{sides[0]}<td>{sides[1]}'
                    f'<td class="{status}">{change}<td>{rounds}')
    return ('<table><thead><tr><th>Series<th>Base<th>Current<th>Screen<th>Rounds</thead><tbody>'
            + '\n'.join(body) + '</tbody></table>')


# Paired verdicts by section: open sections hold what needs a look tonight.
PAIRED_SECTIONS = [
    ({'regression'}, 'Confirmed regressions', True),
    ({'unavailable'}, 'Not compared', True),
    ({'faster'}, 'Faster in the screen', True),
    ({'unconfirmed', 'noise'}, 'Slower in the screen, not confirmed', False),
    ({'new', 'removed'}, 'Measured on one side only', False),
    ({'unchanged'}, 'Unchanged', False),
]


def paired_section(directory):
    """Render the published paired comparisons: tonight's commit against the previous nightly's, per source."""
    path = directory / 'bench' / 'paired.json'
    if not path.exists():
        return ''
    sources = json.loads(path.read_text())['sources']
    if not sources:
        return '<h2>Paired comparison</h2><p class="muted">No paired comparison finished on the latest nightly.</p>'
    cases, notes, unavailable = [], [], []
    for source, comparison in sorted(sources.items()):
        commit, base = comparison['commit'], comparison['base']
        notes.append(f'{html.escape(source)}: <a href="https://github.com/moq-dev/moq/compare/{base}...{commit}">'
                     f'{base[:9]}...{commit[:9]}</a>')
        cases += [{**case, 'name': f'{source}/{case["name"]}'} for case in comparison['cases']]
        unavailable += [f'<li>{html.escape(source)}/{html.escape(entry["name"])}: {html.escape(entry["reason"])}'
                        for entry in comparison['unavailable']]
    threshold, rounds = comparison['threshold_pct'], comparison['rounds']
    parts = [f'''<h2>Paired comparison</h2>
<p>Each source built the previous nightly's commit beside this one and ran both on one runner, alternating
which went first. A quick screen flags a change beyond {threshold}%; each slowdown then reruns in {rounds}
alternating rounds of longer measurements, and fails the nightly when every round is slower and their median
exceeds {threshold}%. {"; ".join(notes)}</p>''']
    if unavailable:
        parts.append(f'<p class="worse">Targets not compared:</p><ul>{"".join(unavailable)}</ul>')
    for verdicts, label, flagged in PAIRED_SECTIONS:
        rows = [case for case in cases if case['verdict'] in verdicts]
        if rows or flagged:
            attributes = ' open class="flagged"' if flagged else ''
            parts.append(f'<details{attributes}><summary>{label} ({len(rows)})</summary>'
                         f'{paired_table(rows) if rows else ""}</details>')
    return '\n'.join(parts) + '\n<h2>Nightly trend</h2>\n'


def regressions(directory, commit=None, now=None):
    """Render every series' newest point against its own baseline as one static table."""
    series = history(directory)
    if not series:
        raise ValueError('no benchmark history to publish')
    now = time.time() if now is None else now
    if commit is None:
        commit = max((points[-1] for points in series.values()), key=lambda point: point['order'])['commit']
    # A source records every series or none, so a series missing from its source's newest run was retired.
    sources = {}
    for name, points in series.items():
        source = name.split('/', 1)[0]
        if source not in sources or sources[source]['order'] < points[-1]['order']:
            sources[source] = points[-1]
    flagged, sections = [], {source: [] for source in sorted(sources)}
    for name, points in sorted(series.items()):
        source = name.split('/', 1)[0]
        if points[-1]['commit'] != sources[source]['commit']:
            continue
        row = {**assess(points), 'name': name, 'stale': sources[source]['commit'] != commit}
        if row['stale']:
            row.pop('flag', None)
        (flagged if 'flag' in row else sections[source]).append(row)
    flagged.sort(key=lambda row: -row['relative'])
    parts = []
    for flag, label in [('worse', 'Regressions'), ('better', 'Improvements')]:
        rows = [row for row in flagged if row['flag'] == flag]
        parts.append(f'<details open class="flagged"><summary>{label} ({len(rows)})</summary>{table(rows)}</details>')
    for source, rows in sections.items():
        newest = sources[source]
        label = f'{html.escape(source)} ({len(rows)})'
        if newest['commit'] != commit:
            measured = datetime.fromtimestamp(newest['date'] / 1000, timezone.utc)
            days = math.floor((now - newest['date'] / 1000) / 86400)
            label += (f' <span class="muted">stale: last measured {measured:%Y-%m-%d} at '
                      f'{newest["commit"][:9]}, {days} days ago</span>')
        parts.append(f'<details><summary>{label}</summary>{table(rows)}</details>')
    return f'''<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>MoQ benchmarks</title>
<style>
body{{font:15px system-ui;max-width:72rem;margin:2rem auto;padding:0 1rem;line-height:1.4}}a{{color:#066fbb}}
summary{{font-weight:600;margin:1rem 0 .3rem;cursor:pointer}}table{{border-collapse:collapse;width:100%;
table-layout:fixed}}th,td{{padding:.1rem .5rem;text-align:right;white-space:nowrap}}th{{width:9rem}}
th:first-child{{width:auto}}th:last-child{{width:6rem}}th:first-child,td:first-child{{text-align:left}}
td:first-child{{word-break:break-all;white-space:normal;font:13px ui-monospace,monospace}}
tr:nth-child(even){{background:#f6f6f6}}
td{{font-variant-numeric:tabular-nums}}polyline{{fill:none;stroke:#555}}.worse{{background:#fcc}}
.better{{background:#cec}}.muted{{color:#888;font-weight:400}}input{{font:inherit;width:100%;padding:.3rem}}
</style>
<h1>MoQ nightly benchmarks</h1>
<p>Nightly <a href="https://github.com/moq-dev/moq/commit/{commit}">{commit[:9]}</a>, {len(flagged)} flagged
of {sum(map(len, sections.values())) + len(flagged)} series. Each series compares its newest point with the median
of its previous {BASELINE}. Red is worse and green is better by more than 10% and 3 times that window's median
absolute deviation. Grey rows lack history or come from a source that did not finish tonight.
<a href="https://github.com/moq-dev/moq/blob/main/bench/README.md#nightly-trends">Metrics and local
reproduction</a></p>
<input id="filter" type="search" placeholder="Filter series">
{paired_section(directory)}{''.join(parts)}
<script>
const filter = document.getElementById('filter');
filter.oninput = () => {{
  const query = filter.value.toLowerCase();
  for (const row of document.querySelectorAll('tbody tr')) {{
    row.hidden = !row.cells[0].textContent.toLowerCase().includes(query);
  }}
  for (const section of document.querySelectorAll('details')) {{
    section.open = query !== '' || section.classList.contains('flagged');
  }}
}};
</script>
</html>
'''


def index(directory, commit=None, paired_path=None):
    destination = directory / 'bench'
    destination.mkdir(parents=True, exist_ok=True)
    if paired_path is not None:
        (destination / 'paired.json').write_text(paired_path.read_text())
    (destination / 'index.html').write_text(regressions(directory, commit))
    # The table replaces the action's chart pages, which it only writes when missing.
    for kind in ('costs', 'rates'):
        if (destination / kind).is_dir():
            (destination / kind / 'index.html').write_text(
                '<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0; url=../">'
                '<title>MoQ benchmarks</title><a href="../">MoQ benchmarks</a>\n')
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
    sub = commands.add_parser('gate')
    sub.add_argument('directory', type=Path)
    sub = commands.add_parser('index')
    sub.add_argument('directory', type=Path)
    sub.add_argument('--commit', help='the nightly commit; sources without a point for it are stale')
    sub.add_argument('--paired', type=Path, help="tonight's prepared paired comparisons, replacing the published ones")
    args = parser.parse_args()
    if args.command == 'rust':
        write(args.output, rust(args.directory))
        if (comparison := paired(args.directory)) is not None:
            args.output.with_name('paired.json').write_text(json.dumps(comparison, indent=2) + '\n')
    elif args.command == 'gate':
        raise SystemExit(gate(args.directory))
    elif args.command == 'js':
        write(args.output, javascript(args.directory))
    elif args.command == 'prepare':
        prepare(args.directory, args.output)
    elif args.command == 'status':
        args.directory.mkdir(parents=True, exist_ok=True)
        (args.directory / 'outcome.json').write_text(json.dumps({'status': args.status}) + '\n')
    else:
        index(args.directory, args.commit, args.paired)


if __name__ == '__main__':
    main()
