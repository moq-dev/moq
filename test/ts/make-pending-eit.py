#!/usr/bin/env python3
"""Rewrite an EIT to a version that is not yet in force.

Generic section syntax carries a `current_next_indicator`: set, the section describes
the state in force; clear, it describes a revision that applies later. A receiver, and
anything relaying SI, must keep serving the current version until the successor becomes
current, so a stream in which the *only* EIT on the wire is a pending one is the case
that separates a correct implementation from one that stores whatever it last parsed.

`tsp -P eitinject` cannot produce it: it re-derives present/following from the event
list and stamps its own version, ignoring `version` and `current` in the input XML. So
this patches the generated stream instead — bumping the version and clearing
`current_next_indicator` over a trailing window, then recomputing the section CRC so
the result is a legal stream and a rejection downstream means the guard fired rather
than the section being malformed.

Only sections that lie wholly within one TS packet are patched. Refusing the rest is
deliberate: silently mangling a section that spans packets would produce a fixture that
tests the wrong thing, and EIT p/f at fixture sizes always fits.

Usage:
  make-pending-eit.py [options] <input.ts> <output.ts>

  --pid N            SI PID to patch (default 0x0012)
  --table-id N       table_id to patch (default 0x4E, EIT p/f actual)
  --from-fraction F  patch sections after this fraction of the file (default 0.5)
  --version-bump N   add this to the version, modulo 32 (default 1)
  --quiet            suppress the summary
"""

import argparse
import sys

PACKET = 188
SYNC = 0x47


def crc32_mpeg(data: bytes) -> int:
    """ISO 13818-1 Annex B: poly 0x04C11DB7, all ones in, MSB first, no final inversion."""
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if crc & 0x80000000 else (crc << 1) & 0xFFFFFFFF
    return crc


def parse_int(value: str) -> int:
    return int(value, 0)


def section_starts(ts: bytes | bytearray, pid: int):
    for off in range(0, len(ts) - PACKET + 1, PACKET):
        pkt = ts[off : off + PACKET]
        if pkt[0] != SYNC or ((pkt[1] & 0x1F) << 8 | pkt[2]) != pid or not pkt[1] & 0x40:
            continue
        if not pkt[3] & 0x10:
            continue
        body = 4 + (1 + pkt[4] if pkt[3] & 0x20 else 0)
        if body >= PACKET:
            continue
        sec = body + 1 + pkt[body]  # past the pointer_field
        while sec + 3 <= PACKET and pkt[sec] != 0xFF:
            length = 3 + ((pkt[sec + 1] & 0x0F) << 8 | pkt[sec + 2])
            yield off, sec, length
            if sec + length > PACKET:
                break
            sec += length


def main() -> int:
    ap = argparse.ArgumentParser(add_help=False)
    ap.add_argument("--pid", type=parse_int, default=0x0012)
    ap.add_argument("--table-id", type=parse_int, default=0x4E)
    ap.add_argument("--from-fraction", type=float, default=0.5)
    ap.add_argument("--version-bump", type=int, default=1)
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("-h", "--help", action="store_true")
    ap.add_argument("args", nargs="*")
    opts = ap.parse_args()

    if opts.help or len(opts.args) != 2:
        print(__doc__.strip(), file=sys.stderr if not opts.help else sys.stdout)
        return 0 if opts.help else 2
    if not 0.0 <= opts.from_fraction < 1.0:
        print(f"error: --from-fraction must be in [0, 1): {opts.from_fraction}", file=sys.stderr)
        return 2

    with open(opts.args[0], "rb") as fh:
        ts = bytearray(fh.read())
    if len(ts) < PACKET or len(ts) % PACKET or ts[0] != SYNC:
        print(f"error: {opts.args[0]} is not a 188-byte-aligned transport stream", file=sys.stderr)
        return 1

    start = int(len(ts) // PACKET * opts.from_fraction) * PACKET
    patched = spanning = matched = 0

    for off, sec, length in section_starts(ts, opts.pid):
        i = off + sec
        if ts[i] != opts.table_id:
            continue
        matched += 1
        if off < start:
            continue
        if sec + length > PACKET:
            spanning += 1
            continue
        if length < 8:
            print(f"error: malformed table 0x{opts.table_id:02X} section at byte {i}", file=sys.stderr)
            return 1

        version = (ts[i + 5] >> 1) & 0x1F
        ts[i + 5] = (ts[i + 5] & 0xC0) | (((version + opts.version_bump) % 32) << 1)  # current_next = 0
        crc = crc32_mpeg(bytes(ts[i : i + length - 4]))
        ts[i + length - 4 : i + length] = crc.to_bytes(4, "big")
        patched += 1

    if spanning:
        print(
            f"error: {spanning} section(s) of table 0x{opts.table_id:02X} span more than one "
            f"packet; this tool patches single-packet sections only",
            file=sys.stderr,
        )
        return 1
    if not patched:
        print(
            f"error: no section of table 0x{opts.table_id:02X} on PID 0x{opts.pid:04X} after "
            f"{opts.from_fraction:.0%} of the file ({matched} found before it)",
            file=sys.stderr,
        )
        return 1

    with open(opts.args[1], "wb") as fh:
        fh.write(ts)

    with open(opts.args[1], "rb") as fh:
        output = fh.read()
    current = bad_crc = verify_spanning = 0
    for off, sec, length in section_starts(output, opts.pid):
        i = off + sec
        if off < start or output[i] != opts.table_id:
            continue
        if sec + length > PACKET:
            verify_spanning += 1
            continue
        if length < 8:
            print(f"error: malformed output table 0x{opts.table_id:02X} section at byte {i}", file=sys.stderr)
            return 1
        current += output[i + 5] & 1
        bad_crc += crc32_mpeg(output[i : i + length]) != 0
    if verify_spanning or current or bad_crc:
        print(
            f"error: output verification failed for table 0x{opts.table_id:02X} after "
            f"{opts.from_fraction:.0%}: {current} current, {bad_crc} bad CRC, "
            f"{verify_spanning} spanning",
            file=sys.stderr,
        )
        return 1

    if not opts.quiet:
        print(
            f"### pending-version fixture: {patched} of {matched} section(s) of table "
            f"0x{opts.table_id:02X} on PID 0x{opts.pid:04X} rewritten"
        )
        print(f"### version +{opts.version_bump}, current_next_indicator cleared, CRC recomputed")
        tail = 100 - opts.from_fraction * 100
        print(f"### the last {tail:.0f}% of the stream carries no current EIT -> {opts.args[1]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
