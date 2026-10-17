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


if __name__ == "__main__":
    unittest.main()
