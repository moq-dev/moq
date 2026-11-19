#!/usr/bin/env python3
"""Tests for the mesh replay's parsing and accounting, which need no relay."""

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("replay", HERE / "replay.py")
assert spec and spec.loader
replay = importlib.util.module_from_spec(spec)
sys.modules["replay"] = replay
spec.loader.exec_module(replay)


def mesh_file(body: str) -> Path:
    f = tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False)
    f.write(body)
    f.close()
    return Path(f.name)


RING = """
relays = ["a", "b", "c", "d"]
[[links]]
dial = "a"
accept = "b"
[[links]]
dial = "b"
accept = "c"
[[links]]
dial = "c"
accept = "d"
[[links]]
dial = "d"
accept = "a"
cost = 2
"""


class MeshTest(unittest.TestCase):
    def test_nightly_mesh_crosses_three_links(self):
        mesh = replay.load_mesh(HERE / "ring6.toml")
        self.assertEqual(mesh.diameter(), 3)
        script = replay.load_script(HERE / "script.toml", mesh)
        self.assertEqual(script.start, "old")
        # Every relay both dials and accepts, so a roll reaches both kinds of link.
        for relay in mesh.relays:
            self.assertTrue(any(link.dial == relay for link in mesh.links), relay)
            self.assertTrue(any(link.accept == relay for link in mesh.links), relay)

    def test_nightly_sweep_meshes(self):
        self.assertEqual(replay.load_mesh(HERE / "ring4.toml").diameter(), 2)

    def test_half_roll_stops_mid_roll(self):
        mesh = replay.load_mesh(HERE / "ring6.toml")
        script = replay.load_script(HERE / "half-roll.toml", mesh)
        self.assertEqual([event["relays"] for event in script.events], [["a", "b"]])
        self.assertEqual(script.known, [])

    def test_ring(self):
        mesh = replay.load_mesh(mesh_file(RING))
        self.assertEqual(mesh.diameter(), 2)
        self.assertEqual(sorted(mesh.neighbours("a")), ["b", "d"])
        self.assertEqual(mesh.links[3].cost, 2)
        self.assertEqual(replay.ports_needed(mesh), 2 * 4 + 4 + 1)

    def test_refuses(self):
        for body, error in [
            ('relays = ["a"]', "at least two"),
            ('relays = ["a", "A"]', "lowercase"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "c"', "two listed relays"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\n[[links]]\ndial = "b"\naccept = "a"', "twice"),
            ('relays = ["a", "b", "c"]\n[[links]]\ndial = "a"\naccept = "b"', "not connected"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\ncost = 0', "at least 1"),
            ('relays = ["a", "b"]\nlinks = []\nextra = 1', "unknown keys"),
        ]:
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, error):
                replay.load_mesh(mesh_file(body))

    def test_script_defaults_to_the_busiest_relay(self):
        mesh = replay.load_mesh(mesh_file(RING + '[[links]]\ndial = "a"\naccept = "c"\n'))
        script = replay.load_script(mesh_file('[[events]]\nkind = "kill"\n[[events]]\nkind = "reopen-link"\n'), mesh)
        self.assertEqual(script.events[0]["relay"], "a")
        self.assertEqual(script.events[1]["link"], "a-b")
        with self.assertRaisesRegex(ValueError, "not in the mesh"):
            replay.load_script(mesh_file('[[events]]\nkind = "kill"\nrelay = "z"\n'), mesh)
        with self.assertRaisesRegex(ValueError, "duration"):
            replay.load_script(mesh_file('down = "soon"\n'), mesh)


METRICS = """# HELP moq_relay_route_changes_total route changes
moq_relay_route_changes_total{tier=""} 3
moq_relay_route_changes_total{tier="edge"} 2
moq_relay_route_withdrawals_total{tier="",direction="sent",kind="dead"} 1
moq_relay_control_tx_messages_total{tier="",class="announce"} 10
moq_relay_control_tx_messages_total{tier="",class="subscribe"} 7
moq_relay_bytes_total{tier="",role="publisher"} 99
"""


class MetricsTest(unittest.TestCase):
    def test_sums_over_tiers_keeping_the_labels_that_matter(self):
        counts = replay.parse_metrics(METRICS)
        self.assertEqual(replay.pick(counts, "moq_relay_route_changes_total"), 5)
        self.assertEqual(replay.pick(counts, "moq_relay_route_withdrawals_total", direction="sent", kind="dead"), 1)
        self.assertEqual(replay.pick(counts, "moq_relay_control_tx_messages_total", **{"class": "announce"}), 10)
        self.assertNotIn("moq_relay_bytes_total", "".join(counts))

    def test_a_restart_counts_from_zero(self):
        totals = replay.Totals()
        totals.observe({"moq_relay_route_changes_total": 5.0}, now=1.0)
        totals.observe({"moq_relay_route_changes_total": 7.0}, now=2.0)
        # The process restarted: its counter fell, so everything it shows is new.
        totals.observe({"moq_relay_route_changes_total": 3.0}, now=3.0)
        self.assertEqual(totals.total["moq_relay_route_changes_total"], 10.0)
        self.assertEqual(totals.changed, 3.0)

    def test_only_route_and_announce_messages_break_the_quiet(self):
        totals = replay.Totals()
        totals.observe({"moq_relay_control_tx_messages_total|class=subscribe": 4.0}, now=1.0)
        self.assertEqual(totals.changed, 0.0)
        totals.observe({"moq_relay_control_tx_messages_total|class=announce": 1.0}, now=2.0)
        self.assertEqual(totals.changed, 2.0)


class ReaderTest(unittest.TestCase):
    def log(self, lines):
        reader = replay.ReaderLog(Path("/nonexistent"))
        for line in lines:
            reader.apply(line)
        return reader

    def test_interruption_counts_gaps_and_a_pause_still_going(self):
        reader = self.log(
            [
                {"ms": 1000, "event": "online", "path": "replay/a/0"},
                {"ms": 5000, "event": "gap", "path": "replay/a/0", "from": 2000, "missed": 3},
                {"ms": 6000, "event": "status", "paths": {"replay/a/0": {"online": True, "groups": 9, "last": 6000}}},
            ]
        )
        path = reader.paths["replay/a/0"]
        self.assertEqual(replay.interruption(path, 1.0, 6.2, 0.5), (3.0, 3))
        # Outside the window, the gap does not count, but a pause running to its end does.
        self.assertEqual(replay.interruption(path, 5.5, 9.0, 0.5), (3.0, 0))
        never = replay.Follow()
        self.assertEqual(replay.interruption(never, 1.0, 4.0, 0.5), (3.0, 0))

    def test_a_withdrawal_stepping_through_end_start_end(self):
        reader = self.log(
            [
                {"ms": 1000, "event": "online", "path": "p"},
                {"ms": 2000, "event": "offline", "path": "p"},
                {"ms": 2100, "event": "online", "path": "p"},
                {"ms": 2200, "event": "offline", "path": "p"},
            ]
        )
        path = reader.paths["p"]
        self.assertTrue(replay.withdrawn_twice(path, 1.5, 3.0))
        # Ending once, or coming back for good, is not.
        self.assertFalse(replay.withdrawn_twice(path, 2.15, 3.0))
        self.assertFalse(replay.withdrawn_twice(path, 0.0, 2.15))

    def test_a_follow_holding_an_ended_track_is_stuck_until_it_changes(self):
        reader = self.log(
            [
                {"ms": 1000, "event": "online", "path": "p"},
                {"ms": 2000, "event": "ended", "path": "p", "error": "no error"},
            ]
        )
        self.assertEqual(reader.paths["p"].ended, "no error")
        reader.apply({"ms": 3000, "event": "offline", "path": "p"})
        self.assertIsNone(reader.paths["p"].ended)


class KnownTest(unittest.TestCase):
    def test_a_known_flaw_never_excuses_the_start(self):
        run = SimpleNamespace(
            script=SimpleNamespace(known=[{"check": "unfed", "kind": "starved", "quest": "/q.md"}]),
            violations=[],
        )
        for name in ("start", "kill"):
            replay.Run.violation(run, {"name": name, "kind": name}, "unfed", [{"reader": "a"}], "starved")
        self.assertNotIn("known", run.violations[0])
        self.assertEqual(run.violations[1]["known"], "/q.md")


class ProcTest(unittest.TestCase):
    def test_only_an_exit_the_run_did_not_ask_for_is_a_death(self):
        log = Path(tempfile.mkdtemp()) / "proc.log"
        proc = replay.Proc("sleeper", log)
        self.assertFalse(proc.died)
        proc.start(["sleep", "30"])
        self.assertFalse(proc.died)
        proc.end(replay.signal.SIGKILL)
        proc.wait(5)
        self.assertFalse(proc.died)
        proc.start(["true"])
        proc.wait(5)
        self.assertTrue(proc.died)


if __name__ == "__main__":
    unittest.main()
