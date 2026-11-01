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

    def measure(self, home, target, benchmark, mean):
        path = home / target / benchmark.replace("/", "_") / "new"
        path.mkdir(parents=True)
        (path / "benchmark.json").write_text(json.dumps({"full_id": benchmark}))
        (path / "estimates.json").write_text(json.dumps({"mean": {"point_estimate": mean}}))

    def paired_run(self, root, cases, rounds=3):
        """Write a paired run's quick screen from {(target, id): (base, current)}; None skips a side."""
        (root / "metadata.json").write_text(json.dumps({"revision": "current", "base": "base", "rounds": rounds}))
        for (target, benchmark), sides in cases.items():
            for home, mean in zip(("criterion-base", "criterion"), sides):
                if mean is not None:
                    self.measure(root / home, target, benchmark, mean)

    def confirm(self, root, index, key, rounds):
        for number, (base, current) in enumerate(rounds, 1):
            for side, mean in (("base", base), ("current", current)):
                self.measure(root / "confirm" / str(index) / f"round-{number}" / side, *key, mean)

    def test_screen_confirms_the_worst_slowdowns_within_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cases = {("moq-net/session", f"case/{index}"): (100, 111 + index) for index in range(compare.CONFIRMATIONS + 5)}
            cases[("moq-net/session", "steady")] = (100, 109)
            cases[("kio/lock", "faster")] = (100, 50)
            self.paired_run(root, cases)
            screen = compare.criterion_screen(root)
        self.assertEqual(len(screen), compare.CONFIRMATIONS)
        self.assertEqual(screen[0], ("moq-net/session", f"case/{compare.CONFIRMATIONS + 4}"))
        self.assertNotIn(("moq-net/session", "steady"), screen)

    def test_rounds_decide_each_flagged_slowdown(self):
        regressed, noisy, tied, beyond = ("t/a", "regressed"), ("t/a", "noisy"), ("t/a", "tied"), ("t/a", "beyond")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.paired_run(root, {regressed: (100, 150), noisy: (100, 150), tied: (100, 150), beyond: (100, 115),
                                   ("t/a", "faster"): (100, 80), ("t/a", "same"): (100, 105),
                                   ("t/a", "added"): (None, 10), ("t/a", "dropped"): (10, None),
                                   ("t/broken", "base failed"): (None, 10)})
            (root / "confirm.tsv").write_text("t/a\tregressed\nt/a\tnoisy\nt/a\ttied\n")
            self.confirm(root, 1, regressed, [(100, 120), (100, 130), (100, 125)])
            # A bimodal case: slower in two rounds, faster in one.
            self.confirm(root, 2, noisy, [(100, 150), (150, 100), (100, 150)])
            # A tied round is not slower, even with a median past the threshold.
            self.confirm(root, 3, tied, [(100, 100), (100, 130), (100, 125)])
            (root / "paired-unavailable.tsv").write_text("t/broken\tbase run failed\n")
            result = compare.criterion_paired(root)
        verdicts = {case["id"]: case["verdict"] for case in result["cases"]}
        self.assertEqual(verdicts, {"regressed": "regression", "noisy": "noise", "tied": "noise", "beyond": "unconfirmed",
                                    "faster": "faster", "same": "unchanged", "added": "new", "dropped": "removed",
                                    "base failed": "unavailable"})
        regression = next(case for case in result["cases"] if case["id"] == "regressed")
        self.assertEqual([round(delta) for delta in regression["rounds"]["paired_deltas_pct"]], [20, 30, 25])
        self.assertEqual(result["unavailable"], [{"target": "t/broken", "reason": "base run failed"}])

    def test_missing_confirmation_round_fails(self):
        key = ("t/a", "slow")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.paired_run(root, {key: (100, 150)})
            (root / "confirm.tsv").write_text("t/a\tslow\n")
            self.confirm(root, 1, key, [(100, 150), (100, 150)])
            with self.assertRaises(ValueError):
                compare.criterion_paired(root)

    def test_invalid_means_fail(self):
        for mean in (0, -1, float("nan"), None, True):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.measure(root, "t/a", "case", mean)
                with self.subTest(mean=mean), self.assertRaises(ValueError):
                    compare.criterion_means(root)

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
