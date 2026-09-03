#!/usr/bin/env python3
"""Grade an MPEG-TS stream's PCR against its own asserted clock.

    moq ... export ts | ./pcr-timing.py --live --seconds 45
    ./pcr-timing.py capture.ts

A PCR is three claims at once, and a fix in one of them is invisible to an
instrument pointed at another:

    value       the intervals between PCR values         (ISO 13818-1 / TR 101 290)
    release     when the bytes carrying them were handed over  (--live only)
    position    where the PCR packets sit among the media bytes

Every check is an invariant on the stream, not an assertion about how the stream
was produced. `release` grades arrival against the PCR's *own* values rather than
against a wall clock, so it needs no reference clock, no source file and no
declared mux rate; the price is that a clock running at the wrong rate stays
internally consistent under it and has to be caught by `value` and `position`.

Exit status is 0 when every hard check passes, 1 otherwise. `--strict` promotes
the report-only (shape) checks to hard.
"""

import argparse
import collections
import json
import select
import statistics
import sys
import time

PKT = 188
SYNC = 0x47
HARD, SHAPE = "hard", "shape"
TICKS_PER_MS = 27_000.0
# The 33-bit PCR base counts 90 kHz ticks and the 9-bit extension counts 27 MHz ticks
# within one of them, so the whole field restarts after this many 27 MHz ticks, roughly
# every 26.5 hours.
PCR_MODULUS = (1 << 33) * 300


def percentile(xs, p):
    s = sorted(xs)
    if not s:
        return float("nan")
    k = min(len(s) - 1, max(0, int(round(p / 100.0 * (len(s) - 1)))))
    return s[k]


def parse_pcr(p):
    """The 33+9-bit PCR of an adaptation field, in 27 MHz ticks, or None."""
    if (p[3] >> 4) & 0x3 not in (2, 3):  # adaptation_field_control
        return None
    # A PCR occupies six bytes after the flags byte, so an adaptation field shorter than
    # seven cannot hold one however the flag is set. Without this a stream that sets
    # PCR_flag on a short field yields a value read out of stuffing or payload, and that
    # sample then feeds all three timing checks.
    if p[4] < 7 or not (p[5] & 0x10):  # adaptation_field_length, PCR_flag
        return None
    base = (p[6] << 25) | (p[7] << 17) | (p[8] << 9) | (p[9] << 1) | (p[10] >> 7)
    ext = ((p[10] & 0x01) << 8) | p[11]
    return base * 300 + ext


class Scan:
    """One pass over the packets, collecting only what the checks need."""

    def __init__(self):
        self.pcr = []  # (packet_index, ticks, arrival_or_None, pid)
        self.packets = 0
        self.bad_sync = 0
        self.transport_error = 0
        self.cc_errors = []
        self.cc_on_empty = []
        self.wraps = 0
        self._cc = {}
        self._dup = {}
        self._pcr_raw = {}
        self._pcr_offset = {}

    def feed(self, p, arrival=None):
        index = self.packets
        self.packets += 1
        if p[0] != SYNC:
            self.bad_sync += 1
            return
        if p[1] & 0x80:
            self.transport_error += 1
        pid = ((p[1] & 0x1F) << 8) | p[2]

        ticks = parse_pcr(p)
        if ticks is not None:
            # Unwrap, so the checks see a monotone timeline. A capture crossing the
            # rollover would otherwise show one huge negative interval, which the value
            # check accepts (it only rejects intervals that are too long) while the
            # release check reads it as a day of lateness. A rollover drops the value by
            # nearly the whole modulus, so only a drop past halfway is treated as one and
            # a merely backwards PCR stays visible as the defect it is.
            prev_raw = self._pcr_raw.get(pid)
            if prev_raw is not None and ticks < prev_raw - PCR_MODULUS // 2:
                self._pcr_offset[pid] = self._pcr_offset.get(pid, 0) + PCR_MODULUS
                self.wraps += 1
            self._pcr_raw[pid] = ticks
            self.pcr.append((index, ticks + self._pcr_offset.get(pid, 0), arrival, pid))

        # ISO 13818-1 2.4.3.3: the continuity counter advances only on a packet
        # carrying a payload, so an adaptation-only PCR packet must repeat the
        # previous value rather than increment it.
        cc = p[3] & 0x0F
        payload = bool((p[3] >> 4) & 0x1)
        # The same clause allows the counter to jump when the adaptation field sets
        # discontinuity_indicator, and allows one packet to be sent twice, repeating its
        # counter. Both are legal, and treating either as an error fails a conforming
        # stream on a hard check.
        adaptation = bool((p[3] >> 4) & 0x2)
        discontinuity = adaptation and p[4] > 0 and bool(p[5] & 0x80)
        prev = self._cc.get(pid)
        self._cc[pid] = cc
        if prev is None or pid == 0x1FFF:
            return
        if discontinuity:
            self._dup[pid] = False
            return
        if payload:
            if cc == prev:
                # Legal only once: a second repeat is a stuck counter, not a duplicate.
                if self._dup.get(pid):
                    self.cc_errors.append((index, pid, (prev + 1) & 0x0F, cc))
                self._dup[pid] = not self._dup.get(pid)
            elif cc == (prev + 1) & 0x0F:
                self._dup[pid] = False
            else:
                self.cc_errors.append((index, pid, (prev + 1) & 0x0F, cc))
                self._dup[pid] = False
        elif cc != prev:
            self.cc_on_empty.append((index, pid, prev, cc))


def scan_file(path):
    scan = Scan()
    with open(path, "rb") as f:
        while True:
            p = f.read(PKT)
            if len(p) < PKT:
                return scan
            scan.feed(p)


def scan_live(seconds):
    """Read stdin one packet at a time, stamping each read.

    The granularity is deliberate: a coarser read cannot distinguish a run of
    packets released together from a run released on a cadence, which is the
    distinction `release` exists to measure. Each stamp is taken after its read
    returns, so it is an upper bound on when the writer released those bytes.
    """
    scan = Scan()
    fd = sys.stdin.buffer
    deadline = time.monotonic() + seconds

    def read_exactly(n):
        """Read n bytes, or return None once the capture window has expired.

        A plain read blocks until the producer sends something, so a producer that
        holds the pipe open and stops writing suspends the loop indefinitely and
        --seconds never takes effect. That is the likeliest state to be in while
        diagnosing an exporter or a network stall, which is when the tool has to
        return a report rather than hang.
        """
        buf = b""
        while len(buf) < n:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([fd], [], [], left)[0]:
                return None
            chunk = fd.read1(n - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    # Find the first sync byte, then stay aligned on it.
    while True:
        b = read_exactly(1)
        if not b:
            return scan
        if b[0] == SYNC:
            rest = read_exactly(PKT - 1)
            if not rest:
                return scan
            scan.feed(b + rest, time.monotonic())
            break

    while time.monotonic() < deadline:
        p = read_exactly(PKT)
        if not p:
            return scan
        scan.feed(p, time.monotonic())
    return scan


# --- checks: each returns (name, severity, ok, headline, detail) -------------


def check_sync(scan, args):
    detail = {
        "packets": scan.packets,
        "bad_sync": scan.bad_sync,
        "transport_error": scan.transport_error,
    }
    return (
        "sync",
        HARD,
        not (scan.bad_sync or scan.transport_error),
        f"{scan.packets} packets, {scan.bad_sync} bad sync bytes, "
        f"{scan.transport_error} with transport_error_indicator",
        detail,
    )


def check_continuity(scan, args):
    detail = {
        "discontinuities": len(scan.cc_errors),
        "empty_packets_advancing_cc": len(scan.cc_on_empty),
        "first_few": scan.cc_errors[:5],
    }
    return (
        "continuity",
        HARD,
        not (scan.cc_errors or scan.cc_on_empty),
        f"{len(scan.cc_errors)} continuity discontinuities, {len(scan.cc_on_empty)} payload-less "
        f"packets advanced the counter (ISO 13818-1 2.4.3.3)",
        detail,
    )


def check_pcr_single_pid(scan, args):
    """Every PCR must ride the one PID the PMT declares."""
    pids = collections.Counter(pid for _, _, _, pid in scan.pcr)
    return (
        "pcr-single-pid",
        HARD,
        len(pids) <= 1,
        f"PCR carried on {len(pids)} PID(s): " + ", ".join(f"{k} ({v})" for k, v in pids.most_common()),
        {"pids": {str(k): v for k, v in pids.items()}},
    )


def check_value_interval(scan, args):
    """PCR values must be spaced within the repetition limit."""
    iv = [(b[1] - a[1]) / TICKS_PER_MS for a, b in zip(scan.pcr, scan.pcr[1:])]
    if not iv:
        return ("pcr-value-interval", HARD, False, "no PCR intervals in the stream", {})
    over = [m for m in iv if m > args.repetition_ms]
    # A PCR that goes backwards is as much a clock defect as one that arrives too late,
    # and testing only the upper bound makes it invisible: the negative interval passes,
    # and it drags the median and the over-limit share down with it. Rollover is already
    # unwrapped in the scan, so a negative interval here is the stream's own doing. Zero
    # is not: ISO 13818-1 2.4.3.3 allows a packet to be sent twice, and the duplicate
    # repeats its PCR exactly, so testing `<= 0` would fail a conforming stream.
    backwards = [m for m in iv if m < 0]
    detail = {
        "count": len(iv),
        "median_ms": round(statistics.median(iv), 3),
        "p95_ms": round(percentile(iv, 95), 3),
        "max_ms": round(max(iv), 3),
        "over_limit": len(over),
        "over_limit_pct": round(100.0 * len(over) / len(iv), 2),
        "sub_ms": sum(1 for m in iv if m < 1.0),
        "non_positive": len(backwards),
        "wraps_unwrapped": scan.wraps,
    }
    backwards_note = f", {len(backwards)} non-positive" if backwards else ""
    return (
        "pcr-value-interval",
        HARD,
        not over and not backwards,
        f"{len(over)}/{len(iv)} intervals over {args.repetition_ms:g} ms "
        f"(median {detail['median_ms']:g} ms, worst {detail['max_ms']:g} ms{backwards_note})",
        detail,
    )


def check_release(scan, args):
    """The bytes carrying a PCR must be released at the time that PCR asserts.

    Graded against the stream's own values, so it holds at any clock rate: if two
    consecutive PCRs are 25 ms apart in value they must be ~25 ms apart in
    arrival. Two statistics, because they fail independently: per-interval error,
    which is what a receiver PLL and any downstream re-timing stage sees, and
    accumulated drift, which must stay bounded or the pipe is not running at the
    media rate at all.
    """
    pts = [(ticks, arrival) for _, ticks, arrival, _ in scan.pcr if arrival is not None]
    if len(pts) < 3:
        return ("pcr-release-timing", HARD, True, "not measured (no arrival stamps)", {})

    err = [((b[1] - a[1]) * 1000.0) - ((b[0] - a[0]) / TICKS_PER_MS) for a, b in zip(pts, pts[1:])]
    magnitude = [abs(e) for e in err]
    drift = ((pts[-1][1] - pts[0][1]) * 1000.0) - ((pts[-1][0] - pts[0][0]) / TICKS_PER_MS)
    late = [e for e in err if e > args.release_ms]
    early = [e for e in err if e < -args.release_ms]
    detail = {
        "count": len(err),
        "median_abs_ms": round(statistics.median(magnitude), 3),
        "p95_abs_ms": round(percentile(magnitude, 95), 3),
        "p99_abs_ms": round(percentile(magnitude, 99), 3),
        "max_abs_ms": round(max(magnitude), 3),
        "outside_tolerance": len(late) + len(early),
        "outside_tolerance_pct": round(100.0 * (len(late) + len(early)) / len(err), 2),
        "released_early": len(early),
        "released_late": len(late),
        "total_drift_ms": round(drift, 1),
        "drift_limit_ms": args.drift_ms,
    }
    # Per-interval error and accumulated drift fail independently, and bounding only the
    # first leaves the second unbounded: a consistent 1 ms error on a 25 ms grid sits well
    # inside a 10 ms tolerance while reaching nearly two seconds over a 45 s sample. The
    # docstring claimed drift was bounded; until now only the per-interval error was.
    drifted = abs(drift) > args.drift_ms
    drift_note = f" > ±{args.drift_ms:g} ms" if drifted else ""
    return (
        "pcr-release-timing",
        HARD,
        not (late or early or drifted),
        f"{detail['outside_tolerance']}/{len(err)} releases outside ±{args.release_ms:g} ms of the "
        f"interval the PCR asserts ({detail['released_early']} early, {detail['released_late']} late; "
        f"p95 {detail['p95_abs_ms']:g} ms, worst {detail['max_abs_ms']:g} ms; "
        f"drift {detail['total_drift_ms']:g} ms{drift_note})",
        detail,
    )


def check_position(scan, args):
    """A PCR packet must sit among the media bytes whose arrival it describes.

    The grid is uniform in media time, so if position tracks time then the packet
    gap between consecutive PCRs is roughly uniform too. That needs no assumption
    that the stream is CBR, only that comparable spans of media time carry
    comparable numbers of bytes. What it catches is a bimodal layout, PCR packets
    laid back-to-back with the media bytes they label heaped between the clusters.
    A consumer holding only the byte stream cannot recover the clock from such a
    layout, and one that re-stamps PCR from byte position regenerates exactly the
    clustering the value domain was fixed to remove.
    """
    if len(scan.pcr) < 3:
        return ("pcr-position", SHAPE, True, "not measured (too few PCRs)", {})
    gap = [b[0] - a[0] for a, b in zip(scan.pcr, scan.pcr[1:])]
    adjacent = sum(1 for g in gap if g <= args.adjacent_packets)
    median = statistics.median(gap)
    detail = {
        "count": len(gap),
        "median_packets": median,
        "p95_packets": percentile(gap, 95),
        "max_packets": max(gap),
        "adjacent": adjacent,
        "adjacent_pct": round(100.0 * adjacent / len(gap), 2),
        # Dispersion about the middle of the distribution: near 1 for a uniform
        # layout, large for cluster-and-hole. Guard a zero median.
        "p95_over_median": round(percentile(gap, 95) / median, 1) if median else None,
    }
    return (
        "pcr-position",
        SHAPE,
        detail["adjacent_pct"] <= args.adjacent_pct_max,
        f"{detail['adjacent_pct']:g}% of PCR packets sit within {args.adjacent_packets} packet(s) of "
        f"the previous one (median gap {median} packets, p95 {detail['p95_packets']}, worst {max(gap)})",
        detail,
    )


CHECKS = [
    check_sync,
    check_continuity,
    check_pcr_single_pid,
    check_value_interval,
    check_release,
    check_position,
]


def coincidence(scan, args):
    """Cross-tabulate release error against byte position, for one pass's PCRs.

    `release` and `position` are separate invariants and a stream can fail either
    alone, but when they fail *on the same PCRs* they have one cause rather than
    two, and that is worth reporting rather than leaving to be inferred from two
    aggregate percentages. Report-only: it explains a failure, it does not define
    one.
    """
    pts = [(i, ticks, arrival) for i, ticks, arrival, _ in scan.pcr if arrival is not None]
    if len(pts) < 3:
        return None
    rows = collections.Counter()
    for a, b in zip(pts, pts[1:]):
        err = ((b[2] - a[2]) * 1000.0) - ((b[1] - a[1]) / TICKS_PER_MS)
        when = "early" if err < -args.release_ms else "late" if err > args.release_ms else "on time"
        where = "adjacent" if (b[0] - a[0]) <= args.adjacent_packets else "spaced"
        rows[(where, when)] += 1
    return rows


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("path", nargs="?", help="TS file; omit with --live to read stdin")
    ap.add_argument("--live", action="store_true", help="read stdin and stamp arrivals, grading release timing")
    ap.add_argument("--seconds", type=float, default=45.0, help="live capture window (default 45)")
    ap.add_argument(
        "--repetition-ms", type=float, default=40.0, help="max PCR value interval, TR 101 290 (default 40)"
    )
    ap.add_argument(
        "--release-ms",
        type=float,
        default=10.0,
        help="tolerance on release timing against the asserted interval (default 10)",
    )
    ap.add_argument(
        "--drift-ms",
        type=float,
        default=250.0,
        help="bound on accumulated release drift over the whole sample (default 250)",
    )
    ap.add_argument(
        "--adjacent-packets", type=int, default=1, help="packet gap at or below which two PCRs count as clustered"
    )
    ap.add_argument(
        "--adjacent-pct-max", type=float, default=1.0, help="share of clustered PCRs before pcr-position flags"
    )
    ap.add_argument("--strict", action="store_true", help="fail on shape checks too")
    ap.add_argument("--report-json", help="write the full report here")
    args = ap.parse_args()

    if args.live:
        scan = scan_live(args.seconds)
    elif args.path:
        scan = scan_file(args.path)
    else:
        ap.error("give a path or --live")

    results = [check(scan, args) for check in CHECKS]
    width = max(len(r[0]) for r in results)

    print(f"### PCR timing report - {scan.packets} packets, {len(scan.pcr)} PCR")
    print()
    failed = 0
    for name, severity, ok, headline, _ in results:
        if ok:
            verdict = "PASS"
        elif severity == HARD or args.strict:
            verdict = "FAIL"
            failed += 1
        else:
            verdict = "WARN"
        print(f"  {verdict:4}  {name:<{width}}  {headline}")
    rows = coincidence(scan, args)
    if rows:
        total = sum(rows.values())
        print()
        print("  release timing by byte position (report only)")
        for where in ("adjacent", "spaced"):
            for when in ("early", "on time", "late"):
                n = rows[(where, when)]
                if n:
                    print(f"    {where:<8} + {when:<7}  {n:6}  ({100.0 * n / total:5.1f}%)")

    print()
    print(f"{failed} check(s) failed" if failed else "all checks passed")

    if args.report_json:
        with open(args.report_json, "w") as f:
            json.dump(
                {
                    "packets": scan.packets,
                    "pcr_count": len(scan.pcr),
                    "live": args.live,
                    "checks": [
                        {"name": n, "severity": s, "ok": ok, "headline": h, "detail": d}
                        for n, s, ok, h, d in results
                    ],
                },
                f,
                indent=2,
            )

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
