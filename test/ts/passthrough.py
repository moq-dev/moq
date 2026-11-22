#!/usr/bin/env python3
"""Grade an `export ts --passthrough` capture against the source `import ts --passthrough` read.

    ./passthrough.py source.ts capture.ts [--loss-ok] [--report-json report.json]
    ./passthrough.py source.ts capture.ts --restart first.ts second.ts

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
nothing about whether the rest decodes. The capture must still end on the source's last packet:
a stall, or a latency grown past the exporter's delay, loses the tail.

`--restart` grades an import killed and restarted under a running export, given what each
import read. The capture may miss one run of the source, no longer in seconds of the clip than
the import's downtime plus the restarted import's first group: from the killed import's last
PCR, where the object it held began, to the restarted import's first packet, plus the span
from its first group's random access point to its second's. The importer opens its first group
at the first random access point once it has seen the PMT and a PCR. The run is the only loss:
the capture still ends on the source's last packet.

Exit status is 0 when the capture passes, 1 otherwise.
"""

import argparse
import bisect
import json
import sys
import tempfile
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

PACKET = 188
# Matching packets a position needs before it is claimed.
RUN = 16
PCR_HZ = 27_000_000
# The stream types the importer groups on: video, as `passthrough.rs` lists them.
VIDEO_STREAM_TYPES = {0x01, 0x02, 0x10, 0x1B, 0x1F, 0x20, 0x24, 0x33, 0x42, 0xD1, 0xEA}


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


def adaptation(pkt: bytes) -> bytes:
    """The packet's adaptation field after its length byte, empty when it has none."""
    if not pkt[3] & 0x20 or not pkt[4]:
        return b""
    return pkt[5 : 5 + pkt[4]]


def pcr(pkt: bytes) -> int | None:
    """The packet's PCR in 27 MHz ticks, or None."""
    field = adaptation(pkt)
    if len(field) < 7 or not field[0] & 0x10:
        return None
    base = int.from_bytes(field[1:5], "big") << 1 | field[5] >> 7
    return base * 300 + ((field[5] & 1) << 8 | field[6])


def section(pkt: bytes) -> bytes | None:
    """The PSI section a packet starts, from its table_id on, or None."""
    if not pkt[1] & 0x40 or not pkt[3] & 0x10:
        return None
    start = 4 + (1 + pkt[4] if pkt[3] & 0x20 else 0)
    body = pkt[start:]
    return body[1 + body[0] :] if body else None


@dataclass
class Layout:
    """What the importer reads of a single-program source: where its tables and clock are."""

    pmt_pid: int
    pcr_pid: int
    group_pid: int  # random access on this PID starts a group: the video, else the clock


def layout(source: list[bytes]) -> Layout:
    """The program's PMT, clock and grouping PIDs, from the source's first PAT and PMT."""
    pmt_pid = None
    for pkt in source:
        sec = section(pkt)
        if sec is None:
            continue
        if pmt_pid is None and pid(pkt) == 0 and sec[0] == 0x00:
            end = 3 + ((sec[1] & 0x0F) << 8 | sec[2]) - 4
            programs = [(sec[i + 2] & 0x1F) << 8 | sec[i + 3] for i in range(8, end, 4) if sec[i] << 8 | sec[i + 1]]
            if len(programs) != 1:
                raise Mismatch(f"the PAT lists {len(programs)} programs; --restart grades one")
            pmt_pid = programs[0]
        elif pmt_pid is not None and pid(pkt) == pmt_pid and sec[0] == 0x02:
            end = 3 + ((sec[1] & 0x0F) << 8 | sec[2]) - 4
            pcr_pid = (sec[8] & 0x1F) << 8 | sec[9]
            i = 12 + ((sec[10] & 0x0F) << 8 | sec[11])
            video = []
            while i + 5 <= end:
                if sec[i] in VIDEO_STREAM_TYPES:
                    video.append((sec[i + 1] & 0x1F) << 8 | sec[i + 2])
                i += 5 + ((sec[i + 3] & 0x0F) << 8 | sec[i + 4])
            if len(video) > 1:
                raise Mismatch("the PMT lists several video streams; --restart grades one")
            return Layout(pmt_pid, pcr_pid, video[0] if video else pcr_pid)
    raise Mismatch("the source has no PAT and PMT")


class Clock:
    """Each source position's time in seconds, interpolated between its PCRs."""

    def __init__(self, source: list[bytes], pcr_pid: int):
        pcrs = [(i, value) for i, pkt in enumerate(source) if pid(pkt) == pcr_pid and (value := pcr(pkt)) is not None]
        if len(pcrs) < 2:
            raise Mismatch("the source carries fewer than two PCRs")
        self.positions = [i for i, _ in pcrs]
        self.values = [value for _, value in pcrs]

    def at(self, position: int) -> float:
        k = max(0, min(bisect.bisect_right(self.positions, position) - 1, len(self.positions) - 2))
        (i0, i1), (v0, v1) = self.positions[k : k + 2], self.values[k : k + 2]
        return (v0 + (v1 - v0) * (position - i0) / (i1 - i0)) / PCR_HZ

    def last_before(self, position: int) -> int:
        """The last PCR position before `position`."""
        k = bisect.bisect_left(self.positions, position)
        if not k:
            raise Mismatch(f"no PCR before source packet {position}")
        return self.positions[k - 1]


@dataclass
class Restart:
    """What an import killed and restarted may cost a capture, in source positions."""

    held: int  # the killed import's last PCR: the object it held began here
    resumed: int  # the restarted import's first packet
    first_group: int  # its first group's random access point
    second_group: int  # the next group's
    bound: float  # seconds of the clip the capture may miss


def restart(source: list[bytes], first: Placement, second: Placement) -> Restart:
    """The bound on what the capture may miss between `first` import's input and `second`'s."""
    if first.gaps or second.gaps:
        raise Mismatch("an import's input skips source packets, so its downtime is not one run")
    lay = layout(source)
    clock = Clock(source, lay.pcr_pid)
    stopped = first.total - first.tail
    if second.start < stopped:
        raise Mismatch(f"the restarted import read from packet {second.start}, before the first stopped at {stopped}")
    held = clock.last_before(stopped)
    seen_pmt = seen_pcr = False
    groups = []
    for i in range(second.start, len(source)):
        pkt = source[i]
        seen_pmt |= pid(pkt) == lay.pmt_pid and section(pkt) is not None
        seen_pcr |= pid(pkt) == lay.pcr_pid and pcr(pkt) is not None
        field = adaptation(pkt)
        if pid(pkt) == lay.group_pid and field and field[0] & 0x40 and (groups or (seen_pmt and seen_pcr)):
            groups.append(i)
            if len(groups) == 2:
                break
    if len(groups) < 2:
        raise Mismatch("the restarted import's input holds fewer than two groups")
    bound = clock.at(second.start) - clock.at(held) + clock.at(groups[1]) - clock.at(groups[0])
    return Restart(held, second.start, groups[0], groups[1], bound)


def restart_shortfall(placement: Placement, clock: Clock, bound: Restart) -> str | None:
    """Why the capture of a restarted import misses more than `bound` allows, or None."""
    if placement.tail:
        return f"the capture ends {placement.tail} packets before its source"
    if len(placement.gaps) > 1:
        return f"the capture misses {len(placement.gaps)} runs of its source; a restart may cost one"
    for start, length in placement.gaps:
        lost = clock.at(start + length) - clock.at(start)
        if lost > bound.bound:
            return f"the capture misses {lost:.3f} s at the restart, past the {bound.bound:.3f} s bound"
    return None


def shortfall(placement: Placement, loss_ok: bool) -> str | None:
    """Why the source packets `placement` lacks fail the capture, or None when they do not."""
    if placement.tail:
        return f"the capture ends {placement.tail} packets before its source"
    if placement.gaps and not loss_ok:
        return "the capture is missing source packets; --loss-ok reports gaps instead"
    return None


def verdicts(path: Path) -> dict[str, list[str]]:
    """Every compliance.py check's status and detail for `path`."""
    import compliance

    return {c.name: [c.status.value, c.detail] for c in compliance.analyze(str(path))}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path)
    parser.add_argument("capture", type=Path)
    parser.add_argument(
        "--loss-ok", action="store_true", help="report gaps instead of failing on them, but not a lost tail"
    )
    parser.add_argument(
        "--restart",
        nargs=2,
        type=Path,
        metavar=("FIRST", "SECOND"),
        help="what the killed import and its restart read: the capture may miss one bounded run",
    )
    parser.add_argument("--report-json", type=Path)
    args = parser.parse_args()

    source_data = args.source.read_bytes()
    capture_data = args.capture.read_bytes()
    try:
        source = packets(source_data)
        placement = place(source, packets(capture_data))
        if args.restart:
            first, second = (place(source, packets(path.read_bytes())) for path in args.restart)
            bound = restart(source, first, second)
            clock = Clock(source, layout(source).pcr_pid)
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
    if args.restart:
        lost = [clock.at(start + length) - clock.at(start) for start, length in placement.gaps]
        down = clock.at(bound.resumed) - clock.at(bound.held)
        group = clock.at(bound.second_group) - clock.at(bound.first_group)
        resumed = any(start + length < placement.total - placement.tail for start, length in placement.gaps)
        print("### restart")
        print(f"  down: {down:.3f} s, from the killed import's last PCR to the restarted import's first packet")
        print(f"  first group: {group:.3f} s; bound {bound.bound:.3f} s")
        print(f"  lost: {', '.join(f'{s:.3f} s' for s in lost) or 'nothing'}; resumed on its own: {resumed}")
        report["restart"] = {
            "down_seconds": down,
            "first_group_seconds": group,
            "bound_seconds": bound.bound,
            "lost_seconds": lost,
            "resumed": resumed,
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
                print(
                    f"passthrough: FAIL: {name} grades the capture {actual.get(name)}, its source {expected.get(name)}",
                    file=sys.stderr,
                )
                code = 1
    else:
        report["compliance"] = {"capture": verdicts(args.capture)}
        print("### compliance.py on the capture (report only: it has gaps)")
        for name, (status, detail) in report["compliance"]["capture"].items():
            print(f"  {name}: {status}  {detail}")
        reason = restart_shortfall(placement, clock, bound) if args.restart else shortfall(placement, args.loss_ok)
        if reason:
            print(f"passthrough: FAIL: {reason}", file=sys.stderr)
            code = 1

    if args.report_json:
        args.report_json.write_text(json.dumps(report, indent=2))
    if code == 0:
        print("passthrough: PASS" + (" (with gaps)" if lost else ""))
    return code


if __name__ == "__main__":
    sys.exit(main())
