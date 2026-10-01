#!/usr/bin/env python3
"""Positive and negative controls for the T-STD model in compliance.py.

The positive control is a real broadcast encoder's output (a Kyrion contribution
feed: AVC High@4.0 plus two MPEG-1 Layer II tracks), which a broadcast chain only
carries because it is T-STD compliant. Each negative restamps that capture's PCRs
at a different constant rate with TSDuck's `pcradjust`, leaving every PES, PTS and
DTS untouched, so the only thing that changes is when the bytes arrive:

    as captured     the broadcast itself                       must pass
    restamped 1x    PCRs rewritten at the capture's own rate   must pass (the rewrite is not the fault)
    0.7x            delivered too slowly                       EB and B underflow
    4x              delivered too early                        B overflow
    15x             delivered in a burst                       TB, MB and B overflow

A model that passes everything, or fails everything, cannot tell these apart.
"""

import os
import subprocess
import sys
import tempfile

DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DIR)

import compliance  # noqa: E402

FIXTURE = os.path.join(DIR, "../../rs/moq-mux/src/container/ts/test_data/scte35/kyrion_dirtystart.ts")

# (name, PCR rate as a multiple of the capture's own, violations that must all appear).
CASES = [
    ("as captured", None, set()),
    ("restamped 1x", 1.0, set()),
    ("0.7x", 0.7, {"EB underflow", "B underflow"}),
    ("4x", 4.0, {"B overflow"}),
    ("15x", 15.0, {"TB overflow", "MB overflow", "B overflow"}),
]


def grade(path: str) -> compliance.Check:
    """compliance.py's tstd verdict on one file."""
    size = compliance.detect_packet_size(compliance.run_tsanalyze(path))
    return compliance.check_tstd(path, size, compliance.scan_packets(path, size))


def main() -> int:
    """Run every case and exit non-zero if any verdict is not the expected one."""
    rate = compliance.run_tsanalyze(FIXTURE)["ts"]["bitrate"]
    failed = 0
    with tempfile.TemporaryDirectory() as tmp:
        for name, scale, expected in CASES:
            path = FIXTURE
            if scale is not None:
                path = os.path.join(tmp, f"{scale}.ts")
                subprocess.run(
                    ["tsp", "-I", "file", FIXTURE, "-P", "pcradjust", "--bitrate", f"{rate * scale:.0f}"]
                    + ["--ignore-pts", "--ignore-dts", "-O", "file", path],
                    check=True,
                )
            check = grade(path)
            streams = check.metrics.get("streams", {})
            seen = {v for s in streams.values() for v in s["violations"]}
            graded = all(s["access_units"] for s in streams.values()) and len(streams) == 3
            if expected:
                ok = expected <= seen
            else:
                # Not vacuous: all three streams must have had access units to grade.
                ok = check.status == compliance.Status.PASS and graded
            failed += not ok
            want = ", ".join(sorted(expected)) or "pass"
            print(f"  {'ok  ' if ok else 'FAIL'}  {name:<14} want {want:<40} got {check.detail}")
    if failed:
        print(f"tstd controls: {failed} of {len(CASES)} cases wrong", file=sys.stderr)
        return 1
    print("tstd controls: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
