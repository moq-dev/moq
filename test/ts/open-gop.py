#!/usr/bin/env python3
"""Grade whether an open-GOP H.264 stream keeps its leading pictures through a round-trip.

    ./open-gop.py source.ts capture.ts

An open-GOP recovery point is a non-IDR I picture flagged by a recovery-point SEI. The
pictures that follow it in decode order but are presented before it, its leading
pictures, reference the previous GOP, so only a viewer that already holds that GOP can
decode them. A viewer playing continuously needs every one of them, in decode order.
Dropping them is right only at a tune-in, which the transport cannot see, so the
round-trip has to hand them all on.

Access units are matched by their slice data. That makes the comparison indifferent to
the parameter sets and delimiters the round-trip may re-insert and to the timestamp
rebase, and exact about order: the capture must be one contiguous run of the source's
access units, starting at a random-access point.

Hard checks: the source is open GOP with leading pictures (else nothing here is tested),
the capture is that contiguous run with DTS strictly increasing, and every captured
picture keeps the presentation offset the source gave it, which is what makes a leading
picture leading. Shape checks, reported and failing only under `--strict`: whether each
exported random-access point carries the random_access_indicator and keeps its
recovery-point SEI, and whether any access unit is stamped to decode after it presents.

Exit status is 0 when every hard check passes, 1 otherwise.
"""

import argparse
import hashlib
import sys
from dataclasses import dataclass

PKT = 188
SYNC = 0x47
HARD, SHAPE = "hard", "shape"
AVC_STREAM_TYPE = 0x1B
NAL_SLICE, NAL_IDR, NAL_SEI = 1, 5, 6
SEI_RECOVERY_POINT = 6
TS_WRAP = 1 << 33
# Two recovery points with leading pictures is the least that crosses a GOP boundary
# the fixture did not start at, which the first GOP after an IDR never does.
MIN_RECOVERY_POINTS = 2
# The round-trip carries time in microseconds, so a 90 kHz tick can come back one off.
PTS_TOLERANCE = 1


@dataclass
class AccessUnit:
    key: bytes  # digest of the slice NAL units, which the round-trip must not touch
    pts: int
    dts: int
    rai: bool  # random_access_indicator on the packet that starts the PES
    idr: bool
    recovery: bool  # carries a recovery-point SEI


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def packets(path):
    with open(path, "rb") as f:
        data = f.read()
    if not data or len(data) % PKT or data[0] != SYNC:
        fail(f"{path} is not a 188-byte-aligned transport stream")
    return [data[i : i + PKT] for i in range(0, len(data), PKT)]


def pid_of(pkt):
    return ((pkt[1] & 0x1F) << 8) | pkt[2]


def payload(pkt):
    """The packet's payload, or None when it has none."""
    afc = (pkt[3] >> 4) & 0x3
    if not afc & 0x1:
        return None
    start = 4 + (1 + pkt[4] if afc & 0x2 else 0)
    return pkt[start:] if start < PKT else None


def section(pkt):
    """The PSI section starting in this packet (PAT and PMT fit in one here)."""
    body = payload(pkt)
    if body is None or not pkt[1] & 0x40:
        return None
    return body[1 + body[0] :]


def video_pid(path, pkts):
    pmt = None
    for pkt in pkts:
        pid = pid_of(pkt)
        if pmt is None and pid == 0 and (sec := section(pkt)):
            # First program entry after the 8-byte header; program 0 is the NIT.
            for i in range(8, 3 + (((sec[1] & 0x0F) << 8) | sec[2]) - 4, 4):
                if (sec[i] << 8 | sec[i + 1]) != 0:
                    pmt = ((sec[i + 2] & 0x1F) << 8) | sec[i + 3]
                    break
        elif pid == pmt and (sec := section(pkt)):
            end = 3 + (((sec[1] & 0x0F) << 8) | sec[2]) - 4
            i = 12 + (((sec[10] & 0x0F) << 8) | sec[11])
            while i + 5 <= end:
                if sec[i] == AVC_STREAM_TYPE:
                    return ((sec[i + 1] & 0x1F) << 8) | sec[i + 2]
                i += 5 + (((sec[i + 3] & 0x0F) << 8) | sec[i + 4])
            fail(f"{path}: the PMT lists no H.264 stream")
    fail(f"{path}: no PAT/PMT found")


def timestamp(b):
    return ((b[0] >> 1) & 0x7) << 30 | b[1] << 22 | (b[2] >> 1) << 15 | b[3] << 7 | b[4] >> 1


def nal_units(es):
    """NAL units of an Annex-B elementary stream, without their start codes."""
    starts = []
    i = es.find(b"\x00\x00\x01")
    while i >= 0:
        starts.append(i + 3)
        i = es.find(b"\x00\x00\x01", i + 3)
    for n, start in enumerate(starts):
        end = starts[n + 1] - 3 if n + 1 < len(starts) else len(es)
        nal = es[start:end].rstrip(b"\x00")
        if nal:
            yield nal


def has_recovery_point(nal):
    """True if an SEI NAL carries a recovery-point message (payload type 6)."""
    rbsp = nal[1:].replace(b"\x00\x00\x03", b"\x00\x00")
    i = 0
    try:
        while i < len(rbsp) and rbsp[i] != 0x80:
            kind = size = 0
            while rbsp[i] == 0xFF:
                kind, i = kind + 255, i + 1
            kind, i = kind + rbsp[i], i + 1
            while rbsp[i] == 0xFF:
                size, i = size + 255, i + 1
            size, i = size + rbsp[i], i + 1
            if kind == SEI_RECOVERY_POINT:
                return True
            i += size
    except IndexError:
        fail("an SEI NAL ends inside a message header")
    return False


def access_units(path):
    """One AccessUnit per video PES (the exporter and ffmpeg both write one AU per PES)."""
    pkts = packets(path)
    pid = video_pid(path, pkts)
    pes = []
    for pkt in pkts:
        if pid_of(pkt) != pid or (body := payload(pkt)) is None:
            continue
        if pkt[1] & 0x40:
            rai = bool(pkt[3] & 0x20 and pkt[4] and pkt[5] & 0x40)
            pes.append((rai, bytearray(body)))
        elif pes:
            pes[-1][1].extend(body)
    units = []
    for rai, b in pes:
        if b[:3] != b"\x00\x00\x01" or not b[7] & 0x80:
            fail(f"{path}: a video PES carries no PTS")
        pts = timestamp(b[9:14])
        dts = timestamp(b[14:19]) if b[7] & 0x40 else pts
        digest, idr, recovery = hashlib.sha1(), False, False
        for nal in nal_units(bytes(b[9 + b[8] :])):
            kind = nal[0] & 0x1F
            if kind in (NAL_SLICE, NAL_IDR):
                digest.update(nal)
            idr |= kind == NAL_IDR
            recovery |= kind == NAL_SEI and has_recovery_point(nal)
        units.append(AccessUnit(digest.digest(), pts, dts, rai, idr, recovery))
    return units


def delta(a, b):
    """a - b on the 33-bit timestamp circle."""
    return (a - b + TS_WRAP // 2) % TS_WRAP - TS_WRAP // 2


def is_rap(unit):
    return unit.idr or unit.recovery


def leading(units):
    """Per random-access point index, the indices of its leading pictures."""
    out, rap = {}, None
    for i, unit in enumerate(units):
        if is_rap(unit):
            rap = i
            out[rap] = []
        elif rap is not None and delta(unit.pts, units[rap].pts) < 0:
            out[rap].append(i)
    return out


def main():
    ap = argparse.ArgumentParser(description="Grade open-GOP leading pictures across a TS round-trip.")
    ap.add_argument("source", help="TS that was published")
    ap.add_argument("capture", help="TS the subscriber exported")
    ap.add_argument("--strict", action="store_true", help="fail on shape checks too")
    args = ap.parse_args()

    src = access_units(args.source)
    cap = access_units(args.capture)
    results = []

    def check(name, severity, ok, detail):
        results.append((name, severity, ok, detail))

    src_leading = leading(src)
    open_rps = [i for i, lead in src_leading.items() if lead and not src[i].idr]
    check(
        "fixture",
        HARD,
        len(open_rps) >= MIN_RECOVERY_POINTS,
        f"source: {len(src)} AUs, {len(open_rps)} non-IDR recovery point(s) with "
        f"{sum(len(src_leading[i]) for i in open_rps)} leading picture(s)",
    )

    # Where the capture starts in the source. Frames can repeat in principle, so take the
    # first candidate the whole run agrees with rather than the first match on one key.
    starts = [i for i, unit in enumerate(src) if cap and unit.key == cap[0].key]
    start = next((i for i in starts if [u.key for u in src[i : i + len(cap)]] == [u.key for u in cap]), None)
    if start is None:
        detail = "the capture's first AU is not in the source"
        if starts:
            s, prefix = starts[0], 0
            while prefix < len(cap) and s + prefix < len(src) and src[s + prefix].key == cap[prefix].key:
                prefix += 1
            want = s + prefix
            kind = (
                "a leading picture"
                if any(want in lead for lead in src_leading.values())
                else "a random-access point"
                if want < len(src) and is_rap(src[want])
                else "a picture"
            )
            detail = (
                f"capture AU {prefix} of {len(cap)} departs from the source; "
                f"source AU {want} ({kind}) is missing or moved"
            )
        check("decode-order", HARD, False, detail)
    else:
        backwards = [i for i in range(1, len(cap)) if delta(cap[i].dts, cap[i - 1].dts) <= 0]
        ok = is_rap(src[start]) and not backwards
        detail = f"{len(cap)} AUs, source AUs {start}..{start + len(cap) - 1} in order"
        if not is_rap(src[start]):
            detail += f"; but starts at source AU {start}, which is not a random-access point"
        if backwards:
            detail += f"; DTS does not advance at {len(backwards)} AU(s), first at {backwards[0]}"
        check("decode-order", HARD, ok, detail)

    if start is not None:
        # Leading pictures are defined by presentation offset, so check the offset of every
        # captured picture rather than only counting the ones that still come out leading.
        moved = [
            i
            for i in range(len(cap))
            if abs(delta(cap[i].pts, cap[0].pts) - delta(src[start + i].pts, src[start].pts)) > PTS_TOLERANCE
        ]
        # Keyed on the source's random-access points, so an exporter that strips the SEI
        # fails the SEI check below rather than hiding its leading pictures from this one.
        kept = {
            i - start: [j - start for j in lead if j < start + len(cap)]
            for i, lead in src_leading.items()
            if lead and start <= i < start + len(cap)
        }
        # A capture cut short can end inside a GOP; only a leading picture it reached is owed.
        owed = sum(len(lead) for lead in kept.values())
        found = sum(1 for i, lead in kept.items() for j in lead if delta(cap[j].pts, cap[i].pts) < 0)
        # Only a recovery point with a captured leading picture tests anything.
        crossed = len([i for i, lead in kept.items() if lead and not src[start + i].idr])
        ok = not moved and found == owed and crossed >= MIN_RECOVERY_POINTS
        detail = f"{found}/{owed} leading picture(s) across {crossed} recovery point(s)"
        if moved:
            detail += f"; {len(moved)} AU(s) presented away from the source's offset, first at {moved[0]}"
        if crossed < MIN_RECOVERY_POINTS:
            detail += f"; the capture reaches leading pictures at fewer than {MIN_RECOVERY_POINTS} recovery points"
        check("leading-pictures", HARD, ok, detail)

        raps = [i for i in range(len(cap)) if is_rap(src[start + i])]
        flagged = [i for i in raps if cap[i].rai]
        stray = [i for i in range(len(cap)) if cap[i].rai and not is_rap(src[start + i])]
        detail = f"{len(flagged)}/{len(raps)} random-access AU(s) flagged"
        if stray:
            detail += f"; {len(stray)} non-random-access AU(s) flagged too"
        check("random-access-indicator", SHAPE, len(flagged) == len(raps) and not stray, detail)

        rps = [i for i in raps if src[start + i].recovery]
        survived = [i for i in rps if cap[i].recovery]
        check("recovery-point-sei", SHAPE, len(survived) == len(rps), f"{len(survived)}/{len(rps)} kept the SEI")

        late = [i for i in range(len(cap)) if delta(cap[i].dts, cap[i].pts) > 0]
        worst = max((delta(cap[i].dts, cap[i].pts) for i in late), default=0)
        check(
            "dts-before-pts",
            SHAPE,
            not late,
            f"{len(late)}/{len(cap)} AU(s) decode after they present, worst by {worst / 90:.1f} ms",
        )

    print("### open-GOP report")
    failed = 0
    for name, severity, ok, detail in results:
        gating = severity == HARD or args.strict
        status = "PASS" if ok else ("FAIL" if gating else "WARN")
        failed += not ok and gating
        print(f"  {status}  {name:<24} [{severity}]  {detail}")
    print(f"{failed} check(s) failed" if failed else "open-gop: PASS")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
