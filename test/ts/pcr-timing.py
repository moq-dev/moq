#!/usr/bin/env python3
"""Grade an MPEG-TS stream's PCR against its own asserted clock.

    moq ... export ts | ./pcr-timing.py --live --seconds 45
    ./pcr-timing.py capture.ts

A PCR is three claims at once, and a fix in one of them is invisible to an
instrument pointed at another:

    value       the intervals between PCR values         (ISO 13818-1 / TR 101 290)
    release     when the bytes carrying them were handed over  (--live only)
    position    where the PCR packets sit among the media bytes

and a constant-rate stream makes a fourth, which `schedule` grades: that the bytes
between consecutive PCRs are the bytes the mux rate implies for that interval.

Every check is an invariant on the stream, not an assertion about how the stream
was produced. `release` grades arrival against the PCR's *own* values rather than
against a wall clock, so it needs no reference clock, no source file and no
declared mux rate; the price is that a clock running at the wrong rate stays
internally consistent under it and has to be caught by `value` and `position`.
`schedule` takes the rate from --mux-rate when the caller knows it, and otherwise
estimates it from the capture and says so.

Exit status is 0 when every hard check passes, 1 otherwise. `--strict` promotes
the report-only (shape) checks to hard.
"""

import argparse
import collections
import json
import math
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
        self.new_base = set()  # positions in self.pcr that state a new time base
        self.pcr_pid = None  # the PID graded, once keep_busiest_pcr_pid has run
        self.other_pcr_pids = []
        self._cc = {}
        self._dup = {}
        self._new_base = set()  # PIDs whose next PCR states a new time base
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

        # ISO 13818-1 2.4.3.3 allows the counter to jump when the adaptation field sets
        # discontinuity_indicator, and allows one packet to be sent twice, repeating its
        # counter. Both are legal, and treating either as an error fails a conforming
        # stream on a hard check. Read the flag before the PCR, because 2.4.3.4 makes the
        # same flag mean the clock may jump too, and the value and release checks both need
        # to know which of their intervals spans that.
        cc = p[3] & 0x0F
        payload = bool((p[3] >> 4) & 0x1)
        adaptation = bool((p[3] >> 4) & 0x2)
        discontinuity = adaptation and p[4] > 0 and bool(p[5] & 0x80)

        ticks = parse_pcr(p)
        if ticks is not None:
            # Unwrap, so the checks see a monotone timeline. A capture crossing the
            # rollover would otherwise show one huge negative interval, which the value
            # check accepts (it only rejects intervals that are too long) while the
            # release check reads it as a day of lateness. A rollover drops the value by
            # nearly the whole modulus, so only a drop past halfway is treated as one and
            # a merely backwards PCR stays visible as the defect it is.
            #
            # A signalled discontinuity is the exception in both directions: the next PCR
            # states a new time base, so a drop across it is neither a wrap to unwrap nor a
            # backwards clock to report, and the interval spanning it is not a measurement
            # of anything. Mark it instead, and let the checks drop that one interval.
            if discontinuity or pid in self._new_base:
                self._new_base.discard(pid)
                self.new_base.add(len(self.pcr))
                self._pcr_raw.pop(pid, None)
            else:
                prev_raw = self._pcr_raw.get(pid)
                if prev_raw is not None and ticks < prev_raw - PCR_MODULUS // 2:
                    self._pcr_offset[pid] = self._pcr_offset.get(pid, 0) + PCR_MODULUS
                    self.wraps += 1
            self._pcr_raw[pid] = ticks
            self.pcr.append((index, ticks + self._pcr_offset.get(pid, 0), arrival, pid))
        elif discontinuity:
            # The flag rode a packet with no PCR in it, so the new base arrives with the
            # next PCR on this PID rather than with this packet.
            self._new_base.add(pid)

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

    def keep_busiest_pcr_pid(self):
        """Grade one clock: the PID carrying the most PCRs.

        Each program may carry its own PCR, and two correct grids offset from one another
        pool into one grid of half the interval that neither keeps.
        """
        pids = collections.Counter(pid for _, _, _, pid in self.pcr)
        self.pcr_pid = pids.most_common(1)[0][0] if pids else None
        self.other_pcr_pids = sorted(p for p in pids if p != self.pcr_pid)
        if not self.other_pcr_pids:
            return
        kept = [(k, e) for k, e in enumerate(self.pcr) if e[3] == self.pcr_pid]
        self.new_base = {n for n, (k, _) in enumerate(kept) if k in self.new_base}
        self.pcr = [e for _, e in kept]


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


def check_value_interval(scan, args):
    """PCR values must be spaced within the repetition limit.

    Within one time base. ISO 13818-1 2.4.3.4 lets a source declare a new one by setting
    discontinuity_indicator, after which the next PCR is a fresh value rather than the next
    point on the old ramp, so the difference across that boundary measures nothing. Counting
    it fails a conforming stream: a signalled splice reads as one enormous interval, which is
    the same class of false positive review found in the continuity check.
    """
    iv = [
        (b[1] - a[1]) / TICKS_PER_MS
        for i, (a, b) in enumerate(zip(scan.pcr, scan.pcr[1:]))
        if (i + 1) not in scan.new_base
    ]
    spliced = sum(1 for i in range(1, len(scan.pcr)) if i in scan.new_base)
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
        "signalled_discontinuities": spliced,
    }
    backwards_note = f", {len(backwards)} non-positive" if backwards else ""
    if spliced:
        backwards_note += f", {spliced} signalled discontinuity not counted"
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

    Bounded is the whole of the drift requirement, and the bound is the sender's
    latency budget. An exporter that buffers builds a standing lag once, at
    startup, and then runs at the media rate; the lag is a constant offset, which
    no receiver can see and which cannot grow past the budget the sender is
    allowed to hold. A pipe running slow never stops accumulating and so breaches
    any fixed bound given a long enough sample. So the total is the gate and the
    lag's rate over the tail of the sample is reported beside it, which is what
    distinguishes a lag that has settled from one still growing.
    """
    stamped = [i for i, (_, _, arrival, _) in enumerate(scan.pcr) if arrival is not None]
    pts = [(scan.pcr[i][1], scan.pcr[i][2]) for i in stamped]
    # scan.new_base indexes scan.pcr; pts drops the unstamped entries, so remap.
    live_new_base = {j for j, i in enumerate(stamped) if i in scan.new_base}
    # A file has no arrival stamps in it, so there is nothing to grade and saying so is
    # the honest verdict. Live is the opposite case: arrivals were expected, and too few
    # of them is a truncated capture rather than a clean stream. Passing that is how a
    # gate reports success on a producer that died, which is the state this tool exists
    # to catch, so the two have to be told apart rather than sharing one branch.
    if not args.live:
        if len(pts) < 3:
            return ("pcr-release-timing", HARD, True, "not measured (no arrival stamps)", {})
    else:
        covered_ms = (pts[-1][0] - pts[0][0]) / TICKS_PER_MS if len(pts) >= 2 else 0.0
        want_ms = 1000.0 * args.seconds * args.live_cover_pct / 100.0
        short = len(pts) < args.live_min_pcr or covered_ms < want_ms
        if short:
            return (
                "pcr-release-timing",
                HARD,
                False,
                f"insufficient live sample: {len(pts)} PCR with arrival stamps spanning "
                f"{covered_ms / 1000.0:.3f} s, against a floor of {args.live_min_pcr} PCR and "
                f"{want_ms / 1000.0:g} s ({args.live_cover_pct:g} % of the {args.seconds:g} s "
                f"window). A truncated capture cannot be graded and must not pass",
                {
                    "count": len(pts),
                    "covered_s": round(covered_ms / 1000.0, 3),
                    "required_pcr": args.live_min_pcr,
                    "required_s": round(want_ms / 1000.0, 3),
                },
            )

    # Same exclusion as the value check: an interval spanning a signalled new time base is
    # not a release measurement, and counting it reports a splice as seconds of lateness.
    graded = [
        (((b[1] - a[1]) * 1000.0) - ((b[0] - a[0]) / TICKS_PER_MS), (b[0] - a[0]) / TICKS_PER_MS)
        for i, (a, b) in enumerate(zip(pts, pts[1:]))
        if (i + 1) not in live_new_base
    ]
    err = [e for e, _ in graded]
    span = [s for _, s in graded]
    if not err:
        return ("pcr-release-timing", HARD, False, "no gradable release intervals", {})
    magnitude = [abs(e) for e in err]
    # Accumulated drift is the sum of the intervals actually graded. That telescopes to the
    # endpoint difference on an unspliced sample, and stays correct on a spliced one, where
    # the endpoints straddle a time base the stream never claimed to be running on.
    drift = sum(err)
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
    # Whether the lag has settled is the difference between a buffer that filled and a
    # pipe running slow, and the two are only separable over a sample longer than the
    # fill. Grade the last third: a settled lag adds nothing to it.
    cut = (2 * len(err)) // 3
    tail_span_ms = sum(span[cut:])
    if tail_span_ms > 0:
        detail["tail_drift_rate_ms_per_s"] = round(1000.0 * sum(err[cut:]) / tail_span_ms, 3)
        detail["tail_span_s"] = round(tail_span_ms / 1000.0, 1)
    # Per-interval error and accumulated drift fail independently, and bounding only the
    # first leaves the second unbounded: a consistent 1 ms error on a 25 ms grid sits well
    # inside a 10 ms tolerance while reaching nearly two seconds over a 45 s sample. The
    # docstring claimed drift was bounded; until now only the per-interval error was.
    drifted = abs(drift) > args.drift_ms
    drift_note = f" > ±{args.drift_ms:g} ms" if drifted else ""
    rate = detail.get("tail_drift_rate_ms_per_s")
    tail_note = "" if rate is None else f", {rate:+g} ms/s over the last {detail['tail_span_s']:g} s"
    over = detail["outside_tolerance_pct"] > args.release_pct_max
    return (
        "pcr-release-timing",
        HARD,
        not (over or drifted),
        f"{detail['outside_tolerance']}/{len(err)} releases outside ±{args.release_ms:g} ms of the "
        f"interval the PCR asserts ({detail['released_early']} early, {detail['released_late']} late; "
        f"p95 {detail['p95_abs_ms']:g} ms, worst {detail['max_abs_ms']:g} ms; "
        f"drift {detail['total_drift_ms']:g} ms{drift_note}{tail_note})",
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


def check_schedule(scan, args):
    """The bytes between consecutive PCRs must be the bytes the mux rate implies.

    `position` asks whether PCR packets are spread among the media bytes; this asks
    whether they are spread at the right rate. A constant-rate stream is one where the
    byte distance between two PCRs, over the time between their values, is the mux rate
    for every interval and not merely on average, and that is what a receiver recovering
    its clock from packet arrival depends on. A census of the whole capture cannot see
    it: a stream padded to the right total with its bytes heaped between a few PCRs reads
    as constant-rate in aggregate and at almost no individual interval.

    The rate comes from --mux-rate when the caller knows it, which also pins the absolute
    rate the way nothing else here does. Otherwise it is estimated from the graded
    intervals themselves, so the aggregate is right by construction and what is left to
    grade is only how evenly the bytes are laid over the PCRs. The report says which.
    The estimate is total bytes over total time, so a transient in a short sample, such
    as a start that is not yet padded, biases every interval's error by the same amount;
    pass the rate whenever it is known.

    Report-only unless --schedule-pct-min is given: a VBR stream has no schedule to keep,
    and `export ts` is VBR unless a mux rate is declared, so only the caller knows whether
    this property was promised.
    """
    hard = args.schedule_pct_min is not None
    severity = HARD if hard else SHAPE
    required = args.schedule_pct_min if hard else 99.0
    graded = []
    skipped = 0
    for kb, (a, b) in enumerate(zip(scan.pcr, scan.pcr[1:]), start=1):
        seconds = (b[1] - a[1]) / TICKS_PER_MS / 1000.0
        # An interval spanning a signalled new time base measures nothing, as in the value
        # check. A non-positive one is a duplicate packet (legal) or a backwards clock (the
        # value check's defect to report); neither has a rate to compare against.
        if kb in scan.new_base or seconds <= 0:
            skipped += 1
            continue
        graded.append(((b[0] - a[0]) * PKT, seconds))
    if len(graded) < 3:
        # Asked to gate, an ungradable stream is a failure rather than a clean one.
        return ("pcr-schedule", severity, not hard, "not measured (too few PCR intervals)", {})

    aggregate = 8.0 * sum(n for n, _ in graded) / sum(s for _, s in graded)
    rate = args.mux_rate if args.mux_rate else aggregate
    source = "declared" if args.mux_rate else "estimated from the capture"
    want = [rate * s / 8.0 for _, s in graded]
    # Adding 0.0 turns an exact -0.0 into 0.0, so an exact schedule does not print as -0%.
    err = [100.0 * (n / w - 1.0) + 0.0 for (n, _), w in zip(graded, want)]
    # PCR packets can only sit on packet boundaries, so a mux whose PCR values are on a
    # time grid is up to a packet off at every interval even when its schedule is exact.
    # Below ~19 kB per interval one packet is more than 1 %, and a percentage alone would
    # fail a correct low-rate stream on quantisation.
    slack = [max(args.schedule_tolerance_pct / 100.0 * w, PKT) for w in want]
    within = sum(1 for (n, _), w, s in zip(graded, want, slack) if abs(n - w) <= s)
    median_s = statistics.median(s for _, s in graded)
    detail = {
        "pid": scan.pcr_pid,
        "other_pcr_pids": scan.other_pcr_pids,
        "count": len(graded),
        "skipped": skipped,
        "rate_bps": round(rate),
        "rate_source": "declared" if args.mux_rate else "estimated",
        "aggregate_bps": round(aggregate),
        "median_bytes": statistics.median(n for n, _ in graded),
        "nominal_bytes_at_median_interval": round(rate * median_s / 8.0),
        "min_bytes": min(n for n, _ in graded),
        "max_bytes": max(n for n, _ in graded),
        "rel_err_p1_pct": round(percentile(err, 1), 2),
        "rel_err_median_pct": round(statistics.median(err), 2),
        "rel_err_p99_pct": round(percentile(err, 99), 2),
        "rel_err_max_abs_pct": round(max(abs(e) for e in err), 2),
        "tolerance_pct": args.schedule_tolerance_pct,
        "within_tolerance": within,
        "within_tolerance_pct": round(100.0 * within / len(graded), 2),
        "required_pct": required,
    }
    aggregate_note = f", aggregate {aggregate:,.0f} b/s" if args.mux_rate else ""
    return (
        "pcr-schedule",
        severity,
        # The exact counts, not the rounded share: 20,000 of 20,001 rounds to 100.
        100.0 * within >= required * len(graded),
        f"{within}/{len(graded)} intervals ({detail['within_tolerance_pct']:g}%) within "
        f"±{args.schedule_tolerance_pct:g}% (or one packet) of the bytes {rate:,.0f} b/s implies "
        f"({source}{aggregate_note}); median gap {detail['median_bytes']:,.0f} B against "
        f"{detail['nominal_bytes_at_median_interval']:,} B, error p1 {detail['rel_err_p1_pct']:+g}% "
        f"median {detail['rel_err_median_pct']:+g}% p99 {detail['rel_err_p99_pct']:+g}%, "
        f"worst {detail['rel_err_max_abs_pct']:g}%",
        detail,
    )


# A file is graded for sync and continuity by compliance.py, through TSDuck's tsanalyze.
# A pipe cannot be: tsp in front of it would rebuffer the very arrivals `release` stamps.
LIVE_CHECKS = [check_sync, check_continuity]
CHECKS = [check_value_interval, check_release, check_position, check_schedule]


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
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("path", nargs="?", help="TS file; omit with --live to read stdin")
    ap.add_argument("--live", action="store_true", help="read stdin and stamp arrivals, grading release timing")
    ap.add_argument("--seconds", type=float, default=45.0, help="live capture window (default 45)")
    ap.add_argument(
        "--repetition-ms", type=float, default=100.0, help="max PCR value interval, TR 101 290 V1.4.1 (default 100)"
    )
    ap.add_argument(
        "--release-ms",
        type=float,
        default=10.0,
        help="tolerance on release timing against the asserted interval (default 10)",
    )
    ap.add_argument(
        "--release-pct-max",
        type=float,
        default=0.0,
        help="share of intervals allowed outside the release tolerance (default 0)",
    )
    ap.add_argument(
        "--live-min-pcr",
        type=int,
        default=20,
        help="fewest PCR samples a --live run may grade before it is a truncated capture "
        "rather than a result (default 20)",
    )
    ap.add_argument(
        "--live-cover-pct",
        type=float,
        default=50.0,
        help="share of the --seconds window a --live sample must span, or it is treated as truncated (default 50)",
    )
    ap.add_argument(
        "--drift-ms",
        type=float,
        default=500.0,
        help="bound on accumulated release drift, being the standing lag the sender may "
        "hold; set it to the sender's latency budget (moq export ts --max-age, "
        "itself 500ms by default)",
    )
    ap.add_argument(
        "--adjacent-packets", type=int, default=1, help="packet gap at or below which two PCRs count as clustered"
    )
    ap.add_argument(
        "--adjacent-pct-max", type=float, default=1.0, help="share of clustered PCRs before pcr-position flags"
    )
    ap.add_argument(
        "--mux-rate",
        type=float,
        metavar="BPS",
        help="the constant rate pcr-schedule grades against, in bits per second (default: estimated from the capture)",
    )
    ap.add_argument(
        "--schedule-tolerance-pct",
        type=float,
        default=1.0,
        help="how far an interval's bytes may be from what the mux rate implies, as a percentage; "
        "one packet is always allowed (default 1)",
    )
    ap.add_argument(
        "--schedule-pct-min",
        type=float,
        metavar="PCT",
        help="share of PCR intervals that must be within the schedule tolerance; giving it makes "
        "pcr-schedule a hard check (default: report-only, flagging below 99)",
    )
    ap.add_argument("--strict", action="store_true", help="fail on shape checks too")
    ap.add_argument("--report-json", help="write the full report here")
    args = ap.parse_args()
    for flag in ("mux_rate", "schedule_tolerance_pct", "schedule_pct_min"):
        value = getattr(args, flag)
        if value is not None and not math.isfinite(value):
            ap.error(f"--{flag.replace('_', '-')} must be finite")
    if args.mux_rate is not None and args.mux_rate <= 0:
        ap.error("--mux-rate must be positive")

    if args.live:
        scan = scan_live(args.seconds)
    elif args.path:
        scan = scan_file(args.path)
    else:
        ap.error("give a path or --live")

    scan.keep_busiest_pcr_pid()
    results = [check(scan, args) for check in (LIVE_CHECKS if args.live else []) + CHECKS]
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
                        {"name": n, "severity": s, "ok": ok, "headline": h, "detail": d} for n, s, ok, h, d in results
                    ],
                },
                f,
                indent=2,
            )

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
