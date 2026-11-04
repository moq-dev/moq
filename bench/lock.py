#!/usr/bin/env python3
"""Take the host's benchmark lock on a descriptor the caller opened; see bench/lock.sh.

    lock.py PATH FD PID COMMAND...

While another session holds the lock, says which and waits for it. Then records
PID, the start time, the working directory, and COMMAND as the holder, once FD
is checked to be PATH itself rather than a file a link there points to.
"""

import datetime
import fcntl
import json
import os
import shlex
import stat
import sys


def describe(record):
    """Name the holder a lock record describes, noting when it exited but left a process holding the lock."""
    try:
        holder = json.loads(record)
        pid, started, cwd, command = holder["pid"], holder["started"], holder["cwd"], shlex.join(holder["command"])
        os.kill(pid, 0)
        state = ""
    except (ValueError, KeyError, TypeError):
        # A holder between truncating and writing its record, or a file another tool wrote.
        return "an unrecorded holder"
    except ProcessLookupError:
        state = " (exited; a process it started still holds the lock)"
    except PermissionError:
        state = ""
    return f"PID {pid}{state}, started {started} in {cwd}: {command}"


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


def check(path, fd):
    """Refuse fd unless it is the regular file at path itself, so a link planted there can't redirect the record."""
    entry, opened = os.lstat(path), os.fstat(fd)
    if not stat.S_ISREG(entry.st_mode) or (entry.st_dev, entry.st_ino) != (opened.st_dev, opened.st_ino):
        sys.exit(f"lock.py: {path} is not a regular file of its own; remove it and retry")


def main():
    if len(sys.argv) < 5 or not sys.argv[2].isdigit() or not sys.argv[3].isdigit():
        sys.exit("usage: lock.py PATH FD PID COMMAND...")
    path, fd = sys.argv[1], int(sys.argv[2])
    check(path, fd)
    try:
        acquire(fd, int(sys.argv[3]), sys.argv[4:])
    except KeyboardInterrupt:
        sys.exit(130)


if __name__ == "__main__":
    main()
