#!/usr/bin/env python3
"""Split generated PES headers into adjacent null slots without moving PCRs."""

import argparse
import collections
import json
from pathlib import Path

SIZE = 188


def pid(packet):
    return ((packet[1] & 31) << 8) | packet[2]


def payload(packet):
    control = (packet[3] >> 4) & 3
    if control == 1:
        return packet[4:]
    if control == 3:
        start = 5 + packet[4]
        if start <= SIZE:
            return packet[start:]
    return b""


def packet(original, body, start, counter):
    out = bytearray(b"\xff" * SIZE)
    out[:4] = original[:4]
    out[1] = (out[1] & ~0x40) | (0x40 if start else 0)
    if len(body) == 184:
        out[3] = 0x10 | (counter & 15)
        out[4:] = body
        return bytes(out)
    out[3] = 0x30 | (counter & 15)
    out[4] = 183 - len(body)
    if out[4]:
        out[5] = 0
        # Keep PCR and every other adaptation field at its original byte position.
        if original[3] & 0x20:
            out[5 : 5 + original[4]] = original[5 : 5 + original[4]]
    out[SIZE - len(body) :] = body
    return bytes(out)


def fragment(data):
    if len(data) % SIZE or any(data[at] != 0x47 for at in range(0, len(data), SIZE)):
        raise ValueError("expected aligned 188-byte TS packets")
    packets = [data[at : at + SIZE] for at in range(0, len(data), SIZE)]
    extra = collections.Counter()
    counts = collections.Counter()
    splits = collections.defaultdict(set)
    classes = set()
    at = 0
    while at < len(packets):
        original = packets[at]
        current = bytearray(original)
        route = pid(original)
        current[3] = (current[3] & 0xF0) | ((current[3] + extra[route]) & 15)
        packets[at] = bytes(current)
        body = payload(original)
        media = len(body) >= 9 and body[:3] == b"\0\0\1" and body[3] & 0xE0 == 0xC0
        video = len(body) >= 9 and body[:3] == b"\0\0\1" and body[3] & 0xF0 == 0xE0
        if original[1] & 0x40 and (media or video):
            # FFmpeg emits a PES's packets in a burst, then nulls. Find a slot
            # before the next start on this PID and shift payloads into it, keeping
            # all other PIDs and every PCR at the same byte position.
            slot = at + 1
            while slot < len(packets):
                candidate = packets[slot]
                if pid(candidate) == 0x1FFF:
                    break
                if pid(candidate) == route and candidate[1] & 0x40:
                    slot = len(packets)
                    break
                slot += 1
            if slot < len(packets):
                header = 9 + body[8]
                if body[6] & 0xC0 != 0x80 or header > len(body):
                    raise ValueError("generated source lacks a complete clear PES header")
                split = 1 + counts[route] % (header - 1)
                counter = current[3] & 15
                packets[at] = packet(original, body[:split], True, counter)
                carried = body[split:]
                for follow in range(at + 1, slot):
                    original_follow = packets[follow]
                    follow_pid = pid(original_follow)
                    follow_counter = (original_follow[3] + extra[follow_pid]) & 15
                    if follow_pid == route and payload(original_follow):
                        capacity = len(payload(original_follow))
                        carried += payload(original_follow)
                        packets[follow] = packet(original_follow, carried[:capacity], False, follow_counter)
                        carried = carried[capacity:]
                        counter = follow_counter
                    else:
                        updated = bytearray(original_follow)
                        updated[3] = (updated[3] & 0xF0) | follow_counter
                        packets[follow] = bytes(updated)
                base = bytes([0x47, (route >> 8) & 31, route & 255, 0x10]) + b"\xff" * 184
                packets[slot] = packet(base, carried, False, counter + 1)
                extra[route] += 1
                counts[route] += 1
                splits[route].add(split)
                classes.add("video" if video else "audio")
                at = slot
        at += 1
    if classes != {"audio", "video"}:
        raise ValueError(f"fragmented headers must cover generated audio and video, got {classes}")
    return b"".join(packets), {
        "counts": dict(counts),
        "splits": {route: sorted(values) for route, values in splits.items()},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    data, report = fragment(args.source.read_bytes())
    args.output.write_bytes(data)
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print("fragmented PES headers:", report["counts"])


if __name__ == "__main__":
    main()
