#!/usr/bin/env python3
"""Inject KLV into a generated clip, and grade a round-trip's capture of it against the source.

    ./klv.py inject source.ts out.ts [--report klv.json]
    ./klv.py grade source.ts capture.ts [--report-json klv.json]

`inject` adds two private PES streams in the clip's null slots, leaving every other packet in
place: asynchronous KLV (stream_type 0x06 with a `KLVA` registration, private_stream_1, no PTS)
every 100 ms of program clock, and synchronous KLV (stream_type 0x15 with a metadata
descriptor, metadata_stream 0xFC) on each picture, presenting with it. Every PES is unique, so
a capture's can be found in the source.

`grade` holds the capture to what the importer and exporter promise for a PES they do not
decode. Each stream keeps its PID, stream_type and descriptors. Its PES arrive in source order
with none missing, from within the join (holes.JOIN) of the source's first to the source's last,
each byte-equal to its source PES outside the PTS and DTS, which all move by one shift modulo
2^33. Each goes out no earlier than its source placement (the program clock at its first byte)
moved by that shift, to a packet; a PES with a PTS arrives by it. FFmpeg must read each stream as
SMPTE KLV, with the same PES and PTS. compliance.py grades the rest of the capture with TSDuck.

Exit status is 0 when every check passes, 1 otherwise.
"""

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

import compliance
import holes

SIZE = 188
NULL = 0x1FFF
ASYNC_PID = 0x110
SYNC_PID = 0x111
# PMT entries: (stream_type, PID, ES descriptors).
KLVA = bytes([0x05, 0x04]) + b"KLVA"
# metadata_descriptor (ISO 13818-1 2.6.60): application format 0xFFFF named by its identifier,
# format 0xFF named by 'KLVA', service 0, no decoder config.
METADATA = bytes([0x26, 0x0D, 0xFF, 0xFF]) + b"KLVA" + bytes([0xFF]) + b"KLVA" + bytes([0x00, 0x0F])
STREAMS = [(0x06, ASYNC_PID, KLVA), (0x15, SYNC_PID, METADATA)]
# Program clock between asynchronous KLV, in 27 MHz ticks.
ASYNC_PERIOD = 27_000_000 // 10
# Every tenth asynchronous KLV spans several packets.
LONG = 10
PTS_WRAP = 1 << 33
PCR_WRAP = PTS_WRAP * 300
# A PCR's reference byte: the last of its base, in its packet.
PCR_OFFSET = 10
# Universal key of a MISB ST 0601 local set.
KEY = bytes([0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01, 0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00])


class Failure(Exception):
    """A named grading failure."""

    def __init__(self, check, detail):
        self.check = check
        super().__init__(f"{check}: {detail}")


def pid(packet):
    return ((packet[1] & 0x1F) << 8) | packet[2]


def payload(packet):
    control = (packet[3] >> 4) & 3
    if control == 1:
        return packet[4:]
    if control == 3 and 5 + packet[4] <= SIZE:
        return packet[5 + packet[4] :]
    return b""


def pcr(packet):
    if packet[3] & 0x20 and packet[4] >= 7 and packet[5] & 0x10:
        base = int.from_bytes(packet[6:10], "big") << 1 | packet[10] >> 7
        return base * 300 + ((packet[10] & 1) << 8 | packet[11])
    return None


def clock_value(field):
    return (field[0] >> 1 & 7) << 30 | field[1] << 22 | (field[2] >> 1) << 15 | field[3] << 7 | field[4] >> 1


def clock_field(prefix, ticks):
    return bytes([
        prefix << 4 | (ticks >> 29 & 0x0E) | 1,
        ticks >> 22 & 0xFF,
        (ticks >> 14 & 0xFE) | 1,
        ticks >> 7 & 0xFF,
        (ticks << 1 & 0xFE) | 1,
    ])


def packets(data):
    if len(data) % SIZE or any(data[at] != 0x47 for at in range(0, len(data), SIZE)):
        raise ValueError("expected aligned 188-byte TS packets")
    return [data[at : at + SIZE] for at in range(0, len(data), SIZE)]


def split(route, pes, counter):
    """`pes` on `route` in packets numbered from `counter`, the last padded with stuffing."""
    out = []
    for i, at in enumerate(range(0, len(pes), 184)):
        chunk = pes[at : at + 184]
        head = bytes([0x47, (0x40 if i == 0 else 0) | route >> 8, route & 0xFF])
        cc = (counter + i) & 15
        if len(chunk) == 184:
            out.append(head + bytes([0x10 | cc]) + chunk)
            continue
        stuffing = 183 - len(chunk)
        field = bytes([stuffing]) + (bytes([0]) + b"\xff" * (stuffing - 1) if stuffing else b"")
        out.append(head + bytes([0x30 | cc]) + field + chunk)
    return out


def pes_with(stream_id, flags, fields, data):
    return bytes([0, 0, 1, stream_id]) + (3 + len(fields) + len(data)).to_bytes(2, "big") + bytes([0x84, flags, len(fields)]) + fields + data


def async_klv(index):
    value = index.to_bytes(4, "big") + bytes([0xA5] * (400 if index % LONG == LONG - 1 else 24))
    return pes_with(0xBD, 0x00, b"", KEY + bytes([0x82]) + len(value).to_bytes(2, "big") + value)


def sync_klv(index, pts):
    value = index.to_bytes(4, "big") + bytes([0x5A] * 12)
    klv = KEY + bytes([len(value)]) + value
    # The metadata AU cell (2.12.4): service 0, its sequence number, one whole cell with a random
    # access point, then its length.
    cell = bytes([0x00, index & 0xFF, 0xDF]) + len(klv).to_bytes(2, "big") + klv
    return pes_with(0xFC, 0x80, clock_field(0x2, pts), cell)


@dataclass
class Program:
    pmt_pid: int
    pcr_pid: int
    streams: dict = field(default_factory=dict)  # PID -> (stream_type, descriptors)


def read_pmt(section):
    """(PCR PID, {PID: (stream_type, descriptors)}) of a PMT section."""
    length = (section[1] & 0x0F) << 8 | section[2]
    end = 3 + length - 4
    pcr_pid = (section[8] & 0x1F) << 8 | section[9]
    at = 12 + ((section[10] & 0x0F) << 8 | section[11])
    streams = {}
    while at + 5 <= end:
        info = (section[at + 3] & 0x0F) << 8 | section[at + 4]
        streams[(section[at + 1] & 0x1F) << 8 | section[at + 2]] = (section[at], bytes(section[at + 5 : at + 5 + info]))
        at += 5 + info
    return pcr_pid, streams


def single_section(packet):
    """The PSI section a packet starts and holds whole, or None."""
    if not packet[1] & 0x40:
        return None
    body = payload(packet)
    if not body:
        return None
    section = body[1 + body[0] :]
    if len(section) < 3:
        return None
    length = 3 + ((section[1] & 0x0F) << 8 | section[2])
    return section[:length] if length <= len(section) else None


def program(data):
    """The first program's PMT PID, PCR PID and streams."""
    pmt_pid = None
    for packet in packets(data):
        section = single_section(packet)
        if section is None:
            continue
        if pid(packet) == 0 and section[0] == 0x00 and pmt_pid is None:
            for at in range(8, len(section) - 4, 4):
                if section[at] << 8 | section[at + 1]:
                    pmt_pid = (section[at + 2] & 0x1F) << 8 | section[at + 3]
                    break
        elif pmt_pid is not None and pid(packet) == pmt_pid and section[0] == 0x02:
            pcr_pid, streams = read_pmt(section)
            return Program(pmt_pid, pcr_pid, streams)
    raise ValueError("no PAT and PMT")


def add_streams(packet):
    """A PMT packet with the KLV streams appended to its section, its CRC recomputed."""
    body = payload(packet)
    start = len(packet) - len(body) + 1 + body[0]
    section = bytearray(single_section(packet))
    entries = b"".join(bytes([kind, 0xE0 | route >> 8, route & 0xFF, 0xF0 | len(info) >> 8, len(info) & 0xFF]) + info for kind, route, info in STREAMS)
    section = section[:-4] + entries
    length = len(section) + 4 - 3
    section[1] = (section[1] & 0xF0) | length >> 8
    section[2] = length & 0xFF
    section += compliance.crc32_mpeg(bytes(section)).to_bytes(4, "big")
    if start + len(section) > SIZE:
        raise ValueError("the PMT outgrows its packet")
    out = bytearray(packet[:start]) + section
    out += b"\xff" * (SIZE - len(out))
    return bytes(out)


def inject(data):
    """`data` with the KLV streams in its null slots, and a report of what went in."""
    source = packets(data)
    prog = program(data)
    if ASYNC_PID in prog.streams or SYNC_PID in prog.streams:
        raise ValueError("the clip already carries the KLV PIDs")
    video = next(route for route, (kind, _) in prog.streams.items() if kind == 0x1B)
    out = []
    queue = []
    counters = {ASYNC_PID: 0, SYNC_PID: 0}
    counts = {ASYNC_PID: 0, SYNC_PID: 0}
    deadline = None
    last = None

    def enqueue(route, pes):
        queue.extend(split(route, pes, counters[route]))
        counters[route] += -(-len(pes) // 184)
        counts[route] += 1

    for packet in source:
        route = pid(packet)
        if route == prog.pmt_pid and (single_section(packet) or b"\0")[0] == 0x02:
            packet = add_streams(packet)
        if route == prog.pcr_pid and (now := pcr(packet)) is not None:
            # Unwrapped, so the period runs on across a wrap.
            now = now if last is None else last + (now - last % PCR_WRAP) % PCR_WRAP
            last = now
            deadline = deadline or now
            while now >= deadline:
                enqueue(ASYNC_PID, async_klv(counts[ASYNC_PID]))
                deadline += ASYNC_PERIOD
        body = payload(packet)
        if route == video and packet[1] & 0x40 and body[:3] == b"\0\0\1" and body[7] & 0x80:
            enqueue(SYNC_PID, sync_klv(counts[SYNC_PID], clock_value(body[9:14])))
        if route == NULL and queue:
            packet = queue.pop(0)
        out.append(packet)
    if queue:
        raise ValueError(f"{len(queue)} KLV packets found no null slot")
    return b"".join(out), {hex(route): count for route, count in counts.items()}


@dataclass
class Pes:
    at: int  # input offset of its start code
    data: bytes


def private_pes(data, routes):
    """Every whole PES on each of `routes`, with where it starts."""
    found = {route: [] for route in routes}
    open_ = {}
    for index, packet in enumerate(packets(data)):
        route = pid(packet)
        if route not in found:
            continue
        body = payload(packet)
        if packet[1] & 0x40:
            open_[route] = Pes(index * SIZE + SIZE - len(body), bytearray())
            found[route].append(open_[route])
        if route in open_:
            open_[route].data += body
    for route, pes in found.items():
        for item in pes:
            length = item.data[4] << 8 | item.data[5]
            if length == 0 or len(item.data) < 6 + length:
                raise Failure("pes", f"PID {route:#x}: a PES at byte {item.at} is truncated or unbounded")
            item.data = bytes(item.data[: 6 + length])
    return found


def clock(data, pcr_pid):
    """The program clock at an input offset, in unwrapped 27 MHz ticks: interpolated between the
    PCRs either side of it, extrapolated from the nearest two outside them."""
    samples = []
    for index, packet in enumerate(packets(data)):
        if pid(packet) == pcr_pid and (value := pcr(packet)) is not None:
            if samples:
                value = samples[-1][1] + (value - samples[-1][1] % PCR_WRAP) % PCR_WRAP
            samples.append((index * SIZE + PCR_OFFSET, value))
    if len(samples) < 2:
        raise Failure("pcr", "fewer than two PCRs")

    def at(offset):
        lo, hi = 0, len(samples) - 1
        while hi - lo > 1:
            mid = (lo + hi) // 2
            lo, hi = (mid, hi) if samples[mid][0] <= offset else (lo, mid)
        (a, ta), (b, tb) = samples[lo], samples[hi]
        return ta + Fraction((offset - a) * (tb - ta), b - a)

    rate = Fraction((samples[-1][0] - samples[0][0]) * 8 * 27_000_000, samples[-1][1] - samples[0][1])
    return at, rate


def clocks(pes):
    """The offsets of `pes`'s PTS and DTS fields."""
    if pes[3] in (0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xF2, 0xF8, 0xFF):
        return []
    return [9 + 5 * k for k in range({0b10: 1, 0b11: 2}.get(pes[7] >> 6, 0))]


def masked(pes):
    out = bytearray(pes)
    for at in clocks(pes):
        out[at : at + 5] = bytes(5)
    return bytes(out)


def ffprobe_klv(path):
    """FFmpeg's KLV streams by PID: their codec, and each packet's PTS (None without)."""
    report = json.loads(subprocess.run(
        ["ffprobe", "-v", "error", "-probesize", str(Path(path).stat().st_size), "-analyzeduration", str(2**62),
         "-show_entries", "stream=index,id,codec_name:packet=stream_index,pts", "-of", "json", str(path)],
        check=True, capture_output=True, text=True,
    ).stdout)
    streams = {s["index"]: (int(s["id"], 16), s.get("codec_name")) for s in report["streams"]}
    found = {route: (codec, []) for route, codec in streams.values() if route in (ASYNC_PID, SYNC_PID)}
    for packet in report.get("packets", []):
        route, _ = streams[packet["stream_index"]]
        if route in found:
            pts = packet.get("pts")
            found[route][1].append(None if pts is None else int(pts) % PTS_WRAP)
    return found


def grade(source, capture, probe=None):
    """Grade `capture` against `source`, returning a report; raises Failure on the first miss.

    `probe`, when given, returns FFmpeg's reading of the capture, as `ffprobe_klv` does.
    """
    src_prog, cap_prog = program(source), program(capture)
    report = {}
    for kind, route, info in STREAMS:
        if src_prog.streams.get(route) != (kind, info):
            raise Failure("source", f"PID {route:#x} is not the injected stream")
        if cap_prog.streams.get(route) != (kind, info):
            raise Failure("pmt", f"PID {route:#x}: {cap_prog.streams.get(route)} in the capture, not {(kind, info)}")
    routes = [route for _, route, _ in STREAMS]
    src_pes, cap_pes = private_pes(source, routes), private_pes(capture, routes)
    src_at, _ = clock(source, src_prog.pcr_pid)
    cap_at, rate = clock(capture, cap_prog.pcr_pid)
    # One packet at the capture's rate, in 27 MHz ticks: how finely a placement can be kept.
    packet = SIZE * 8 * 27_000_000 / rate

    matched = {}
    for route in routes:
        index = {masked(pes.data): k for k, pes in enumerate(src_pes[route])}
        found = []
        for pes in cap_pes[route]:
            k = index.get(masked(pes.data))
            if k is None:
                raise Failure("bytes", f"PID {route:#x}: a PES at byte {pes.at} matches none of the source's outside its PTS and DTS")
            found.append(k)
        if not found:
            raise Failure("missing", f"PID {route:#x}: no PES in the capture")
        first, last = found[0], len(src_pes[route]) - 1
        if found != list(range(first, last + 1)):
            gaps = sorted(set(range(first, last + 1)) - set(found))
            raise Failure("missing", f"PID {route:#x}: not the source's PES {first}..{last} in order: missing {gaps[:5]}, got {len(found)}")
        late = Fraction(src_at(src_pes[route][first].at) - src_at(src_pes[route][0].at), 27_000_000)
        if late > holes.JOIN:
            raise Failure("join", f"PID {route:#x}: starts {float(late):.3f}s after the source's first PES, past the {float(holes.JOIN)}s join")
        matched[route] = [(pes, src_pes[route][k]) for pes, k in zip(cap_pes[route], found)]

    # Every PTS and DTS moves by the one shift the importer's timebase gives the media.
    shift = None
    for route, pairs in matched.items():
        for k, (pes, src) in enumerate(pairs):
            for at in clocks(pes.data):
                moved = (clock_value(pes.data[at : at + 5]) - clock_value(src.data[at : at + 5])) % PTS_WRAP
                shift = moved if shift is None else shift
                if moved != shift:
                    raise Failure("shift", f"PID {route:#x}: capture PES {k} moved {moved} ticks, the rest {shift}")
    if shift is None:
        raise Failure("shift", "no PES carries a PTS")

    for route, pairs in matched.items():
        lateness = []
        for k, (pes, src) in enumerate(pairs):
            # Modulo the wrap, as each clock is unwrapped from its own first PCR.
            ahead = cap_at(pes.at) - (src_at(src.at) + shift * 300)
            ahead = (ahead + PCR_WRAP // 2) % PCR_WRAP - PCR_WRAP // 2
            if ahead < -packet:
                raise Failure("early", f"PID {route:#x}: capture PES {k} goes out {float(-ahead) / 27_000:.3f} ms before its placement")
            lateness.append(ahead)
            if clocks(pes.data):
                due = clock_value(pes.data[9:14]) * 300 - cap_at(pes.at)
                if (due + PCR_WRAP // 2) % PCR_WRAP - PCR_WRAP // 2 < 0:
                    raise Failure("late", f"PID {route:#x}: capture PES {k} arrives after its PTS")
        report[hex(route)] = {
            "pes": len(pairs),
            "source": len(src_pes[route]),
            "first": len(src_pes[route]) - len(pairs),
            "late_ms_max": float(max(lateness)) / 27_000,
        }

    if probe is not None:
        found = probe()
        for kind, route, _ in STREAMS:
            codec, pts = found.get(route, (None, []))
            if codec != "klv":
                raise Failure("ffmpeg", f"PID {route:#x}: FFmpeg reads {codec}, not SMPTE KLV")
            ours = [clock_value(pes.data[9:14]) if clocks(pes.data) else None for pes in cap_pes[route]]
            if pts != ours:
                raise Failure("ffmpeg", f"PID {route:#x}: FFmpeg reads {len(pts)} PES, the grader {len(ours)}, or their PTS differ")
    report["shift"] = shift
    return report


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
        print("injected KLV PES:", report)
        return 0

    print("### KLV (capture against the source)")
    try:
        report = grade(args.source.read_bytes(), args.capture.read_bytes(), lambda: ffprobe_klv(args.capture))
    except Failure as err:
        print(f"  FAIL  {err}")
        return 1
    if args.report_json:
        args.report_json.write_text(json.dumps(report, indent=2) + "\n")
    for route, row in report.items():
        if route != "shift":
            print(f"  PASS  PID {route}: {row['pes']} PES of {row['source']} from #{row['first']}, "
                  f"at most {row['late_ms_max']:.3f} ms after placement")
    print(f"  PTS and DTS moved {report['shift']} ticks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
