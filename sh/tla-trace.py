#!/usr/bin/env python3
"""Check recorded traces against a TLA+ model: `sh/tla-trace.py <trace spec> <trace>...`.

Trace validation: a test records each run as newline-delimited JSON, and a trace
spec beside the model (`verify/<model>/trace/<Name>.tla`, with `<Name>.cfg`)
follows one trace, named by the environment variable TRACE, as a behaviour of the
model it EXTENDS. TLC reports a deadlock where the run leaves the model, so each
trace must pass with TLC's deadlock check on.

The trace spec and the model's modules are linked into one directory under the
checkout's `.tmp/`, since TLC finds a module only beside the spec. Each trace is
one TLC run on one worker through sh/tlc.py; four run at once. A trace that
passes is removed, so what is left is what failed.
"""

import os
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
JOBS = 4


def check(spec, cfg, trace):
    env = dict(os.environ, TRACE=str(trace.resolve()))
    command = [sys.executable, str(ROOT / "sh/tlc.py"), str(spec), "-workers", "1", "-config", str(cfg)]
    run = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return trace, run.returncode, run.stdout


def main(argv):
    if len(argv) < 2:
        sys.exit("usage: sh/tla-trace.py <trace spec> <trace>...")
    spec, traces = Path(argv[0]).resolve(), [Path(trace) for trace in argv[1:]]
    model = spec.parent.parent
    (ROOT / ".tmp").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=ROOT / ".tmp", prefix="tla-trace-") as run:
        run = Path(run)
        for module in [*model.glob("*.tla"), spec]:
            (run / module.name).symlink_to(module)
        cfg = spec.with_suffix(".cfg")
        failed = []
        with ThreadPoolExecutor(JOBS) as pool:
            for trace, status, out in pool.map(lambda trace: check(run / spec.name, cfg, trace), traces):
                if status == 0:
                    trace.unlink()
                else:
                    failed.append(trace)
                    print(f"::group::{trace}: exit {status}\n{out}::endgroup::", flush=True)
    print(f"{len(traces) - len(failed)} of {len(traces)} traces are behaviours of the model")
    if failed:
        print("error: left the model:", *failed, sep="\n  ", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
