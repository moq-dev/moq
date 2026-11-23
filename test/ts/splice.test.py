#!/usr/bin/env python3
"""Controls for the generated encoder switch: one splice, every PID broken once, no indicator set."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from splice import AUDIO_LEAD, KEY, KEY_SLACK, NULL, SIZE, WRAP, flags, payload_at, pcr, pid, pusi, stamp, streams

DIR = Path(__file__).resolve().parent
STEP = 16844.5


def generate(direction: str) -> list[bytes]:
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "splice.ts"
        subprocess.run(
            [sys.executable, str(DIR / "splice.py"), str(out), direction, "--duration", "6"],
            check=True,
            capture_output=True,
        )
        data = out.read_bytes()
    return [data[i : i + SIZE] for i in range(0, len(data), SIZE)]


def breaks(pkts: list[bytes]) -> dict[int, list[int]]:
    """Where each PID's continuity counter fails to advance by one, by packet index."""
    last: dict[int, int] = {}
    found: dict[int, list[int]] = {}
    for k, p in enumerate(pkts):
        q = pid(p)
        if q == NULL or payload_at(p) is None:
            continue
        cc = p[3] & 0x0F
        if q in last and cc != (last[q] + 1) & 0x0F:
            found.setdefault(q, []).append(k)
        last[q] = cc
    return found


def seconds(a: int, b: int) -> float:
    """b - a on the 90 kHz clock, the shorter way round its wrap."""
    return ((b - a + WRAP // 2) % WRAP - WRAP // 2) / 90000


class Splice(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.clips = {direction: generate(direction) for direction in ("back", "forward")}

    def splice_at(self, pkts: list[bytes]) -> int:
        return min(k for ks in breaks(pkts).values() for k in ks)

    def test_every_pid_breaks_once_at_the_splice(self):
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                found = breaks(pkts)
                pids = {pid(p) for p in pkts} - {NULL}
                self.assertEqual(set(found), pids)
                self.assertTrue(all(len(ks) == 1 for ks in found.values()), found)
                # Each PID's first packet after the switch, so all within a few table repeats.
                at = self.splice_at(pkts)
                self.assertLess(max(ks[0] for ks in found.values()) - at, len(pkts) // 10)

    def test_nothing_sets_discontinuity_indicator(self):
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                self.assertFalse(any(flags(p) & 0x80 for p in pkts))

    def test_the_clock_steps_by_the_step_in_its_direction(self):
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                _, video, _ = streams(pkts)
                at = self.splice_at(pkts)
                clocks = [(k, pcr(p)) for k, p in enumerate(pkts) if pid(p) == video and pcr(p) is not None]
                before = next(c for k, c in reversed(clocks) if k < at)
                after = next(c for k, c in clocks if k >= at)
                expected = -STEP if direction == "back" else STEP
                self.assertAlmostEqual(seconds(before, after), expected, delta=1.0)

    def test_the_second_encode_joins_mid_gop(self):
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                _, video, _ = streams(pkts)
                at = self.splice_at(pkts)
                starts = [k for k, p in enumerate(pkts) if k >= at and pid(p) == video and pusi(p)]
                first = starts[0]
                key = next(k for k in starts if flags(pkts[k]) & 0x40)
                self.assertNotEqual(first, key)
                self.assertAlmostEqual(seconds(stamp(pkts[first]), stamp(pkts[key])), KEY, delta=KEY_SLACK)

    def test_no_pid_arrives_in_a_burst(self):
        # FFmpeg writes a PES back to back, a keyframe's hundreds of packets. Each leaves 30 of its
        # 188 bytes in the video's 512-byte transport buffer, which drains at 8.4 Mb/s of the
        # multiplex's 10, so a run of 17 overflows it.
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                _, _, es = streams(pkts)
                for q, _ in es:
                    longest = run = 0
                    for p in pkts:
                        run = run + 1 if pid(p) == q else 0
                        longest = max(longest, run)
                    self.assertLessEqual(longest, 16, f"PID 0x{q:04x}")

    def test_audio_arrives_shortly_before_it_decodes(self):
        # Sent with the video's lead, it would overflow its 3,584-byte buffer.
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                _, video, es = streams(pkts)
                audio = next(q for q, _ in es if q != video)
                at = self.splice_at(pkts)
                clocks = [(k, pcr(p)) for k, p in enumerate(pkts[:at]) if pcr(p) is not None]
                (k0, c0), (k1, c1) = clocks[0], clocks[-1]
                leads = [
                    seconds(c0 + (k - k0) * (c1 - c0) / (k1 - k0), stamp(p))
                    for k, p in enumerate(pkts[k0:k1], k0)
                    if pid(p) == audio and stamp(p) is not None
                ]
                self.assertGreater(min(leads), 0)
                self.assertLess(max(leads), AUDIO_LEAD + 0.01)

    def test_tables_are_identical_either_side(self):
        for direction, pkts in self.clips.items():
            with self.subTest(direction):
                pmt, _, _ = streams(pkts)
                at = self.splice_at(pkts)
                for q in (0x0000, 0x0011, pmt):
                    before = next(p for p in reversed(pkts[:at]) if pid(p) == q and pusi(p))
                    after = next(p for p in pkts[at:] if pid(p) == q and pusi(p))
                    self.assertEqual(before[4:], after[4:], f"PID 0x{q:04x}")


if __name__ == "__main__":
    unittest.main()
