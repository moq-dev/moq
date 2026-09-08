#!/usr/bin/env python3

"""Join and hold a harness process group until the harness reaps it."""

import os
import signal
import sys


def main() -> None:
    group = int(sys.argv[1])
    with open(sys.argv[2], "w", encoding="utf-8") as ready:
        try:
            os.setpgid(0, group)
        except OSError as error:
            ready.write(f"error: {error}\n")
            return
        ready.write("ok\n")
        ready.flush()
        while True:
            signal.pause()


if __name__ == "__main__":
    main()
