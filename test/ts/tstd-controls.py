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
of it can overflow MB. An 8 s generated Baseline@2.0 clip without NAL HRD
uses the smaller level-default MB instead: as encoded it passes, and at 4x
it overflows MB (as well as TB). A second generated AVC/AAC reference uses
a 0.1 s mux delay and 2 Mb/s transport rate so AAC passes both TB and B.

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
    table burst         a PAT and three PMT copies back to back      TBsys overflow
    Bsys burst          ten PMT copies, each as TBsys passes the      Bsys overflow (80 kb/s Rsys)
                        last on
    TBsys kept busy     PMT copies at Rxsys for 1.5 s, one ahead      TBsys not emptied within 1 s,
                                                                      Bsys overflow
    duplicate tables    six PMT copies each sent twice, the second    must pass (TBsys only, not Bsys)
                        with a refreshed OPCR
    moved PMT burst     the PAT moves the PMT, then the Bsys burst    Bsys overflow (on the new PMT)
                        on its new PID
    split PMT burst     the Bsys burst, six copies before the PAT     Bsys overflow (one Bsys across
                        moves the PMT and four after                  the move)

and tables-only streams on a PCR PID at 100 Mb/s:

    rate-fall burst     PMT copies filling Bsys, three more once      Bsys overflow (it keeps its bytes
                        the rate falls to 25 Mb/s                     as Rsys falls)
    rate fall           the same copies, the fall 10 ms later, two    must pass (Bsys drains at the old
                        more                                          Rsys until the fall)
    PCR PID move        the PMT moves the PCR to a 10 Mb/s PID, then  must pass (each PID's clock only
                        four copies spaced for TBsys at 10 Mb/s       while it is the PCR PID)
    PCR switch in burst ten copies, the tenth moving the PCR to a     Bsys overflow (the tenth drains
                        PID at 25 Mb/s, then one more                 at the new Rsys from the switch)
    PCR move, new base  the PMT moves the PCR to a PID whose first    must pass (the new time base
                        PCR signals a discontinuity, then copies      starts the buffers afresh)
    PCR move, jump      the same, the new PID's PCRs 10 s ahead with  PCR PID switch without a common
                        no discontinuity                              time base
    program removal     a PAT first seen as its second section, then  must pass (the dropped program
                        a version dropping the second program, its    takes nothing after the PAT)
                        PCRs stopping, the rate falling to 10 Mb/s
                        and four PAT copies spaced for TBsys
    program addition    four PAT copies spaced for TBsys at 10 Mb/s,  must pass (the added program
                        then a rise to 100 Mb/s and a version adding  takes nothing before the PAT)
                        a second program
    program restored    the same, the opening PAT listing the second  must pass (the restored program
                        program and a version dropping it before the  takes nothing before the PAT)
                        burst, with no PMT for it
    unclocked tables    a discontinuity with one PCR, then copies     outside a clocked time base

and an MPEG audio stream whose frames end partway through a packet, each decoded
just after its last byte leaves TB, but before the rest of that packet has; its
last frame is cut off by the end of the capture:

    straddling audio    as built                                      must pass
    mid-packet overflow seven frames per PES, the first decoded as    B overflow
                        B passes its size partway through a packet

A model that passes everything, or fails everything, cannot tell these apart.
"""

import json
import os
import subprocess
import sys

DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, DIR)

import compliance  # noqa: E402

FIXTURE = os.path.join(DIR, "../../rs/moq-mux/src/container/ts/test_data/scte35/kyrion_dirtystart.ts")

# The Kyrion SPS (High@4.0, NAL HRD 1.935 Mb/s CBR, 755 kbit CPB), so the synthetic
# streams get the same declared buffers.
SPS = bytes.fromhex("6764 0028 acd1 0078 044f de03 6a02 0202 8000 01f4 8000 7530 7500 0762 0002 e11f af7f 072a 1629 92")
VIDEO_PID, TELETEXT_PID, PMT_PID, MOVED_PID = 0x100, 0x101, 0x1000, 0x1001
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


def section(table_id: int, extension: int, body: bytes, version: int = 0, number: int = 0, last: int = 0) -> bytes:
    """A long-form PSI section, CRC included."""
    head = bytes([table_id, 0xB0 | ((len(body) + 9) >> 8), (len(body) + 9) & 0xFF])
    head += extension.to_bytes(2, "big") + bytes([0xC1 | version << 1, number, last])
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


def clock(flag: int, ticks: int) -> bytes:
    """An adaptation field's flags byte and the one PCR (0x10) or OPCR (0x08) it carries, in
    27 MHz ticks."""
    return bytes([flag]) + ((ticks // 300) << 15 | 0x7E00 | ticks % 300).to_bytes(6, "big")


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
    # The PAT's next version, which moves the PMT.
    moved = section(0x00, 1, (1).to_bytes(2, "big") + (0xE000 | MOVED_PID).to_bytes(2, "big"), version=1)
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
    # The next position a PMT copy takes in place of a null, and how many are left to go.
    table_at, tables = 0, 0
    busy = layout == "TBsys kept busy"
    for k in range(frames):
        begin = len(out)
        pmt_pid = MOVED_PID if layout in ("moved PMT burst", "split PMT burst") and k >= 25 else PMT_PID
        if not busy or k == 0:
            emit(0, b"\x00" + (moved if pmt_pid == MOVED_PID else pat), pusi=True)
            emit(pmt_pid, b"\x00" + pmt, pusi=True)
        if layout == "table burst" and k == 20:
            # With the PAT and PMT, four packets at 10 Mb/s against TBsys's 1 Mb/s: 169 B
            # stay in it per packet, 677 B by the fourth.
            for _ in range(2):
                emit(PMT_PID, b"\x00" + pmt, pusi=True)
        if (layout, k) in (("Bsys burst", 20), ("moved PMT burst", 30)):
            # Ten copies, one every 10 positions: the 1,504 us TBsys takes to pass one on, so
            # it holds 320 B at most, while Bsys takes 2 KB against its 10 kB/s drain.
            table_at, tables = len(out) + 10, 10
        if layout == "split PMT burst" and k == 24:
            # The same ten, six of them on the old PID before the PAT moves the PMT.
            table_at, tables = begin + 206, 10
        if layout == "split PMT burst" and k == 25:
            # The moved PAT and its PMT take two copies' places in TBsys.
            table_at += 20
        if layout == "duplicate tables" and k == 20:
            # Six copies, each sent twice, every 21 positions once TBsys has passed the PMT
            # on: 1.1 KB into Bsys, but 2.3 KB were each duplicate charged to it as well.
            table_at, tables = len(out) + 20, 6
        if busy and k == 1:
            # Copies at TBsys's own rate for 1.5 s, one packet ahead, so it never empties.
            emit(PMT_PID, b"\x00" + pmt, pusi=True)
            table_at, tables = len(out), round(1.5 / (10 * SLOT_S))
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
            elif tables and len(out) >= table_at and layout == "duplicate tables":
                # The duplicate repeats the copy but for a refreshed OPCR (2.4.3.3).
                opcr = round((1.0 + len(out) * SLOT_S) * compliance.PCR_HZ)
                emit(pmt_pid, b"\x00" + pmt, pusi=True, adaptation=clock(0x08, opcr))
                out.append(packet(pmt_pid, cc[pmt_pid], b"\x00" + pmt, True, clock(0x08, opcr + 4_050)))
                table_at, tables = table_at + 21, tables - 1
            elif tables and len(out) >= table_at:
                emit(pmt_pid, b"\x00" + pmt, pusi=True)
                table_at, tables = table_at + 10, tables - 1
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


def synthetic_system(path: str, layout: str) -> None:
    """A PAT and PMT, then PMT copies among PCRs and nulls at 100 Mb/s, where Rsys is 200 kb/s,
    until the transport rate falls at `fall`, bent into `layout`. A copy every 107 positions
    (1.6 ms) meets TBsys just as it passes the last one on."""
    old, new, second = 0x102, 0x103, 0x104
    one = (1).to_bytes(2, "big") + (0xE000 | PMT_PID).to_bytes(2, "big")
    two = (2).to_bytes(2, "big") + (0xE000 | MOVED_PID).to_bytes(2, "big")
    pat = section(0x00, 1, one)
    if layout == "program removal":
        # Seen first as the second of two sections, which lists both programs.
        pat = section(0x00, 1, one + two, number=1, last=1)
    elif layout == "program restored":
        pat = section(0x00, 1, one + two)
    # The PAT's next versions: one dropping the second program, one adding it.
    dropped = section(0x00, 1, one, version=1)
    added = section(0x00, 1, one + two, version=2)
    pmt = section(0x02, 1, (0xE000 | old).to_bytes(2, "big") + bytes([0xF0, 0x00]))
    # The PMT's next version, which moves the PCR.
    moved = section(0x02, 1, (0xE000 | new).to_bytes(2, "big") + bytes([0xF0, 0x00]), version=1)
    other = section(0x02, 2, (0xE000 | second).to_bytes(2, "big") + bytes([0xF0, 0x00]))
    moving = ("PCR PID move", "PCR switch in burst", "PCR move, new base", "PCR move, jump")
    adding = ("program addition", "program restored")
    burst = [6_650 + n * 107 for n in range(9)]
    pats: list[int] = []
    others: list[int] = []
    before = 100_000_000
    if layout == "rate-fall burst":
        # Bsys holds 1.3 KB as the rate falls to 25 Mb/s, where Rsys is 80 kb/s, and three
        # more copies overflow it: its 54 ms to drain at the old Rsys would pass them, had
        # it drained that long at the new one.
        fall, after = burst[-1] + 107, 25_000_000
        copies = burst + [fall + 13 + n * 27 for n in range(3)]
    elif layout == "rate fall":
        # The same nine, then the fall 10 ms later and two more copies: Bsys drained at 200
        # kb/s until the fall, so they take it to 1,427 B.
        fall, after = burst[0] + 1_621, 25_000_000
        copies = burst + [fall + 1, fall + 28]
    elif layout == "PCR PID move":
        # The PMT moves the PCR to a PID that carries it at 10 Mb/s, then four copies every
        # 11 positions (1.65 ms): the old PID's clock, run on, would put them 165 us apart.
        fall, after = 6_650, 10_000_000
        copies = [fall - 1] + [fall + 11 * n for n in range(1, 5)]
    elif layout in ("PCR switch in burst", "PCR move, jump"):
        # Ten copies, the tenth moving the PCR to a PID that carries it on at 25 Mb/s, and one
        # more: Bsys drains at 80 kb/s from the switch, the tenth copy still reaching it, so
        # the eleventh takes it to 1,551 B. With the jump, the new PID's PCRs run 10 s ahead
        # of the old one's and signal no discontinuity.
        fall, after = burst[0] + 964, 25_000_000
        copies = [burst[0] + n * 107 for n in range(10)] + [burst[0] + 1_196]
    elif layout == "PCR move, new base":
        # The PMT moves the PCR to a PID whose first PCR signals a time base 0.5 s behind the
        # old one, then copies spaced for TBsys.
        fall, after = 6_650, 100_000_000
        copies = [fall - 1] + [fall + 107 * n for n in range(1, 5)]
    elif layout == "program removal":
        # The PAT's next version drops the second program, whose PCRs stop with it, then the
        # rate falls to 10 Mb/s and four PAT copies follow every 11 positions (1.65 ms): the
        # dropped program's clock, run on, would put them 165 us apart.
        fall, after = 6_650, 10_000_000
        copies, pats, others = [], [fall - 100] + [fall + 11 * n for n in range(1, 5)], [3]
    elif layout in adding:
        # Four PAT copies every 11 positions (1.65 ms) at 10 Mb/s, then the rate rises to 100
        # Mb/s and the PAT's next version adds the second program: its clock, run back, would
        # put the copies 165 us apart. Restored, the opening PAT lists the second program and
        # a version dropping it follows before any PMT for it.
        fall, after, before = 6_650, 100_000_000, 10_000_000
        copies, pats = [], [fall - 50 + 11 * n for n in range(4)] + [fall + 100, fall + 400]
        pats += [100] if layout == "program restored" else []
        others = [fall + 101, fall + 300]
    else:  # "unclocked tables": a signalled discontinuity, one PCR, then three copies
        fall, after = 6_650, 100_000_000
        copies = [fall + 20 + n * 107 for n in range(3)]
    out: list[bytes] = []
    at, cc, pat_cc, other_cc = 0.0, 0, 0, 0
    for index in range(fall + 1_700):
        pcr_pid = new if layout in moving and index >= fall else old
        behind = {"PCR move, new base": 0.5, "PCR move, jump": -10.0}.get(layout, 0.0) if index >= fall else 0.0
        ticks = round((1.0 + at - behind) * compliance.PCR_HZ)
        if index == 1:
            out.append(packet(0, 0, b"\x00" + pat, pusi=True))
        elif index in pats:
            pat_cc = (pat_cc + 1) & 0x0F
            if layout == "program removal" or layout == "program restored" and index < fall:
                table = dropped
            else:
                table = added if index > fall else pat
            out.append(packet(0, pat_cc, b"\x00" + table, pusi=True))
        elif index in others:
            out.append(packet(MOVED_PID, other_cc, b"\x00" + other, pusi=True))
            other_cc = (other_cc + 1) & 0x0F
        elif index == 2 or index in copies:
            table = moved if layout in moving and index >= fall - 1 else pmt
            out.append(packet(PMT_PID, cc, b"\x00" + table, pusi=True))
            cc = (cc + 1) & 0x0F
        elif layout == "unclocked tables" and index == fall:
            # The discontinuity indicator (0x80) with the new time base's only PCR.
            out.append(packet(old, 0, adaptation=clock(0x90, ticks + 10 * compliance.PCR_HZ)))
        elif layout == "PCR move, new base" and index == fall:
            out.append(packet(new, 0, adaptation=clock(0x90, ticks)))
        elif (index % 16 == 0 or index == fall) and not (layout == "unclocked tables" and index > fall):
            out.append(packet(pcr_pid, 0, adaptation=clock(0x10, ticks)))
        elif index % 16 == 8 and (
            layout == "program removal" and index < fall - 100 or layout in adding and index > fall + 101
        ):
            out.append(packet(second, 0, adaptation=clock(0x10, ticks)))
        else:
            out.append(packet(0x1FFF, 0, b"\xff" * 184))
        at += 188 * 8 / (before if index < fall else after)
    with open(path, "wb") as handle:
        handle.write(b"".join(out))


def encoded(audio: bool):
    """A builder for an 8 s level-default AVC reference, optionally with stereo AAC."""

    def build(path: str) -> str:
        command = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                   "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25"]
        if audio:
            command += ["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000"]
        command += ["-map", "0:v"] + (["-map", "1:a"] if audio else [])
        command += ["-t", "8", "-c:v", "libx264", "-threads", "2",
                    "-preset", "ultrafast", "-tune", "zerolatency",
                    "-profile:v", "baseline", "-level:v", "2.0", "-b:v", "1M",
                    "-x264-params", "keyint=25:min-keyint=25:scenecut=0:aud=1:repeat-headers=1:nal-hrd=none"]
        if audio:
            command += ["-c:a", "aac", "-b:a", "128k", "-ac", "2"]
        # The 0.1 s lead avoids underflow without filling AAC's 3584-byte B;
        # the 2 Mb/s mux rate also keeps audio packet bursts within its TB drain.
        command += ["-f", "mpegts", "-muxrate", "2000000", "-pcr_period", "20",
                    "-pes_payload_size", "0", "-muxdelay", "0.1", "-muxpreload", "0.1", path]
        subprocess.run(command, check=True)
        probe = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "stream=codec_name",
                                "-of", "json", path], capture_output=True, text=True, check=True)
        codecs = [stream["codec_name"] for stream in json.loads(probe.stdout)["streams"]]
        if codecs != (["h264", "aac"] if audio else ["h264"]):
            raise RuntimeError(f"generated reference has unexpected codecs: {codecs}")
        return path

    return build


def restamp(scale: float, source=lambda _path: FIXTURE):
    """A builder that restamps a source's PCRs at `scale` times its own rate."""

    def build(path: str) -> str:
        original = source(path + ".source.ts")
        rate = compliance.run_tsanalyze(original)["ts"]["bitrate"]
        subprocess.run(
            ["tsp", "-I", "file", original, "-P", "pcradjust", "--bitrate", f"{rate * scale:.0f}"]
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


def system(layout: str):
    """A builder for one tables-only layout."""

    def build(path: str) -> str:
        synthetic_system(path, layout)
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
# ("fails", check) for a check other than tstd that must fail, or (check, violations)
# or (check, "pass") for the system buffers.
CASES = [
    ("as captured", lambda _path: FIXTURE, "pass"),
    ("restamped 1x", restamp(1.0), "pass"),
    ("0.7x", restamp(0.7), {"EB underflow", "B underflow"}),
    ("4x", restamp(4.0), {"TB overflow", "B overflow", "held over 1 s"}),
    ("15x", restamp(15.0), {"TB overflow", "B overflow", "held over 1 s"}),
    ("level defaults", encoded(False), "pass"),
    ("MB burst", restamp(4.0, encoded(False)), {"MB overflow"}),
    ("AAC reference", encoded(True), "pass"),
    ("synthetic", built("synthetic"), "pass"),
    ("two AUs, one PES", built("two AUs, one PES"), "several access units"),
    ("adaptation burst", built("adaptation burst"), {"TB overflow"}),
    ("duplicate packet", built("duplicate packet"), "pass"),
    ("teletext burst", built("teletext burst"), {"TB overflow"}),
    ("PCR PID mismatch", built("PCR PID mismatch"), ("fails", "pcr-presence")),
    ("table burst", built("table burst"), ("system-buffers", {"TBsys overflow"})),
    ("Bsys burst", built("Bsys burst"), ("system-buffers", {"Bsys overflow"})),
    (
        "TBsys kept busy",
        built("TBsys kept busy"),
        ("system-buffers", {"TBsys not emptied within 1 s", "Bsys overflow"}),
    ),
    ("duplicate tables", built("duplicate tables"), "pass"),
    ("moved PMT burst", built("moved PMT burst"), ("system-buffers", {"Bsys overflow"})),
    ("split PMT burst", built("split PMT burst"), ("system-buffers", {"Bsys overflow"})),
    ("rate-fall burst", system("rate-fall burst"), ("system-buffers", {"Bsys overflow"})),
    ("rate fall", system("rate fall"), ("system-buffers", "pass")),
    ("PCR PID move", system("PCR PID move"), ("system-buffers", "pass")),
    ("PCR switch in burst", system("PCR switch in burst"), ("system-buffers", {"Bsys overflow"})),
    ("PCR move, new base", system("PCR move, new base"), ("system-buffers", "pass")),
    ("program removal", system("program removal"), ("system-buffers", "pass")),
    ("program addition", system("program addition"), ("system-buffers", "pass")),
    ("program restored", system("program restored"), ("system-buffers", "pass")),
    ("PCR move, jump", system("PCR move, jump"), ("system-buffers", {"PCR PID switch without a common time base"})),
    ("unclocked tables", system("unclocked tables"), ("system-buffers", {"outside a clocked time base"})),
    ("straddling audio", audio(4, STRADDLE_S), "pass"),
    ("mid-packet overflow", audio(7, MID_PACKET_S), {"B overflow"}),
]

def grade(path: str) -> dict[str, compliance.Check]:
    """compliance.py's verdicts on one file, by check name."""
    return {check.name: check for check in compliance.analyze(path)}


def main(tmp: str) -> int:
    """Run every case and exit non-zero if any verdict is not the expected one."""
    failed = 0
    for n, (name, build, expected) in enumerate(CASES):
        checks = grade(build(os.path.join(tmp, f"{n}.ts")))
        with open(os.path.join(tmp, f"{n}.json"), "w") as report:
            json.dump({key: {"status": value.status.value, "detail": value.detail, "metrics": value.metrics}
                       for key, value in checks.items()}, report, indent=2)
        check = checks["tstd"]
        streams = check.metrics.get("streams", {})
        seen = {v for s in streams.values() for v in s["violations"]}
        system = checks["system-buffers"]
        if expected == "pass":
            # Not vacuous: every stream must have had access units to grade.
            ok = check.status == compliance.Status.PASS and all(s["access_units"] for s in streams.values())
            ok = ok and checks["pcr-presence"].status == compliance.Status.PASS
            ok = ok and system.status == compliance.Status.PASS
        elif isinstance(expected, tuple) and expected[0] == "fails":
            check = checks[expected[1]]
            ok = check.status == compliance.Status.FAIL
        elif isinstance(expected, tuple) and expected[1] == "pass":
            check = system
            ok = system.status == compliance.Status.PASS
        elif isinstance(expected, tuple):
            check = system
            ok = expected[1] <= {v for p in system.metrics.get("programs", {}).values() for v in p["violations"]}
        elif isinstance(expected, str):
            ok = any(expected in why for why in check.metrics.get("refused", {}).values())
        else:
            ok = expected <= seen
        failed += not ok
        if isinstance(expected, tuple) and expected[1] == "pass":
            want = f"{expected[0]} pass"
        elif isinstance(expected, tuple):
            want = f"{expected[1]} fails" if expected[0] == "fails" else ", ".join(sorted(expected[1]))
        else:
            want = expected if isinstance(expected, str) else ", ".join(sorted(expected))
        print(f"  {'ok  ' if ok else 'FAIL'}  {name:<18} want {want:<40} got {check.detail}")
    if failed:
        print(f"tstd controls: {failed} of {len(CASES)} cases wrong", file=sys.stderr)
        return 1
    print("tstd controls: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
