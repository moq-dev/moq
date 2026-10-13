#!/usr/bin/env python3
"""Assert TSDuck PCR accuracy against a known CBR byte rate."""

import argparse
import re
import subprocess
from pathlib import Path


def check(path, bitrate, skip, log):
    command = [
        "tsp",
        "-I",
        "file",
        str(path),
        "-P",
        "skip",
        str(skip),
        "-P",
        "pcrverify",
        "--bitrate",
        str(bitrate),
        "--absolute",
        "--jitter-max",
        "13",
        "-O",
        "drop",
    ]
    # 13 ticks at 27 MHz is the integral threshold within +/-500 ns. Do not
    # rely on pcrverify's much looser default or treat no measured PCRs as a pass.
    with log.open("w") as output:
        result = subprocess.run(command, stdout=output, stderr=subprocess.STDOUT, check=False)
    if result.returncode:
        raise RuntimeError(f"tsp exited {result.returncode}: {log}")
    matches = re.findall(r"([\d,]+) PCR OK, ([\d,]+) with", log.read_text())
    if len(matches) != 1:
        raise RuntimeError(f"expected one pcrverify verdict: {log}")
    good, bad = (int(v.replace(",", "")) for v in matches[0])
    if not good or bad:
        raise RuntimeError(f"PCR accuracy failed: {good} OK, {bad} outside threshold: {log}")
    print(f"PCR accuracy: {good} OK, 0 outside +/-500 ns")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ts", type=Path)
    parser.add_argument("--bitrate", required=True, type=int)
    parser.add_argument("--skip", default=0, type=int)
    parser.add_argument("--log", required=True, type=Path)
    args = parser.parse_args()
    check(args.ts, args.bitrate, args.skip, args.log)
