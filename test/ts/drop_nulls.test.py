#!/usr/bin/env python3
"""Controls for dropping a generated clip's nulls."""

import unittest

from drop_nulls import NULL, SIZE, drop, pid

VIDEO = 0x100
AUDIO = 0x102


def packet(route, counter):
    return bytes([0x47, route >> 8, route & 0xFF, 0x10 | (counter & 15)]) + bytes([counter >> 4]) * (SIZE - 4)


def clip():
    """Nulls crowded into the second half, an audio packet every tenth slot throughout."""
    packets = []
    counters = {VIDEO: 0, AUDIO: 0, NULL: 0}
    for index in range(2000):
        if index % 10 == 0:
            route = AUDIO
        elif index >= 1000 and index % 2:
            route = NULL
        else:
            route = VIDEO
        packets.append(packet(route, counters[route]))
        counters[route] += 1
    return packets


def times(packets, route):
    """Where each of `route`'s packets sits, as a fraction of the clip."""
    return [index / len(packets) for index, p in enumerate(packets) if pid(p) == route]


class DropNulls(unittest.TestCase):
    def test_other_pids_keep_their_padded_time(self):
        padded = clip()
        result = drop(b"".join(padded), VIDEO)
        out = [result[at : at + SIZE] for at in range(0, len(result), SIZE)]
        slot = 1 / len(out)

        # Filtering the nulls alone would send the first half's audio up to a seventh of the
        # clip late: the bug this guards.
        filtered = [p for p in padded if pid(p) != NULL]
        late = max(new - old for new, old in zip(times(filtered, AUDIO), times(padded, AUDIO)))
        self.assertGreater(late, 0.1)

        for new, old in zip(times(out, AUDIO), times(padded, AUDIO), strict=True):
            self.assertGreaterEqual(new, old - slot)
            self.assertLessEqual(new, old + slot)

    def test_every_packet_survives_in_its_pids_order(self):
        padded = clip()
        result = drop(b"".join(padded), VIDEO)
        out = [result[at : at + SIZE] for at in range(0, len(result), SIZE)]
        self.assertNotIn(NULL, {pid(p) for p in out})
        for route in (VIDEO, AUDIO):
            self.assertEqual([p for p in out if pid(p) == route], [p for p in padded if pid(p) == route])

    def test_refuses_a_clip_without_the_absorbing_pid(self):
        with self.assertRaises(ValueError):
            drop(packet(AUDIO, 0) + packet(NULL, 0), VIDEO)


if __name__ == "__main__":
    unittest.main()
