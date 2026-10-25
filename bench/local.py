#!/usr/bin/env python3
"""Compare suite fanout across isolated checkouts, separating admission from work."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import signal
import sys
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]


def main():
    if sys.argv[1:2] == ["--started"]:
        started = time.monotonic()
        result = subprocess.run(sys.argv[3:])
        Path(sys.argv[2]).write_text(json.dumps({"start": started, "end": time.monotonic()}))
        sys.exit(result.returncode)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkout", action="append", type=Path, required=True)
    parser.add_argument("--workers", nargs="+", type=int, required=True)
    parser.add_argument("--jobs", nargs="+", type=int, help="MOQ_CHECK_JOBS lane counts to sweep")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--cache", required=True, help="Describe the prepared cache state")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or args.samples < 1 or any(n < 1 for n in args.workers + (args.jobs or [])):
        parser.error("a command, positive samples and positive worker and job counts are required")
    if os.name != "posix":
        parser.error("CPU accounting requires a POSIX host")
    checkouts = [checkout.resolve() for checkout in args.checkout]
    if not all(checkout.is_dir() for checkout in checkouts) or len(set(checkouts)) != len(checkouts):
        parser.error("each checkout must be a distinct existing directory")
    # Every checkout runs solo, so a concurrent run is compared against its own baseline.
    groups = [[checkout] for checkout in checkouts] + ([checkouts] if len(checkouts) > 1 else [])
    def cancel(number, _frame):
        raise SystemExit(128 + number)
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, cancel)
    # Alternating the order spreads drift in host load or thermals over solo and concurrent runs.
    for sample in range(args.samples):
        for group in groups if sample % 2 == 0 else reversed(groups):
            for workers, jobs in ((w, j) for w in args.workers for j in args.jobs or [None]):
                processes = []
                load = os.getloadavg()
                try:
                    for checkout in group:
                        directory = checkout / ".scratch/local-bench"
                        directory.mkdir(parents=True, exist_ok=True)
                        stem = directory / str(uuid.uuid4())
                        stamp = stem.with_suffix(".started")
                        with stem.with_suffix(".log").open("w") as log:
                            env = dict(os.environ, NEXTEST_TEST_THREADS=str(workers))
                            # Without --jobs, the record's null means sh/dispatch.sh's default.
                            env.pop("MOQ_CHECK_JOBS", None)
                            if jobs:
                                env["MOQ_CHECK_JOBS"] = str(jobs)
                            env.pop("QUEST_GATE_ADMITTED", None)
                            env.pop("MOQ_LOCAL_SUPERVISED", None)
                            env.pop("GITHUB_ACTIONS", None)
                            began = time.monotonic()
                            process = subprocess.Popen(["bash", str(ROOT / "sh/local.sh"), sys.executable,
                                                        str(Path(__file__).resolve()), "--started", str(stamp),
                                                        *command], cwd=checkout, env=env, stdout=log, stderr=log)
                            processes.append((process, began, stamp))
                    results = []
                    for process, began, stamp in processes:
                        _, status, usage = os.wait4(process.pid, 0)
                        process.returncode = os.waitstatus_to_exitcode(status)
                        if process.returncode:
                            raise RuntimeError(f"command failed ({process.returncode}); see {stamp.with_suffix('.log')}")
                        timing = json.loads(stamp.read_text())
                        started, ended = timing["start"], timing["end"]
                        results.append({"admission_s": started - began, "work_s": ended - started,
                                        "total_s": ended - began, "cpu_s": usage.ru_utime + usage.ru_stime})
                finally:
                    for process, _, _ in processes:
                        if process.returncode is None:
                            process.terminate()
                            process.wait()
                print(json.dumps({"copies": len(group), "checkouts": [str(checkout) for checkout in group],
                                  "nextest_workers": workers, "check_jobs": jobs, "sample": sample,
                                  "cache": args.cache, "host_load": load, "command": command,
                                  "runs": results}), flush=True)


if __name__ == "__main__":
    main()
