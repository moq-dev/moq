#!/usr/bin/env python3
"""Combine the binding benchmark's per-runner results into one table and report.

Each runner writes `<dir>/<runner>.json`, a list of {name, unit, value}. The table shows
every runner beside its ratio to the `rust-ffi` baseline, run on the same machine, which
cancels most of the runner's own speed. `report.json` holds every value and ratio, named
`<runner>/<name>`, for the trend history.
"""
import argparse
import json
import os
from pathlib import Path

BASELINE = 'rust-ffi'
# Baselines first, then the wrappers in a stable order.
ORDER = ('rust-net', 'rust-ffi')
SMALLER_IS_BETTER = {'ms/MB', 'ns'}


def load(directory):
    runners = {}
    for path in sorted(Path(directory).glob('*.json')):
        if path.name == 'report.json':
            continue
        runners[path.stem] = {entry['name']: entry for entry in json.loads(path.read_text())}
    if BASELINE not in runners:
        raise ValueError(f'missing the {BASELINE} baseline in {directory}')
    return runners


def order(runners):
    return sorted(runners, key=lambda r: (ORDER.index(r) if r in ORDER else len(ORDER), r))


def ratio(entry, baseline):
    """How many times worse than the baseline: above 1 is slower or costlier."""
    if baseline is None or not baseline['value'] or not entry['value']:
        return None
    if entry['unit'] in SMALLER_IS_BETTER:
        return entry['value'] / baseline['value']
    return baseline['value'] / entry['value']


def report(runners):
    entries = []
    baseline = runners[BASELINE]
    for runner in order(runners):
        for name, entry in runners[runner].items():
            entries.append({'name': f'{runner}/{name}', 'unit': entry['unit'], 'value': entry['value']})
            if runner != BASELINE and (r := ratio(entry, baseline.get(name))) is not None:
                entries.append({'name': f'{runner}/{name} vs {BASELINE}', 'unit': 'x', 'value': r})
    return entries


def table(runners):
    columns = order(runners)
    names = list(dict.fromkeys(name for runner in columns for name in runners[runner]))
    baseline = runners[BASELINE]
    lines = [
        '| benchmark | unit | ' + ' | '.join(columns) + ' |',
        '|---|---|' + '---|' * len(columns),
    ]
    for name in names:
        unit = next(runners[r][name]['unit'] for r in columns if name in runners[r])
        cells = []
        for runner in columns:
            entry = runners[runner].get(name)
            if entry is None:
                cells.append('')
                continue
            cell = f"{entry['value']:.4g}"
            if runner != BASELINE and (r := ratio(entry, baseline.get(name))) is not None:
                cell += f' ({r:.2f}x)'
            cells.append(cell)
        lines.append(f'| {name} | {unit} | ' + ' | '.join(cells) + ' |')
    return '\n'.join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory')
    args = parser.parse_args()
    runners = load(args.directory)
    (Path(args.directory) / 'report.json').write_text(json.dumps(report(runners), indent=2))
    markdown = (
        f'## Binding benchmark\n\nRatios are against `{BASELINE}`; above 1x is slower or costlier.\n\n'
        + table(runners)
        + '\n'
    )
    print(markdown)
    if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(summary, 'a') as f:
            f.write(markdown)


if __name__ == '__main__':
    main()
