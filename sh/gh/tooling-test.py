"""Run a unittest script with explicitly named slow classes or methods excluded."""

import argparse
import runpy
import sys
import unittest
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("script", type=Path)
    parser.add_argument("--exclude", action="append", default=[])
    args = parser.parse_args()
    excluded = set(args.exclude)
    matched = set()

    class Loader(unittest.TestLoader):
        def getTestCaseNames(self, case):
            names = super().getTestCaseNames(case)
            selected = []
            for name in names:
                hits = excluded & {case.__name__, f"{case.__name__}.{name}"}
                matched.update(hits)
                if not hits:
                    selected.append(name)
            return selected

    original = unittest.main

    def run(*argv, **kwargs):
        program = original(*argv, **kwargs, testLoader=Loader(), exit=False)
        if missing := excluded - matched:
            parser.error(f"unknown exclusions: {', '.join(sorted(missing))}")
        sys.exit(not program.result.wasSuccessful())

    unittest.main = run
    sys.argv = [str(args.script)]
    sys.path.insert(0, str(args.script.resolve().parent))
    runpy.run_path(str(args.script), run_name="__main__")


if __name__ == "__main__":
    main()
