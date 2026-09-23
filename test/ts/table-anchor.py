#!/usr/bin/env python3
"""Grade whether a table's emission points belong to the broadcast or to the process.

    ./table-anchor.py a.ts b.ts

`export ts` writes a frame by emitting any due tables and then that frame's PES
packets into one buffer, so the first PES header after a table belongs to the
frame that triggered it. Tagging each table emission with that frame's PTS says
what two exporters of one broadcast cannot otherwise be asked: whether they put
a table in the same place in *media* time, or merely at the same rate.

That distinction is the whole property. A cadence driven by the exporter's own
clock still produces a correct stream on each leg and still emits at the right
average interval; it just emits at different points, so the legs are two
transports of one programme rather than one transport sent twice. Anything that
merges or switches between them - an ST 2022-7 receiver, a standby packager, a
late joiner expected to converge on a running peer - needs the second.

Why not compare bytes. Two legs order the same frames differently, because the
exporter emits whichever track has a frame in hand and arrival order is a
property of the network path rather than of the broadcast. A packet-by-packet
walk desynchronises on that regardless of where the tables land, so it cannot
answer this question even in principle. Indexing by PTS is immune to it, which
isolates the cadence from the interleave.

Comparison is restricted to the media-time window the two legs share, so a late
joiner is graded on the overlap rather than penalised for the start it missed.

Exit status is 0 when every hard check passes, 1 otherwise. `--strict` promotes
the report-only (shape) checks to hard.
"""

import argparse
import json
import sys
from collections import defaultdict

PKT = 188
SYNC = 0x47
HARD, SHAPE = "hard", "shape"
NULL_PID = 0x1FFF

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
    """188-byte packets from a TS file, resynchronising on the first stable sync run."""
    with open(path, "rb") as f:
        data = f.read()
    start = 0
    while start + PKT * 8 < len(data):
        if all(data[start + i * PKT] == SYNC for i in range(8)):
            break
        start += 1
    end = len(data) - (len(data) - start) % PKT
    return (data[i : i + PKT] for i in range(start, end, PKT))


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
    return (
        ((b[0] >> 1) & 0x07) << 30
        | b[1] << 22
        | ((b[2] >> 1) & 0x7F) << 15
        | b[3] << 7
        | ((b[4] >> 1) & 0x7F)
    )


def pmt_pids(path):
    """PIDs the PAT maps a programme to, so a PMT is found rather than guessed.

    Sections are reassembled across packets. A PAT carrying more than about forty
    programmes does not fit in one packet, and stopping at the packet boundary would
    silently find only the programmes that happened to land in the first one — which
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


def anchors(path, pmts):
    """Frame PTS values each table was emitted at, and the capture's media bounds.

    The bounds are returned alongside because the scoring window has to come from
    the media the capture covers, not from the emissions themselves — see
    `agreement()`.
    """
    out = defaultdict(list)
    pending = []
    lo = hi = None
    watched = set(WELL_KNOWN) | pmts
    for pkt in packets(path):
        pid = pid_of(pkt)
        if pid == NULL_PID:
            continue
        if pid in watched:
            if pkt[1] & 0x40:
                pending.append(pid)
            continue
        pts = parse_pts(pkt)
        if pts is None:
            continue
        lo = pts if lo is None or pts < lo else lo
        hi = pts if hi is None or pts > hi else hi
        if pending:
            for table in pending:
                out[table].append(pts)
            pending = []
    return out, (lo, hi)


def name(pid, pmts):
    if pid in WELL_KNOWN:
        return WELL_KNOWN[pid]
    return f"PMT {pid:#06x}" if pid in pmts else f"PID {pid:#06x}"


def agreement(a_pts, b_pts, window):
    """Share of emission points the legs share, over the media time they both cover.

    The window is the media both captures carry, and it must come from the captures
    rather than from the emissions being scored. Deriving it from the emissions
    instead is a false pass: a leg that stops emitting a table halfway through pulls
    the upper bound back to its own last emission, so the partner's later emissions
    fall outside the window and the desertion scores 100 %.

    Returns None when the captures do not overlap in media time at all, which is a
    different answer from "they overlap and disagree" and must not be scored as 0.
    """
    lo, hi = window
    if lo is None or hi is None or lo > hi:
        return None
    oa = {t for t in a_pts if lo <= t <= hi}
    ob = {t for t in b_pts if lo <= t <= hi}
    union = oa | ob
    if not union:
        return None
    shared = oa & ob
    return len(oa), len(ob), len(union), len(shared), 100.0 * len(shared) / len(union)


def cadence(pts):
    """The period a leg emits on, in seconds, or None if it keeps no steady one.

    "Steady" has to tolerate a few odd gaps rather than demand none: a leg that
    re-phases once on joining, or drops an emission, is still running a timer, and a
    spread test over the extremes would miss exactly the case worth catching. So the
    period is the median gap and the leg counts as steady when most gaps sit on it.
    """
    pts = sorted(pts)
    if len(pts) < 4:
        return None
    gaps = sorted((pts[i + 1] - pts[i]) / 90000.0 for i in range(len(pts) - 1))
    period = gaps[len(gaps) // 2]
    if period <= 0:
        return None
    on_period = sum(1 for g in gaps if abs(g - period) <= 0.05 * period)
    return period if on_period >= 0.6 * len(gaps) else None


def phase(pts, period):
    """Where a leg's grid sits inside one period, as a robust median.

    Taken relative to the first point so that a grid straddling the modulus does not
    split into two clusters and median to a value in neither.
    """
    base = min(pts) / 90000.0
    offs = sorted(((t / 90000.0 - base) % period) for t in pts)
    return offs[len(offs) // 2] + base


def diagnose(a_pts, b_pts):
    """Say why two legs disagree, when the shape of the disagreement is legible.

    The case worth naming is a table both legs emit on the same steady period but at
    different phase: that is a timer started when the exporter started, and no amount
    of running time will bring the two legs back together.
    """
    pa, pb = cadence(a_pts), cadence(b_pts)
    if pa is None or pb is None or abs(pa - pb) > 0.05 * max(pa, pb):
        return None
    offset = abs(phase(a_pts, pa) - phase(b_pts, pa)) % pa
    offset = min(offset, pa - offset)
    if offset <= 0.01 * pa:
        return None
    return (
        f"both legs emit every {pa:.3f}s, {offset:.3f}s out of phase "
        f"-> a timer started with the exporter, not an anchor in the media"
    )


def main():
    ap = argparse.ArgumentParser(description="Grade table emission points across two exporters.")
    ap.add_argument("a", help="TS captured from the first exporter")
    ap.add_argument("b", help="TS captured from the second exporter")
    ap.add_argument(
        "--min-agreement",
        type=float,
        default=90.0,
        help="percent of shared emission points required per table (default 90)",
    )
    ap.add_argument(
        "--min-emissions",
        type=int,
        default=8,
        help="a table with fewer emissions in the overlap is reported, not graded (default 8)",
    )
    ap.add_argument(
        "--min-window",
        type=float,
        default=20.0,
        help="seconds of shared media required before any verdict is given (default 20)",
    )
    ap.add_argument("--strict", action="store_true", help="fail on shape checks too")
    ap.add_argument("--report-json", help="write the full report here")
    args = ap.parse_args()

    pmts = pmt_pids(args.a) | pmt_pids(args.b)
    a, (a_lo, a_hi) = anchors(args.a, pmts)
    b, (b_lo, b_hi) = anchors(args.b, pmts)
    if not a and not b:
        print("error: neither capture carries a table on a known PID", file=sys.stderr)
        return 1
    if None in (a_lo, a_hi, b_lo, b_hi):
        print("error: a capture carries no PTS, so there is no media time to compare in", file=sys.stderr)
        return 1
    # The media both captures carry. Every table is scored inside this one window, so a
    # table one leg abandons is scored against the partner's emissions rather than
    # silently shrinking the window to hide them.
    window = (max(a_lo, b_lo), min(a_hi, b_hi))
    shared_s = (window[1] - window[0]) / 90000.0
    print(f"### shared media window: {shared_s:.1f}s")
    # A window too short to carry the slower tables would put them under the emission
    # floor and report them without a verdict, which reads as a pass. Refuse the run
    # instead: a capture that came up short is the commonest way this grades clean.
    if shared_s < args.min_window:
        print(
            f"error: the captures share only {shared_s:.1f}s of media, below the "
            f"{args.min_window:.0f}s needed for a verdict",
            file=sys.stderr,
        )
        print("  run for longer, join earlier, or lower --min-window deliberately", file=sys.stderr)
        return 1

    rows = []
    for pid in sorted(set(a) | set(b)):
        label = name(pid, pmts)
        scored = agreement(a.get(pid, []), b.get(pid, []), window)
        if scored is None:
            rows.append((pid, label, len(a.get(pid, [])), len(b.get(pid, [])), None, None, "one leg only"))
            continue
        na, nb, union, shared, pct = scored
        # A table emitted a handful of times cannot separate "anchored to the media"
        # from "coincidence", so it is reported without a verdict rather than graded
        # on a sample too small to mean anything.
        severity = SHAPE if pid in NOT_GRADED or union < args.min_emissions else HARD
        rows.append((pid, label, na, nb, union, shared, pct, severity))

    width = max(len(r[1]) for r in rows)
    print(f"### table anchor report - {len(rows)} table(s)")
    print()
    print(f"  {'':4}  {'table':<{width}}  {'A':>7} {'B':>7} {'either':>7} {'both':>7}  agreement")
    failed = 0
    report = []
    for row in rows:
        if len(row) == 7:
            pid, label, na, nb, _, _, note = row
            print(f"  {'--':4}  {label:<{width}}  {na:>7,} {nb:>7,} {'-':>7} {'-':>7}  {note}")
            report.append({"pid": pid, "table": label, "a": na, "b": nb, "graded": False, "note": note})
            continue
        pid, label, na, nb, union, shared, pct, severity = row
        ok = pct >= args.min_agreement
        if ok:
            verdict = "PASS"
        elif severity == HARD or args.strict:
            verdict = "FAIL"
            failed += 1
        else:
            verdict = "WARN"
        print(
            f"  {verdict:4}  {label:<{width}}  {na:>7,} {nb:>7,} {union:>7,} {shared:>7,}  {pct:8.2f}%"
        )
        why = None if ok else diagnose(a.get(pid, []), b.get(pid, []))
        if why:
            print(f"  {'':4}  {'':<{width}}  {why}")
        report.append(
            {
                "pid": pid,
                "table": label,
                "a": na,
                "b": nb,
                "either": union,
                "both": shared,
                "agreement_pct": round(pct, 4),
                "severity": severity,
                "graded": True,
                "ok": ok,
                "diagnosis": why,
            }
        )

    print()
    print(
        "agreement = emission points both legs used, over those either used, inside the\n"
        "media time they share. 100% means the emission points are a function of the\n"
        "broadcast; a low figure with a healthy count on both legs means each leg is\n"
        "emitting to its own clock."
    )
    print()
    print(f"{failed} check(s) failed" if failed else "all checks passed")

    if args.report_json:
        with open(args.report_json, "w") as f:
            json.dump({"min_agreement": args.min_agreement, "tables": report}, f, indent=2)

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
