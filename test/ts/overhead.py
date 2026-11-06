#!/usr/bin/env python3
"""Grade whether a round-trip spends no more packets than its source on clocks and audio.

    ./overhead.py source.ts capture.ts

A source that fills its multiplex with no null packets leaves the export no room to spend
more than it does. Each audio PES costs a header and a part-filled last packet, and a PCR
outside a media packet costs a packet of its own (moq-dev/moq#5362).

Both are counted on each stream: the packets on every PES PID that is not video (stream ids
0xC0-0xDF, and private stream 1, which DVB AC-3 uses), and the packets on any PID that carry
a PCR and no payload. The capture joins after the source starts, so each PID's count is scaled
by how much of the source's elementary stream on that PID the capture carries.

Exit status is 0 when the capture spends no more than the source, 1 otherwise.
"""

import sys
from collections import defaultdict

PKT = 188
SYNC = 0x47
NULL_PID = 0x1FFF


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def count(path: str) -> tuple[dict[int, int], dict[int, int], dict[int, int], set[int]]:
    """Per PID: its packets, its elementary bytes, and its packets carrying only a PCR; and
    the PIDs whose PES are audio."""
    packets: dict[int, int] = defaultdict(int)
    elementary: dict[int, int] = defaultdict(int)
    clocks: dict[int, int] = defaultdict(int)
    audio: set[int] = set()
    with open(path, "rb") as f:
        data = f.read()
    if not data or len(data) % PKT:
        fail(f"{path} is not a whole number of {PKT}-byte packets")
    for offset in range(0, len(data), PKT):
        packet = data[offset : offset + PKT]
        if packet[0] != SYNC:
            fail(f"{path} loses sync at byte {offset}")
        pid = (packet[1] & 0x1F) << 8 | packet[2]
        if pid == NULL_PID:
            continue
        packets[pid] += 1
        control = packet[3] >> 4 & 3
        field = 1 + packet[4] if control & 2 else 0
        if control & 2 and packet[4] and packet[5] & 0x10 and not control & 1:
            clocks[pid] += 1
        if not control & 1:
            continue
        body = packet[4 + field :]
        if packet[1] & 0x40 and body[:3] == b"\0\0\1" and len(body) >= 9:
            if 0xC0 <= body[3] <= 0xDF or body[3] == 0xBD:
                audio.add(pid)
            body = body[9 + body[8] :]
        elementary[pid] += len(body)
    return packets, elementary, clocks, audio


def main() -> int:
    if len(sys.argv) != 3:
        fail("usage: overhead.py source.ts capture.ts")
    source, capture = count(sys.argv[1]), count(sys.argv[2])
    rows = [(pid, "audio", source[0][pid], capture[0][pid]) for pid in sorted(source[3] | capture[3])]
    rows += [(pid, "PCR only", source[2][pid], capture[2][pid]) for pid in sorted(source[2].keys() | capture[2].keys())]
    spent = [0.0, 0.0]
    print("### packets spent on clocks and audio (source -> capture, scaled to the source)")
    for pid, kind, before, after in rows:
        if not source[1][pid] or not capture[1][pid]:
            fail(f"PID {pid} carries no elementary stream in both the source and the capture")
        share = capture[1][pid] / source[1][pid]
        print(f"  PID {pid:<5} {kind:<9} {before:>8} {after / share:>10.0f}  ({after} for {share:.1%} of it)")
        spent[0] += before
        spent[1] += after / share
    verdict = "PASS" if spent[1] <= spent[0] else "FAIL"
    print(f"  {verdict}  {spent[1]:.0f} packets against the source's {spent[0]:.0f}")
    return 0 if verdict == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
