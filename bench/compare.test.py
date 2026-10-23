#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
spec = importlib.util.spec_from_file_location("compare", Path(__file__).with_name("compare.py"))
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def pair(delta=0):
    result = {"base": dict.fromkeys(compare.METRICS, 100), "current": dict.fromkeys(compare.METRICS, 100 + delta)}
    for row in result.values():
        row['generator'] = {'cpu_cores': row.pop('generator_cpu_cores')}
    return result


class Paired(unittest.TestCase):
    def test_stable_aa(self):
        metric = compare.reduce([pair()] * 3)["metrics"]["recv_mbps"]
        self.assertEqual(metric["delta_pct"], 0)
        self.assertEqual(metric["mad_pct"], 0)

    def test_known_degradation(self):
        self.assertAlmostEqual(compare.reduce([pair(-20)] * 3)["metrics"]["recv_mbps"]["delta_pct"], -20)

    def test_noisy(self):
        metric = compare.reduce([pair(-10), pair(), pair(10)])["metrics"]["recv_mbps"]
        self.assertEqual(metric["status"], "sign varies across pairs")
        self.assertGreater(metric["mad_pct"], 9)

    def test_insufficient(self):
        self.assertEqual(compare.reduce([pair()])["metrics"]["recv_mbps"]["status"], "insufficient samples")

    def test_cpu_unavailable_on_both_sides_is_explicit(self):
        p = pair()
        del p["base"]["cpu_cores"]
        del p["current"]["cpu_cores"]
        result = compare.reduce([p] * 3)
        self.assertEqual(result["metrics"]["cpu_cores"]["status"], "unavailable: no host CPU samples")
        self.assertEqual(result["metrics"]["recv_mbps"]["delta_pct"], 0)

    def test_missing_invalid(self):
        for value in [None, -1, float("nan"), float("inf"), True]:
            p = pair(); p["base"]["recv_mbps"] = value
            with self.assertRaises(ValueError):
                compare.reduce([p])
        with self.assertRaises(ValueError):
            compare.reduce([])

    def test_zero_baseline_keeps_absolute_regression(self):
        p = pair()
        p['base']['loss_pct'] = 0
        p['current']['loss_pct'] = 2
        metric = compare.reduce([p] * 3)['metrics']['loss_pct']
        self.assertEqual(metric['paired_absolute_deltas'], [2, 2, 2])


class Load(unittest.TestCase):
    def write(self, root, label, rounds):
        for index in rounds:
            path = root / "relay" / label / f"round-{index}" / "video"
            path.mkdir(parents=True)
            (path / "summary.json").write_text(json.dumps({"round": index}))

    def test_numeric_round_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for label in ("base", "current"):
                self.write(root, label, [1, 2, 10])
            pairs = compare.load(root, "video")
        self.assertEqual([p["base"]["round"] for p in pairs], [1, 2, 10])

    def test_unpaired_rounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "base", [1, 2])
            self.write(root, "current", [1, 2, 3])
            with self.assertRaises(ValueError):
                compare.load(root, "video")


class Criterion(unittest.TestCase):
    def test_full_ids_with_the_same_truncated_title(self):
        prefix = "session_fanout/" + "a" * 100
        ids = [prefix + "/publishers=1/readers=16", prefix + "/publishers=16/readers=1"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, benchmark in enumerate(ids):
                path = root / f"truncated_{index}" / "discovery"
                path.mkdir(parents=True)
                (path / "benchmark.json").write_text(json.dumps({"full_id": benchmark}))
            # Stale real measurements must not join the discovery inventory.
            path = root / "old" / "new"
            path.mkdir(parents=True)
            (path / "benchmark.json").write_text(json.dumps({"full_id": "stale"}))
            self.assertEqual(compare.criterion_cases(root), sorted(ids))

    def test_baseline_follows_full_id_when_directory_suffix_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            samples = root / "samples"
            old = samples / "truncated" / "base"
            current = root / "discovery"
            new = current / "truncated_2" / "discovery"
            old.mkdir(parents=True)
            new.mkdir(parents=True)
            benchmark = "session/" + "long-" * 30
            for path in (old, new):
                (path / "benchmark.json").write_text(json.dumps({"full_id": benchmark}))
            (old / "sample.json").write_text('{"measured": true}')
            compare.criterion_baseline(samples, current, benchmark)
            self.assertEqual((samples / "truncated_2" / "base" / "sample.json").read_text(), '{"measured": true}')
            with self.assertRaises(ValueError):
                compare.criterion_baseline(samples, current, "unmatched")

    def test_empty_or_unrepresentable_inventory_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                compare.criterion_cases(root)
            path = root / "case" / "discovery"
            path.mkdir(parents=True)
            for value in [None, "", "bad\tcase", "bad\ncase"]:
                (path / "benchmark.json").write_text(json.dumps({"full_id": value}))
                with self.subTest(value=value), self.assertRaises(ValueError):
                    compare.criterion_cases(root)


unittest.main()
