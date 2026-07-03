#!/usr/bin/env python3
"""MPEG-TS / IRD compliance analyzer for a captured transport stream.

Given a `.ts` file (typically the output of `moq ... export ts`), this runs the
checks an Integrated Receiver/Decoder cares about and prints a PASS/WARN/FAIL
summary, exiting non-zero on failure.

Division of labour: TSDuck does the transport-stream parsing (we shell out to
`tsanalyze --json` for PSI/service/structure), and this script does the model
math TSDuck does not cover directly (PCR jitter/repetition, packet
inter-arrival, burstiness, instantaneous bitrate, and a transport-buffer model).
The PCR/PTS/DTS timeline the timing model needs comes from a 188/204-byte
packet-header scan done here, which also gives the per-packet PID that
`tsp -P pcrextract` does not expose.

Checks split into two severities:
  - HARD (structural): fail the run by default. PAT/PMT, packet size, sync,
    continuity counters, PSI CRC, PCR presence, PCR monotonicity.
  - SHAPE (broadcast profile): reported as WARN and only fail the run under
    `--strict`. PCR repetition interval, PCR jitter, null-packet ratio, bitrate
    consistency / burstiness, service descriptors (SDT), transport-buffer model.

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
    tb_size_bytes: int = 512
    video_leak_bps: float = 1.8e7
    audio_leak_bps: float = 2.0e6
    data_leak_bps: float = 1.0e6
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


def scan_packets(ts_path: str, packet_size: int) -> Scan:
    """Walk the TS packet by packet, recovering PID, payload size, and PCR values.

    This is a header-only scan (no PES/PSI parsing): enough for the timing and
    buffer models, which need per-packet PID and the PCR timeline.
    """
    with open(ts_path, "rb") as handle:
        data = handle.read()

    pid_packets: dict[int, list[tuple[int, int]]] = {}
    pcr_by_pid: dict[int, list[tuple[int, int]]] = {}
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
    """PCR must strictly increase per PID (a single 33-bit wrap is tolerated)."""
    breaks = 0
    for _pid, samples in scan.pcr_by_pid.items():
        prev = None
        for _i, pcr in samples:
            if prev is not None:
                delta = pcr - prev
                # A legitimate wrap shows as a large negative jump; anything else is a fault.
                if delta <= 0 and not delta < -PCR_WRAP / 2:
                    breaks += 1
            prev = pcr
    metrics = {"pcr_backwards": breaks}
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


def check_tstd(analysis: dict, scan: Scan, clock: PcrClock, th: Thresholds) -> Check:
    """Approximate T-STD transport-buffer check: TB fills on arrival, leaks at Rx.

    This models only the transport buffer (TB) smoothing stage of ISO 13818-1
    2.4.2, not the full multiplex/elementary buffer decode model. TB size is
    fixed at 512 bytes; the leak rate Rx defaults per stream type. An overflow
    means the stream delivers a PID's bytes faster than a receiver drains them.
    """
    kinds: dict[int, float] = {}
    for pid in analysis.get("pids", []):
        if pid.get("video"):
            kinds[pid["id"]] = th.video_leak_bps
        elif pid.get("audio"):
            kinds[pid["id"]] = th.audio_leak_bps

    overflows = 0
    worst_pid = None
    worst_occ = 0.0
    for pid, leak_bps in kinds.items():
        packets = scan.pid_packets.get(pid, [])
        occ = 0.0
        last_t = None
        pid_over = 0
        pid_peak = 0.0
        for i, payload in packets:
            t = clock.time_at(i)
            if last_t is not None and t > last_t:
                occ = max(0.0, occ - leak_bps * (t - last_t) / 8.0)
            occ += payload
            pid_peak = max(pid_peak, occ)
            if occ > th.tb_size_bytes:
                pid_over += 1
            last_t = t
        overflows += pid_over
        if pid_peak > worst_occ:
            worst_occ = pid_peak
            worst_pid = pid
    if not kinds:
        return Check("tstd", Severity.SHAPE, Status.WARN, "no video/audio PID to model", {})
    metrics = {
        "tb_overflows": overflows,
        "worst_pid": worst_pid,
        "worst_peak_bytes": round(worst_occ, 1),
        "tb_size_bytes": th.tb_size_bytes,
    }
    detail = f"TB overflows {overflows}, worst peak {worst_occ:.0f}B on PID {worst_pid} (TB {th.tb_size_bytes}B)"
    status = Status.PASS if overflows == 0 else Status.WARN
    return Check("tstd", Severity.SHAPE, status, detail, metrics)


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
        checks.append(check_tstd(analysis, scan, main_clock, th))
    else:
        for name in ("bitrate-consistency", "burstiness", "inter-arrival", "tstd"):
            checks.append(Check(name, Severity.SHAPE, Status.WARN, "not enough PCRs to build a clock", {}))
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
        tb_size_bytes=args.tb_size_bytes,
        video_leak_bps=args.video_leak_bps,
        audio_leak_bps=args.audio_leak_bps,
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
    parser.add_argument("--tb-size-bytes", type=int, default=Thresholds.tb_size_bytes)
    parser.add_argument("--video-leak-bps", type=float, default=Thresholds.video_leak_bps)
    parser.add_argument("--audio-leak-bps", type=float, default=Thresholds.audio_leak_bps)
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
