#!/usr/bin/env python3
"""Drop a CBR clip's null packets, letting one PID absorb the time they leave.

Restamping the null-free packets at their average rate moves each by the running excess of
nulls before it, which in a VBR video's clip wanders by tens of milliseconds. Video sent
most of a second ahead absorbs that, but audio sent just in time can then arrive after it
decodes, and a stream carried at its placement, such as DVB AC-3, keeps it. So every other
PID's packets keep their time in the padded clip, and the absorbing PID's fill the slots
between, in its own order.
"""

import argparse
from pathlib import Path

SIZE = 188
NULL = 0x1FFF


def pid(packet):
    return ((packet[1] & 31) << 8) | packet[2]


def drop(data, absorb):
    """The packets of `data` without its nulls, each other than `absorb`'s at its padded time."""
    if len(data) % SIZE or any(data[at] != 0x47 for at in range(0, len(data), SIZE)):
        raise ValueError("expected aligned 188-byte TS packets")
    total = len(data) // SIZE
    absorbing = []
    timed = []  # (padded index, packet)
    for index in range(total):
        packet = data[index * SIZE : (index + 1) * SIZE]
        route = pid(packet)
        if route == NULL:
            continue
        if route == absorb:
            absorbing.append(packet)
        else:
            timed.append((index, packet))
    if not absorbing:
        raise ValueError(f"no packets on PID {absorb:#x} to absorb the nulls")
    kept = len(absorbing) + len(timed)
    out = bytearray()
    a = t = 0
    for slot in range(kept):
        # Slot `slot` of `kept` starts no earlier than padded index `i` of `total` once
        # slot * total >= i * kept: the same fraction of the clip.
        due = t < len(timed) and slot * total >= timed[t][0] * kept
        if due or a == len(absorbing):
            out += timed[t][1]
            t += 1
        else:
            out += absorbing[a]
            a += 1
    return bytes(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--pid", type=lambda value: int(value, 0), required=True, help="the PID that absorbs the nulls")
    args = parser.parse_args()
    args.output.write_bytes(drop(args.input.read_bytes(), args.pid))


if __name__ == "__main__":
    main()
