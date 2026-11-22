"""Run a test module's cases on threads, for tests that spend their time waiting on subprocesses."""

import os
import sys
import unittest
from concurrent.futures import ThreadPoolExecutor


def _cases(suite):
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            yield from _cases(item)
        else:
            yield item


def main(module):
    """Run every case of `module` in parallel, or with arguments defer to unittest for selecting cases and durations."""
    if len(sys.argv) > 1:
        unittest.main(module=module)
        return

    cases = list(_cases(unittest.defaultTestLoader.loadTestsFromModule(module)))
    workers = min(os.cpu_count() or 4, 8, max(len(cases), 1))

    def run(case):
        result = unittest.TestResult()
        case.run(result)
        return result

    failures = []
    errors = []
    unexpected = []
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for result in pool.map(run, cases):
            failures.extend(result.failures)
            errors.extend(result.errors)
            unexpected.extend(result.unexpectedSuccesses)

    for test, tb in errors:
        print(f"ERROR: {test}\n{tb}", file=sys.stderr)
    for test, tb in failures:
        print(f"FAIL: {test}\n{tb}", file=sys.stderr)
    for test in unexpected:
        print(f"UNEXPECTED SUCCESS: {test}", file=sys.stderr)
    print(
        f"Ran {len(cases)} tests ({len(failures)} failures, {len(errors)} errors, {len(unexpected)} unexpected)",
        file=sys.stderr,
    )
    sys.exit(1 if failures or errors or unexpected else 0)
