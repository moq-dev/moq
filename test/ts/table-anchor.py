#!/usr/bin/env python3
"""Compare table placement on the PCR timeline after a complete acquisition group.

Tables and PES are interleaved by the slot scheduler. The next PES PTS is not the
emission time: interpolate packet position between PCRs instead. Start at the
second video random-access PES on each leg, when its first acquired group is out,
as the exporter's late-join convergence tests do. The end comes from the clock,
never from the tables, so disappearing tables cannot shorten their own test.

This checks table placement, not packet identity or hitless switching. It supports
one continuous program clock and video carrying random_access_indicator; refuse
captures that cannot establish those conditions rather than guess a grace period.
"""

import argparse
import json
import sys
from bisect import bisect_right
from collections import defaultdict
from dataclasses import dataclass
from importlib import import_module

parse_pcr = import_module("pcr-timing").parse_pcr
CLOCK = 27_000_000
WRAP = (1 << 33) * 300

PKT = 188
SYNC = 0x47

# PIDs whose table is fixed by the standard. A PMT sits on a PID the PAT chooses,
# so it is discovered rather than listed.
WELL_KNOWN = {
    0x0000: "PAT",
    0x0001: "CAT",
    0x0010: "NIT",
    0x0011: "SDT/BAT",
    0x0012: "EIT",
    0x0014: "TDT/TOT",
}

# TDT/TOT carries wall-clock time rather than a cadence anchored to the media, so a
# disagreement there is not the defect this grades. Reported, never graded.
NOT_GRADED = {0x0014}


def packets(path):
    """Read the exporter's complete 188-byte packets, refusing broken framing."""
    with open(path, "rb") as f:
        data = f.read()
    if not data or len(data) % PKT:
        raise ValueError(f"{path}: empty or truncated transport")
    for at in range(0, len(data), PKT):
        pkt = data[at : at + PKT]
        if pkt[0] != SYNC or pkt[1] & 0x80 or not pkt[3] & 0x30:
            raise ValueError(f"{path}: malformed packet at {at}")
        yield pkt


def pid_of(pkt):
    return ((pkt[1] & 0x1F) << 8) | pkt[2]


def payload_offset(pkt):
    """First payload byte, or None when the packet carries no payload."""
    afc = (pkt[3] >> 4) & 0x3
    if afc == 0 or afc == 2:
        return None
    off = 4
    if afc == 3:
        off += 1 + pkt[4]
    return off if off < PKT else None


def parse_pts(pkt):
    """PTS of a PES packet starting in this TS packet, or None."""
    if not pkt[1] & 0x40:  # payload_unit_start_indicator
        return None
    off = payload_offset(pkt)
    if off is None or off + 14 > PKT:
        return None
    p = pkt[off:]
    if p[0:3] != b"\x00\x00\x01":
        return None
    if len(p) < 14 or not (p[7] & 0x80):  # PTS_DTS_flags
        return None
    b = p[9:14]
    return ((b[0] >> 1) & 0x07) << 30 | b[1] << 22 | ((b[2] >> 1) & 0x7F) << 15 | b[3] << 7 | ((b[4] >> 1) & 0x7F)


def pmt_pids(path):
    """PIDs the PAT maps a programme to, so a PMT is found rather than guessed.

    Sections are reassembled across packets. A PAT carrying more than about forty
    programmes does not fit in one packet, and stopping at the packet boundary would
    silently find only the programmes that happened to land in the first one; which
    on a full multiplex is a subset, with no error to say so.
    """
    found = set()
    section, want = bytearray(), 0
    for pkt in packets(path):
        if pid_of(pkt) != 0x0000:
            continue
        off = payload_offset(pkt)
        if off is None or off >= PKT:
            continue
        if pkt[1] & 0x40:  # payload_unit_start: a section begins here
            off += 1 + pkt[off]  # pointer_field
            if off >= PKT or pkt[off] != 0x00:  # table_id 0x00 = PAT
                section, want = bytearray(), 0
                continue
            section = bytearray(pkt[off:])
            want = 0
        elif section:
            section += pkt[off:]
        else:
            continue
        if not want:
            if len(section) < 3:
                continue
            want = 3 + (((section[1] & 0x0F) << 8) | section[2])
        if len(section) < want:
            continue
        body, end = 8, want - 4  # past the section header, stopping before the CRC
        while body + 4 <= end:
            program = (section[body] << 8) | section[body + 1]
            pid = ((section[body + 2] & 0x1F) << 8) | section[body + 3]
            if program != 0:  # programme 0 is the NIT, not a PMT
                found.add(pid)
            body += 4
        section, want = bytearray(), 0
    return found


@dataclass
class Capture:
    tables: dict
    start: float
    end: float
    positions: list[int]
    clocks: list[int]

    def rate(self, window):
        """Packet duration in the shared window, refusing non-CBR interpolation."""
        lo = max(0, bisect_right(self.clocks, window[0]) - 1)
        hi = bisect_right(self.clocks, window[1])
        clocks, positions = self.clocks[lo:hi], self.positions[lo:hi]
        if len(clocks) < 2:
            raise ValueError("insufficient shared PCR samples")
        ticks = (clocks[-1] - clocks[0]) / (positions[-1] - positions[0])
        # Two integer PCR samples bound the rounding error in the fitted slope.
        if any(abs(t - clocks[0] - (i - positions[0]) * ticks) > 2 for i, t in zip(positions, clocks)):
            raise ValueError("pair grading requires a constant-rate shared PCR/packet timeline")
        return ticks


def anchors(path, pmts):
    """Table positions and the independently observed, acquired clock window."""
    clocks, positions = [], []
    tables, access = defaultdict(list), defaultdict(list)
    clock_pid = None
    watched = set(WELL_KNOWN) | pmts
    for index, pkt in enumerate(packets(path)):
        pid = pid_of(pkt)
        pcr = parse_pcr(pkt)
        if pcr is not None:
            if clock_pid is not None and pid != clock_pid:
                raise ValueError("pair grading requires a single program clock")
            clock_pid = pid
            if clocks:
                if pkt[5] & 0x80:
                    raise ValueError("PCR discontinuity inside pair capture")
                delta = (pcr - clocks[-1]) % WRAP
                if not 0 < delta < WRAP // 2:
                    raise ValueError("non-increasing program clock")
                pcr = clocks[-1] + delta
            clocks.append(pcr)
            positions.append(index)
        if pid in watched and pkt[1] & 0x40:
            tables[pid].append(index)
        off = payload_offset(pkt)
        if parse_pts(pkt) is not None and 0xE0 <= pkt[off + 3] <= 0xEF:
            # Record the PID even when every random-access flag is missing.
            points = access[pid]
            if pkt[3] & 0x20 and pkt[4] and pkt[5] & 0x40:
                points.append(index)
    if len(clocks) < 2 or not access or any(len(v) < 2 for v in access.values()):
        raise ValueError("need PCR and two video random-access points per video PID")

    def at(index):
        i = bisect_right(positions, index) - 1
        if i < 0 or i >= len(clocks) - 1:
            raise ValueError("acquisition point is outside the measured PCR window")
        return clocks[i] + (index - positions[i]) * (clocks[i + 1] - clocks[i]) / (positions[i + 1] - positions[i])

    start = max(at(v[1]) for v in access.values())
    return Capture(
        {pid: [at(i) for i in points if positions[0] <= i < positions[-1]] for pid, points in tables.items()},
        start,
        clocks[-1],
        positions,
        clocks,
    )


def name(pid, pmts):
    return WELL_KNOWN.get(pid, f"PMT {pid:#06x}" if pid in pmts else f"PID {pid:#06x}")


def agreement(a, b, window, tolerance):
    """One-to-one matches within one packet, retaining losses and duplicates."""
    lo, hi = window
    a = [t for t in a if lo <= t <= hi]
    b = [t for t in b if lo <= t <= hi]
    i = j = shared = 0
    while i < len(a) and j < len(b):
        if abs(a[i] - b[j]) <= tolerance:
            shared += 1
            i += 1
            j += 1
        elif a[i] < b[j]:
            i += 1
        else:
            j += 1
    union = len(a) + len(b) - shared
    return len(a), len(b), union, shared, 100 * shared / union if union else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--min-agreement", type=float, default=90.0)
    ap.add_argument("--min-emissions", type=int, default=8)
    ap.add_argument("--min-window", type=float, default=20.0)
    ap.add_argument("--source", help="require the source's non-clock SI tables on both legs")
    ap.add_argument("--strict", action="store_true", help="also grade clock tables")
    ap.add_argument("--report-json")
    args = ap.parse_args()
    try:
        pmts = pmt_pids(args.a) | pmt_pids(args.b)
        if args.source:
            pmts |= pmt_pids(args.source)
        if not pmts:
            raise ValueError("no PAT program mapping")
        expected = {pid_of(pkt) for pkt in packets(args.source) if pid_of(pkt) in WELL_KNOWN} if args.source else set()
        a, b = anchors(args.a, pmts), anchors(args.b, pmts)
        # Captures may start on opposite sides of the 33-bit wrap.
        shift = round((a.start - b.start) / WRAP) * WRAP
        b.start += shift
        b.end += shift
        b.clocks = [t + shift for t in b.clocks]
        b.tables = {pid: [t + shift for t in points] for pid, points in b.tables.items()}
        window = max(a.start, b.start), min(a.end, b.end)
        shared_s = (window[1] - window[0]) / CLOCK
        if shared_s < args.min_window:
            raise ValueError(f"only {shared_s:.3f}s of acquired shared clock; need {args.min_window}s")
        tolerance = max(a.rate(window), b.rate(window)) + 1
        rows = []
        # PAT and its declared PMTs are mandatory, even when absent after acquisition.
        for pid in sorted({0} | pmts | expected | set(a.tables) | set(b.tables)):
            na, nb, union, shared, pct = agreement(a.tables.get(pid, []), b.tables.get(pid, []), window, tolerance)
            graded = pid not in NOT_GRADED or args.strict
            enough = min(na, nb) >= args.min_emissions
            ok = enough and pct >= args.min_agreement
            rows.append(
                dict(
                    pid=pid,
                    table=name(pid, pmts),
                    a=na,
                    b=nb,
                    either=union,
                    both=shared,
                    agreement_pct=round(pct, 4),
                    graded=graded,
                    ok=ok,
                    note=None if enough else "insufficient emissions",
                )
            )
        report = dict(
            window_seconds=[t / CLOCK for t in window],
            shared_seconds=shared_s,
            tolerance_ticks=tolerance,
            min_agreement=args.min_agreement,
            tables=rows,
        )
        print(f"### acquired shared clock: {shared_s:.3f}s")
        for row in rows:
            verdict = "PASS" if row["ok"] else "FAIL" if row["graded"] else "INFO"
            print(
                f"{verdict}: {row['table']}: {row['both']}/{row['either']} ({row['agreement_pct']:.2f}%)"
                + (f"; {row['note']}" if row["note"] else "")
            )
        failed = any(row["graded"] and not row["ok"] for row in rows)
    except ValueError as error:
        report = dict(error=str(error))
        print(f"error: {error}", file=sys.stderr)
        failed = True
    if args.report_json:
        with open(args.report_json, "w") as f:
            json.dump(report, f, indent=2)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
