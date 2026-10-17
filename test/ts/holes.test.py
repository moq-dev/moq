import importlib.util
import pathlib
import unittest
from fractions import Fraction

spec = importlib.util.spec_from_file_location("holes", pathlib.Path(__file__).with_name("holes.py"))
holes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(holes)


class Holes(unittest.TestCase):
    def test_continuous(self):
        self.assertEqual(holes.holes([i * 2160 for i in range(50)]), (2160, []))

    def test_one_lost_frame(self):
        dts = [i * 2160 for i in range(50) if i != 20]
        self.assertEqual(holes.holes(dts), (2160, [(19 * 2160, 2 * 2160)]))

    def test_jitter_within_half_a_frame(self):
        # 90 kHz rounding of a 1024-sample frame at 44.1 kHz alternates 2089 and 2090.
        dts = [i * 2089 + i // 2 for i in range(50)]
        self.assertEqual(holes.holes(dts)[1], [])

    def test_too_short(self):
        self.assertEqual(holes.holes([0]), (0, []))


class Cuts(unittest.TestCase):
    SOURCE = (Fraction(1), Fraction(31))
    FRAME = Fraction(1, 25)

    def test_joined_on_a_later_group(self):
        self.assertEqual(holes.cuts(self.SOURCE, (Fraction(3), Fraction(31)), self.FRAME), [])

    def test_lost_tail(self):
        # One continuous stream that stops early has no hole and nothing to compare against.
        self.assertEqual(len(holes.cuts(self.SOURCE, (Fraction(3), Fraction(27)), self.FRAME)), 1)

    def test_lost_prefix(self):
        self.assertEqual(len(holes.cuts(self.SOURCE, (Fraction(10), Fraction(31)), self.FRAME)), 1)


if __name__ == "__main__":
    unittest.main()
