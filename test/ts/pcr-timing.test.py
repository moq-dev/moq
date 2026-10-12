import importlib.util
import pathlib
import types
import unittest

spec = importlib.util.spec_from_file_location("pcr_timing", pathlib.Path(__file__).with_name("pcr-timing.py"))
timing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timing)


class Schedule(unittest.TestCase):
    def test_unpadded_start_is_graded(self):
        scan = types.SimpleNamespace(
            pcr=[(index, i * 1_080_000) for i, index in enumerate([0, 10, 20, 120, 220, 320])],
            first_null=20, new_base=set(), pcr_pid=256, other_pcr_pids=set(),
        )
        args = types.SimpleNamespace(mux_rate=3_760_000, schedule_pct_min=99, schedule_tolerance_pct=1)
        result = timing.check_schedule(scan, args)
        self.assertFalse(result[2], result)
        self.assertEqual(result[4]["count"], 5)


if __name__ == "__main__":
    unittest.main()
