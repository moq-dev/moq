#!/usr/bin/env python3
"""Run the upstream Docker interop matrices that touch moq-dev."""

import argparse
import copy
import html
import json
from pathlib import Path
import shutil
import subprocess
import time


OWN = ('moq-dev-rs', 'moq-dev-js')
RELAY_IMAGE = 'moq-interop-relay:local'
# Only our clients against this checkout's relay gate the run. Every other pair
# depends on a third party or a feature we don't serve, so it is reported but
# optional, as relays.sh treats every relay but moq-dev's.
REQUIRED = {(client, 'moq-dev-rs') for client in OWN}


def matrices(registry):
    """Keep published images and partition the pairs around our relay and clients."""
    registry = copy.deepcopy(registry)
    implementations = registry['implementations']
    relay = implementations['moq-dev-rs']['roles']['relay']['docker']
    relay['image'] = RELAY_IMAGE
    relay.pop('build', None)
    inventory = []
    for name, implementation in implementations.items():
        for role in ('relay', 'client'):
            docker = implementation.get('roles', {}).get(role, {}).get('docker')
            if not docker:
                continue
            image = docker['image']
            # Local adapter tags need a build even when their base is published.
            local = bool(docker.get('build')) and '/' not in image
            inventory.append({'implementation': name, 'role': role, 'image': image,
                              'status': 'not run: local build only' if local else 'selected'})
            if local:
                del implementation['roles'][role]['docker']
    if any('docker' not in implementations[name]['roles']['client'] for name in OWN):
        raise ValueError('the registry must provide both moq-dev client images')

    to_relay = copy.deepcopy(registry)
    from_clients = copy.deepcopy(registry)
    for name in implementations:
        if name != 'moq-dev-rs':
            to_relay['implementations'][name]['roles'].pop('relay', None)
        if name not in OWN:
            from_clients['implementations'][name]['roles'].pop('client', None)
    # The first matrix already covers both of our clients against our relay.
    from_clients['implementations']['moq-dev-rs']['roles'].pop('relay', None)
    return (to_relay, from_clients), inventory


def expected_pairs(registry):
    implementations = registry['implementations']
    return {(client, relay) for client, client_info in implementations.items()
            if client_info.get('roles', {}).get('client', {}).get('docker')
            for relay, relay_info in implementations.items()
            if relay_info.get('roles', {}).get('relay', {}).get('docker')
            and set(client_info['draft_versions']) & set(relay_info['draft_versions'])}


def validate_summary(summary, expected):
    runs = summary['runs']
    actual = {(run['client'], run['relay']) for run in runs}
    if actual != expected or len(runs) != len(expected):
        raise ValueError(f'incomplete runner coverage: expected {sorted(expected)}, got {sorted(actual)}')
    if any(run['mode'] != 'docker' or run['status'] not in ('pass', 'fail', 'timeout') for run in runs):
        raise ValueError('unexpected mode, skipped image, or unknown verdict in runner summary')


def run(upstream, output):
    if output.exists():
        raise ValueError(f'report destination already exists: {output}')
    registry_file = upstream / 'implementations.json'
    original = registry_file.read_text()
    plans, inventory = matrices(json.loads(original))
    for command in (['docker', 'info'], ['docker', 'compose', 'version']):
        subprocess.run(command, check=True)
    images = {entry['image'] for entry in inventory if entry['status'] == 'selected'}
    for image in sorted(images - {RELAY_IMAGE}):
        # A registry blip would otherwise abort both matrices before any report.
        for attempt in range(1, 4):
            if not subprocess.run(['docker', 'pull', image]).returncode:
                break
            if attempt == 3:
                raise SystemExit(f'failed to pull {image}')
            time.sleep(attempt * 2)
    subprocess.run(['docker', 'image', 'inspect', RELAY_IMAGE], check=True)
    subprocess.run(['make', 'certs'], cwd=upstream, check=True)
    results = upstream / 'results'
    if results.exists():
        raise ValueError(f'runner checkout already contains results: {results}')
    combined = None
    target = None
    try:
        for index, plan in enumerate(plans):
            registry_file.write_text(json.dumps(plan))
            expected = expected_pairs(plan)
            if not expected:
                raise ValueError(f'runner matrix {index} has no compatible pairs')
            # The runner exits nonzero on any red pair; the validated summary decides instead.
            subprocess.run(['bash', 'run-interop-tests.sh', '--docker-only'], cwd=upstream)
            summaries = list(results.glob('*/summary.json'))
            if len(summaries) != 1:
                raise ValueError('runner must produce exactly one summary per matrix')
            summary_file = summaries[0]
            summary = json.loads(summary_file.read_text())
            validate_summary(summary, expected)
            if combined is None:
                combined = summary
                target = upstream / 'combined'
                summary_file.parent.rename(target)
            else:
                combined['runs'].extend(summary['runs'])
                for log in summary_file.parent.glob('*.log'):
                    shutil.move(log, target / log.name)
                shutil.rmtree(summary_file.parent)
        target.rename(results / 'nightly')
        (results / 'nightly' / 'summary.json').write_text(json.dumps(combined, indent=2) + '\n')
        subprocess.run(['bash', 'generate-report.sh', 'results/nightly'], cwd=upstream, check=True)
        if not (results / 'index.html').is_file() or not (results / 'nightly' / 'report.html').is_file():
            raise ValueError('upstream HTML report is missing')
        (results / 'inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
        skipped = [entry for entry in inventory if entry['status'] != 'selected']
        if skipped:
            rows = ''.join(f'<li>{html.escape(entry["implementation"])} {entry["role"]}: '
                           f'{html.escape(entry["image"])} (local build only)</li>' for entry in skipped)
            index_file = results / 'index.html'
            index_file.write_text(index_file.read_text().replace('</body>',
                                  f'<h2>Not run</h2><ul>{rows}</ul></body>'))
        shutil.copytree(results, output)
    finally:
        registry_file.write_text(original)
        subprocess.run(['docker', 'compose', '-f', 'docker-compose.test.yml', 'down', '--timeout', '1'],
                       cwd=upstream, check=True)
    # Fail only after the report shows every pair. A required pair the registry
    # no longer pairs fails too, rather than passing unchecked.
    status = {(run['client'], run['relay']): run['status'] for run in combined['runs']}
    optional = sorted(pair for pair, verdict in status.items() if verdict != 'pass' and pair not in REQUIRED)
    if optional:
        # A workflow annotation, so a green job still shows them.
        print(f'::warning::optional pairs not passing (reported only): {optional}')
    failed = sorted(pair for pair in REQUIRED if status.get(pair) != 'pass')
    if failed:
        raise SystemExit(f'required pairs failed: {failed}; see the report')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('upstream', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    run(args.upstream.resolve(), args.output.resolve())


if __name__ == '__main__':
    main()
