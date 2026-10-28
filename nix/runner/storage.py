"""Check copy-on-write isolation and measure artifact copies against reflinks."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


with tempfile.TemporaryDirectory(dir=sys.argv[1]) as directory:
    root = Path(directory)
    source = root / "source"
    destination = root / "destination"
    block = b"x" * (1024 * 1024)
    for size in (1, 16, 64):
        with source.open("wb") as output:
            for _ in range(size):
                output.write(block)
            output.flush()
            os.fdatasync(output.fileno())
        for mode in ("never", "always"):
            started = time.perf_counter()
            subprocess.run(
                ["cp", f"--reflink={mode}", source, destination], check=True
            )
            with destination.open("r+b") as output:
                os.fdatasync(output.fileno())
                elapsed = time.perf_counter() - started
                assert output.read() == block * size
                output.seek(0)
                output.write(b"y")
                output.flush()
            with source.open("rb") as original:
                assert original.read(1) == b"x", "the clone changed the source"
            print(json.dumps({"MiB": size, "reflink": mode, "seconds": elapsed}))
            destination.unlink()
