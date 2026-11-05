#!/usr/bin/env python3
"""Verify the relay started exactly the requested QUIC worker threads."""

import argparse
import json
import re
from pathlib import Path


def verify(tasks, runtime, count):
    names = sorted(path.read_text().strip() for path in tasks.glob("*/comm"))
    workers = sorted(name for name in names if re.fullmatch(r"moq-(?:uring|quic)-[0-9]+", name))
    prefix = {"tokio-workers": "moq-quic", "io-uring-workers": "moq-uring"}[runtime]
    expected = sorted(f"{prefix}-{index}" for index in range(count))
    if workers != expected:
        raise ValueError(f"{runtime} needs {expected}; relay started {workers}")
    return {"runtime": runtime, "workers": workers}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int)
    parser.add_argument("runtime", choices=["tokio-workers", "io-uring-workers"])
    parser.add_argument("count", type=int)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        result = verify(Path(f"/proc/{args.pid}/task"), args.runtime, args.count)
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")
    args.output.write_text(json.dumps(result, indent=2) + "\n")
