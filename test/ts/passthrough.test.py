import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location("passthrough", pathlib.Path(__file__).with_name("passthrough.py"))
passthrough = importlib.util.module_from_spec(spec)
spec.loader.exec_module(passthrough)

NULL = bytes([0x47, 0x1F, 0xFF, 0x10]) + bytes(184)


def packet(pid, n):
    """A packet on `pid` whose payload says which one it is."""
    return bytes([0x47, pid >> 8, pid & 0xFF, 0x10 | n & 0x0F]) + n.to_bytes(4, "big") + bytes(180)


# A table, a run of media with a null between each, and a PID no table lists, repeated.
SOURCE = []
for n in range(200):
    SOURCE.append(packet(0x1000 if n % 50 == 0 else 0x100, n))
    SOURCE.append(NULL)
    if n % 7 == 0:
        SOURCE.append(packet(0x1ABC, n))


class Place(unittest.TestCase):
    def test_a_suffix_is_complete(self):
        placement = passthrough.place(SOURCE, SOURCE[100:])
        self.assertEqual((placement.start, placement.gaps, placement.tail), (100, [], 0))
        self.assertTrue(placement.complete)

    def test_a_dropped_run_is_a_gap_at_its_position(self):
        capture = SOURCE[10:60] + SOURCE[90:]
        placement = passthrough.place(SOURCE, capture)
        self.assertEqual(placement.gaps, [(60, 30)])
        self.assertEqual(placement.missing_pids[0x1FFF], SOURCE[60:90].count(NULL))

    def test_a_missing_tail_is_counted(self):
        placement = passthrough.place(SOURCE, SOURCE[:-40])
        self.assertEqual((placement.gaps, placement.tail), ([], 40))
        self.assertFalse(placement.complete)

    def test_loss_ok_admits_gaps_but_not_a_lost_tail(self):
        gapped = passthrough.place(SOURCE, SOURCE[10:60] + SOURCE[90:])
        self.assertIsNone(passthrough.shortfall(gapped, loss_ok=True))
        self.assertIsNotNone(passthrough.shortfall(gapped, loss_ok=False))
        truncated = passthrough.place(SOURCE, SOURCE[10:60] + SOURCE[90:-40])
        self.assertIsNotNone(passthrough.shortfall(truncated, loss_ok=True))

    def test_a_rewritten_packet_fails(self):
        capture = list(SOURCE)
        capture[50] = capture[50][:100] + b"\xff" + capture[50][101:]
        with self.assertRaises(passthrough.Mismatch):
            passthrough.place(SOURCE, capture)

    def test_a_duplicated_run_fails(self):
        with self.assertRaises(passthrough.Mismatch):
            passthrough.place(SOURCE, SOURCE[:80] + SOURCE[60:80] + SOURCE[80:])

    def test_a_duplicated_null_does_not_pass_as_a_later_one(self):
        # Every other packet is an identical null, so the duplicate matches the source a
        # packet on; only the run after it shows it is out of place.
        with self.assertRaises(passthrough.Mismatch):
            passthrough.place(SOURCE, SOURCE[:41] + [NULL] + SOURCE[41:])

    def test_reordered_packets_fail(self):
        capture = list(SOURCE)
        capture[30], capture[32] = capture[32], capture[30]
        with self.assertRaises(passthrough.Mismatch):
            passthrough.place(SOURCE, capture)

    def test_a_capture_from_elsewhere_fails(self):
        with self.assertRaises(passthrough.Mismatch):
            passthrough.place(SOURCE, [packet(0x200, n) for n in range(20)])


def section_packet(pid, table_id, body, n):
    """A packet starting a PSI section, `n` in its stuffing so no two are equal."""
    length = len(body) + 4
    section = bytes([table_id, 0xB0 | length >> 8, length & 0xFF]) + body + bytes(4)
    payload = bytes([0]) + section + n.to_bytes(4, "big")
    return bytes([0x47, 0x40 | pid >> 8, pid & 0xFF, 0x10]) + payload + b"\xff" * (184 - len(payload))


def clock_packet(pid, ticks, rap, n):
    """A packet on `pid` carrying a PCR of `ticks`, flagged as a random access point when `rap`."""
    base, ext = divmod(ticks, 300)
    pcr = bytes([base >> 25 & 0xFF, base >> 17 & 0xFF, base >> 9 & 0xFF, base >> 1 & 0xFF])
    pcr += bytes([(base & 1) << 7 | 0x7E | ext >> 8, ext & 0xFF])
    field = bytes([7, 0x10 | (0x40 if rap else 0)]) + pcr
    return bytes([0x47, pid >> 8, pid & 0xFF, 0x30]) + field + n.to_bytes(4, "big") + bytes(172)


PAT = (1).to_bytes(2, "big") + bytes([0xC1, 0, 0]) + (1).to_bytes(2, "big") + (0xE000 | 0x20).to_bytes(2, "big")
# Program 1, the clock on the H.264 video PID 0x100, audio on 0x101.
PMT = (
    (1).to_bytes(2, "big")
    + bytes([0xC1, 0, 0])
    + (0xE000 | 0x100).to_bytes(2, "big")
    + (0xF000).to_bytes(2, "big")
    + bytes([0x1B, 0xE1, 0x00, 0xF0, 0x00])
    + bytes([0x0F, 0xE1, 0x01, 0xF0, 0x00])
)
INTERVAL = 40 * 27_000  # 40 ms between PCRs
GOP = 25  # intervals between random access points: a second

# Tables every 5 intervals, a PCR every 40 ms on the video, and four media packets after each.
PROGRAM = []
for i in range(250):
    if i % 5 == 0:
        PROGRAM += [section_packet(0, 0x00, PAT, i), section_packet(0x20, 0x02, PMT, i)]
    PROGRAM.append(clock_packet(0x100, 27_000_000 + i * INTERVAL, i % GOP == 0, i))
    PROGRAM += [packet(0x101 if k == 2 else 0x100, i * 4 + k) for k in range(4)]
assert all(len(pkt) == passthrough.PACKET for pkt in PROGRAM)


def interval(i):
    """The position of interval `i`'s clock packet in PROGRAM."""
    return [n for n, pkt in enumerate(PROGRAM) if passthrough.pcr(pkt) == 27_000_000 + i * INTERVAL][0]


class Restart(unittest.TestCase):
    def setUp(self):
        # The first import read through interval 102 and a packet; the second from interval 120 on.
        self.first = passthrough.place(PROGRAM, PROGRAM[: interval(102) + 2])
        self.second = passthrough.place(PROGRAM, PROGRAM[interval(120) - 3 :])
        self.bound = passthrough.restart(PROGRAM, self.first, self.second)
        self.clock = passthrough.Clock(PROGRAM, 0x100)

    def test_the_bound_is_the_downtime_and_the_first_group(self):
        # Held from interval 102's PCR, down until interval 120 less three packets; its first group
        # opens at the random access point of interval 125, once tables and a PCR were seen.
        self.assertEqual(self.bound.held, interval(102))
        self.assertEqual((self.bound.first_group, self.bound.second_group), (interval(125), interval(150)))
        down = self.clock.at(interval(120) - 3) - self.clock.at(interval(102))
        self.assertAlmostEqual(self.bound.bound, down + 1.0)

    def test_a_capture_resuming_at_the_first_group_passes(self):
        capture = PROGRAM[: interval(102)] + PROGRAM[interval(125) :]
        self.assertIsNone(passthrough.restart_shortfall(passthrough.place(PROGRAM, capture), self.clock, self.bound))

    def test_a_capture_resuming_at_the_third_group_fails(self):
        capture = PROGRAM[: interval(102)] + PROGRAM[interval(175) :]
        reason = passthrough.restart_shortfall(passthrough.place(PROGRAM, capture), self.clock, self.bound)
        self.assertIn("past the", reason)

    def test_a_second_gap_or_a_lost_tail_fails(self):
        gapped = PROGRAM[: interval(50)] + PROGRAM[interval(51) : interval(102)] + PROGRAM[interval(125) :]
        self.assertIn("runs", passthrough.restart_shortfall(passthrough.place(PROGRAM, gapped), self.clock, self.bound))
        short = PROGRAM[: interval(102)] + PROGRAM[interval(125) : -10]
        self.assertIn("ends", passthrough.restart_shortfall(passthrough.place(PROGRAM, short), self.clock, self.bound))

    def test_a_capture_missing_the_restart_fails(self):
        # Only the restarted import's output, whole from its first group: nothing before the restart.
        suffix = passthrough.place(PROGRAM, PROGRAM[interval(125) :])
        self.assertIn("opens", passthrough.restart_shortfall(suffix, self.clock, self.bound))
        # Nothing missing at all, so it never crossed the restart.
        whole = passthrough.place(PROGRAM, PROGRAM)
        self.assertIn("0 runs", passthrough.restart_shortfall(whole, self.clock, self.bound))
        # One run missing, but not where the import was down.
        early = passthrough.place(PROGRAM, PROGRAM[: interval(50)] + PROGRAM[interval(51) :])
        self.assertIn("not the restart", passthrough.restart_shortfall(early, self.clock, self.bound))

    def test_a_pmt_spanning_packets_is_reassembled(self):
        # 170 bytes of program descriptors push the streams into the PMT's second packet.
        info = bytes([0x05, 168]) + bytes(168)
        body = PMT[:7] + (0xF000 | len(info)).to_bytes(2, "big") + info + PMT[9:]
        length = len(body) + 4
        whole = bytes([0, 0x02, 0xB0 | length >> 8, length & 0xFF]) + body + bytes(4)
        first = bytes([0x47, 0x40, 0x20, 0x10]) + whole[:184]
        rest = whole[184:]
        second = bytes([0x47, 0x00, 0x20, 0x11]) + rest + b"\xff" * (184 - len(rest))
        source = [section_packet(0, 0x00, PAT, 0), first, second]
        self.assertEqual(passthrough.layout(source), passthrough.Layout(0x20, 0x100, 0x100))
        # The PMT's tail before the pointer of a packet that starts the next section.
        tail = bytes([0x47, 0x40, 0x20, 0x11, len(rest)]) + rest + whole[:4]
        tail += b"\xff" * (188 - len(tail))
        source = [section_packet(0, 0x00, PAT, 0), first, tail]
        self.assertEqual(passthrough.layout(source), passthrough.Layout(0x20, 0x100, 0x100))

    def test_the_clock_runs_on_across_a_pcr_wrap(self):
        start = passthrough.PCR_WRAP - 2 * INTERVAL
        source = [clock_packet(0x100, (start + i * INTERVAL) % passthrough.PCR_WRAP, False, i) for i in range(5)]
        clock = passthrough.Clock(source, 0x100)
        self.assertAlmostEqual(clock.at(4) - clock.at(1), 0.12)

    def test_a_restart_needs_one_program_and_a_downtime(self):
        with self.assertRaises(passthrough.Mismatch):
            passthrough.restart(PROGRAM, self.second, self.first)


if __name__ == "__main__":
    unittest.main()
