import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("fanout", Path(__file__).with_name("fanout.py"))
fanout = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fanout)


class Fanout(unittest.TestCase):
    def test_hybrid_counts_sum_each_pmu_that_ran(self):
        stat = "\n".join(
            [
                "1200,,cpu_atom/instructions/u,100,100.00,,",
                "<not counted>,,cpu_core/instructions/u,0,0.00,,",
                "34,,cpu_atom/instructions/u,100,100.00,,",
            ]
        )
        self.assertEqual(fanout.parse_stat(stat), 1234)

    def test_multiplexed_or_absent_counts_are_refused(self):
        with self.assertRaisesRegex(ValueError, "multiplexed"):
            fanout.parse_stat("1200,,instructions/u,100,50.00,,")
        with self.assertRaisesRegex(ValueError, "no instructions"):
            fanout.parse_stat("<not counted>,,cpu_core/instructions/u,0,0.00,,")

    def test_each_count_divides_the_cycles_it_added_by_the_groups_delivered(self):
        counts = {0: 1_000, fanout.cycles("model", 4): 1_000 + 7 * fanout.cycles("model", 4) * 4}
        with patch.object(fanout, "count", side_effect=lambda _, args, __: counts[args[1]]):
            self.assertEqual(fanout.per_group("model", "bin", 4, "0"), 7)
        self.assertEqual(fanout.cycles("session", 512), 196)
        self.assertEqual(fanout.cycles("model", 2_000_000), 1)

    def test_summary_names_unsupported_cells(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            rows = [
                dict(label="base", scenario="session", subscribers=8, per_group=None),
                dict(label="current", scenario="session", subscribers=8, per_group=12.5),
            ]
            (run / "fanout.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
            with patch("builtins.print") as printed:
                fanout.summarize(run)
            table = "\n".join(str(call.args[0]) for call in printed.call_args_list)
            self.assertIn("unsupported", table)
            self.assertIn("12.5", table)
            self.assertEqual(json.loads((run / "fanout-summary.json").read_text()), rows)


if __name__ == "__main__":
    unittest.main()
