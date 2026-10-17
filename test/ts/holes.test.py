import importlib.util
import pathlib
import unittest
from fractions import Fraction

spec = importlib.util.spec_from_file_location("holes", pathlib.Path(__file__).with_name("holes.py"))
holes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(holes)


class Holes(unittest.TestCase):
    SOURCE = [i * 2160 for i in range(50)]

    def test_continuous(self):
        self.assertEqual(holes.step(self.SOURCE), 2160)
        self.assertEqual(holes.holes(self.SOURCE, 2160), [])

    def test_one_lost_frame(self):
        dts = [i * 2160 for i in range(50) if i != 20]
        self.assertEqual(holes.holes(dts, holes.step(self.SOURCE)), [(19 * 2160, 2 * 2160)])

    def test_every_other_frame_lost(self):
        # The capture's own step is two frames and its last frame is the source's, so only
        # the source's step shows that half the media is gone.
        dts = self.SOURCE[1::2]
        self.assertEqual(len(holes.holes(dts, holes.step(self.SOURCE))), len(dts) - 1)

    def test_jitter_within_half_a_frame(self):
        # 90 kHz rounding of a 1024-sample frame at 44.1 kHz alternates 2089 and 2090.
        dts = [i * 2089 + i // 2 for i in range(50)]
        self.assertEqual(holes.holes(dts, holes.step(dts)), [])

    def test_too_short(self):
        self.assertEqual(holes.step([0]), 0)


class Cuts(unittest.TestCase):
    # First PTS, last PTS, last DTS: the last pictures in decode order are B pictures presented before the P.
    SOURCE = (Fraction(1), Fraction(31), Fraction(30) + Fraction(23, 25))
    FRAME = Fraction(1, 25)

    def test_joined_on_a_later_group(self):
        capture = (Fraction(3), *self.SOURCE[1:])
        self.assertEqual(holes.cuts(self.SOURCE, capture, self.FRAME), [])

    def test_lost_tail(self):
        # One continuous stream that stops early has no hole and nothing to compare against.
        capture = (Fraction(3), Fraction(27), Fraction(26))
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)

    def test_lost_trailing_b_pictures(self):
        # The last P picture still presents last, and nothing after it leaves a step.
        capture = (Fraction(3), Fraction(31), Fraction(30) + Fraction(20, 25))
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)

    def test_lost_prefix(self):
        capture = (Fraction(10), *self.SOURCE[1:])
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)

if __name__ == "__main__":
    unittest.main()
