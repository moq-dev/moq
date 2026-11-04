#!/usr/bin/env python3
"""Take the host's benchmark lock on a descriptor the caller opened; see bench/lock.sh.

    lock.py FD PID COMMAND...

While another session holds the lock, says which and waits for it. Then records
PID, the start time, the working directory, and COMMAND as the holder.
"""

import datetime
import fcntl
import json
import os
import shlex
import sys


def describe(record):
    """Name the holder a lock record describes, noting when it exited but left a process holding the lock."""
    try:
        holder = json.loads(record)
        pid, started, cwd, command = holder["pid"], holder["started"], holder["cwd"], holder["command"]
        os.kill(pid, 0)
        state = ""
    except (ValueError, KeyError, TypeError):
        # A holder between truncating and writing its record, or a file another tool wrote.
        return "an unrecorded holder"
    except ProcessLookupError:
        state = " (exited; a process it started still holds the lock)"
    except PermissionError:
        state = ""
    return f"PID {pid}{state}, started {started} in {cwd}: {shlex.join(command)}"


def acquire(fd, pid, command):
    """Lock fd exclusively, waiting while another session holds it, then record pid and command as the holder."""
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        holder = describe(os.pread(fd, 1 << 16, 0))
        print(f"waiting for the benchmark session holding the host lock: {holder}", file=sys.stderr, flush=True)
        fcntl.flock(fd, fcntl.LOCK_EX)
    started = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    record = json.dumps({"pid": pid, "started": started, "cwd": os.getcwd(), "command": command})
    os.ftruncate(fd, 0)
    os.pwrite(fd, (record + "\n").encode(), 0)


def main():
    if len(sys.argv) < 4 or not sys.argv[1].isdigit() or not sys.argv[2].isdigit():
        sys.exit("usage: lock.py FD PID COMMAND...")
    try:
        acquire(int(sys.argv[1]), int(sys.argv[2]), sys.argv[3:])
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
