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
        row["generator"] = {"cpu_cores": row.pop("generator_cpu_cores")}
    return result


class Paired(unittest.TestCase):
    def test_stable_aa(self):
        metric = compare.reduce([pair()] * 3)["metrics"]["recv_mbps"]
        self.assertEqual(metric["delta_pct"], 0)
        self.assertEqual(metric["mad_pct"], 0)

    def test_known_degradation(self):
        self.assertAlmostEqual(compare.reduce([pair(-20)] * 3)["metrics"]["recv_mbps"]["delta_pct"], -20)

    def test_memory_is_compared(self):
        self.assertAlmostEqual(compare.reduce([pair(17)] * 3)["metrics"]["rss_bytes"]["delta_pct"], 17)

    def test_noisy(self):
        metric = compare.reduce([pair(-10), pair(), pair(10)])["metrics"]["recv_mbps"]
        self.assertEqual(metric["status"], "sign varies across pairs")
        self.assertGreater(metric["mad_pct"], 9)

    def test_insufficient(self):
        self.assertEqual(compare.reduce([pair()])["metrics"]["recv_mbps"]["status"], "insufficient samples")

    def test_host_metrics_unavailable_on_both_sides_are_explicit(self):
        p = pair()
        for metric in ("cpu_cores", "rss_bytes"):
            del p["base"][metric]
            del p["current"][metric]
        result = compare.reduce([p] * 3)
        for metric in ("cpu_cores", "rss_bytes"):
            self.assertEqual(result["metrics"][metric]["status"], "unavailable: no host samples")
        self.assertEqual(result["metrics"]["recv_mbps"]["delta_pct"], 0)

    def test_missing_invalid(self):
        for value in [None, -1, float("nan"), float("inf"), True]:
            p = pair()
            p["base"]["recv_mbps"] = value
            with self.assertRaises(ValueError):
                compare.reduce([p])
        with self.assertRaises(ValueError):
            compare.reduce([])

    def test_zero_baseline_keeps_absolute_regression(self):
        p = pair()
        p["base"]["loss_pct"] = 0
        p["current"]["loss_pct"] = 2
        metric = compare.reduce([p] * 3)["metrics"]["loss_pct"]
        self.assertEqual(metric["paired_absolute_deltas"], [2, 2, 2])


class Load(unittest.TestCase):
    def write(self, root, label, rounds, case="quic/video"):
        for index in rounds:
            path = root / "relay" / label / f"round-{index}" / case
            path.mkdir(parents=True)
            (path / "summary.json").write_text(json.dumps({"round": index}))

    def test_numeric_round_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for label in ("base", "current"):
                self.write(root, label, [1, 2, 10])
            pairs = compare.load(root, "quic/video")
        self.assertEqual([p["base"]["round"] for p in pairs], [1, 2, 10])

    def test_unpaired_rounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write(root, "base", [1, 2])
            self.write(root, "current", [1, 2, 3])
            with self.assertRaises(ValueError):
                compare.load(root, "quic/video")

    def test_cases_span_transports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for label in ("base", "current"):
                for case in ("quic/video", "ws/chat"):
                    self.write(root, label, [1, 2], case)
            self.assertEqual(compare.cases(root), ["quic/video", "ws/chat"])

    def test_a_case_missing_from_one_round(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for label in ("base", "current"):
                self.write(root, label, [1, 2])
            self.write(root, "current", [2], "ws/chat")
            with self.assertRaises(ValueError):
                compare.cases(root)


unittest.main()
