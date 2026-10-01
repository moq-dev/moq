#!/usr/bin/env python3
"""MPEG-TS / IRD compliance analyzer for a captured transport stream.

Given a `.ts` file (typically the output of `moq ... export ts`), this runs the
checks an Integrated Receiver/Decoder cares about and prints a PASS/WARN/FAIL
summary, exiting non-zero on failure.

Division of labour: TSDuck does the transport-stream parsing (we shell out to
`tsanalyze --json` for PSI/service/structure and `tstables` for the PMT), and
this script does the model math TSDuck does not cover (PCR jitter/repetition,
packet inter-arrival, burstiness, instantaneous bitrate, and the ISO 13818-1
T-STD buffer model, which no maintained tool implements).
The PCR/PTS/DTS timeline the timing model needs comes from a 188/204-byte
packet-header scan done here, which also gives the per-packet PID that
`tsp -P pcrextract` does not expose.

Checks split into two severities:
  - HARD (structural): fail the run by default. PAT/PMT, packet size, sync,
    continuity counters, PSI CRC, PCR presence, PCR monotonicity.
  - SHAPE (broadcast profile): reported as WARN and only fail the run under
    `--strict`. PCR repetition interval, PCR jitter, null-packet ratio, bitrate
    consistency / burstiness, service descriptors (SDT), T-STD buffer model.

Timing basis is the stream's own PCR clock (an IRD locks to PCR), so the harness
needs no wall-clock capture and results are deterministic for a given file.
"""

from __future__ import annotations

import argparse
import bisect
import json
import subprocess
import sys
from dataclasses import dataclass, field
from enum import Enum

# PCR runs on the 27 MHz system clock; PTS/DTS on the 90 kHz clock.
PCR_HZ = 27_000_000
PTS_HZ = 90_000
# The PCR base field is 33 bits at 90 kHz, so the full 27 MHz PCR wraps here.
PCR_WRAP = (1 << 33) * 300
NULL_PID = 0x1FFF


class Status(Enum):
    """Outcome of a single compliance check."""

    PASS = "PASS"
    WARN = "WARN"
    FAIL = "FAIL"


class Severity(Enum):
    """Whether a failing check aborts the run by default (HARD) or only under --strict (SHAPE)."""

    HARD = "hard"
    SHAPE = "shape"


@dataclass
class Check:
    """One named compliance check plus its verdict and supporting numbers."""

    name: str
    severity: Severity
    status: Status
    detail: str
    metrics: dict = field(default_factory=dict)


@dataclass
class Thresholds:
    """IRD limits and T-STD model parameters, all overridable from the CLI."""

    pcr_repetition_ms: float = 40.0
    pcr_jitter_us: float = 500.0
    null_ratio_max: float = 0.90
    bitrate_cov_max: float = 0.10
    burstiness_max: float = 3.0
    inst_windows_ms: tuple[float, ...] = (1.0, 10.0)


# --------------------------------------------------------------------------- IO


def run_tsanalyze(ts_path: str) -> dict:
    """Return TSDuck's structural analysis as a dict, capturing stderr for CRC warnings."""
    proc = subprocess.run(
        ["tsanalyze", "--json", ts_path],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0 and not proc.stdout:
        raise RuntimeError(f"tsanalyze failed: {proc.stderr.strip()}")
    data = json.loads(proc.stdout)
    data["_stderr"] = proc.stderr
    return data


@dataclass
class Scan:
    """Per-packet facts recovered from a raw header scan of the TS file."""

    packet_size: int
    total_packets: int
    # (ts_index, payload_bytes) per PID, in file order.
    pid_packets: dict[int, list[tuple[int, int]]]
    # (ts_index, pcr_27mhz) samples for every PID that carries PCR.
    pcr_by_pid: dict[int, list[tuple[int, int]]]
    # ts_index of every PCR that starts a new time base (discontinuity_indicator).
    pcr_new_base: set[int]


def scan_packets(ts_path: str, packet_size: int) -> Scan:
    """Walk the TS packet by packet, recovering PID, payload size, and PCR values.

    This is a header-only scan (no PES/PSI parsing): enough for the timing and
    buffer models, which need per-packet PID and the PCR timeline.
    """
    with open(ts_path, "rb") as handle:
        data = handle.read()

    pid_packets: dict[int, list[tuple[int, int]]] = {}
    pcr_by_pid: dict[int, list[tuple[int, int]]] = {}
    pcr_new_base: set[int] = set()
    # PIDs whose discontinuity_indicator rode a packet without a PCR, so the new time
    # base starts at their next PCR (ISO 13818-1 2.4.3.5).
    pending_base: set[int] = set()
    index = 0
    offset = 0
    n = len(data)
    while offset + packet_size <= n:
        if data[offset] != 0x47:
            # Try to resync on the next sync byte; tsanalyze already reports the count.
            nxt = data.find(0x47, offset + 1)
            if nxt < 0:
                break
            offset = nxt
            continue

        b1, b2, b3 = data[offset + 1], data[offset + 2], data[offset + 3]
        pid = ((b1 & 0x1F) << 8) | b2
        afc = (b3 >> 4) & 0x3

        payload_len = 0
        af_len = 0
        if afc in (2, 3):
            af_len = data[offset + 4]
            if af_len > 0:
                flags = data[offset + 5]
                if flags & 0x80:
                    pending_base.add(pid)
                if (flags & 0x10) and af_len >= 7:  # PCR present
                    base = (
                        (data[offset + 6] << 25)
                        | (data[offset + 7] << 17)
                        | (data[offset + 8] << 9)
                        | (data[offset + 9] << 1)
                        | (data[offset + 10] >> 7)
                    )
                    ext = ((data[offset + 10] & 0x01) << 8) | data[offset + 11]
                    pcr_by_pid.setdefault(pid, []).append((index, base * 300 + ext))
                    if pid in pending_base:
                        pending_base.discard(pid)
                        pcr_new_base.add(index)
        if afc in (1, 3):
            # Payload = 184 minus the adaptation field (its length byte + body).
            consumed = (1 + af_len) if afc == 3 else 0
            payload_len = 184 - consumed

        if pid != NULL_PID:
            pid_packets.setdefault(pid, []).append((index, max(0, payload_len)))

        index += 1
        offset += packet_size

    return Scan(
        packet_size=packet_size,
        total_packets=index,
        pid_packets=pid_packets,
        pcr_by_pid=pcr_by_pid,
        pcr_new_base=pcr_new_base,
    )


# --------------------------------------------------------------- small helpers


def percentile(values: list[float], pct: float) -> float:
    """Linear-interpolated percentile of an unsorted list (0..100). 0 for empty."""
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (pct / 100.0) * (len(ordered) - 1)
    low = int(rank)
    high = min(low + 1, len(ordered) - 1)
    frac = rank - low
    return ordered[low] * (1 - frac) + ordered[high] * frac


def mean(values: list[float]) -> float:
    """Arithmetic mean, 0 for an empty list."""
    return sum(values) / len(values) if values else 0.0


def pcr_seconds(pcr_27mhz: int) -> float:
    """Convert a 27 MHz PCR value to seconds."""
    return pcr_27mhz / PCR_HZ


class PcrClock:
    """Maps a TS packet index to a wall-clock second via the PCR-PID samples.

    An IRD reconstructs time by locking to PCR, so packet i is 'delivered' at the
    time obtained by linear interpolation between the surrounding PCRs (and by the
    edge segment's slope beyond the first/last PCR).
    """

    def __init__(self, samples: list[tuple[int, int]]):
        # Unwrap the 27 MHz PCR across its 33-bit boundary, then keep only forward samples.
        self.idx: list[int] = []
        self.sec: list[float] = []
        unwrapped = 0
        prev = None
        for i, pcr in samples:
            if prev is not None and pcr < prev - PCR_WRAP / 2:
                unwrapped += PCR_WRAP
            prev = pcr
            secs = pcr_seconds(pcr + unwrapped)
            if self.sec and secs <= self.sec[-1]:
                continue
            self.idx.append(i)
            self.sec.append(secs)

    def ok(self) -> bool:
        """True when there are enough PCRs to interpolate a timeline."""
        return len(self.idx) >= 2

    def time_at(self, index: int) -> float:
        """Interpolate (or extrapolate on the edge slope) the delivery time of a packet index."""
        idx, sec = self.idx, self.sec
        pos = bisect.bisect_left(idx, index)
        if pos <= 0:
            i0, i1 = 0, 1
        elif pos >= len(idx):
            i0, i1 = len(idx) - 2, len(idx) - 1
        elif idx[pos] == index:
            return sec[pos]
        else:
            i0, i1 = pos - 1, pos
        span = idx[i1] - idx[i0]
        if span == 0:
            return sec[i0]
        slope = (sec[i1] - sec[i0]) / span
        return sec[i0] + slope * (index - idx[i0])


# ------------------------------------------------------------- structural checks


def check_packet_size(analysis: dict) -> Check:
    """Packets must be 188 (or 204 with the FEC trailer); anything else breaks demuxers."""
    ts = analysis["ts"]
    total = ts["packets"]["total"]
    size = round(ts["bytes"] / total) if total else 0
    if size in (188, 204):
        return Check("packet-size", Severity.HARD, Status.PASS, f"{size} bytes/packet", {"packet_size": size})
    return Check("packet-size", Severity.HARD, Status.FAIL, f"unexpected {size} bytes/packet", {"packet_size": size})


def check_sync(analysis: dict) -> Check:
    """No invalid sync bytes and no transport_error_indicator packets."""
    pk = analysis["ts"]["packets"]
    bad = pk.get("invalid-syncs", 0)
    tei = pk.get("transport-errors", 0)
    metrics = {"invalid_syncs": bad, "transport_errors": tei}
    if bad == 0 and tei == 0:
        return Check("sync", Severity.HARD, Status.PASS, "no sync loss / transport errors", metrics)
    return Check("sync", Severity.HARD, Status.FAIL, f"invalid_syncs={bad} transport_errors={tei}", metrics)


def check_pat_pmt(analysis: dict) -> tuple[Check, Check]:
    """PAT must map at least one program to a PMT that names its elementary streams."""
    tables = analysis.get("tables", [])
    services = analysis.get("services", [])
    has_pat = any(t.get("tid") == 0 for t in tables)
    has_pmt_table = any(t.get("tid") == 2 for t in tables)
    has_pmt_pid = any(s.get("pmt-pid") is not None for s in services)

    pat = (
        Check("pat", Severity.HARD, Status.PASS, f"{len(services)} program(s)", {"services": len(services)})
        if has_pat and services
        else Check("pat", Severity.HARD, Status.FAIL, "no valid PAT / program", {"services": len(services)})
    )
    if has_pmt_table and has_pmt_pid:
        components = sum(s.get("components", {}).get("total", 0) for s in services)
        pmt = Check("pmt", Severity.HARD, Status.PASS, f"{components} elementary stream(s)", {"components": components})
    else:
        pmt = Check("pmt", Severity.HARD, Status.FAIL, "no valid PMT", {})
    return pat, pmt


def check_psi_crc(analysis: dict) -> Check:
    """TSDuck drops sections with a bad CRC and logs it; treat any such log as a failure."""
    stderr = analysis.get("_stderr", "") or ""
    hits = [ln for ln in stderr.splitlines() if "crc" in ln.lower()]
    if hits:
        return Check("psi-crc", Severity.HARD, Status.FAIL, hits[0].strip(), {"crc_errors": len(hits)})
    return Check("psi-crc", Severity.HARD, Status.PASS, "no CRC errors reported", {"crc_errors": 0})


def check_continuity(analysis: dict) -> Check:
    """Sum the per-PID continuity-counter discontinuities reported by TSDuck."""
    total = 0
    worst = None
    for pid in analysis.get("pids", []):
        disc = pid.get("packets", {}).get("discontinuities", 0)
        total += disc
        if disc and (worst is None or disc > worst[1]):
            worst = (pid["id"], disc)
    metrics = {"cc_errors": total}
    if total == 0:
        return Check("continuity", Severity.HARD, Status.PASS, "no CC errors", metrics)
    where = f" (worst PID {worst[0]}: {worst[1]})" if worst else ""
    return Check("continuity", Severity.HARD, Status.FAIL, f"{total} CC error(s){where}", metrics)


def check_pcr_presence(analysis: dict, clock_by_pid: dict[int, PcrClock]) -> Check:
    """A PCR PID must be declared and actually carry PCR samples."""
    pcr_pids = [s.get("pcr-pid") for s in analysis.get("services", []) if s.get("pcr-pid") is not None]
    carrying = [pid for pid, clock in clock_by_pid.items() if clock.idx]
    metrics = {"pcr_pids": pcr_pids, "pcr_carrying_pids": carrying}
    if pcr_pids and carrying:
        return Check("pcr-presence", Severity.HARD, Status.PASS, f"PCR on PID {carrying}", metrics)
    return Check("pcr-presence", Severity.HARD, Status.FAIL, "no PCR samples found", metrics)


def check_pcr_monotonic(scan: Scan) -> Check:
    """PCR must strictly increase per PID (a single 33-bit wrap is tolerated).

    Except into a PCR that signals a new time base (ISO 13818-1 2.4.3.4), which may
    take any value; those are counted so a stream that leans on them still shows it.
    """
    breaks = 0
    for _pid, samples in scan.pcr_by_pid.items():
        prev = None
        for i, pcr in samples:
            if prev is not None and i not in scan.pcr_new_base:
                delta = pcr - prev
                # A legitimate wrap shows as a large negative jump; anything else is a fault.
                if delta <= 0 and not delta < -PCR_WRAP / 2:
                    breaks += 1
            prev = pcr
    metrics = {"pcr_backwards": breaks, "signalled_discontinuities": len(scan.pcr_new_base)}
    if breaks == 0:
        return Check("pcr-monotonic", Severity.HARD, Status.PASS, "PCR strictly increasing", metrics)
    return Check("pcr-monotonic", Severity.HARD, Status.FAIL, f"{breaks} backwards PCR step(s)", metrics)


# ------------------------------------------------------------------- shape checks


def check_pcr_repetition(scan: Scan, th: Thresholds) -> Check:
    """Consecutive PCRs on a PID should be no more than `pcr_repetition_ms` apart."""
    worst_ms = 0.0
    intervals: list[float] = []
    over = 0
    for samples in scan.pcr_by_pid.values():
        prev = None
        for _i, pcr in samples:
            if prev is not None:
                delta = pcr - prev
                if delta <= 0:
                    prev = pcr
                    continue
                ms = pcr_seconds(delta) * 1000.0
                intervals.append(ms)
                worst_ms = max(worst_ms, ms)
                if ms > th.pcr_repetition_ms:
                    over += 1
            prev = pcr
    metrics = {
        "max_interval_ms": round(worst_ms, 3),
        "mean_interval_ms": round(mean(intervals), 3),
        "intervals_over_limit": over,
        "limit_ms": th.pcr_repetition_ms,
    }
    detail = f"max {worst_ms:.1f} ms (limit {th.pcr_repetition_ms:.0f} ms), {over} over"
    status = Status.PASS if over == 0 else Status.WARN
    return Check("pcr-repetition", Severity.SHAPE, status, detail, metrics)


def check_pcr_jitter(scan: Scan, ts_bitrate: float, th: Thresholds) -> Check:
    """Per-interval PCR jitter vs the nominal bitrate (pcrverify's model).

    For a true CBR mux the actual PCR delta matches the byte delta clocked at the
    stream bitrate; the difference is the jitter. On a VBR stream it is large by
    construction, which is exactly the IRD-relevant signal.
    """
    if ts_bitrate <= 0:
        return Check("pcr-jitter", Severity.SHAPE, Status.WARN, "unknown bitrate", {})
    bits_per_packet = scan.packet_size * 8
    jitters_us: list[float] = []
    for samples in scan.pcr_by_pid.values():
        prev = None
        for i, pcr in samples:
            if prev is not None:
                pi, ppcr = prev
                d_pcr = pcr - ppcr
                if d_pcr <= 0:
                    prev = (i, pcr)
                    continue
                expected_s = (i - pi) * bits_per_packet / ts_bitrate
                actual_s = pcr_seconds(d_pcr)
                jitters_us.append((actual_s - expected_s) * 1e6)
            prev = (i, pcr)
    if not jitters_us:
        return Check("pcr-jitter", Severity.SHAPE, Status.WARN, "no PCR intervals", {})
    abs_jit = [abs(j) for j in jitters_us]
    max_us = max(abs_jit)
    p95 = percentile(abs_jit, 95)
    metrics = {
        "max_abs_us": round(max_us, 1),
        "p95_abs_us": round(p95, 1),
        "limit_us": th.pcr_jitter_us,
    }
    detail = f"max |jitter| {max_us:.0f} us, p95 {p95:.0f} us (limit {th.pcr_jitter_us:.0f} us)"
    status = Status.PASS if max_us <= th.pcr_jitter_us else Status.WARN
    return Check("pcr-jitter", Severity.SHAPE, status, detail, metrics)


def check_null_ratio(analysis: dict, th: Thresholds) -> Check:
    """Report the null-packet (stuffing) fraction; flag only a pathological excess."""
    total = analysis["ts"]["packets"]["total"] or 1
    null = 0
    for pid in analysis.get("pids", []):
        if pid["id"] == NULL_PID:
            null = pid.get("packets", {}).get("total", 0)
    ratio = null / total
    metrics = {"null_ratio": round(ratio, 4), "null_packets": null, "limit": th.null_ratio_max}
    detail = f"{ratio * 100:.2f}% null packets"
    status = Status.PASS if ratio <= th.null_ratio_max else Status.WARN
    return Check("null-ratio", Severity.SHAPE, status, detail, metrics)


def check_service_descriptors(analysis: dict) -> Check:
    """An IRD expects an SDT naming the service; PAT/PMT-only streams get a WARN."""
    tables = analysis.get("tables", [])
    has_sdt = any(t.get("tid") == 0x42 for t in tables)
    services = analysis.get("services", [])
    named = [s.get("name") for s in services if s.get("name")]
    metrics = {"sdt": has_sdt, "service_names": named}
    if has_sdt and named:
        return Check("service-descriptors", Severity.SHAPE, Status.PASS, f"SDT: {named}", metrics)
    return Check(
        "service-descriptors",
        Severity.SHAPE,
        Status.WARN,
        "no SDT (service name/provider absent)",
        metrics,
    )


def windowed_bitrates(times: list[float], sizes: list[int], window_s: float) -> list[float]:
    """Bytes-per-window converted to bit/s, over contiguous windows spanning the capture."""
    if not times:
        return []
    start, end = times[0], times[-1]
    if end <= start:
        return []
    n_windows = max(1, int((end - start) / window_s) + 1)
    buckets = [0] * n_windows
    for t, size in zip(times, sizes):
        idx = min(n_windows - 1, int((t - start) / window_s))
        buckets[idx] += size
    # Drop the last (partial) window so a short tail doesn't skew the minimum.
    if n_windows > 1:
        buckets = buckets[:-1]
    return [b * 8 / window_s for b in buckets]


def check_bitrate_and_burstiness(scan: Scan, clock: PcrClock, ts_bitrate: float, th: Thresholds) -> tuple[Check, Check]:
    """Instantaneous bitrate spread (CBR-ness) and delivery burstiness, on the PCR clock."""
    # Every non-null packet, timed on the PCR clock, weighted by its full 188 bytes.
    events: list[tuple[float, int]] = []
    for _pid, packets in scan.pid_packets.items():
        for i, _payload in packets:
            events.append((clock.time_at(i), scan.packet_size))
    events.sort(key=lambda e: e[0])
    times = [t for t, _ in events]
    sizes = [s for _, s in events]

    inst_metrics: dict = {"nominal_bps": round(ts_bitrate)}
    worst_cov = 0.0
    worst_burst = 0.0
    for window_ms in th.inst_windows_ms:
        rates = windowed_bitrates(times, sizes, window_ms / 1000.0)
        if not rates:
            continue
        avg = mean(rates)
        peak = max(rates)
        low = min(rates)
        var = mean([(r - avg) ** 2 for r in rates])
        cov = (var**0.5 / avg) if avg else 0.0
        burst = (peak / avg) if avg else 0.0
        worst_cov = max(worst_cov, cov)
        worst_burst = max(worst_burst, burst)
        inst_metrics[f"w{int(window_ms)}ms"] = {
            "min_bps": round(low),
            "mean_bps": round(avg),
            "max_bps": round(peak),
            "p95_bps": round(percentile(rates, 95)),
            "cov": round(cov, 3),
            "peak_over_mean": round(burst, 2),
        }

    bitrate_status = Status.PASS if worst_cov <= th.bitrate_cov_max else Status.WARN
    bitrate = Check(
        "bitrate-consistency",
        Severity.SHAPE,
        bitrate_status,
        f"worst CoV {worst_cov:.2f} (limit {th.bitrate_cov_max:.2f})",
        inst_metrics | {"worst_cov": round(worst_cov, 3)},
    )
    burst_status = Status.PASS if worst_burst <= th.burstiness_max else Status.WARN
    burst = Check(
        "burstiness",
        Severity.SHAPE,
        burst_status,
        f"peak/mean {worst_burst:.2f} (limit {th.burstiness_max:.2f})",
        {"worst_peak_over_mean": round(worst_burst, 2), "limit": th.burstiness_max},
    )
    return bitrate, burst


def check_inter_arrival(scan: Scan, clock: PcrClock) -> Check:
    """Report the packet inter-arrival spread on the PCR clock (informational)."""
    indices = sorted(i for packets in scan.pid_packets.values() for i, _ in packets)
    gaps_us: list[float] = []
    prev = None
    for i in indices:
        t = clock.time_at(i)
        if prev is not None and t >= prev:
            gaps_us.append((t - prev) * 1e6)
        prev = t
    if not gaps_us:
        return Check("inter-arrival", Severity.SHAPE, Status.WARN, "no packets timed", {})
    metrics = {
        "mean_us": round(mean(gaps_us), 2),
        "p95_us": round(percentile(gaps_us, 95), 2),
        "max_us": round(max(gaps_us), 2),
    }
    return Check(
        "inter-arrival",
        Severity.SHAPE,
        Status.PASS,
        f"mean {metrics['mean_us']:.1f} us, p95 {metrics['p95_us']:.1f} us, max {metrics['max_us']:.1f} us",
        metrics,
    )


# ---------------------------------------------------------------- T-STD model
#
# ISO/IEC 13818-1 2.4.2 (Rec. ITU-T H.222.0 10/2014, free from the ITU), fed each
# elementary stream's packets at the times its program's PCR assigns them:
#
#   video (AVC 2.14.3.1, HEVC 2.17.2)  TB -Rx-> MB -Rbx (leak)-> EB -DTS-> decoder
#   audio (2.4.2.3)                    TB -Rx-> B  ---------------PTS-> decoder
#
# and graded on 2.4.2.6 and its AVC/HEVC counterparts: no buffer overflows, TB
# empties at least once a second, every access unit is complete in EB/B at its
# decoding time, and no byte waits longer than the STD delay bound.
#
# No maintained tool implements this (TSDuck has no T-STD analyzer), so the
# parameters are transcribed from the specs named beside each table.

TB_SIZE = 512
# 2.4.2.6: TB must empty at least once a second.
TB_EMPTY_S = 1.0
# PTS/DTS are 33 bits at 90 kHz.
STAMP_WRAP_S = (1 << 33) / PTS_HZ

# H.264 Table A-1: level_idc -> (MaxBR, MaxCPB) as tabulated. H.222.0 2.14.3.1
# scales both by 1200 bits for the buffers, and Rx by the profile's cpbBrNalFactor.
AVC_LEVELS = {
    10: (64, 175),
    11: (192, 500),
    12: (384, 1000),
    13: (768, 2000),
    20: (2000, 2000),
    21: (4000, 4000),
    22: (4000, 4000),
    30: (10000, 10000),
    31: (14000, 14000),
    32: (20000, 20000),
    40: (20000, 25000),
    41: (50000, 62500),
    42: (50000, 62500),
    50: (135000, 135000),
    51: (240000, 240000),
    52: (240000, 240000),
    60: (240000, 240000),
    61: (480000, 480000),
    62: (800000, 800000),
}
AVC_LEVEL_1B = (128, 350)
# H.264 Table A-2 cpbBrNalFactor, by profile_idc: the default BitRate that sets Rx.
AVC_NAL_FACTOR = {66: 1200, 77: 1200, 88: 1200, 100: 1500, 110: 3600, 122: 4800, 244: 4800, 44: 4800}

# H.265 Table A.8: general_level_idc -> ((MaxBR, MaxCPB) Main tier, (MaxBR, MaxCPB) High tier).
HEVC_LEVELS = {
    30: ((128, 350), None),
    60: ((1500, 1500), None),
    63: ((3000, 3000), None),
    90: ((6000, 6000), None),
    93: ((10000, 10000), None),
    120: ((12000, 12000), (30000, 30000)),
    123: ((20000, 20000), (50000, 50000)),
    150: ((25000, 25000), (100000, 100000)),
    153: ((40000, 40000), (160000, 160000)),
    156: ((60000, 60000), (240000, 240000)),
    180: ((60000, 60000), (240000, 240000)),
    183: ((120000, 120000), (480000, 480000)),
    186: ((240000, 240000), (800000, 800000)),
}
# H.265 Table A.9 CpbNalFactor for Main, Main 10 and Main Still Picture (profile_idc 1-3).
HEVC_NAL_FACTOR = 1100

# H.222.0 2.4.2.3: ADTS (Rx, BSn) by channels, the LFE not counted.
ADTS_BUFFERS = ((2, 2_000_000, 3584), (8, 5_529_600, 8976), (12, 8_294_400, 12804), (48, 33_177_600, 51216))
# channel_configuration -> full-bandwidth channels (5.1 and 7.1 drop the LFE).
ADTS_CHANNELS = {1: 1, 2: 2, 3: 3, 4: 4, 5: 5, 6: 5, 7: 7}
ADTS_RATES = (96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350)
# 2.4.2.3 "other audio".
AUDIO_RX = 2_000_000
MPEG_AUDIO_BS = 3584
# ATSC A/53 Part 5 5.7 (AC-3) and A/52 Annex G 3.6.1 (E-AC-3: 736 + 64 + 12096).
AC3_ATSC_BS = 2592
EAC3_ATSC_BS = 12896
# ATSC A/52 Annex A 5.4: AC-3 carried as DVB private data.
AC3_DVB_BS = 5696

MPEG_AUDIO_KBPS = {
    (1, 1): (0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448),
    (1, 2): (0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384),
    (1, 3): (0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320),
    (2, 1): (0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256),
    (2, 2): (0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160),
    (2, 3): (0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160),
}
# stream_type -> model, for the types identified by stream_type alone.
STREAM_KINDS = {
    0x1B: "avc",
    0x24: "hevc",
    0x0F: "adts",
    0x03: "mpeg-audio",
    0x04: "mpeg-audio",
    0x81: "ac3-atsc",
    0x87: "eac3-atsc",
}
AC3_KBPS = (32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640)


class Refused(Exception):
    """An elementary stream the model cannot grade: named, never skipped silently."""


@dataclass
class Params:
    """One elementary stream's T-STD buffers (bytes) and rates (bit/s)."""

    label: str
    rx: float
    # Video: MB and EB with the leak rate between them. Audio: the main buffer B alone.
    mb: float = 0.0
    eb: float = 0.0
    rbx: float = 0.0
    b: float = 0.0
    # tdn(j) - t(i) bound: 10 s for AVC and HEVC, 1 s otherwise (2.4.2.6, 2.14.3.1, 2.17.2).
    max_delay_s: float = 1.0

    @property
    def video(self) -> bool:
        return self.rbx > 0


@dataclass
class AccessUnit:
    """ES byte span [start, end), where its first byte arrived, and its decode time."""

    start: int
    end: int
    first_packet: int
    # 90 kHz DTS (or PTS) when the PES stamps it, else derived from the one before.
    stamp: int | None
    duration_s: float = 0.0


@dataclass
class Stream:
    """One elementary stream's packets and PES timestamps, and its ES bytes where needed."""

    pid: int
    stream_type: int
    # TSDuck's names for the PMT descriptors, plus "registration:<format_identifier>".
    descriptors: set[str]
    # (ts_index, PES header bytes, ES payload bytes, ES offset before this packet).
    packets: list[tuple[int, int, int, int]] = field(default_factory=list)
    # (ES offset, 90 kHz PTS, DTS or None, ts_index) per PES that carries a PTS.
    pes: list[tuple[int, int, int | None, int]] = field(default_factory=list)
    es: bytearray = field(default_factory=bytearray)
    es_len: int = 0


def read_programs(ts_path: str) -> list[tuple[int, list[Stream]]]:
    """(PCR PID, elementary streams) of each program's first PMT, as TSDuck decodes it."""
    proc = subprocess.run(
        ["tstables", ts_path, "--psi-si", "--tid", "2", "--json-output", "-"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"tstables failed: {proc.stderr.strip()}")
    programs: dict[int, tuple[int, list[Stream]]] = {}
    for pmt in json.loads(proc.stdout or "[]"):
        if pmt.get("service_id") in programs:
            continue
        streams = []
        for node in pmt.get("#nodes", []):
            if node.get("#name") != "component":
                continue
            descriptors = [d for d in node.get("#nodes", []) if isinstance(d, dict)]
            names = {d["#name"] for d in descriptors}
            names |= {f"registration:{d['format_identifier']}" for d in descriptors if "format_identifier" in d}
            streams.append(Stream(node["elementary_pid"], node["stream_type"], names))
        programs[pmt.get("service_id")] = (pmt["pcr_pid"], streams)
    return list(programs.values())


def _stamp(b: bytes) -> int:
    """A 33-bit PTS/DTS from its 5-byte PES encoding."""
    return ((b[0] >> 1) & 0x07) << 30 | b[1] << 22 | (b[2] >> 1) << 15 | b[3] << 7 | b[4] >> 1


def read_pes(data: bytes, packet_size: int, streams: dict[int, Stream], keep_es: set[int]) -> dict[int, str]:
    """Split each stream's packets into PES header and ES bytes, and note every PES timestamp.

    Packets before a PID's first PES start still count toward its TB, but their bytes
    belong to an access unit whose start was never captured, so they go no further.
    Returns why each stream that could not be read was refused.
    """
    refused: dict[int, str] = {}
    for index, offset in enumerate(range(0, len(data) - packet_size + 1, packet_size)):
        if data[offset] != 0x47:
            continue
        pid = ((data[offset + 1] & 0x1F) << 8) | data[offset + 2]
        stream = streams.get(pid)
        if stream is None or pid in refused:
            continue
        afc = (data[offset + 3] >> 4) & 0x3
        if afc not in (1, 3):
            continue
        start = offset + 4 + (1 + data[offset + 4] if afc == 3 else 0)
        length = offset + 188 - start
        header = 0
        if data[offset + 1] & 0x40:
            if data[start : start + 3] != b"\x00\x00\x01":
                refused[pid] = f"packet {index} starts a payload without a PES start code"
                continue
            if length < 9 or 9 + data[start + 8] > length:
                refused[pid] = f"the PES header at packet {index} spans packets"
                continue
            header = 9 + data[start + 8]
            flags = data[start + 7]
            if flags & 0x80:
                pts = _stamp(data[start + 9 : start + 14])
                dts = _stamp(data[start + 14 : start + 19]) if flags & 0x40 else None
                stream.pes.append((stream.es_len, pts, dts, index))
        if not stream.pes:
            stream.packets.append((index, 0, 0, 0))
            continue
        body = length - header
        stream.packets.append((index, header, body, stream.es_len))
        if pid in keep_es or len(stream.es) < 1 << 20:
            stream.es += data[start + header : start + length]
        stream.es_len += body
    return refused


def _nal_units(es: bytes, limit: int = 1 << 20):
    """Annex-B NAL units (without start codes) in the first `limit` bytes of `es`."""
    view = bytes(es[:limit])
    pos = view.find(b"\x00\x00\x01")
    while pos >= 0:
        nxt = view.find(b"\x00\x00\x01", pos + 3)
        yield view[pos + 3 : nxt if nxt >= 0 else len(view)]
        pos = nxt


def avc_params(es: bytes) -> Params:
    """H.222.0 2.14.3.1 buffers from the first SPS's profile and level, without VUI HRD."""
    for nal in _nal_units(es):
        if nal and nal[0] & 0x1F == 7 and len(nal) >= 4:
            profile, constraints, level = nal[1], nal[2], nal[3]
            break
    else:
        raise Refused("AVC stream carries no SPS")
    if (level == 11 and constraints & 0x10 and profile in (66, 77, 88)) or level == 9:
        max_br, max_cpb, name = *AVC_LEVEL_1B, "1b"
    elif level in AVC_LEVELS:
        max_br, max_cpb = AVC_LEVELS[level]
        name = f"{level // 10}.{level % 10}"
    else:
        raise Refused(f"AVC level_idc {level} has no Table A-1 entry")
    if profile not in AVC_NAL_FACTOR:
        raise Refused(f"AVC profile_idc {profile} has no Table A-2 cpbBrNalFactor")
    overhead = max(1200 * max_br, 2_000_000)
    return Params(
        label=f"AVC profile {profile} level {name}",
        rx=1.2 * AVC_NAL_FACTOR[profile] * max_br,
        mb=(0.004 + 1 / 750) * overhead / 8,
        eb=1200 * max_cpb / 8,
        rbx=1200 * max_br,
        max_delay_s=10.0,
    )


def hevc_params(es: bytes) -> Params:
    """H.222.0 2.17.2 buffers from the first SPS's tier and level, without VUI HRD."""
    for nal in _nal_units(es):
        if len(nal) >= 2 and (nal[0] >> 1) & 0x3F == 33:
            # Emulation prevention can land inside the 32 compatibility flags.
            rbsp = nal[2:40].replace(b"\x00\x00\x03", b"\x00\x00")
            if len(rbsp) >= 13:
                break
    else:
        raise Refused("HEVC stream carries no SPS")
    tier, profile, level = (rbsp[1] >> 5) & 1, rbsp[1] & 0x1F, rbsp[12]
    if profile not in (1, 2, 3):
        raise Refused(f"HEVC general_profile_idc {profile} has no CpbNalFactor here (Main, Main 10, Still only)")
    limits = HEVC_LEVELS.get(level, (None, None))[tier]
    if limits is None:
        raise Refused(f"HEVC level_idc {level} tier {tier} has no Table A.8 entry")
    max_br, max_cpb = limits
    rate = HEVC_NAL_FACTOR * max_br
    return Params(
        label=f"HEVC profile {profile} {'high' if tier else 'main'} tier level {level / 30:g}",
        rx=rate,
        mb=(0.004 + 1 / 750) * max(rate, 2_000_000) / 8,
        eb=HEVC_NAL_FACTOR * max_cpb / 8,
        rbx=rate,
        max_delay_s=10.0,
    )


def adts_frame(es: bytes, pos: int) -> tuple[int, float]:
    """(length, duration) of the ADTS frame at `pos`."""
    h = es[pos : pos + 7]
    if len(h) < 7 or h[0] != 0xFF or h[1] & 0xF0 != 0xF0:
        raise Refused(f"no ADTS sync at ES offset {pos}")
    rate_index = (h[2] >> 2) & 0x0F
    if rate_index >= len(ADTS_RATES):
        raise Refused(f"ADTS sampling_frequency_index {rate_index} at ES offset {pos}")
    length = ((h[3] & 0x03) << 11) | (h[4] << 3) | (h[5] >> 5)
    return length, 1024 * ((h[6] & 0x03) + 1) / ADTS_RATES[rate_index]


def mpeg_audio_frame(es: bytes, pos: int) -> tuple[int, float]:
    """(length, duration) of the MPEG-1/2 audio frame at `pos`."""
    h = es[pos : pos + 4]
    if len(h) < 4 or h[0] != 0xFF or h[1] & 0xE0 != 0xE0:
        raise Refused(f"no MPEG audio sync at ES offset {pos}")
    version_bits = (h[1] >> 3) & 0x03
    layer = 4 - ((h[1] >> 1) & 0x03)
    rate_index = (h[2] >> 2) & 0x03
    kbps_index = h[2] >> 4
    if version_bits == 1 or layer == 4 or rate_index == 3 or kbps_index in (0, 15):
        raise Refused(f"MPEG audio header at ES offset {pos} is reserved or free-format")
    # MPEG-1, MPEG-2 (half rate) and MPEG-2.5 (quarter rate).
    version = 1 if version_bits == 3 else 2
    rate = (44100, 48000, 32000)[rate_index] >> {3: 0, 2: 1, 0: 2}[version_bits]
    bits = MPEG_AUDIO_KBPS[(version, layer)][kbps_index] * 1000
    pad = (h[2] >> 1) & 0x01
    if layer == 1:
        return (12 * bits // rate + pad) * 4, 384 / rate
    samples = 576 if layer == 3 and version == 2 else 1152
    return samples // 8 * bits // rate + pad, samples / rate


def ac3_frame(es: bytes, pos: int) -> tuple[int, float]:
    """(length, duration) of the AC-3 or E-AC-3 frame at `pos`; 0 duration for a dependent substream."""
    h = es[pos : pos + 6]
    if len(h) < 6 or h[0] != 0x0B or h[1] != 0x77:
        raise Refused(f"no AC-3 sync at ES offset {pos}")
    if h[5] >> 3 > 10:  # bsid 11-16: E-AC-3 (A/52 Annex E)
        blocks = (1, 2, 3, 6)[(h[4] >> 4) & 0x03] if h[4] >> 6 != 3 else 6
        fscod = h[4] >> 6
        rate = (48000, 44100, 32000)[fscod] if fscod != 3 else (24000, 22050, 16000)[(h[4] >> 4) & 0x03]
        dependent = h[2] >> 6 == 1
        return ((((h[2] & 0x07) << 8) | h[3]) + 1) * 2, 0.0 if dependent else 256 * blocks / rate
    fscod, code = h[4] >> 6, h[4] & 0x3F
    if fscod == 3 or code >> 1 >= len(AC3_KBPS):
        raise Refused(f"AC-3 fscod/frmsizecod reserved at ES offset {pos}")
    kbps = AC3_KBPS[code >> 1]
    words = (2 * kbps, 320 * kbps // 147 + (code & 1), 3 * kbps)[fscod]
    return words * 2, 1536 / (48000, 44100, 32000)[fscod]


def stream_kind(stream: Stream) -> str | None:
    """Which T-STD model the stream takes, or None for one with no elementary stream buffers (sections, data)."""
    kind = stream.stream_type
    tags = stream.descriptors
    if kind in STREAM_KINDS:
        return STREAM_KINDS[kind]
    if kind == 0x06 and tags & {"DVB_AC3_descriptor", "AC3_descriptor"}:
        return "ac3-dvb"
    if kind == 0x06 and f"registration:{int.from_bytes(b'Opus', 'big')}" in tags:
        raise Refused("Opus in TS leaves the T-STD buffer size unspecified (ETSI draft, Rx only)")
    if kind == 0x06 and tags & {"DVB_enhanced_AC3_descriptor", "enhanced_AC3_descriptor"}:
        raise Refused("E-AC-3 as DVB private data takes its buffer from ETSI TS 101 154, not modelled")
    if kind in (0x01, 0x02, 0x10, 0x11, 0x1C, 0x20, 0x21, 0x42, 0xD1, 0xEA):
        raise Refused(f"stream_type 0x{kind:02X} is audio/video this model has no parameters for")
    return None


def stream_params(kind: str, es: bytes) -> tuple[Params, object]:
    """The stream's T-STD parameters, and its audio frame parser (None for video)."""
    if kind == "avc":
        return avc_params(es), None
    if kind == "hevc":
        return hevc_params(es), None
    if kind == "adts":
        adts_frame(es, 0)
        config = ((es[2] & 0x01) << 2) | (es[3] >> 6)
        if config not in ADTS_CHANNELS:
            raise Refused("ADTS channel_configuration 0 defers the layout to a PCE, which this model does not read")
        channels = ADTS_CHANNELS[config]
        rx, bs = next((rx, bs) for top, rx, bs in ADTS_BUFFERS if channels <= top)
        return Params(label=f"ADTS AAC {channels} ch", rx=rx, b=bs), adts_frame
    label, bs, parse = {
        "mpeg-audio": ("MPEG audio", MPEG_AUDIO_BS, mpeg_audio_frame),
        "ac3-atsc": ("AC-3 (ATSC)", AC3_ATSC_BS, ac3_frame),
        "eac3-atsc": ("E-AC-3 (ATSC)", EAC3_ATSC_BS, ac3_frame),
        "ac3-dvb": ("AC-3 (DVB)", AC3_DVB_BS, ac3_frame),
    }[kind]
    return Params(label=label, rx=AUDIO_RX, b=bs), parse


def access_units(stream: Stream, parse) -> list[AccessUnit]:
    """Video: one access unit per PES that carries a timestamp. Audio: one per codec frame."""
    units: list[AccessUnit] = []
    pes = stream.pes
    if parse is None:
        for n, (offset, pts, dts, first) in enumerate(pes):
            end = pes[n + 1][0] if n + 1 < len(pes) else stream.es_len
            units.append(AccessUnit(offset, end, first, dts if dts is not None else pts))
        return units
    # A PES timestamp belongs to the first frame that starts in that PES (2.4.3.7).
    starts = [(offset, index) for index, _h, body, offset in stream.packets if body]
    keys = [offset for offset, _ in starts]
    stamps = iter(pes)
    nxt = next(stamps, None)
    pos = 0
    # A header cut short by the end of the capture is not a malformed frame.
    while pos + 8 <= len(stream.es):
        length, duration = parse(stream.es, pos)
        if length <= 0:
            raise Refused(f"zero-length audio frame at ES offset {pos}")
        stamp = None
        while nxt is not None and nxt[0] <= pos:
            stamp = nxt[1]
            nxt = next(stamps, None)
        if duration == 0.0 and units:
            units[-1].end = pos + length
        else:
            first = starts[bisect.bisect_right(keys, pos) - 1][1]
            units.append(AccessUnit(pos, pos + length, first, stamp, duration))
        pos += length
    return units


@dataclass
class Grade:
    """What one elementary stream did in the model."""

    pid: int
    label: str
    violations: dict[str, int] = field(default_factory=dict)
    peaks: dict[str, float] = field(default_factory=dict)
    worst_late_ms: float = 0.0
    worst_delay_s: float = 0.0
    graded_units: int = 0

    def flag(self, name: str) -> None:
        self.violations[name] = self.violations.get(name, 0) + 1

    def peak(self, name: str, fill: float, size: float) -> None:
        self.peaks[name] = max(self.peaks.get(name, 0.0), fill / size)


def simulate(
    stream: Stream, params: Params, units: list[AccessUnit], segments: list[tuple[PcrClock, int, int]]
) -> Grade:
    """Run one elementary stream through its buffers, with fresh buffers for each time base.

    `segments` are (clock, first packet, end packet) per time base: the timestamps on
    either side of a signalled discontinuity are on different clocks.
    """
    grade = Grade(stream.pid, params.label)
    eps = 1e-9
    for clock, lo, hi in segments:
        # TB: packet i arrives over [t(i), t(i+1)] and leaves at Rx. `leave` is when its
        # last byte does; the bytes still in TB as the packet finishes arriving are its
        # peak, and a stretch where TB never empties may not exceed a second.
        deliveries: list[tuple[float, int, int, int]] = []
        leave = float("-inf")
        busy_since = None
        for index, header, body, offset in stream.packets:
            if not lo <= index < hi:
                continue
            start, end = clock.time_at(index), clock.time_at(index + 1)
            if leave <= start:
                busy_since = start
            leave = max(end, max(leave, start) + 188 * 8 / params.rx)
            fill = (leave - end) * params.rx / 8
            grade.peak("TB", fill, TB_SIZE)
            if fill > TB_SIZE + 0.5:
                grade.flag("TB overflow")
            if busy_since is not None and leave - busy_since > TB_EMPTY_S:
                grade.flag("TB not emptied within 1 s")
                busy_since = None
            if header or body:
                deliveries.append((leave, header, body, offset))

        horizon = clock.time_at(hi)
        removals: list[tuple[float, AccessUnit]] = []
        td = None
        for unit in units:
            if not lo <= unit.first_packet < hi:
                continue
            if unit.stamp is not None:
                base = unit.stamp / PTS_HZ
                stamped = base + round((clock.time_at(unit.first_packet) - base) / STAMP_WRAP_S) * STAMP_WRAP_S
                if removals and stamped < removals[-1][0]:
                    grade.flag("decode time goes backwards")
                td = max(stamped, removals[-1][0]) if removals else stamped
            elif td is None:
                continue  # an audio frame ahead of the first timestamp has no decoding time
            if td > horizon:
                break
            removals.append((td, unit))
            td += unit.duration_s

        # MB -> EB (video, leak method) or B (audio): walk deliveries and removals in time
        # order. EB/B is tracked by ES offset: everything below `into` has reached it and
        # everything below `out` has been removed, so an access unit whose bytes arrive
        # after its decoding time passes through as underflow rather than lingering as fill.
        into = out = deliveries[0][3] if deliveries else 0
        mb: list[list[int]] = []  # [header, payload] per delivered packet, FIFO
        mb_header = mb_payload = 0
        headers: list[tuple[int, int]] = []  # audio: (ES offset, bytes) of PES headers held in B
        b_header = 0
        now = float("-inf")
        late: list[tuple[int, float]] = []

        def settle(t: float, moved_from: int, rate: float) -> None:
            # Record how late each underflowed unit finished arriving.
            while late and into >= late[0][0]:
                end, td = late.pop(0)
                done = t if rate <= 0 else now + (end - moved_from) * 8 / rate
                grade.worst_late_ms = max(grade.worst_late_ms, (done - td) * 1000)

        def leak(until: float) -> None:
            nonlocal into, mb_header, mb_payload, now
            if not params.video or until <= now or not mb_payload:
                now = max(now, until)
                return
            room = params.eb - max(0, into - out) + max(0, out - into)
            amount = min(params.rbx * (until - now) / 8, mb_payload, room)
            before = into
            remaining = amount
            while remaining > eps and mb:
                head = mb[0]
                mb_header -= head[0]
                head[0] = 0
                take = min(head[1], remaining)
                head[1] -= take
                remaining -= take
                if head[1] <= eps:
                    mb.pop(0)
            mb_payload -= amount
            into += amount
            settle(until, before, params.rbx)
            grade.peak("EB", max(0, into - out), params.eb)
            now = until

        events = sorted(
            [(t, 1, n) for n, (t, *_rest) in enumerate(deliveries)] + [(t, 0, n) for n, (t, _u) in enumerate(removals)]
        )
        for t, kind, n in events:
            leak(t)
            if kind == 1:
                _t, header, body, offset = deliveries[n]
                if params.video:
                    mb.append([header, body])
                    mb_header += header
                    mb_payload += body
                    grade.peak("MB", mb_header + mb_payload, params.mb)
                    if mb_header + mb_payload > params.mb + 0.5:
                        grade.flag("MB overflow")
                else:
                    if header:
                        headers.append((offset, header))
                        b_header += header
                    into += body
                    settle(t, into, 0)
                    fill = max(0, into - out) + b_header
                    grade.peak("B", fill, params.b)
                    if fill > params.b + 0.5:
                        grade.flag("B overflow")
                continue
            _t, unit = removals[n]
            grade.graded_units += 1
            grade.worst_delay_s = max(grade.worst_delay_s, t - clock.time_at(unit.first_packet))
            if t - clock.time_at(unit.first_packet) > params.max_delay_s:
                grade.flag(f"held over {params.max_delay_s:g} s")
            if into + eps < unit.end:
                grade.flag("EB underflow" if params.video else "B underflow")
                late.append((unit.end, t))
            out = max(out, unit.end)
            while headers and headers[0][0] < unit.end:
                b_header -= headers.pop(0)[1]
    return grade


def check_tstd(ts_path: str, packet_size: int, scan: Scan) -> Check:
    """Full T-STD buffer model (ISO 13818-1 2.4.2) for every audio and video stream.

    Each program's streams run on that program's PCR, one time base at a time: a
    signalled discontinuity starts fresh buffers, since the timestamps before and
    after it are on different clocks.
    """
    with open(ts_path, "rb") as handle:
        data = handle.read()
    programs = read_programs(ts_path)

    grades: list[Grade] = []
    refused: dict[int, str] = {}
    skipped: list[int] = []
    for pcr_pid, streams in programs:
        kinds: dict[int, str] = {}
        for stream in streams:
            try:
                kind = stream_kind(stream)
            except Refused as err:
                refused[stream.pid] = str(err)
                continue
            if kind is None:
                skipped.append(stream.pid)
            else:
                kinds[stream.pid] = kind
        modelled = {s.pid: s for s in streams if s.pid in kinds}
        audio = {pid for pid, kind in kinds.items() if kind not in ("avc", "hevc")}
        for pid, why in read_pes(data, packet_size, modelled, audio).items():
            refused[pid] = why
            del modelled[pid]
        samples = scan.pcr_by_pid.get(pcr_pid, [])
        bounds = [0, *sorted(i for i, _ in samples if i in scan.pcr_new_base), scan.total_packets]
        segments = [(PcrClock([s for s in samples if lo <= s[0] < hi]), lo, hi) for lo, hi in zip(bounds, bounds[1:])]
        segments = [segment for segment in segments if segment[0].ok()]
        for stream in modelled.values():
            try:
                params, parse = stream_params(kinds[stream.pid], stream.es)
                units = access_units(stream, parse)
            except Refused as err:
                refused[stream.pid] = str(err)
                continue
            grades.append(simulate(stream, params, units, segments))

    metrics = {
        "streams": {
            str(g.pid): {
                "type": g.label,
                "access_units": g.graded_units,
                "violations": g.violations,
                "peak_fill_pct": {k: round(v * 100, 1) for k, v in g.peaks.items()},
                "worst_underflow_late_ms": round(g.worst_late_ms, 1),
                "worst_delay_s": round(g.worst_delay_s, 3),
            }
            for g in grades
        },
        "refused": {str(pid): why for pid, why in refused.items()},
        "not_elementary": skipped,
    }
    faults = [f"PID {g.pid} {name} x{count}" for g in grades for name, count in sorted(g.violations.items())]
    faults += [f"PID {pid} refused: {why}" for pid, why in refused.items()]
    if not grades and not refused:
        return Check("tstd", Severity.SHAPE, Status.WARN, "no audio/video stream to model", metrics)
    if not faults and not any(g.graded_units for g in grades):
        return Check("tstd", Severity.SHAPE, Status.WARN, "no access unit fell inside a modelled time base", metrics)
    if faults:
        return Check("tstd", Severity.SHAPE, Status.WARN, "; ".join(faults), metrics)
    peaks = ", ".join(f"PID {g.pid} " + "/".join(f"{k} {v * 100:.0f}%" for k, v in g.peaks.items()) for g in grades)
    return Check("tstd", Severity.SHAPE, Status.PASS, f"no overflow or underflow; peak fill {peaks}", metrics)


def detect_packet_size(analysis: dict) -> int:
    """188, or 204 when the stream carries the Reed-Solomon FEC trailer."""
    ts = analysis["ts"]
    total = ts["packets"]["total"]
    return round(ts["bytes"] / total) if total and ts["bytes"] // total in (188, 204) else 188


def pcr_span_seconds(scan: Scan) -> float:
    """Seconds between the first and last PCR on the PID that carries the most PCRs."""
    clocks = [PcrClock(samples) for samples in scan.pcr_by_pid.values()]
    main = max(clocks, key=lambda c: len(c.idx), default=PcrClock([]))
    return main.sec[-1] - main.sec[0] if main.ok() else 0.0


def source_duration(ts_path: str) -> float:
    """PCR span of a reference TS, used to pin the exported stream's absolute rate."""
    analysis = run_tsanalyze(ts_path)
    scan = scan_packets(ts_path, detect_packet_size(analysis))
    return pcr_span_seconds(scan)


def check_duration_fidelity(captured_s: float, reference_s: float) -> Check:
    """The exported stream's duration must track the source it was muxed from.

    Every timing check above reads the stream's own PCR, so a PCR emitted on the
    wrong clock rate stays internally consistent and passes them all. Comparing
    the exported PCR span against the source's independent duration is the one
    check that pins the absolute rate. Keyframe alignment drops up to a GOP of
    lead, so the captured span runs a shade short, never materially long; the
    band is wide enough to ignore that yet catch a gross scale error.
    """
    metrics = {"captured_s": round(captured_s, 2), "reference_s": round(reference_s, 2)}
    if reference_s <= 0:
        return Check("duration-fidelity", Severity.HARD, Status.WARN, "no reference duration", metrics)
    ratio = captured_s / reference_s
    metrics["ratio"] = round(ratio, 3)
    detail = f"captured {captured_s:.1f}s vs source {reference_s:.1f}s (ratio {ratio:.2f})"
    status = Status.PASS if 0.6 <= ratio <= 1.25 else Status.FAIL
    return Check("duration-fidelity", Severity.HARD, status, detail, metrics)


# ------------------------------------------------------------------------ driver


def analyze(ts_path: str, th: Thresholds, reference_seconds: float | None = None) -> list[Check]:
    """Run every check against `ts_path` and return the ordered results.

    `reference_seconds` (the source's PCR span, round-trip only) enables the
    duration-fidelity check that pins the exported stream's absolute rate.
    """
    analysis = run_tsanalyze(ts_path)
    ts = analysis["ts"]
    packet_size = detect_packet_size(analysis)

    scan = scan_packets(ts_path, packet_size)
    clock_by_pid = {pid: PcrClock(samples) for pid, samples in scan.pcr_by_pid.items()}
    # The reference clock is the PID with the most PCR samples (the PCR PID).
    main_clock = max(clock_by_pid.values(), key=lambda c: len(c.idx), default=PcrClock([]))

    # Nominal bitrate: total bytes clocked over the PCR span (the rate an IRD would
    # play the stream at). Self-consistent with the PCR clock used everywhere else,
    # and far more stable than tsanalyze's instantaneous PCR bitrate on a bursty
    # capture. Fall back to tsanalyze only when there is no usable PCR clock.
    span = main_clock.sec[-1] - main_clock.sec[0] if main_clock.ok() else 0.0
    if span > 0:
        ts_bitrate = scan.total_packets * scan.packet_size * 8 / span
    else:
        ts_bitrate = float(ts.get("bitrate") or ts.get("pcr-bitrate") or 0)

    checks: list[Check] = []
    checks.append(check_packet_size(analysis))
    checks.append(check_sync(analysis))
    pat, pmt = check_pat_pmt(analysis)
    checks.append(pat)
    checks.append(pmt)
    checks.append(check_psi_crc(analysis))
    checks.append(check_continuity(analysis))
    checks.append(check_pcr_presence(analysis, clock_by_pid))
    checks.append(check_pcr_monotonic(scan))
    if reference_seconds is not None:
        checks.append(check_duration_fidelity(span, reference_seconds))

    checks.append(check_service_descriptors(analysis))
    checks.append(check_pcr_repetition(scan, th))
    checks.append(check_pcr_jitter(scan, ts_bitrate, th))
    checks.append(check_null_ratio(analysis, th))

    if main_clock.ok():
        bitrate, burst = check_bitrate_and_burstiness(scan, main_clock, ts_bitrate, th)
        checks.append(bitrate)
        checks.append(burst)
        checks.append(check_inter_arrival(scan, main_clock))
    else:
        for name in ("bitrate-consistency", "burstiness", "inter-arrival"):
            checks.append(Check(name, Severity.SHAPE, Status.WARN, "not enough PCRs to build a clock", {}))
    checks.append(check_tstd(ts_path, packet_size, scan))
    return checks


def print_report(checks: list[Check]) -> None:
    """Print the PASS/WARN/FAIL summary table and per-check metrics to stdout."""
    width = max(len(c.name) for c in checks)
    print("=" * 72)
    print("TS / IRD compliance report")
    print("=" * 72)
    for c in checks:
        print(f"  {c.status.value:4}  {c.name.ljust(width)}  [{c.severity.value}]  {c.detail}")
    print("-" * 72)
    for c in checks:
        if c.metrics:
            print(f"  {c.name}: {json.dumps(c.metrics, separators=(',', ':'))}")
    print("=" * 72)


def verdict(checks: list[Check], strict: bool) -> int:
    """Exit code: hard failures always fail; shape failures only fail under --strict."""
    hard_fail = any(c.status == Status.FAIL and c.severity == Severity.HARD for c in checks)
    shape_issue = any(c.status in (Status.FAIL, Status.WARN) and c.severity == Severity.SHAPE for c in checks)
    if hard_fail:
        return 1
    if strict and shape_issue:
        return 1
    return 0


def build_thresholds(args: argparse.Namespace) -> Thresholds:
    """Assemble a Thresholds from parsed CLI arguments."""
    return Thresholds(
        pcr_repetition_ms=args.pcr_repetition_ms,
        pcr_jitter_us=args.pcr_jitter_us,
        null_ratio_max=args.null_ratio_max,
        bitrate_cov_max=args.bitrate_cov_max,
        burstiness_max=args.burstiness_max,
    )


def main() -> int:
    """CLI entry point: parse args, analyze the TS, print the report, return the exit code."""
    parser = argparse.ArgumentParser(description="MPEG-TS / IRD compliance analyzer")
    parser.add_argument("--ts", required=True, help="transport stream file to analyze")
    parser.add_argument("--strict", action="store_true", help="also fail on broadcast-shape warnings")
    parser.add_argument("--report-json", help="write the full report as JSON to this path")
    parser.add_argument(
        "--reference",
        help="source TS the capture was muxed from; enables the duration-fidelity check",
    )
    parser.add_argument("--pcr-repetition-ms", type=float, default=Thresholds.pcr_repetition_ms)
    parser.add_argument("--pcr-jitter-us", type=float, default=Thresholds.pcr_jitter_us)
    parser.add_argument("--null-ratio-max", type=float, default=Thresholds.null_ratio_max)
    parser.add_argument("--bitrate-cov-max", type=float, default=Thresholds.bitrate_cov_max)
    parser.add_argument("--burstiness-max", type=float, default=Thresholds.burstiness_max)
    args = parser.parse_args()

    th = build_thresholds(args)
    try:
        reference_seconds = source_duration(args.reference) if args.reference else None
        checks = analyze(args.ts, th, reference_seconds)
    except (RuntimeError, FileNotFoundError, json.JSONDecodeError) as err:
        print(f"error: {err}", file=sys.stderr)
        return 2

    print_report(checks)
    code = verdict(checks, args.strict)

    if args.report_json:
        report = {
            "ts": args.ts,
            "strict": args.strict,
            "exit_code": code,
            "checks": [
                {
                    "name": c.name,
                    "severity": c.severity.value,
                    "status": c.status.value,
                    "detail": c.detail,
                    "metrics": c.metrics,
                }
                for c in checks
            ],
        }
        with open(args.report_json, "w") as handle:
            json.dump(report, handle, indent=2)

    if code == 0:
        print("ts: PASS" + (" (strict)" if args.strict else ""))
    else:
        print("ts: FAIL", file=sys.stderr)
    return code


if __name__ == "__main__":
    sys.exit(main())
