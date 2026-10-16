#!/usr/bin/env python3
"""MPEG-TS / IRD compliance analyzer for a captured transport stream.

Given a `.ts` file (typically the output of `moq ... export ts`), this runs the
checks an Integrated Receiver/Decoder cares about and prints a PASS/WARN/FAIL
summary, exiting non-zero on failure.

Division of labour: TSDuck does the transport-stream parsing (we shell out to
`tsanalyze --json` for PSI/service/structure and `tstables` for the PMT), and
this script does what TSDuck does not cover: the ISO 13818-1 T-STD buffer
model for supported elementary streams and each program's system buffers, and
the PCR checks that need the time-base discontinuities a 188/204-byte header
scan here recovers. PCR value intervals, byte schedule and release timing live
in `pcr-timing.py`.

Checks split into two severities:
  - HARD (structural): fail the run by default. PAT/PMT, packet size, sync,
    continuity counters, PSI CRC, PCR presence, PCR monotonicity, T-STD buffers,
    system buffers.
  - SHAPE (broadcast profile): reported as WARN and only fail the run under
    `--strict`. Service descriptors (SDT).

Timing basis is the stream's own PCR clock (an IRD locks to PCR), so the harness
needs no wall-clock capture and results are deterministic for a given file.
"""

from __future__ import annotations

import argparse
import bisect
import json
import subprocess
import sys
from collections import deque
from dataclasses import dataclass, field
from enum import Enum

# PCR runs on the 27 MHz system clock; PTS/DTS on the 90 kHz clock.
PCR_HZ = 27_000_000
PTS_HZ = 90_000
# The PCR base field is 33 bits at 90 kHz, so the full 27 MHz PCR wraps here.
PCR_WRAP = (1 << 33) * 300


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


# --------------------------------------------------------------------------- IO


def run_tsanalyze(ts_path: str) -> dict:
    """Return TSDuck's structural analysis as a dict, capturing stderr for CRC warnings."""
    proc = subprocess.run(
        ["tsanalyze", "--json", ts_path],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"tsanalyze failed: {proc.stderr.strip()}")
    data = json.loads(proc.stdout)
    data["_stderr"] = proc.stderr
    return data


@dataclass
class Scan:
    """Per-packet facts recovered from a raw header scan of the TS file."""

    total_packets: int
    # (ts_index, pcr_27mhz) samples for every PID that carries PCR.
    pcr_by_pid: dict[int, list[tuple[int, int]]]
    # ts_index of every PCR that starts a new time base (discontinuity_indicator).
    pcr_new_base: set[int]


def scan_packets(ts_path: str, packet_size: int) -> Scan:
    """Walk the TS packet by packet, recovering the PCR timeline.

    A header-only scan: the PCR samples and which of them start a new time base,
    which `tsp -P pcrextract` does not report.
    """
    with open(ts_path, "rb") as handle:
        data = handle.read()

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

        index += 1
        offset += packet_size

    return Scan(
        total_packets=index,
        pcr_by_pid=pcr_by_pid,
        pcr_new_base=pcr_new_base,
    )


# --------------------------------------------------------------- small helpers


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
        self._rsys: tuple[list[float], list[float]] | None = None

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

    def rsys_bits(self, t: float) -> float:
        """Bits Rsys (2.4.2.3) removes from the first PCR to `t`: each stretch between PCRs at
        the transport rate they give, and beyond the edge PCRs at the edge stretch's."""
        if self._rsys is None:
            idx, sec = self.idx, self.sec
            rates = [
                max(RSYS_MIN, 188 * 8 * (idx[k + 1] - idx[k]) / (sec[k + 1] - sec[k]) / 500)
                for k in range(len(idx) - 1)
            ]
            bits = [0.0]
            for k, rate in enumerate(rates):
                bits.append(bits[-1] + rate * (sec[k + 1] - sec[k]))
            self._rsys = (rates, bits)
        rates, bits = self._rsys
        k = min(max(bisect.bisect_right(self.sec, t) - 1, 0), len(rates) - 1)
        return bits[k] + (t - self.sec[k]) * rates[k]


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
    """Every program's declared PCR PID must carry PCR samples.

    PCR_PID 0x1FFF declares a program without one (2.4.4.9), so it needs none.
    """
    pcr_pids = sorted({s["pcr-pid"] for s in analysis.get("services", []) if s.get("pcr-pid") not in (None, 0x1FFF)})
    carrying = sorted(pid for pid, clock in clock_by_pid.items() if clock.idx)
    metrics = {"pcr_pids": pcr_pids, "pcr_carrying_pids": carrying}
    if not pcr_pids:
        return Check("pcr-presence", Severity.HARD, Status.FAIL, "no program declares a PCR PID", metrics)
    missing = [pid for pid in pcr_pids if pid not in carrying]
    if missing:
        return Check("pcr-presence", Severity.HARD, Status.FAIL, f"declared PCR PID {missing} carries no PCR", metrics)
    return Check("pcr-presence", Severity.HARD, Status.PASS, f"PCR on PID {pcr_pids}", metrics)


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
# ETSI EN 300 472 5: teletext's transport buffer is smaller than the 512 bytes of 2.4.2.3.
TELETEXT_TB_SIZE = 480
# 2.4.2.6: TB must empty at least once a second.
TB_EMPTY_S = 1.0
# 2.4.2.3: PIDs 0-3 and the program's PMT share one TBsys (TB_SIZE), which passes their
# bytes on at Rxsys into Bsys, emptied at Rsys = max(80 kb/s, transport rate / 500).
SYSTEM_RX = 1_000_000
BSYS_SIZE = 1536
RSYS_MIN = 80_000
# PTS/DTS are 33 bits at 90 kHz.
STAMP_WRAP_S = (1 << 33) / PTS_HZ
# EB fill is tracked by ES offset, past 2^28 B within hours, where a double resolves
# only ~6e-8 B, so an access unit counts as complete within a bit of its last byte.
BIT = 1 / 8

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
    tb: float = TB_SIZE
    # Video: MB and EB with the leak rate between them. Audio: the main buffer B alone.
    # Teletext: neither, only its transport buffer.
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
    # TSDuck's names for the PMT descriptors, plus "registration:<format_identifier>"
    # and, for one TSDuck has no name for, "tag<n>:<hex body>".
    descriptors: set[str]
    # (ts_index, PES header bytes, ES payload bytes, ES offset before this packet).
    packets: list[tuple[int, int, int, int]] = field(default_factory=list)
    # (ES offset, 90 kHz PTS or None, DTS or None, ts_index) per PES, from the first timestamped one.
    pes: list[tuple[int, int | None, int | None, int]] = field(default_factory=list)
    es: bytearray = field(default_factory=bytearray)
    es_len: int = 0
    # Video: the first SPS as TSDuck decodes it.
    sps: dict[str, str] = field(default_factory=dict)


def read_pmts(ts_path: str) -> list[dict]:
    """Every PMT in the file, as TSDuck decodes it."""
    proc = subprocess.run(
        ["tstables", ts_path, "--psi-si", "--tid", "2", "--json-output", "-"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"tstables failed: {proc.stderr.strip()}")
    return json.loads(proc.stdout or "[]")


def read_programs(ts_path: str) -> list[tuple[int, list[Stream]]]:
    """(PCR PID, elementary streams) of each program's first PMT, as TSDuck decodes it."""
    programs: dict[int, tuple[int, list[Stream]]] = {}
    for pmt in read_pmts(ts_path):
        if pmt.get("service_id") in programs:
            continue
        streams = []
        for node in pmt.get("#nodes", []):
            if node.get("#name") != "component":
                continue
            descriptors = [d for d in node.get("#nodes", []) if isinstance(d, dict)]
            names = {d["#name"] for d in descriptors}
            names |= {f"registration:{d['format_identifier']}" for d in descriptors if "format_identifier" in d}
            names |= {"hrd_management_valid" for d in descriptors if d.get("hrd_management_valid") is True}
            names |= {
                f"tag{d['tag']}:" + "".join(d.get("#nodes", [])).replace(" ", "").lower()
                for d in descriptors
                if d["#name"] == "generic_descriptor"
            }
            streams.append(Stream(node["elementary_pid"], node["stream_type"], names))
        programs[pmt.get("service_id")] = (pmt["pcr_pid"], streams)
    return list(programs.values())


def _stamp(b: bytes) -> int:
    """A 33-bit PTS/DTS from its 5-byte PES encoding."""
    return ((b[0] >> 1) & 0x07) << 30 | b[1] << 22 | (b[2] >> 1) << 15 | b[3] << 7 | b[4] >> 1


def read_pes(data: bytes, packet_size: int, streams: dict[int, Stream]) -> dict[int, str]:
    """Split each stream's packets into PES header and ES bytes, and note every PES start.

    Every packet on the PID enters TB (H.222.0 2.4.2.3), but only PES bytes go on to
    MB/B. So an adaptation-only packet (a PCR, stuffing) costs TB and nothing after it,
    as does a duplicate (2.4.3.3: the same counter and payload twice), which "is not
    delivered" downstream. Packets before a PID's first timestamped PES likewise stop at
    TB: their bytes belong to an access unit whose start was never captured.
    Returns why each stream that could not be read was refused.
    """
    refused: dict[int, str] = {}
    last: dict[int, tuple[int, bytes]] = {}  # PID -> (continuity_counter, payload)
    for index, offset in enumerate(range(0, len(data) - packet_size + 1, packet_size)):
        if data[offset] != 0x47:
            continue
        pid = ((data[offset + 1] & 0x1F) << 8) | data[offset + 2]
        stream = streams.get(pid)
        if stream is None or pid in refused:
            continue
        afc = (data[offset + 3] >> 4) & 0x3
        start = offset + 4 + (1 + data[offset + 4] if afc & 0x2 else 0)
        payload = data[start : offset + 188] if afc & 0x1 else b""
        cc = data[offset + 3] & 0x0F
        duplicate = bool(payload) and last.get(pid) == (cc, payload)
        if payload:
            last[pid] = (cc, payload)
        if not payload or duplicate or not (stream.pes or data[offset + 1] & 0x40):
            stream.packets.append((index, 0, 0, stream.es_len))
            continue
        length = len(payload)
        header = 0
        if data[offset + 1] & 0x40:
            if payload[:3] != b"\x00\x00\x01":
                refused[pid] = f"packet {index} starts a payload without a PES start code"
                continue
            if length < 9 or 9 + payload[8] > length:
                refused[pid] = f"the PES header at packet {index} spans packets"
                continue
            header = 9 + payload[8]
            flags = payload[7]
            pts = _stamp(payload[9:14]) if flags & 0x80 else None
            dts = _stamp(payload[14:19]) if flags & 0xC0 == 0xC0 else None
            if pts is None and not stream.pes:
                stream.packets.append((index, 0, 0, 0))
                continue
            stream.pes.append((stream.es_len, pts, dts, index))
        body = length - header
        stream.packets.append((index, header, body, stream.es_len))
        stream.es += payload[header:]
        stream.es_len += body
    return refused


def read_sps(ts_path: str, pid: int, nal_type: int) -> dict[str, str]:
    """The first SPS on `pid`, field by field, as TSDuck's `pes` plugin decodes it."""
    proc = subprocess.run(
        ["tsp", "-I", "file", ts_path, "-P", "pes", "--pid", str(pid), "--avc-access-unit"]
        + ["--nal-unit-type", str(nal_type), "--max-dump-count", "1", "-O", "drop"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"tsp -P pes failed: {proc.stderr.strip()}")
    fields = {}
    for line in (proc.stdout + proc.stderr).splitlines():
        key, eq, value = line.strip().partition(" = ")
        # A key repeats once per CPB schedule; the last is the one H.222.0 sizes EB from.
        if eq:
            fields[key] = value
    if not fields:
        raise Refused("the stream carries no SPS")
    return fields


def _hrd(sps: dict[str, str], prefix: str, value: str) -> tuple[float, float] | None:
    """(BitRate bit/s, CpbSize bits) of the highest-numbered CPB a NAL HRD declares.

    H.264 E.2.2 and H.265 E.3.3: BitRate = (bit_rate_value_minus1 + 1) * 2^(6 + bit_rate_scale)
    and CpbSize = (cpb_size_value_minus1 + 1) * 2^(4 + cpb_size_scale). H.222.0 2.14.3.1
    sizes EB from CpbSize[cpb_cnt_minus1], so the last schedule is the one that counts.
    """
    rates = [int(v) for k, v in sps.items() if k.startswith(prefix) and "bit_rate_value_minus1" in k]
    sizes = [int(v) for k, v in sps.items() if k.startswith(prefix) and "cpb_size_value_minus1" in k]
    if not rates or not sizes:
        return None
    rate = (rates[-1] + 1) << (6 + int(sps[f"{value}bit_rate_scale"]))
    size = (sizes[-1] + 1) << (4 + int(sps[f"{value}cpb_size_scale"]))
    return rate, size


def avc_params(sps: dict[str, str]) -> Params:
    """H.222.0 2.14.3.1 buffers from the SPS: level limits, overridden by a declared NAL HRD.

    Only the NAL HRD counts: EB sizes the byte stream, which a VCL HRD does not describe,
    so a stream declaring only a VCL HRD takes the level defaults (H.264 E.2.2).
    """
    profile, level = int(sps["profile_idc"]), int(sps["level_idc"])
    if (level == 11 and sps.get("constraint_set3_flag") == "1" and profile in (66, 77, 88)) or level == 9:
        max_br, max_cpb, name = *AVC_LEVEL_1B, "1b"
    elif level in AVC_LEVELS:
        max_br, max_cpb = AVC_LEVELS[level]
        name = f"{level // 10}.{level % 10}"
    else:
        raise Refused(f"AVC level_idc {level} has no Table A-1 entry")
    if profile not in AVC_NAL_FACTOR:
        raise Refused(f"AVC profile_idc {profile} has no Table A-2 cpbBrNalFactor")
    # Absent a NAL HRD, BitRate is cpbBrNalFactor * MaxBR (H.264 E.2.2) and cpb_size is
    # 1200 * MaxCPB (H.222.0 2.14.3.1).
    declared = (
        _hrd(sps, "vui.nal_hrd.", "vui.nal_hrd.") if sps.get("vui.nal_hrd_parameters_present_flag") == "1" else None
    )
    bit_rate, cpb = declared or (AVC_NAL_FACTOR[profile] * max_br, 1200 * max_cpb)
    overhead = (0.004 + 1 / 750) * max(1200 * max_br, 2_000_000)
    return Params(
        label=f"AVC profile {profile} level {name}, "
        + (f"NAL HRD {bit_rate / 1e6:.3f} Mb/s cpb {cpb / 1e3:.0f} kbit" if declared else "level defaults"),
        # H.222.0 2.14.3.1: Rx = 1.2 * BitRate[SchedSelIdx]; MBS = BSmux + BSoh + 1200 *
        # MaxCPB - cpb_size; EBS = cpb_size; Rbx = 1200 * MaxBR whatever the HRD declares.
        rx=1.2 * bit_rate,
        mb=(overhead + 1200 * max_cpb - cpb) / 8,
        eb=cpb / 8,
        rbx=1200 * max_br,
        max_delay_s=10.0,
    )


def hevc_params(sps: dict[str, str]) -> Params:
    """H.222.0 2.17.2 buffers from the SPS: tier and level limits, overridden by a declared NAL HRD."""
    prefix = "profile_tier_level.general_"
    tier, profile, level = (int(sps[f"{prefix}{k}"]) for k in ("tier_flag", "profile_idc", "level_idc"))
    if profile not in (1, 2, 3):
        raise Refused(f"HEVC general_profile_idc {profile} has no CpbNalFactor here (Main, Main 10, Still only)")
    limits = HEVC_LEVELS.get(level, (None, None))[tier]
    if limits is None:
        raise Refused(f"HEVC level_idc {level} tier {tier} has no Table A.8 entry")
    max_br, max_cpb = limits
    declared = (
        _hrd(sps, "vui.hrd.nal_hrd_parameters", "vui.hrd.")
        if sps.get("vui.hrd.nal_hrd_parameters_present_flag") == "1"
        else None
    )
    # Absent a NAL HRD, BitRate is CpbBrVclFactor * MaxBR (H.265 E.3.3), which H.222.0 then
    # scales by CpbBrNalFactor / CpbBrVclFactor; cpb_size is CpbBrNalFactor * MaxCPB.
    bit_rate, cpb = declared or (1000 * max_br, HEVC_NAL_FACTOR * max_cpb)
    rate = HEVC_NAL_FACTOR * max_br
    return Params(
        label=f"HEVC profile {profile} {'high' if tier else 'main'} tier level {level / 30:g}, "
        + (f"NAL HRD {bit_rate / 1e6:.3f} Mb/s cpb {cpb / 1e3:.0f} kbit" if declared else "level defaults"),
        # H.222.0 2.17.2: Rx = CpbBrNalFactor / CpbBrVclFactor * BitRate[SchedSelIdx]; MBS =
        # BSmux + BSoh + CpbBrNalFactor * MaxCPB - cpb_size; EBS = cpb_size; Rbx =
        # CpbBrNalFactor * MaxBR.
        rx=HEVC_NAL_FACTOR / 1000 * bit_rate,
        mb=((0.004 + 1 / 750) * max(rate, 2_000_000) + HEVC_NAL_FACTOR * max_cpb - cpb) / 8,
        eb=cpb / 8,
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


def opus_frame(es: bytes, pos: int) -> tuple[int, float]:
    """(length, duration) of the Opus-in-TS access unit at `pos`: control header, then one packet."""
    if es[pos] != 0x7F or es[pos + 1] & 0xE0 != 0xE0:
        raise Refused(f"no Opus control header at ES offset {pos}")
    flags = es[pos + 1]
    at = pos + 2
    size = 0
    while True:
        size += es[at]
        at += 1
        if es[at - 1] != 0xFF:
            break
    at += 2 * bool(flags & 0x10) + 2 * bool(flags & 0x08)
    if flags & 0x04:
        at += 1 + es[at]
    # RFC 6716 3.1: the TOC byte's config sets the frame size, its code the frame count.
    config, code = es[at] >> 3, es[at] & 0x03
    if config < 12:
        frame_ms = (10, 20, 40, 60)[config % 4]
    elif config < 16:
        frame_ms = (10, 20)[config % 2]
    else:
        frame_ms = (2.5, 5, 10, 20)[config % 4]
    frames = (1, 2, 2, es[at + 1] & 0x3F)[code]
    return at - pos + size, frame_ms * frames / 1000


def stream_kind(stream: Stream) -> str | None:
    """Which T-STD model the stream takes, or None for one with no elementary stream buffers (sections, data)."""
    kind = stream.stream_type
    tags = stream.descriptors
    if kind in STREAM_KINDS:
        return STREAM_KINDS[kind]
    if kind == 0x06 and tags & {"DVB_AC3_descriptor", "AC3_descriptor"}:
        return "ac3-dvb"
    if kind == 0x06 and "teletext_descriptor" in tags:
        return "teletext"
    if kind == 0x06 and f"registration:{int.from_bytes(b'Opus', 'big')}" in tags:
        return "opus"
    if kind == 0x06 and tags & {"DVB_enhanced_AC3_descriptor", "enhanced_AC3_descriptor"}:
        raise Refused("E-AC-3 as DVB private data takes its buffer from ETSI TS 101 154, not modelled")
    if kind in (0x01, 0x02, 0x10, 0x11, 0x1C, 0x20, 0x21, 0x42, 0xD1, 0xEA):
        raise Refused(f"stream_type 0x{kind:02X} is audio/video this model has no parameters for")
    return None


def stream_params(kind: str, stream: Stream) -> tuple[Params, object]:
    """The stream's T-STD parameters, and its audio frame parser (None for video)."""
    es = stream.es
    if kind == "avc":
        return avc_params(stream.sps), None
    if kind == "hevc":
        return hevc_params(stream.sps), None
    if kind == "teletext":
        # ETSI EN 300 472 5: a 480-byte transport buffer drained at 6.75 Mb/s.
        return Params(label="teletext", rx=6_750_000, tb=TELETEXT_TB_SIZE), None
    if kind == "adts":
        adts_frame(es, 0)
        config = ((es[2] & 0x01) << 2) | (es[3] >> 6)
        if config not in ADTS_CHANNELS:
            raise Refused("ADTS channel_configuration 0 defers the layout to a PCE, which this model does not read")
        channels = ADTS_CHANNELS[config]
        rx, bs = next((rx, bs) for top, rx, bs in ADTS_BUFFERS if channels <= top)
        return Params(label=f"ADTS AAC {channels} ch", rx=rx, b=bs), adts_frame
    if kind == "opus":
        # Borrowed: the Opus-in-TS draft gives Rx (2 Mb/s for 1-2 channels, as here) but no
        # buffer size, so Opus is graded against ADTS's buffers for the same channel count.
        config = next((tag[len("tag127:80") :][:2] for tag in stream.descriptors if tag.startswith("tag127:80")), "")
        if not config or not 0 <= int(config, 16) <= 8:
            raise Refused("Opus without a channel_config_code of 0-8 in its extension descriptor")
        channels = int(config, 16) or 2  # 0 is dual mono
        rx, bs = next((rx, bs) for top, rx, bs in ADTS_BUFFERS if channels <= top)
        return Params(label=f"Opus {channels} ch (ADTS buffers)", rx=rx, b=bs), opus_frame
    label, bs, parse = {
        "mpeg-audio": ("MPEG audio", MPEG_AUDIO_BS, mpeg_audio_frame),
        "ac3-atsc": ("AC-3 (ATSC)", AC3_ATSC_BS, ac3_frame),
        "eac3-atsc": ("E-AC-3 (ATSC)", EAC3_ATSC_BS, ac3_frame),
        "ac3-dvb": ("AC-3 (DVB)", AC3_DVB_BS, ac3_frame),
    }[kind]
    return Params(label=label, rx=AUDIO_RX, b=bs), parse


# NAL unit types that open a new access unit when they follow the last VCL NAL unit
# of the previous one (H.264 7.4.1.2.3, H.265 7.4.2.4.4), and the VCL types.
AU_OPENERS = {"avc": {6, 7, 8, 9, 14, 15, 16, 17, 18}, "hevc": {32, 33, 34, 35, 39, 41, 42, 43, 44, *range(48, 56)}}
VCL = {"avc": set(range(1, 6)), "hevc": set(range(0, 32))}
DELIMITER = {"avc": 9, "hevc": 35}


def video_units(stream: Stream, kind: str) -> list[AccessUnit]:
    """One access unit per coded picture, each decoded at its PES's DTS (or PTS).

    An access unit opens at the first delimiter, parameter set or SEI after the previous
    picture's last slice (H.264 7.4.1.2.3, H.265 7.4.2.4.4); H.222.0 2.14.1 and 2.17.1
    also put a delimiter in every one, so a stream without them is refused. A PES may
    carry several access units, but only the first takes its timestamp; the rest would
    need decoding times derived from the stream's own timing (2.4.2.3), which this model
    does not do, so such a layout is refused rather than graded as one unit.
    """
    es = bytes(stream.es)
    starts = []
    delimited = False
    after_vcl = True  # the first opener starts the first whole access unit
    pos = es.find(b"\x00\x00\x01")
    while 0 <= pos < len(es) - 3:
        nal = (es[pos + 3] >> 1) & 0x3F if kind == "hevc" else es[pos + 3] & 0x1F
        delimited |= nal == DELIMITER[kind]
        if nal in AU_OPENERS[kind] and after_vcl:
            # The zero_byte ahead of the start code belongs to this NAL unit (H.264 B.1.1,
            # H.265 B.2.1), so to this access unit rather than the last.
            starts.append(pos - 1 if pos and es[pos - 1] == 0 else pos)
            after_vcl = False
        elif nal in VCL[kind]:
            after_vcl = True
        pos = es.find(b"\x00\x00\x01", pos + 3)
    if not delimited:
        raise Refused("no access unit delimiter, which H.222.0 2.14.1/2.17.1 requires in every access unit")
    pes = stream.pes
    pes_at = [offset for offset, *_ in pes]
    packet_at = [offset for _i, _h, body, offset in stream.packets if body]
    packets = [index for index, _h, body, _o in stream.packets if body]
    units: list[AccessUnit] = []
    stamped: set[int] = set()
    for n, start in enumerate(starts):
        k = bisect.bisect_right(pes_at, start) - 1
        _offset, pts, dts, index = pes[k]
        if pts is None:
            raise Refused(f"an access unit starts in the untimestamped PES at packet {index}")
        if k in stamped:
            raise Refused(f"the PES at packet {index} carries several access units, whose decode times are not derived")
        stamped.add(k)
        end = starts[n + 1] if n + 1 < len(starts) else stream.es_len
        first = packets[bisect.bisect_right(packet_at, start) - 1]
        units.append(AccessUnit(start, end, first, dts if dts is not None else pts))
    return units


def access_units(stream: Stream, parse) -> list[AccessUnit]:
    """Audio: one access unit per codec frame."""
    units: list[AccessUnit] = []
    pes = [entry for entry in stream.pes if entry[1] is not None]
    # A PES timestamp belongs to the first frame that starts in that PES (2.4.3.7).
    starts = [(offset, index) for index, _h, body, offset in stream.packets if body]
    keys = [offset for offset, _ in starts]
    stamps = iter(pes)
    nxt = next(stamps, None)
    pos = 0
    # A header cut short by the end of the capture is not a malformed frame.
    while pos + 8 <= len(stream.es):
        try:
            length, duration = parse(stream.es, pos)
        except IndexError:
            break  # a header that runs past the end of the capture
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
    truncated_units: int = 0

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
    drain = 8 / params.rx  # seconds for one byte to leave TB
    indices = [entry[0] for entry in stream.packets]
    for clock, lo, hi in segments:
        packets = stream.packets[bisect.bisect_left(indices, lo) : bisect.bisect_left(indices, hi)]
        # TB: packet i arrives over [t(i), t(i+1)], a byte every `pace`, and drains at Rx
        # once the packet ahead of it has (at `free`), so its byte j (0-187) leaves at
        # max(t(i) + (j+1) pace, free + (j+1) drain). The bytes still in TB as the packet
        # finishes arriving are its peak, and a stretch where TB never empties may not
        # exceed a second.
        deliveries: list[tuple[float, int, int, int, float, float, float]] = []
        leave = float("-inf")
        busy_since = None
        for index, header, body, offset in packets:
            start, end = clock.time_at(index), clock.time_at(index + 1)
            if leave <= start:
                busy_since = start
            free = max(leave, start)
            leave = max(end, free + 188 * drain)
            fill = (leave - end) * params.rx / 8
            grade.peak("TB", fill, params.tb)
            if fill > params.tb + 0.5:
                grade.flag("TB overflow")
            if busy_since is not None and leave - busy_since > TB_EMPTY_S:
                grade.flag("TB not emptied within 1 s")
                busy_since = None
            if header or body:
                deliveries.append((leave, header, body, offset, start, (end - start) / 188, free))
        carrying = [d for d in deliveries if d[2]]
        carried_at = [d[3] for d in carrying]

        def arrival(pos: int) -> float | None:
            # When ES byte `pos` leaves TB: PES payload ends each packet, so it is byte
            # 188 - body + (pos - offset) of the one that carries it.
            k = bisect.bisect_right(carried_at, pos) - 1
            if k < 0 or pos >= carried_at[k] + carrying[k][2]:
                return None
            _leave, _header, body, offset, start, pace, free = carrying[k]
            j = 188 - body + pos - offset
            return max(start + (j + 1) * pace, free + (j + 1) * drain)

        def left_by(delivery: tuple, t: float) -> int:
            # How many of a packet's 188 bytes have left TB by `t`, inverting the above.
            _leave, _header, _body, _offset, start, pace, free = delivery
            k = min((t - start) / pace if pace > 0 else 188, (t - free) / drain)
            return min(188, max(0, int(k + eps)))

        # Once the stream's last packet is in, every unit it completed is graded through
        # its decoding time, however long after the capture that falls. Before a signalled
        # discontinuity, the next time base takes over.
        last = not indices or indices[-1] < hi
        horizon = clock.time_at(hi)
        removals: list[tuple[float, AccessUnit]] = []
        td = None
        for unit in units:
            if not lo <= unit.first_packet < hi:
                continue
            if unit.end > stream.es_len:
                # Cut off by the end of the capture: bytes never received, not late ones.
                grade.truncated_units += 1
                break
            if unit.stamp is not None:
                base = unit.stamp / PTS_HZ
                stamped = base + round((clock.time_at(unit.first_packet) - base) / STAMP_WRAP_S) * STAMP_WRAP_S
                if removals and stamped <= removals[-1][0]:
                    grade.flag("decode time does not advance")
                td = max(stamped, removals[-1][0]) if removals else stamped
            elif td is None:
                continue  # an audio frame ahead of the first timestamp has no decoding time
            if td > horizon and not last:
                break
            removals.append((td, unit))
            td += unit.duration_s

        # MB -> EB (video, leak method) or B (audio): walk deliveries and removals in time
        # order. EB/B is tracked by ES offset: everything below `into` has reached it and
        # everything below `out` has been removed, so an access unit whose bytes arrive
        # after its decoding time passes through as underflow rather than lingering as fill.
        # Audio bytes enter B as they leave TB (`arrival`), so B peaks either as a packet
        # finishes or just before a removal, with part of the next packet already in.
        into = out = deliveries[0][3] if deliveries else 0
        delivered = into  # ES offset past the last payload byte to reach MB
        mb: deque[list[float]] = deque()  # [header, payload] per delivered packet, FIFO
        mb_header = 0
        mb_payload = 0.0
        headers: deque[tuple[int, int]] = deque()  # audio: (ES offset, bytes) of PES headers held in B
        b_header = 0
        now = float("-inf")
        late: deque[tuple[int, float]] = deque()  # video: (ES end, decoding time) of underflowed units
        taken = 0  # deliveries walked so far

        def check_b(fill: float) -> None:
            grade.peak("B", fill, params.b)
            if fill > params.b + 0.5:
                grade.flag("B overflow")

        def leak(until: float) -> None:
            nonlocal into, mb_header, mb_payload, now
            if not params.video or until <= now or not mb_payload:
                now = max(now, until)
                return
            room = params.eb - max(0, into - out) + max(0, out - into)
            amount = min(params.rbx * (until - now) / 8, room)
            moved_from = into
            if amount >= mb_payload:
                # MB empties: land on the exact offset, so float error cannot build up.
                amount = mb_payload
                mb.clear()
                mb_header = 0
                mb_payload = 0.0
                into = delivered
            else:
                remaining = amount
                while remaining > eps and mb:
                    head = mb[0]
                    mb_header -= head[0]
                    head[0] = 0
                    take = min(head[1], remaining)
                    head[1] -= take
                    remaining -= take
                    if head[1] <= eps:
                        mb.popleft()
                mb_payload -= amount
                into += amount
            # Record how late each underflowed unit finished arriving.
            while late and into + BIT >= late[0][0]:
                end, deadline = late.popleft()
                done = now + (end - moved_from) * 8 / params.rbx
                grade.worst_late_ms = max(grade.worst_late_ms, (done - deadline) * 1000)
            grade.peak("EB", max(0, into - out), params.eb)
            now = until

        events = sorted(
            [(t, 1, n) for n, (t, *_rest) in enumerate(deliveries)] + [(t, 0, n) for n, (t, _u) in enumerate(removals)]
        )
        for t, kind, n in events:
            leak(t)
            if kind == 1:
                taken += 1
                _t, header, body, offset, *_timing = deliveries[n]
                if params.video:
                    mb.append([header, body])
                    mb_header += header
                    mb_payload += body
                    delivered = offset + body
                    grade.peak("MB", mb_header + mb_payload, params.mb)
                    if mb_header + mb_payload > params.mb + 0.5:
                        grade.flag("MB overflow")
                elif params.b:
                    # A header ahead of a unit already decoded left B with it.
                    if header and offset >= out:
                        headers.append((offset, header))
                        b_header += header
                    into += body
                    check_b(max(0, into - out) + b_header)
                continue
            _t, unit = removals[n]
            grade.graded_units += 1
            grade.worst_delay_s = max(grade.worst_delay_s, t - clock.time_at(unit.first_packet))
            if t - clock.time_at(unit.first_packet) > params.max_delay_s:
                grade.flag(f"held over {params.max_delay_s:g} s")
            if params.video:
                if into + BIT < unit.end:
                    grade.flag("EB underflow")
                    late.append((unit.end, t))
            else:
                # What of the next packet is already in B: its PES header, then payload.
                partial = 0
                if taken < len(deliveries):
                    _leave, header, body, offset, *_timing = deliveries[taken]
                    k = left_by(deliveries[taken], t)
                    partial = max(0, k - (188 - body))
                    if offset >= out:  # else the header left B with the unit after it
                        partial += max(0, min(header, k - (188 - body - header)))
                check_b(max(0, into - out) + b_header + partial)
                done = arrival(unit.end - 1)
                if done is None or done > t + eps:
                    grade.flag("B underflow")
                    if done is not None:
                        grade.worst_late_ms = max(grade.worst_late_ms, (done - t) * 1000)
            out = max(out, unit.end)
            while headers and headers[0][0] < unit.end:
                b_header -= headers.popleft()[1]
    return grade


def time_bases(scan: Scan, pcr_pid: int) -> list[tuple[PcrClock, int, int]]:
    """(clock, first packet, end packet) of each time base on `pcr_pid` with a timeline."""
    samples = scan.pcr_by_pid.get(pcr_pid, [])
    bounds = [0, *sorted(i for i, _ in samples if i in scan.pcr_new_base), scan.total_packets]
    segments = [(PcrClock([s for s in samples if lo <= s[0] < hi]), lo, hi) for lo, hi in zip(bounds, bounds[1:])]
    return [segment for segment in segments if segment[0].ok()]


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
        for pid, why in read_pes(data, packet_size, modelled).items():
            refused[pid] = why
            del modelled[pid]
        segments = time_bases(scan, pcr_pid)
        for stream in modelled.values():
            try:
                if kinds[stream.pid] in ("avc", "hevc"):
                    if "hrd_management_valid" in stream.descriptors:
                        raise Refused("the HRD-scheduled MB to EB transfer (H.222.0 2.14.3.1) is not modelled")
                    stream.sps = read_sps(ts_path, stream.pid, 7 if kinds[stream.pid] == "avc" else 33)
                params, parse = stream_params(kinds[stream.pid], stream)
                if kinds[stream.pid] == "teletext":
                    units = []
                elif parse is None:
                    units = video_units(stream, kinds[stream.pid])
                else:
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
                "truncated_units": g.truncated_units,
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
        return Check("tstd", Severity.HARD, Status.FAIL, "; ".join(faults), metrics)
    peaks = ", ".join(f"PID {g.pid} " + "/".join(f"{k} {v * 100:.0f}%" for k, v in g.peaks.items()) for g in grades)
    return Check("tstd", Severity.HARD, Status.PASS, f"no overflow or underflow; peak fill {peaks}", metrics)


def is_duplicate(last: bytes | None, packet: bytes) -> bool:
    """Whether `packet` repeats `last`, the packet before it on its PID (2.4.3.3): byte for
    byte but for a PCR or OPCR, which a duplicate may refresh. As moq-mux's health.rs."""
    if last is None or not packet[3] & 0x10:
        return False
    if last == packet:
        return True
    # Matching the header through the adaptation flags pins every byte before the clocks.
    if not last[3] & 0x20 or last[:6] != packet[:6]:
        return False
    flags = last[5]
    clock_end = 6 + 6 * bool(flags & 0x10) + 6 * bool(flags & 0x08)
    if clock_end == 6 or clock_end > 5 + last[4]:
        return False
    return last[clock_end:] == packet[clock_end:]


def packets_on(data: bytes, packet_size: int, pids: set[int]) -> dict[int, list[tuple[int, bytes]]]:
    """(index, packet) of each packet on `pids`, by PID, indexed as `scan_packets` counts them."""
    out: dict[int, list[tuple[int, bytes]]] = {}
    index = offset = 0
    while offset + packet_size <= len(data):
        if data[offset] != 0x47:
            nxt = data.find(0x47, offset + 1)
            if nxt < 0:
                break
            offset = nxt
            continue
        pid = ((data[offset + 1] & 0x1F) << 8) | data[offset + 2]
        if pid in pids:
            out.setdefault(pid, []).append((index, data[offset : offset + 188]))
        index += 1
        offset += packet_size
    return out


def _crc_table() -> list[int]:
    table = []
    for byte in range(256):
        crc = byte << 24
        for _ in range(8):
            crc = (crc << 1) ^ 0x04C11DB7 if crc & 0x80000000 else crc << 1
        table.append(crc & 0xFFFFFFFF)
    return table


CRC_TABLE = _crc_table()


def crc32_mpeg(data: bytes) -> int:
    """The PSI section CRC (ISO 13818-1 Annex A): 0 over a whole section, its CRC included."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc = ((crc << 8) & 0xFFFFFFFF) ^ CRC_TABLE[(crc >> 24) ^ byte]
    return crc


def sections(packets: list[tuple[int, bytes]]) -> list[tuple[int, bytes]]:
    """(index of the packet completing it, section) for each long-form section on one PID, as
    a receiver reassembles them (2.4.4.2): a duplicate packet (2.4.3.3) adds nothing, and a
    section failing its CRC is dropped."""
    out: list[tuple[int, bytes]] = []

    def complete(index: int, buf: bytes) -> bytes | None:
        # Whole sections leave `buf`; the rest is a section still arriving, or stuffing.
        while buf and buf[0] != 0xFF:
            if len(buf) < 3:
                return buf
            length = 3 + (((buf[1] & 0x0F) << 8) | buf[2])
            if len(buf) < length:
                return buf
            if buf[1] & 0x80 and crc32_mpeg(buf[:length]) == 0:
                out.append((index, buf[:length]))
            buf = buf[length:]
        return None

    pending: bytes | None = None
    last = None
    for index, packet in packets:
        duplicate = is_duplicate(last, packet)
        last = packet
        if duplicate or not packet[3] & 0x10:
            continue
        payload = packet[5 + packet[4] if packet[3] & 0x20 else 4 :]
        if packet[1] & 0x40 and payload:
            if pending is not None:
                complete(index, pending + payload[1 : 1 + payload[0]])
            pending = complete(index, payload[1 + payload[0] :])
        elif pending is not None:
            pending = complete(index, pending + payload)
    return out


def system_timelines(
    data: bytes, packet_size: int
) -> tuple[dict[int, list[tuple[int, int | None, int | None]]], dict[int, list[tuple[int, bytes]]]]:
    """Each program's epochs, (first packet, PMT PID, PCR PID), and the packets on PIDs 0-3
    and every PMT PID, by PID.

    A receiver learns a program's PMT PID from the PAT, and its PCR PID from a PMT on that
    PID, each from the packet after the section that declares it. The first pair a program
    declares holds from the start of the capture, unless a complete PAT before it left the
    program out. A complete PAT version without a program a section has listed ends it, an
    epoch with neither PID, until a PAT and PMT declare it again."""
    by_pid = packets_on(data, packet_size, {0})
    # (index, kind, PMT PID, value): a PAT naming the PMT PID (0), a PMT naming the PCR PID
    # (1), or a complete PAT version without the program (2).
    events: dict[int, list[tuple[int, int, int, int]]] = {}
    listed: set[int] = set()
    version: int | None = None
    parts: dict[int, set[int]] = {}
    first_complete: int | None = None
    for index, section in sections(by_pid.get(0, [])):
        if section[0] != 0x00 or not section[5] & 0x01:
            continue
        programs = set()
        for at in range(8, len(section) - 4, 4):
            program = (section[at] << 8) | section[at + 1]
            pid = ((section[at + 2] & 0x1F) << 8) | section[at + 3]
            if program:
                programs.add(program)
                events.setdefault(program, []).append((index, 0, pid, pid))
        if (section[5] >> 1) & 0x1F != version:
            version, parts = (section[5] >> 1) & 0x1F, {}
        parts[section[6]] = programs
        listed |= programs
        if all(n in parts for n in range(section[7] + 1)):
            current = set().union(*(parts[n] for n in range(section[7] + 1)))
            for program in listed - current:
                events[program].append((index, 2, 0, 0))
            listed = current
            first_complete = index if first_complete is None else first_complete
    pmt_pids = {pid for changes in events.values() for _, _, pid, _ in changes}
    by_pid |= packets_on(data, packet_size, {1, 2, 3} | pmt_pids)
    for pid in pmt_pids:
        for index, section in sections(by_pid.get(pid, [])):
            program = (section[3] << 8) | section[4]
            if section[0] == 0x02 and section[5] & 0x01 and program in events:
                events[program].append((index, 1, pid, ((section[8] & 0x1F) << 8) | section[9]))
    timelines: dict[int, list[tuple[int, int | None, int | None]]] = {}
    for program, changes in events.items():
        pmt = pcr = None
        epochs: list[tuple[int, int | None, int | None]] = []
        absent = first_complete is not None and first_complete < min(i for i, kind, _, _ in changes if kind == 0)
        for index, kind, pid, value in sorted(changes):
            absent |= kind == 2
            if kind == 1 and pid != pmt:
                continue
            new = (value, pcr) if kind == 0 else (pmt, value) if kind == 1 else (None, None)
            if new == (pmt, pcr):
                continue
            pmt, pcr = new
            if pcr is None and (pmt is not None or not epochs or epochs[-1][1] is None):
                continue
            start = index + 1 if epochs or absent else 0
            if epochs and epochs[-1][0] == start:
                epochs.pop()
            epochs.append((start, pmt, pcr))
        if epochs:
            timelines[program] = epochs
    return timelines, by_pid


def program_system_packets(
    epochs: list[tuple[int, int | None, int | None]], by_pid: dict[int, list[tuple[int, bytes]]]
) -> list[tuple[int, bool]]:
    """(index, first copy) of each packet the program takes into TBsys while it is declared:
    PIDs 0-3 and the PMT PID in force. Both copies of a duplicate (2.4.3.3) enter TBsys, and
    only the first goes on to Bsys."""
    starts = [start for start, _, _ in epochs]
    out: list[tuple[int, bool]] = []
    for pid, packets in by_pid.items():
        last = None
        for index, packet in packets:
            first = not is_duplicate(last, packet)
            last = packet
            n = bisect.bisect_right(starts, index) - 1
            pmt = epochs[n][1] if n >= 0 else None
            if pmt is not None and (pid <= 3 or pmt == pid):
                out.append((index, first))
    out.sort()
    return out


def clock_runs(scan: Scan, epochs: list[tuple[int, int | None, int | None]]) -> list[tuple[int, int, int, bool]]:
    """(first packet, end packet, PCR PID, fresh) for each stretch on one PCR PID and time
    base while the program is declared. The program's buffers carry across a change of PMT
    or PCR PID; a signalled discontinuity on the PCR PID in force, even on its first PCR,
    starts them afresh (fresh), as in `check_tstd`, and so does the program's return."""
    spans: list[list] = []
    fresh = True
    for n, (start, _, pcr_pid) in enumerate(epochs):
        end = epochs[n + 1][0] if n + 1 < len(epochs) else scan.total_packets
        if pcr_pid is None:
            fresh = True
        elif spans and spans[-1][2] == pcr_pid and not fresh:
            spans[-1][1] = end
        else:
            spans.append([start, end, pcr_pid, fresh])
            fresh = False
    runs: list[tuple[int, int, int, bool]] = []
    for start, end, pcr_pid, fresh in spans:
        on_pid = {i for i, _ in scan.pcr_by_pid.get(pcr_pid, [])}
        bases = sorted(i for i in scan.pcr_new_base if start <= i < end and i in on_pid)
        fresh = fresh or bases[:1] == [start]
        bases = [i for i in bases if i > start]
        for k, (lo, hi) in enumerate(zip([start, *bases], [*bases, end])):
            runs.append((lo, hi, pcr_pid, fresh or k > 0))
    return runs


@dataclass
class SystemBuffers:
    """One program's TBsys and Bsys, as its system packets pass through."""

    # When TBsys has passed on every byte it took, and when it last started filling from empty.
    leave: float = float("-inf")
    busy_since: float | None = None
    # Bsys's bytes at `at`, as the last first copy's last byte reached it.
    level: float = 0.0
    at: float | None = None
    # (from when, clock) for each PCR PID the buffers have run on, the first from the start.
    clocks: list[tuple[float, PcrClock]] = field(default_factory=list)

    def run_on(self, clock: PcrClock, start: float) -> None:
        """Take the time and transport rate from `clock` from `start` on."""
        start = max(start, self.clocks[-1][0]) if self.clocks else float("-inf")
        self.clocks.append((start, clock))

    def drained(self, t0: float, t1: float) -> float:
        """Bytes Bsys removes from `t0` to `t1` while it holds any: at each instant's Rsys."""
        bits = 0.0
        for n, (start, clock) in enumerate(self.clocks):
            lo = max(t0, start)
            hi = min(t1, self.clocks[n + 1][0]) if n + 1 < len(self.clocks) else t1
            if hi > lo:
                bits += clock.rsys_bits(hi) - clock.rsys_bits(lo)
        return bits / 8

    def take(self, start: float, end: float, first: bool, grade: Grade) -> None:
        """One packet on the wire from `start` to `end`: whole into TBsys, and a first copy's
        bytes on into Bsys as they leave TBsys at Rxsys, never before they arrive."""
        if self.leave <= start:
            self.busy_since = start
        free = max(self.leave, start)
        self.leave = max(end, free + 188 * 8 / SYSTEM_RX)
        fill = (self.leave - end) * SYSTEM_RX / 8
        grade.peak("TBsys", fill, TB_SIZE)
        if fill > TB_SIZE + 0.5:
            grade.flag("TBsys overflow")
        if self.busy_since is not None and self.leave - self.busy_since > TB_EMPTY_S:
            grade.flag("TBsys not emptied within 1 s")
            self.busy_since = None
        if first:
            # Bsys takes the bytes faster than Rsys drains them, so it peaks as the last arrives.
            if self.at is not None:
                self.level = max(0.0, self.level - self.drained(self.at, free))
            self.level = max(0.0, self.level + 188 - self.drained(free, self.leave))
            self.at = self.leave
            grade.peak("Bsys", self.level, BSYS_SIZE)
            if self.level > BSYS_SIZE + 0.5:
                grade.flag("Bsys overflow")


def check_system_buffers(ts_path: str, packet_size: int, scan: Scan) -> Check:
    """TBsys and Bsys (ISO 13818-1 2.4.2.3) for each program, on its PCR.

    Each program is followed through the PAT and its PMTs: a packet on PIDs 0-3 or on the
    PMT PID then in force arrives in the program's one TBsys over its time on the wire, as
    the PCR PID then in force times it, and its bytes leave at Rxsys once those ahead of them
    have, never before they arrive, into the one Bsys, which empties at Rsys whenever it
    holds a byte, at the transport rate of the moment. Bsys takes them faster than it drains,
    so it peaks as a packet's last byte reaches it. Graded on 2.4.2.6: neither overflows, and
    TBsys empties at least once a second. Both buffers keep their bytes across a change of
    PMT PID, PCR PID or transport rate; a signalled discontinuity starts them afresh, as in
    `check_tstd`, and a program a complete PAT drops takes nothing until it is declared again,
    afresh. Across a PCR PID change the last PCR on the old PID and the first on the new are
    the program's successive PCRs: unless the change signals a discontinuity, they must
    increase and lie within 0.1 s (2.7.2), or the check fails and the buffers start afresh.
    A program declaring no PCR (PCR_PID 0x1FFF, 2.4.4.9) has no clock to model;
    a system packet on any other PCR PID's stretch with fewer than two PCRs fails, since it
    was not checked.
    """
    with open(ts_path, "rb") as handle:
        data = handle.read()
    timelines, by_pid = system_timelines(data, packet_size)
    unclocked = sorted(p for p, epochs in timelines.items() if all(pcr in (0x1FFF, None) for _, _, pcr in epochs))
    programs: dict[str, dict] = {}
    for program, epochs in timelines.items():
        if program in unclocked:
            continue
        packets = program_system_packets(epochs, by_pid)
        indices = [index for index, _ in packets]
        grade = Grade(program, "system")
        modelled = 0
        # The runs one set of buffers lives through, each clock known before any packet is
        # taken: a packet still reaching Bsys as the clock changes drains at the next rate.
        # Across a PCR PID change the last PCR on the old PID and the first on the new are the
        # program's successive PCRs, which time the bytes between them (2.4.2.2).
        chains: list[list[tuple[int, list[tuple[int, bool]], PcrClock, list[tuple[int, int]]]]] = []
        chain = None
        for lo, hi, pcr_pid, fresh in clock_runs(scan, epochs):
            run = packets[bisect.bisect_left(indices, lo) : bisect.bisect_left(indices, hi)]
            samples = [s for s in scan.pcr_by_pid.get(pcr_pid, []) if lo <= s[0] < hi]
            clock = PcrClock(samples)
            if pcr_pid == 0x1FFF or not clock.ok():
                if pcr_pid != 0x1FFF and run:
                    name = "outside a clocked time base"
                    grade.violations[name] = grade.violations.get(name, 0) + len(run)
                chain = None
                continue
            if chain is not None and not fresh:
                before, held = chain[-1][3][-1], chain[-1][3]
                if 0 < (samples[0][1] - before[1]) % PCR_WRAP <= PCR_HZ // 10:
                    chain[-1] = (*chain[-1][:2], PcrClock([*held, samples[0]]), held)
                    samples = [before, *samples]
                    clock = PcrClock(samples)
                else:
                    name = "PCR PID switch without a common time base"
                    grade.violations[name] = grade.violations.get(name, 0) + 1
                    fresh = True
            if fresh or chain is None:
                chain = []
                chains.append(chain)
            chain.append((lo, run, clock, samples))
        for chain in chains:
            buffers = SystemBuffers()
            for lo, _, clock, _ in chain:
                buffers.run_on(clock, clock.time_at(lo))
            for _, run, clock, _ in chain:
                for index, first in run:
                    modelled += 1
                    buffers.take(clock.time_at(index), clock.time_at(index + 1), first, grade)
        programs[str(program)] = {
            "pmt_pids": sorted({pmt for _, pmt, _ in epochs if pmt is not None}),
            "pcr_pids": sorted({pcr for _, _, pcr in epochs if pcr is not None}),
            "system_packets": modelled,
            "violations": grade.violations,
            "peak_fill_pct": {k: round(v * 100, 1) for k, v in grade.peaks.items()},
        }
    metrics = {"programs": programs, "unclocked_programs": unclocked}
    if not programs:
        return Check("system-buffers", Severity.SHAPE, Status.WARN, "no clocked program to model", metrics)
    faults = [
        f"program {number} {name} x{count}"
        for number, program in programs.items()
        for name, count in sorted(program["violations"].items())
    ]
    faults += [
        f"program {number} has no system packet inside a time base"
        for number, program in programs.items()
        if not program["system_packets"] and not program["violations"]
    ]
    if faults:
        return Check("system-buffers", Severity.HARD, Status.FAIL, "; ".join(faults), metrics)
    peaks = ", ".join(
        f"program {number} " + "/".join(f"{k} {v:.0f}%" for k, v in program["peak_fill_pct"].items())
        for number, program in programs.items()
    )
    return Check(
        "system-buffers",
        Severity.HARD,
        Status.PASS,
        f"no overflow, TBsys emptied each second; peak fill {peaks}",
        metrics,
    )


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
    check that pins the absolute rate. The exporter joins at the live edge once
    the catalog appears, and import holds the catalog for its two-second mux-rate
    window, so the captured span runs up to that window plus a GOP short, never
    materially long; the band is wide enough to ignore that yet catch a gross
    scale error.
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


def analyze(ts_path: str, reference_seconds: float | None = None) -> list[Check]:
    """Run every check against `ts_path` and return the ordered results.

    `reference_seconds` (the source's PCR span, round-trip only) enables the
    duration-fidelity check that pins the exported stream's absolute rate.
    """
    analysis = run_tsanalyze(ts_path)
    packet_size = detect_packet_size(analysis)

    scan = scan_packets(ts_path, packet_size)
    clock_by_pid = {pid: PcrClock(samples) for pid, samples in scan.pcr_by_pid.items()}

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
        checks.append(check_duration_fidelity(pcr_span_seconds(scan), reference_seconds))

    checks.append(check_service_descriptors(analysis))
    checks.append(check_tstd(ts_path, packet_size, scan))
    checks.append(check_system_buffers(ts_path, packet_size, scan))
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
    args = parser.parse_args()

    try:
        reference_seconds = source_duration(args.reference) if args.reference else None
        checks = analyze(args.ts, reference_seconds)
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
