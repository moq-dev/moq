import importlib.util
import pathlib
import sys
import unittest

spec = importlib.util.spec_from_file_location("open_gop", pathlib.Path(__file__).with_name("open-gop.py"))
gop = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = gop
spec.loader.exec_module(gop)

class Recovery(unittest.TestCase):
    def setUp(self):
        self.source = [gop.AccessUnit(bytes([group * 5 + offset]), group * 1000 + pts, group * 1000 + offset * 100,
                                     offset == 0, False, offset == 0)
                       for group in range(4) for offset, pts in enumerate([1000, 800, 900, 1100, 1200])]
        self.leads = gop.leading(self.source)

    def test_join_and_later_gap_trim_only_their_orphans(self):
        indices = [0, 3, 4, 5, 6, 7, 8, 9, 15, 18, 19]
        self.assertEqual(gop.align(self.source, [self.source[i] for i in indices], self.leads),
                         (indices, {0: 0, 8: 15}))

    def test_orphan_leading_picture_is_refused(self):
        self.assertIsNone(gop.align(self.source, self.source, self.leads))
        indices = [0, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 18, 19]
        self.assertIsNone(gop.align(self.source, [self.source[i] for i in indices], self.leads))

    def test_continuous_leading_picture_and_other_delta_are_owed(self):
        for indices in [[0, 3, 4, 5, 7, 8, 9], [0, 4, 5, 6, 7, 8, 9]]:
            self.assertIsNone(gop.align(self.source, [self.source[i] for i in indices], self.leads))

if __name__ == "__main__":
    unittest.main()
