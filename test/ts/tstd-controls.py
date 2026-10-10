#!/usr/bin/env python3
"""Positive and negative controls for the T-STD model in compliance.py.

The positive control is a real broadcast encoder's output (a Kyrion contribution
feed: AVC High@4.0 plus two MPEG-1 Layer II tracks), which a broadcast chain only
carries because it is T-STD compliant. Each negative restamps that capture's PCRs
at a different constant rate with TSDuck's `pcradjust`, leaving every PES, PTS and
DTS untouched, so the only thing that changes is when the bytes arrive:

    as captured     the broadcast itself                       must pass
    restamped 1x    PCRs rewritten at the capture's own rate   must pass (the rewrite is not the fault)
    0.7x            delivered too slowly                       EB and B underflow
    4x              delivered too early                        TB and B overflow, audio held over 1 s
    15x             delivered in a burst                       TB and B overflow, audio held over 1 s

The video TB drains at 1.2x the bit rate the SPS's NAL HRD declares (1.935 Mb/s),
so any delivery well above real time overflows it. MB holds the level's whole CPB
less the declared one, ~3.6 MB, more than this 4 s capture carries, so no restamp
of it can overflow MB.

A capture cannot be edited into the packet layouts the model has to get right, so
the rest are built here: a 10 Mb/s single-video stream carrying the Kyrion SPS,
with each AU in its own PES, a PCR packet between AUs and nulls elsewhere.

    synthetic           as built                                      must pass
    two AUs, one PES    an AU without its own timestamp               refused
    adaptation burst    four payload-less packets after an AU         TB overflow (they cost TB)
    duplicate packet    one AU's first packet sent twice (2.4.3.3)    must pass (TB only, not MB)
    teletext burst      eight teletext packets back to back           TB overflow (EN 300 472's 480 B,
                                                                      where 2.4.2.3's 512 B holds them)
    PCR PID mismatch    the PMT declares a PID that carries no PCR    pcr-presence fails

and an MPEG audio stream whose frames end partway through a packet, each decoded
just after its last byte leaves TB, but before the rest of that packet has; its
last frame is cut off by the end of the capture:

    straddling audio    as built                                      must pass
    mid-packet overflow seven frames per PES, the first decoded as    B overflow
                        B passes its size partway through a packet

A model that passes everything, or fails everything, cannot tell these apart.
"""

import os
import subprocess
import sys
import tempfile

DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DIR)

import compliance  # noqa: E402

FIXTURE = os.path.join(DIR, "../../rs/moq-mux/src/container/ts/test_data/scte35/kyrion_dirtystart.ts")

# The Kyrion SPS (High@4.0, NAL HRD 1.935 Mb/s CBR, 755 kbit CPB), so the synthetic
# streams get the same declared buffers.
SPS = bytes.fromhex("6764 0028 acd1 0078 044f de03 6a02 0202 8000 01f4 8000 7530 7500 0762 0002 e11f af7f 072a 1629 92")
VIDEO_PID, TELETEXT_PID, PMT_PID = 0x100, 0x101, 0x1000
RATE = 10_000_000
SLOT_S = 188 * 8 / RATE


def crc32_mpeg(data: bytes) -> int:
    """The MPEG-2 section CRC: polynomial 0x04C11DB7, not reflected, initial all ones."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = (crc << 1) ^ 0x04C11DB7 if crc & 0x80000000 else crc << 1
        crc &= 0xFFFFFFFF
    return crc


def section(table_id: int, extension: int, body: bytes) -> bytes:
    """A long-form PSI section, CRC included."""
    head = bytes([table_id, 0xB0 | ((len(body) + 9) >> 8), (len(body) + 9) & 0xFF])
    head += extension.to_bytes(2, "big") + bytes([0xC1, 0, 0])
    return head + body + crc32_mpeg(head + body).to_bytes(4, "big")


def packet(pid: int, cc: int, payload: bytes = b"", pusi: bool = False, adaptation: bytes | None = None) -> bytes:
    """One 188-byte packet; any room the payload leaves is adaptation-field stuffing."""
    room = 184 - len(payload)
    if adaptation is None and room > 0:
        adaptation = b"\x00" if room > 1 else b""
    if adaptation is not None:
        adaptation = bytes([room - 1]) + adaptation.ljust(room - 1, b"\xff") if room > 1 else b"\x00"
    afc = (2 if adaptation is not None else 0) | (1 if payload else 0)
    head = bytes([0x47, (0x40 if pusi else 0) | pid >> 8, pid & 0xFF, afc << 4 | cc])
    return head + (adaptation or b"") + payload


def stamp(prefix: int, ticks: int) -> bytes:
    """A 5-byte PES PTS/DTS field."""
    return bytes(
        [
            prefix << 4 | (ticks >> 29) & 0x0E | 1,
            (ticks >> 22) & 0xFF,
            (ticks >> 14) & 0xFE | 1,
            (ticks >> 7) & 0xFF,
            (ticks << 1) & 0xFE | 1,
        ]
    )


def synthetic(path: str, layout: str) -> None:
    """Write a compliant 2 s stream, bent into `layout` at one access unit."""
    pat = section(0x00, 1, (1).to_bytes(2, "big") + (0xE000 | PMT_PID).to_bytes(2, "big"))
    pcr_pid = 0x1FF if layout == "PCR PID mismatch" else VIDEO_PID
    streams = bytes([0x1B, 0xE1, 0x00, 0xF0, 0x00])
    if layout == "teletext burst":
        # Private data with a teletext_descriptor (EN 300 468 6.2.43): English, initial page 100.
        teletext = bytes([0x56, 0x05]) + b"eng" + bytes([0x09, 0x00])
        streams += bytes([0x06, 0xE1, 0x01, 0xF0, len(teletext)]) + teletext
    pmt = section(0x02, 1, (0xE000 | pcr_pid).to_bytes(2, "big") + bytes([0xF0, 0x00]) + streams)
    cc: dict[int, int] = {}
    out: list[bytes] = []

    def emit(pid: int, payload: bytes = b"", pusi: bool = False, adaptation: bytes | None = None) -> bytes:
        if payload:
            cc[pid] = (cc.get(pid, -1) + 1) & 0x0F
        made = packet(pid, cc.get(pid, 0), payload, pusi, adaptation)
        out.append(made)
        return made

    def pes(units: list[bytes], dts_s: float) -> None:
        ticks = round((1.0 + dts_s) * compliance.PTS_HZ)
        data = bytes.fromhex("000001e0 0000 80c0 0a") + stamp(3, ticks) + stamp(1, ticks) + b"".join(units)
        first = True
        for at in range(0, len(data), 184):
            made = emit(VIDEO_PID, data[at : at + 184], pusi=first)
            if first and layout == "duplicate packet" and dts_s == 0.5 + 30 * 0.04:
                out.append(made)
            first = False

    frames = 50
    pending = None
    for k in range(frames):
        begin = len(out)
        emit(0, b"\x00" + pat, pusi=True)
        emit(PMT_PID, b"\x00" + pmt, pusi=True)
        unit = b"\x00\x00\x00\x01\x09\xf0" + (b"\x00\x00\x00\x01" + SPS if k == 0 else b"")
        unit += b"\x00\x00\x01\x65" + b"\xaa" * 250
        # Each AU decodes 0.5 s after its first byte arrives.
        if layout == "two AUs, one PES" and k == 10:
            pending = unit
        else:
            pes([pending, unit] if pending else [unit], 0.5 + (k - bool(pending)) * 0.04)
            pending = None
        if layout == "adaptation burst" and k == 20:
            for _ in range(4):
                emit(VIDEO_PID, adaptation=b"\x00")
        if layout == "teletext burst" and k == 20:
            # One PES of eight packets, sent back to back: at 10 Mb/s against a 6.75 Mb/s
            # drain each adds 61 bytes to the transport buffer, 489 by the eighth.
            ticks = round((1.0 + 0.5 + k * 0.04) * compliance.PTS_HZ)
            data = bytes.fromhex("000001bd 05be 8480 05") + stamp(2, ticks) + b"\x10" + b"\xff" * (8 * 184 - 15)
            for at in range(0, len(data), 184):
                emit(TELETEXT_PID, data[at : at + 184], pusi=at == 0)
        slots = round(0.04 / SLOT_S)
        while len(out) - begin < slots:
            if len(out) - begin == slots // 2:
                pcr = round((1.0 + len(out) * SLOT_S) * compliance.PCR_HZ)
                field = bytes([0x10]) + ((pcr // 300) << 15 | 0x7E00 | pcr % 300).to_bytes(6, "big")
                emit(VIDEO_PID, adaptation=field)
            else:
                out.append(packet(0x1FFF, 0, b"\xff" * 184))
    with open(path, "wb") as handle:
        handle.write(b"".join(out))


def synthetic_audio(path: str, frames: int, decode_s: float) -> None:
    """MPEG-1 Layer II at 192 kb/s, `frames` 576-byte frames per PES, the first decoded
    `decode_s` after its PES starts arriving."""
    audio_pid, pcr_pid = 0x101, 0x102
    pat = section(0x00, 1, (1).to_bytes(2, "big") + (0xE000 | PMT_PID).to_bytes(2, "big"))
    pmt = section(0x02, 1, bytes([0xE1, 0x02, 0xF0, 0x00, 0x03, 0xE1, 0x01, 0xF0, 0x00]))
    frame = bytes([0xFF, 0xFD, 0xA4, 0x00]) + b"\xaa" * 572
    # An audio packet every six slots, past TB's 0.75 ms drain, so each one meets an
    # empty TB and its byte j leaves (j + 1) x 4 us after it starts arriving.
    every = 6
    cc = 0
    out: list[bytes] = []
    for n in range(20):
        begin = round(n * frames * 0.024 / SLOT_S)
        while len(out) < begin:
            out.append(packet(0x1FFF, 0, b"\xff" * 184))
        ticks = round((1.0 + begin * SLOT_S + decode_s) * compliance.PTS_HZ)
        data = bytes.fromhex("000001c0 0000 8080 05") + stamp(2, ticks) + frame * frames
        chunks = [data[at : at + 184] for at in range(0, len(data), 184)]
        if n == 19:
            chunks = chunks[:-1]  # the capture ends inside the last frame
        for k, chunk in enumerate(chunks):
            while len(out) < begin + k * every:
                out.append(packet(0x1FFF, 0, b"\xff" * 184))
            out.append(packet(audio_pid, cc, chunk, pusi=k == 0))
            cc = (cc + 1) & 0x0F
        out.append(packet(0, n & 0x0F, b"\x00" + pat, pusi=True))
        out.append(packet(PMT_PID, n & 0x0F, b"\x00" + pmt, pusi=True))
        pcr = round((1.0 + len(out) * SLOT_S) * compliance.PCR_HZ)
        field = bytes([0x10]) + ((pcr // 300) << 15 | 0x7E00 | pcr % 300).to_bytes(6, "big")
        out.append(packet(pcr_pid, 0, adaptation=field))
    while len(out) < round(20 * frames * 0.024 / SLOT_S):
        out.append(packet(0x1FFF, 0, b"\xff" * 184))
    with open(path, "wb") as handle:
        handle.write(b"".join(out))


def restamp(scale: float):
    """A builder that restamps the Kyrion capture's PCRs at `scale` times its own rate."""

    def build(path: str) -> str:
        rate = compliance.run_tsanalyze(FIXTURE)["ts"]["bitrate"]
        subprocess.run(
            ["tsp", "-I", "file", FIXTURE, "-P", "pcradjust", "--bitrate", f"{rate * scale:.0f}"]
            + ["--ignore-pts", "--ignore-dts", "-O", "file", path],
            check=True,
        )
        return path

    return build


def built(layout: str):
    """A builder for one synthetic layout."""

    def build(path: str) -> str:
        synthetic(path, layout)
        return path

    return build


def audio(frames: int, decode_s: float):
    """A builder for one synthetic audio stream."""

    def build(path: str) -> str:
        synthetic_audio(path, frames, decode_s)
        return path

    return build


# A packet's byte j leaves an empty TB (j + 1) x 4 us after the packet starts arriving.
BYTE_S = 8 / compliance.AUDIO_RX
# Frame 0 ends at PES byte 14 + 576 = 590, byte 41 of the fourth packet: decode it 0.3 ms
# after that byte leaves TB, 0.28 ms before the packet's last one does.
STRADDLE_S = 3 * 6 * SLOT_S + 42 * BYTE_S + 0.0003
# Seven frames fill 22 packets, 4046 B. Decoding frame 0 once 100 bytes of the 20th have
# left TB finds 3592 B in B, over its 3584; whole packets alone peak at 3496 B before and
# 3470 B after.
MID_PACKET_S = 19 * 6 * SLOT_S + 100 * BYTE_S


# (name, builder, expected): violations that must all appear, "pass", a refusal's text,
# or ("fails", check) for a check other than tstd that must fail.
CASES = [
    ("as captured", lambda _path: FIXTURE, "pass"),
    ("restamped 1x", restamp(1.0), "pass"),
    ("0.7x", restamp(0.7), {"EB underflow", "B underflow"}),
    ("4x", restamp(4.0), {"TB overflow", "B overflow", "held over 1 s"}),
    ("15x", restamp(15.0), {"TB overflow", "B overflow", "held over 1 s"}),
    ("synthetic", built("synthetic"), "pass"),
    ("two AUs, one PES", built("two AUs, one PES"), "several access units"),
    ("adaptation burst", built("adaptation burst"), {"TB overflow"}),
    ("duplicate packet", built("duplicate packet"), "pass"),
    ("teletext burst", built("teletext burst"), {"TB overflow"}),
    ("PCR PID mismatch", built("PCR PID mismatch"), ("fails", "pcr-presence")),
    ("straddling audio", audio(4, STRADDLE_S), "pass"),
    ("mid-packet overflow", audio(7, MID_PACKET_S), {"B overflow"}),
]

def grade(path: str) -> dict[str, compliance.Check]:
    """compliance.py's verdicts on one file, by check name."""
    return {check.name: check for check in compliance.analyze(path)}


def main() -> int:
    """Run every case and exit non-zero if any verdict is not the expected one."""
    failed = 0
    with tempfile.TemporaryDirectory() as tmp:
        for n, (name, build, expected) in enumerate(CASES):
            checks = grade(build(os.path.join(tmp, f"{n}.ts")))
            check = checks["tstd"]
            streams = check.metrics.get("streams", {})
            seen = {v for s in streams.values() for v in s["violations"]}
            if expected == "pass":
                # Not vacuous: every stream must have had access units to grade.
                ok = check.status == compliance.Status.PASS and all(s["access_units"] for s in streams.values())
                ok = ok and checks["pcr-presence"].status == compliance.Status.PASS
            elif isinstance(expected, tuple):
                check = checks[expected[1]]
                ok = check.status == compliance.Status.FAIL
            elif isinstance(expected, str):
                ok = any(expected in why for why in check.metrics.get("refused", {}).values())
            else:
                ok = expected <= seen
            failed += not ok
            want = " ".join(reversed(expected)) if isinstance(expected, tuple) else expected
            want = want if isinstance(want, str) else ", ".join(sorted(want))
            print(f"  {'ok  ' if ok else 'FAIL'}  {name:<18} want {want:<40} got {check.detail}")
    if failed:
        print(f"tstd controls: {failed} of {len(CASES)} cases wrong", file=sys.stderr)
        return 1
    print("tstd controls: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
