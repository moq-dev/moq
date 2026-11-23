#!/usr/bin/env python3
"""Run a command, copy its stdout to a file, and record how long its first byte took.

    ./first-byte.py out.ts first-byte.txt -- moq ... export ts

The delay is measured from starting the command, so it covers the export's whole start: its
connection, the catalog, and the group it joins on. It is written to the second file in seconds,
or `none` if the command wrote nothing. Exit status is the command's, as a shell reports it.
"""

import subprocess
import sys
import time


def main() -> None:
    if len(sys.argv) < 5 or sys.argv[3] != "--":
        sys.exit(__doc__)
    out, report, command = sys.argv[1], sys.argv[2], sys.argv[4:]
    start = time.monotonic()
    first = None
    with open(out, "wb") as sink, subprocess.Popen(command, stdout=subprocess.PIPE) as child:
        while chunk := child.stdout.read1(65536):
            if first is None:
                first = time.monotonic() - start
            sink.write(chunk)
    with open(report, "w") as f:
        f.write("none\n" if first is None else f"{first:.3f}\n")
    sys.exit(128 - child.returncode if child.returncode < 0 else child.returncode)


if __name__ == "__main__":
    main()
