#!/usr/bin/env python3
"""Grade an `export ts --passthrough` capture against the source `import ts --passthrough` read.

    ./passthrough.py source.ts capture.ts [--loss-ok] [--report-json report.json]

The admitted interval is the source from the capture's first packet to the end: the importer
withholds what comes before its first group while it acquires (the prefix), and the export
opens on whichever group it joins. Every captured packet must be the source packet at its exact
position in that interval, in order: null packets, tables, unknown PIDs and adaptation bytes
included, nothing rewritten or duplicated. Where the capture skips source packets, that is a
gap: objects the export dropped late or the path never delivered.

A position is only claimed for a run of RUN packets that matches the source there, so a
duplicated null or table packet cannot pass as the identical one a gap later.

On a complete path (no gap, nothing missing at the end) the capture is the admitted interval
byte for byte, and every compliance.py check, TSDuck's structure and the T-STD and system
buffer models included, must give it the verdict it gives that interval. The export carries
whatever the source is, so it is graded against the source, not against an absolute bar. Under
`--loss-ok` gaps are reported rather than failed, and the checks report on the capture alone:
a gap can take part of a PES or the next table with it, so the bytes being the source's says
nothing about whether the rest decodes.

Exit status is 0 when the capture passes, 1 otherwise.
"""

import argparse
import json
import sys
import tempfile
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

PACKET = 188
# Matching packets a position needs before it is claimed.
RUN = 16


@dataclass
class Placement:
    """Where a capture's packets sit in its source."""

    start: int  # source packet the capture opens on
    total: int  # source packets
    gaps: list[tuple[int, int]] = field(default_factory=list)  # (source packet, length)
    tail: int = 0  # source packets after the capture's last one
    missing_pids: Counter = field(default_factory=Counter)

    @property
    def complete(self) -> bool:
        return not self.gaps and not self.tail


class Mismatch(Exception):
    """A captured packet that is not the source's at any position after the one before it."""


def packets(data: bytes) -> list[bytes]:
    if len(data) % PACKET:
        raise Mismatch(f"{len(data)} bytes is not whole {PACKET}-byte packets")
    return [data[i : i + PACKET] for i in range(0, len(data), PACKET)]


def pid(pkt: bytes) -> int:
    return (pkt[1] & 0x1F) << 8 | pkt[2]


def place(source: list[bytes], capture: list[bytes]) -> Placement:
    """Map every captured packet onto its source position, or raise Mismatch."""
    if not capture:
        raise Mismatch("the capture is empty")
    positions = defaultdict(list)
    for i, pkt in enumerate(source):
        positions[pkt].append(i)

    def runs(i: int, after: int) -> list[int]:
        n = min(RUN, len(capture) - i)
        return [j for j in positions.get(capture[i], ()) if j >= after and source[j : j + n] == capture[i : i + n]]

    starts = runs(0, 0)
    if not starts:
        raise Mismatch("the capture's first packets are nowhere in the source")
    if len(starts) > 1:
        raise Mismatch(f"the capture's first {RUN} packets match the source at packets {starts[:4]}: ambiguous")
    placement = Placement(start=starts[0], total=len(source))

    j = placement.start
    for i, pkt in enumerate(capture):
        if j < len(source) and source[j] == pkt:
            j += 1
            continue
        found = runs(i, j)
        if not found:
            raise Mismatch(
                f"captured packet {i} (PID {pid(pkt):#x}) is not the source's at or after packet {j}: "
                "rewritten, duplicated or reordered"
            )
        placement.gaps.append((j, found[0] - j))
        placement.missing_pids.update(pid(p) for p in source[j : found[0]])
        j = found[0] + 1
    placement.tail = len(source) - j
    placement.missing_pids.update(pid(p) for p in source[j:])
    return placement


def verdicts(path: Path) -> dict[str, list[str]]:
    """Every compliance.py check's status and detail for `path`."""
    import compliance

    return {c.name: [c.status.value, c.detail] for c in compliance.analyze(str(path))}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--loss-ok", action="store_true", help="report gaps instead of failing on them")
    parser.add_argument("--report-json", type=Path)
    args = parser.parse_args()

    source_data = args.source.read_bytes()
    capture_data = args.capture.read_bytes()
    try:
        placement = place(packets(source_data), packets(capture_data))
    except Mismatch as err:
        print(f"passthrough: FAIL: {err}", file=sys.stderr)
        return 1

    lost = sum(n for _, n in placement.gaps) + placement.tail
    admitted = placement.total - placement.start
    print("### passthrough packet positions")
    print(f"  prefix: {placement.start} source packets before the capture's first ({placement.start * PACKET} bytes)")
    print(f"  admitted: {admitted} packets, {admitted - lost} captured at their exact positions")
    print(f"  gaps: {len(placement.gaps)}, {lost} packets missing ({placement.tail} at the end)")
    if lost:
        print("  missing by PID: " + ", ".join(f"{p:#x}={n}" for p, n in sorted(placement.missing_pids.items())))

    report = {
        "prefix_packets": placement.start,
        "admitted_packets": admitted,
        "gaps": placement.gaps,
        "tail_packets": placement.tail,
        "missing_by_pid": {f"{p:#x}": n for p, n in placement.missing_pids.items()},
    }
    code = 0
    if placement.complete:
        with tempfile.NamedTemporaryFile(suffix=".ts") as interval:
            interval.write(source_data[placement.start * PACKET :])
            interval.flush()
            expected = verdicts(Path(interval.name))
        actual = verdicts(args.capture)
        report["compliance"] = {"admitted": expected, "capture": actual}
        print("### compliance.py, capture against the admitted interval")
        for name in sorted(expected.keys() | actual.keys()):
            same = actual.get(name) == expected.get(name)
            status = (actual.get(name) or ["-"])[0]
            print(f"  {'same' if same else 'DIFF'}  {name}: {status}  {(actual.get(name) or ['', ''])[1]}")
            if not same:
                print(f"passthrough: FAIL: {name} grades the capture {actual.get(name)}, its source {expected.get(name)}",
                      file=sys.stderr)
                code = 1
    else:
        report["compliance"] = {"capture": verdicts(args.capture)}
        print("### compliance.py on the capture (report only: it has gaps)")
        for name, (status, detail) in report["compliance"]["capture"].items():
            print(f"  {name}: {status}  {detail}")
        if not args.loss_ok:
            print("passthrough: FAIL: the capture is missing source packets; --loss-ok reports gaps instead", file=sys.stderr)
            code = 1

    if args.report_json:
        args.report_json.write_text(json.dumps(report, indent=2))
    if code == 0:
        print("passthrough: PASS" + (" (with gaps)" if lost else ""))
    return code


if __name__ == "__main__":
    sys.exit(main())
