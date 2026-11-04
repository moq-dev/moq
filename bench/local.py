#!/usr/bin/env python3
"""Compare suite fanout across isolated checkouts, separating admission from work."""

import argparse
import json
import os
import select
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def identity(pid):
    """Name pid with its start time, which tells it apart from a later process reusing the PID. Linux only."""
    stat = Path(f"/proc/{pid}/stat").read_text()
    return f"{pid}:{stat[stat.rindex(')') + 2 :].split()[19]}"


def watch(coordinator):
    """Open a pidfd that becomes readable once the process identity() named exits, or None if it already has."""
    pid = int(coordinator.split(":")[0])
    try:
        pidfd = os.pidfd_open(pid)
    except ProcessLookupError:
        return None
    try:
        if identity(pid) == coordinator:
            return pidfd
    except FileNotFoundError:
        pass
    os.close(pidfd)
    return None


def stop_on(pidfd, process):
    """Stop process once pidfd's process exits."""
    select.select([pidfd], [], [])
    process.terminate()


def run_started(stamp, coordinator, command):
    """Run command inside the gate, recording when it started and ended in stamp.

    Workers can't hold the benchmark lock (see main), so on Linux each stops once the coordinator
    holding it exits, even when killed outright, and Bun's --no-orphans (sh/local.ts) then stops
    whatever command left behind. Watching it from here, rather than signalling the gate, also
    works inside a Quest scope, whose gate holds back signals it receives. macOS has no
    pidfd, so a SIGKILL of the coordinator there leaves workers running: an accepted gap.
    """
    exited = watch(coordinator) if sys.platform == "linux" else None
    if sys.platform == "linux" and exited is None:
        sys.exit("local.py: the coordinator holding the benchmark lock exited")
    started = time.monotonic()
    with subprocess.Popen(command) as process:
        if exited is not None:
            threading.Thread(target=stop_on, args=(exited, process), daemon=True).start()
        process.wait()
    Path(stamp).write_text(json.dumps({"start": started, "end": time.monotonic()}))
    sys.exit(process.returncode)


def main():
    if sys.argv[1:2] == ["--started"]:
        run_started(sys.argv[2], sys.argv[3], sys.argv[4:])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkout", action="append", type=Path, required=True)
    parser.add_argument("--workers", nargs="+", type=int, required=True)
    parser.add_argument("--jobs", nargs="+", type=int, help="CHECK_JOBS lane counts to sweep")
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
    if "BENCH_LOCK_HELD" not in os.environ:
        command = [sys.executable, str(ROOT / "bench/local.py"), *sys.argv[1:]]
        os.execvp("bash", ["bash", str(ROOT / "bench/lock.sh"), *command])
    # Every checkout runs solo, so a concurrent run is compared against its own baseline.
    groups = [[checkout] for checkout in checkouts] + ([checkouts] if len(checkouts) > 1 else [])

    def cancel(number, _frame):
        raise SystemExit(128 + number)

    coordinator = identity(os.getpid()) if sys.platform == "linux" else str(os.getpid())
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
                        directory = checkout / ".tmp/local-bench"
                        directory.mkdir(parents=True, exist_ok=True)
                        stem = directory / str(uuid.uuid4())
                        stamp = stem.with_suffix(".started")
                        with stem.with_suffix(".log").open("w") as log:
                            env = dict(os.environ, NEXTEST_TEST_THREADS=str(workers))
                            # Without --jobs, the record's null means sh/dispatch.sh's default.
                            env.pop("CHECK_JOBS", None)
                            if jobs:
                                env["CHECK_JOBS"] = str(jobs)
                            env.pop("QUEST_GATE_ADMITTED", None)
                            env.pop("LOCAL_SUPERVISED", None)
                            env.pop("GITHUB_ACTIONS", None)
                            began = time.monotonic()
                            process = subprocess.Popen(
                                [
                                    "bash",
                                    str(ROOT / "sh/local.sh"),
                                    sys.executable,
                                    str(Path(__file__).resolve()),
                                    "--started",
                                    str(stamp),
                                    coordinator,
                                    *command,
                                ],
                                cwd=checkout,
                                env=env,
                                stdout=log,
                                stderr=log,
                                # Workers never get the lock's descriptor: close_fds drops it here, and
                                # quest gate and Bun would drop it anyway. Instead each watches this
                                # process (see run_started).
                                close_fds=True,
                            )
                            processes.append((process, began, stamp))
                    results = []
                    for process, began, stamp in processes:
                        _, status, usage = os.wait4(process.pid, 0)
                        process.returncode = os.waitstatus_to_exitcode(status)
                        if process.returncode:
                            raise RuntimeError(
                                f"command failed ({process.returncode}); see {stamp.with_suffix('.log')}"
                            )
                        timing = json.loads(stamp.read_text())
                        started, ended = timing["start"], timing["end"]
                        results.append(
                            {
                                "admission_s": started - began,
                                "work_s": ended - started,
                                "total_s": ended - began,
                                "cpu_s": usage.ru_utime + usage.ru_stime,
                            }
                        )
                finally:
                    for process, _, _ in processes:
                        if process.returncode is None:
                            process.terminate()
                            process.wait()
                print(
                    json.dumps(
                        {
                            "copies": len(group),
                            "checkouts": [str(checkout) for checkout in group],
                            "nextest_workers": workers,
                            "check_jobs": jobs,
                            "sample": sample,
                            "cache": args.cache,
                            "host_load": load,
                            "command": command,
                            "runs": results,
                        }
                    ),
                    flush=True,
                )


if __name__ == "__main__":
    main()
