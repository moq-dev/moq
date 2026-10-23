#!/usr/bin/env python3
"""Revise a generated clip's PMT mid-stream, and grade a round-trip's capture of the revisions.

    ./revision.py inject source.ts out.ts [--report revision.json]
    ./revision.py grade source.ts capture.ts [--report-json revision.json]

`inject` adds asynchronous KLV (stream_type 0x06 with a `KLVA` registration, no PTS) in the
clip's null slots from a third of its program clock to two thirds, every 100 ms, and revises
the PMT at each end: it lists the stream at the next version_number before the stream's first
packet, and drops it at the version after that once its last packet is out. Every other packet
stays in place.

`grade` holds the capture to what a TS export promises of a revision. Its PMT runs through the
source's layouts in order (the source's PIDs and stream types, the KLV entry byte-equal), each
at the version after the one before. Every packet of the stream falls between the PMT that lists
it and the one that drops it, and every PES of the stream arrives, in order and byte-equal.
compliance.py grades the rest of the capture, the version sequence included.

Exit status is 0 when every check passes, 1 otherwise.
"""

import argparse
import json
import sys
from pathlib import Path

import compliance
import klv

PID = 0x112
STREAM = (0x06, PID, klv.KLVA)


class Failure(Exception):
    """A named grading failure."""

    def __init__(self, check, detail):
        self.check = check
        super().__init__(f"{check}: {detail}")


def revise(packet, version, streams):
    """A PMT packet whose section is at `version` with `streams` appended, its CRC recomputed."""
    body = klv.payload(packet)
    start = len(packet) - len(body) + 1 + body[0]
    section = bytearray(klv.single_section(packet))
    section[5] = (section[5] & 0xC1) | version << 1
    entries = b"".join(bytes([kind, 0xE0 | route >> 8, route & 0xFF, 0xF0 | len(info) >> 8, len(info) & 0xFF]) + info for kind, route, info in streams)
    section = section[:-4] + entries
    length = len(section) + 4 - 3
    section[1] = (section[1] & 0xF0) | length >> 8
    section[2] = length & 0xFF
    section += compliance.crc32_mpeg(bytes(section)).to_bytes(4, "big")
    if start + len(section) > klv.SIZE:
        raise ValueError("the PMT outgrows its packet")
    out = bytearray(packet[:start]) + section
    out += b"\xff" * (klv.SIZE - len(out))
    return bytes(out)


def clock_span(source, pcr_pid):
    """The first and last PCR on `pcr_pid`, unwrapped."""
    values = []
    for packet in source:
        if klv.pid(packet) == pcr_pid and (now := klv.pcr(packet)) is not None:
            values.append(now if not values else values[-1] + (now - values[-1] % klv.PCR_WRAP) % klv.PCR_WRAP)
    if len(values) < 2:
        raise ValueError("fewer than two PCRs")
    return values[0], values[-1]


def inject(data):
    """`data` with the KLV stream and its two PMT revisions, and a report of what went in."""
    source = klv.packets(data)
    prog = klv.program(data)
    if PID in prog.streams:
        raise ValueError(f"the clip already carries PID {PID:#x}")
    first, last = clock_span(source, prog.pcr_pid)
    start, end = first + (last - first) // 3, first + 2 * (last - first) // 3
    version = None
    # 0 before the stream, 1 while it runs, 2 once it ended and its last packet is out.
    phase = 0
    listed = False
    queue = []
    counter = count = 0
    deadline = now = None
    revised = []
    out = []
    for index, packet in enumerate(source):
        route = klv.pid(packet)
        if route == prog.pcr_pid and (value := klv.pcr(packet)) is not None:
            now = value if now is None else now + (value - now % klv.PCR_WRAP) % klv.PCR_WRAP
            if phase == 0 and now >= start:
                phase, deadline = 1, now
            while phase == 1 and deadline < end and now >= deadline:
                pes = klv.async_klv(count)
                queue.extend(klv.split(PID, pes, counter))
                counter += -(-len(pes) // 184)
                count += 1
                deadline += klv.ASYNC_PERIOD
        if phase == 1 and now >= end and not queue:
            phase = 2
        if route == prog.pmt_pid and (klv.single_section(packet) or b"\0")[0] == 0x02:
            version = (klv.single_section(packet)[5] >> 1) & 0x1F if version is None else version
            if phase:
                packet = revise(packet, (version + phase) % 32, [STREAM] if phase == 1 else [])
                if len(revised) < phase:
                    revised.append(index)
            listed = phase == 1
        if route == klv.NULL and queue and listed:
            packet = queue.pop(0)
        out.append(packet)
    if phase != 2 or len(revised) != 2:
        raise ValueError("the clip ended before the stream did and the PMT dropped it")
    return b"".join(out), {"pes": count, "revised_at": revised}


def layouts(data):
    """(index, version, {PID: (stream_type, descriptors)}) of each PMT of the first program, each
    run of repeats once."""
    prog = klv.program(data)
    found = []
    for index, packet in enumerate(klv.packets(data)):
        section = klv.single_section(packet)
        if klv.pid(packet) != prog.pmt_pid or section is None or section[0] != 0x02:
            continue
        table = ((section[5] >> 1) & 0x1F, klv.read_pmt(section)[1])
        if not found or found[-1][1:] != table:
            found.append((index, *table))
    return found


def grade(source, capture):
    """Grade `capture` against `source`, returning a report; raises Failure on the first miss."""
    types = lambda streams: {route: kind for route, (kind, _) in streams.items()}
    src, cap = layouts(source), layouts(capture)
    if [types(streams) for _, _, streams in cap] != [types(streams) for _, _, streams in src]:
        raise Failure("layouts", f"the capture's PMTs list {[sorted(types(s).items()) for _, _, s in cap]}, the source's {[sorted(types(s).items()) for _, _, s in src]}")
    for (_, newest, _), (index, version, _) in zip(cap, cap[1:]):
        if version != (newest + 1) % 32:
            raise Failure("version", f"version {version} at packet {index} follows version {newest}")
    with_stream = [k for k, (_, _, streams) in enumerate(cap) if PID in streams]
    if len(with_stream) != 1:
        raise Failure("layouts", f"{len(with_stream)} of the capture's PMTs list PID {PID:#x}, not one")
    listed = with_stream[0]
    if cap[listed][2][PID] != (STREAM[0], STREAM[2]):
        raise Failure("pmt", f"PID {PID:#x}: {cap[listed][2][PID]} in the capture, not {(STREAM[0], STREAM[2])}")
    opened = cap[listed][0]
    closed = cap[listed + 1][0] if listed + 1 < len(cap) else None
    for index, packet in enumerate(klv.packets(capture)):
        if klv.pid(packet) == PID and not (opened < index and (closed is None or index < closed)):
            raise Failure("window", f"a packet on PID {PID:#x} at {index}, outside the PMTs listing it ({opened} to {closed})")
    src_pes = [pes.data for pes in klv.private_pes(source, [PID])[PID]]
    cap_pes = [pes.data for pes in klv.private_pes(capture, [PID])[PID]]
    if cap_pes != src_pes:
        raise Failure("pes", f"PID {PID:#x}: {len(cap_pes)} PES in the capture, not the source's {len(src_pes)} in order")
    return {"versions": [version for _, version, _ in cap], "pes": len(cap_pes), "opened": opened, "closed": closed}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    add = sub.add_parser("inject")
    add.add_argument("source", type=Path)
    add.add_argument("output", type=Path)
    add.add_argument("--report", type=Path)
    check = sub.add_parser("grade")
    check.add_argument("source", type=Path)
    check.add_argument("capture", type=Path)
    check.add_argument("--report-json", type=Path)
    args = parser.parse_args()

    if args.command == "inject":
        data, report = inject(args.source.read_bytes())
        args.output.write_bytes(data)
        if args.report:
            args.report.write_text(json.dumps(report, indent=2) + "\n")
        print("revised the PMT:", report)
        return 0

    print("### PMT revisions (capture against the source)")
    try:
        report = grade(args.source.read_bytes(), args.capture.read_bytes())
    except Failure as err:
        print(f"  FAIL  {err}")
        return 1
    if args.report_json:
        args.report_json.write_text(json.dumps(report, indent=2) + "\n")
    print(f"  PASS  versions {report['versions']}; {report['pes']} PES on PID {PID:#x}, "
          f"between the PMTs at packets {report['opened']} and {report['closed']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
