#!/usr/bin/env python3
"""Tests for the mesh replay's parsing and accounting, which need no relay."""

import importlib.util
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from types import SimpleNamespace

import tomllib

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("replay", HERE / "replay.py")
assert spec and spec.loader
replay = importlib.util.module_from_spec(spec)
sys.modules["replay"] = replay
spec.loader.exec_module(replay)

# Removed when the tests exit, pass or fail.
SCRATCH = tempfile.TemporaryDirectory()


def mesh_file(body: str) -> Path:
    f = tempfile.NamedTemporaryFile("w", suffix=".toml", dir=SCRATCH.name, delete=False)
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

    def test_reopen_runs_the_faults_without_a_roll(self):
        mesh = replay.load_mesh(HERE / "ring4.toml")
        script = replay.load_script(HERE / "reopen.toml", mesh)
        self.assertEqual(script.start, "new")
        self.assertEqual([event["kind"] for event in script.events], ["reopen-link", "reopen-relay", "kill"])
        self.assertEqual(script.known, [])

    def test_half_roll_stops_mid_roll(self):
        mesh = replay.load_mesh(HERE / "ring6.toml")
        script = replay.load_script(HERE / "half-roll.toml", mesh)
        self.assertEqual([event["relays"] for event in script.events], [["a", "b"]])
        self.assertEqual(script.known, [])

    def test_half_roll_hold_runs_untouched_after_the_roll(self):
        mesh = replay.load_mesh(HERE / "ring6.toml")
        script = replay.load_script(HERE / "half-roll-hold.toml", mesh)
        self.assertEqual([event["kind"] for event in script.events], ["roll", "hold"])
        self.assertEqual(script.events[1]["for"], 600.0)

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
            ('relays = ["a", "b-c"]', "dial-accept"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "c"', "two listed relays"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\n[[links]]\ndial = "b"\naccept = "a"', "twice"),
            ('relays = ["a", "b", "c"]\n[[links]]\ndial = "a"\naccept = "b"', "not connected"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\ncost = 0', "at least 1"),
            ('relays = ["a", "b"]\nlinks = []\nextra = 1', "unknown keys"),
            (
                'relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\ncost = 2\ncost_build = "mid"',
                "`old` or `new`",
            ),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\ncost_build = "new"', "no cost"),
            (
                'relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\ncost = 2\ncost_build = "new"\nbuild = "old"',
                "does not dial it",
            ),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\nbuild = "mid"', "`old` or `new`"),
            ('relays = ["a", "b"]\n[[links]]\ndial = "a"\naccept = "b"\nbuild = "old"\nupstream = true', "upstream"),
            (
                'relays = ["a", "b", "c"]\n[[links]]\ndial = "a"\naccept = "b"\n'
                '[[links]]\ndial = "b"\naccept = "c"\nbuild = "old"',
                "not connected on the new build",
            ),
        ]:
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, error):
                replay.load_mesh(mesh_file(body))

    def test_a_roll_can_change_the_graph(self):
        # The old build dials the a-c shortcut, the new one does not.
        body = (
            'relays = ["a", "b", "c"]\n[[links]]\ndial = "a"\naccept = "c"\nbuild = "old"\n'
            '[[links]]\ndial = "a"\naccept = "b"\n[[links]]\ndial = "b"\naccept = "c"\n'
        )
        mesh = replay.load_mesh(mesh_file(body))
        self.assertEqual(mesh.builds(), ["old", "new"])
        self.assertEqual((mesh.diameter("old"), mesh.diameter("new")), (1, 2))
        self.assertEqual(sorted(mesh.neighbours("a")), ["b", "c"])
        self.assertEqual(mesh.neighbours("a", "new"), ["b"])
        self.assertEqual(replay.load_mesh(HERE / "ring4.toml").builds(), [None])
        # A link fault names a link every build dials unless the script names one.
        script = replay.load_script(mesh_file('[[events]]\nkind = "reopen-link"\nrelay = "a"\n'), mesh)
        self.assertEqual(script.events[0]["link"], "a-b")
        only = replay.load_mesh(mesh_file(body.replace('accept = "b"\n', 'accept = "b"\nbuild = "new"\n', 1)))
        with self.assertRaisesRegex(ValueError, "name the `link`"):
            replay.load_script(mesh_file('[[events]]\nkind = "reopen-link"\nrelay = "a"\n'), only)
        # A named link fault needs its dialer's build at that point to dial it.
        reopen = '[[events]]\nkind = "reopen-link"\nrelay = "a"\nlink = "a-c"\n'
        self.assertEqual(replay.load_script(mesh_file('start = "old"\n' + reopen), mesh).events[0]["link"], "a-c")
        with self.assertRaisesRegex(ValueError, "a does not dial on its build then"):
            replay.load_script(mesh_file(reopen), mesh)
        # Each build's graph is whole, but rolling a before c leaves a alone mid-roll.
        split = replay.load_mesh(
            mesh_file(
                'relays = ["a", "b", "c"]\n[[links]]\ndial = "a"\naccept = "b"\nbuild = "old"\n'
                '[[links]]\ndial = "c"\naccept = "a"\nbuild = "new"\n[[links]]\ndial = "b"\naccept = "c"\n'
            )
        )
        roll = 'start = "old"\n[[events]]\nkind = "roll"\nto = "new"\nrelays = [{}]\n'
        replay.load_script(mesh_file(roll.format('"c", "a", "b"')), split)
        with self.assertRaisesRegex(ValueError, r"once a rolls to new, \['b', 'c'\] are cut off"):
            replay.load_script(mesh_file(roll.format('"a", "c", "b"')), split)

    def test_script_defaults_to_the_busiest_relay(self):
        mesh = replay.load_mesh(mesh_file(RING + '[[links]]\ndial = "a"\naccept = "c"\n'))
        script = replay.load_script(mesh_file('[[events]]\nkind = "kill"\n[[events]]\nkind = "reopen-link"\n'), mesh)
        self.assertEqual(script.events[0]["relay"], "a")
        self.assertEqual(script.events[1]["link"], "a-b")
        with self.assertRaisesRegex(ValueError, "not in the mesh"):
            replay.load_script(mesh_file('[[events]]\nkind = "kill"\nrelay = "z"\n'), mesh)
        with self.assertRaisesRegex(ValueError, "duration"):
            replay.load_script(mesh_file('down = "soon"\n'), mesh)
        with self.assertRaisesRegex(ValueError, "`times` is at least 1"):
            replay.load_script(mesh_file('[[events]]\nkind = "roll"\nto = "old"\ntimes = 0\n'), mesh)


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


class ReaderReportTest(unittest.TestCase):
    def test_a_status_line_a_second_old_is_no_pause(self):
        readout = replay.ReaderLog(Path("/nonexistent"))
        readout.apply(
            {"ms": 6000, "event": "status", "paths": {"replay/b/0": {"online": True, "groups": 50, "last": 5950}}}
        )
        relay = SimpleNamespace(name="a", readout=readout)
        run = SimpleNamespace(relays={"a": relay})
        # The window ends almost a second after the reader last reported, as every window may.
        report = replay.Run.reader_report(run, 1.0, 6.9, {"c"})
        self.assertEqual(report["all"]["interrupted"], 0)
        # A gap that ended after that status line still counts in full.
        readout.apply({"ms": 6800, "event": "gap", "path": "replay/b/0", "from": 6100, "missed": 6})
        report = replay.Run.reader_report(run, 1.0, 6.9, {"c"})
        self.assertEqual((report["all"]["interrupted"], report["all"]["missed"]), (1, 6))


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

    def test_a_strand_keeps_the_event_it_began_in(self):
        follow = replay.Follow(last=5.0)
        run = SimpleNamespace(
            script=SimpleNamespace(known=[{"check": "unfed", "event": "kill", "quest": "/q.md"}]),
            relays={
                "a": SimpleNamespace(
                    name="a",
                    readout=SimpleNamespace(paths={"replay/b/0": follow}),
                    publisher=SimpleNamespace(alive=True),
                    # Not alive, so the check asks it nothing over HTTP.
                    proc=SimpleNamespace(alive=False),
                )
            },
            procs=lambda: [],
            violations=[],
            strands={},
        )
        run.violation = lambda *args: replay.Run.violation(run, *args)

        def check(kind, observed=None):
            report = {"name": kind, "kind": kind}
            last = follow.last if observed is None else observed
            replay.Run.check(run, report, [{"reader": "a", "path": "replay/b/0", "kind": "starved", "last": last}], [])
            return report["violations"][0]

        self.assertEqual(check("kill")["known"], "/q.md")
        # Unread since the kill, so the next event carries the kill's strand and its known entry.
        carried = check("end-source")
        self.assertEqual((carried["began"], carried["known"]), ("kill", "/q.md"))
        # Its source stopped, so it is not unfed; unread still, it carries on once the source is back.
        replay.Run.check(run, {"name": "end", "kind": "end-source"}, [], [])
        self.assertEqual(check("end-source")["began"], "kill")
        # Read once since: a new strand, which no entry excuses.
        follow.last = 9.0
        fresh = check("roll")
        self.assertNotIn("began", fresh)
        self.assertNotIn("known", fresh)
        # The run counts each strand once, in the event it began: the kill's and the roll's.
        self.assertEqual([v["event"] for v in run.violations], ["kill", "roll"])

    def test_a_strand_begins_from_the_read_it_was_observed_with(self):
        follow = replay.Follow(last=5.0)
        run = SimpleNamespace(
            script=SimpleNamespace(known=[{"check": "unfed", "event": "kill", "quest": "/q.md"}]),
            relays={
                "a": SimpleNamespace(
                    name="a",
                    readout=SimpleNamespace(paths={"replay/b/0": follow}),
                    publisher=SimpleNamespace(alive=True),
                    proc=SimpleNamespace(alive=False),
                )
            },
            procs=lambda: [],
            violations=[],
            strands={},
        )
        run.violation = lambda *args: replay.Run.violation(run, *args)

        def check(kind, last):
            report = {"name": kind, "kind": kind}
            item = {"reader": "a", "path": "replay/b/0", "kind": "starved", "last": last}
            replay.Run.check(run, report, [item], [])
            return report["violations"][0]

        # Unfed as of 5.0, though the readout refreshed after settling shows a recovery read at 9.0.
        follow.last = 9.0
        self.assertEqual(check("kill", 5.0)["known"], "/q.md")
        # Stalled again after that read: a new strand, which the kill's entry does not excuse.
        fresh = check("roll", 9.0)
        self.assertNotIn("began", fresh)
        self.assertNotIn("known", fresh)


class ProcTest(unittest.TestCase):
    def test_only_an_exit_the_run_did_not_ask_for_is_a_death(self):
        log = Path(tempfile.mkdtemp(dir=SCRATCH.name)) / "proc.log"
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


class BuildTest(unittest.TestCase):
    def fake_run(self, dial, accept):
        from types import SimpleNamespace

        link = replay.Link("a", "b", None, True, {})
        run = SimpleNamespace(
            relays={"a": SimpleNamespace(build=dial), "b": SimpleNamespace(build=accept)},
            mesh=SimpleNamespace(links=[link]),
            shaper_port={link.name: 1},
            auth_port=4,
            log="info",
        )
        return run, link

    def test_a_link_reads_the_version_its_acceptor_negotiated_on_the_newest_open_session(self):
        log = Path(SCRATCH.name) / "relay-b.log"
        log.write_text("")
        negotiated = replay.Negotiated(log)
        new, old = replay.Build("new", "relay", []), replay.Build("old", "relay", [])

        def line(session, version):
            # As a relay writes it, colours included.
            dim, end = "\x1b[2m", "\x1b[0m"
            return (
                f"{dim}2026-11-21T02:34:59Z{end} \x1b[32m INFO{end} \x1b[1mconn{end}\x1b[1m{{{end}"
                f'\x1b[3msession{end}{dim}={end}"{session}"\x1b[1m}}{end}{dim}:{end} '
                f"{dim}moq_relay::connection{end}{dim}:{end} negotiated \x1b[3mversion{end}{dim}={end}{version} "
                f"\x1b[3mtransport{end}{dim}={end}quic\n"
            )

        events = []
        links = [replay.Link("a", "b", None, False, {}), replay.Link("c", "b", None, False, {}, "old")]
        run = SimpleNamespace(
            auth=SimpleNamespace(lock=threading.Lock(), events=events),
            relays={
                name: SimpleNamespace(
                    build=new,
                    negotiated=negotiated if name == "b" else replay.Negotiated(log),
                    proc=SimpleNamespace(alive=True),
                    started=0.0,
                )
                for name in "abc"
            },
            mesh=SimpleNamespace(links=links),
        )

        def versions():
            return replay.Run.versions(run)

        # The new build does not dial c-b, and a-b has no session yet.
        self.assertEqual(versions(), {"a-b": "down"})
        events.append({"event": "connect", "id": "aa", "peer": "a", "t": 1.0})
        with log.open("a") as f:
            f.write(line("aa", "moq-lite-06"))
            f.write("a line still being writ")
        self.assertEqual(versions(), {"a-b": "moq-lite-06"})
        # A newer session wins; the older one ending changes nothing.
        events.append({"event": "connect", "id": "bb", "peer": "a", "t": 2.0})
        with log.open("a") as f:
            f.write("ten\n" + line("bb", "moq-lite-07-wip"))
        events.append({"event": "end", "id": "aa", "peer": "a", "t": 3.0})
        self.assertEqual(versions(), {"a-b": "moq-lite-07-wip"})
        events.append({"event": "end", "id": "bb", "peer": "a", "t": 4.0})
        self.assertEqual(versions(), {"a-b": "down"})
        # A restarted acceptor holds none of its last process's sessions.
        events.append({"event": "connect", "id": "cc", "peer": "a", "t": 5.0})
        with log.open("a") as f:
            f.write(line("cc", "moq-lite-06"))
        self.assertEqual(versions(), {"a-b": "moq-lite-06"})
        # A dead dialer's link is down, though its acceptor still holds the session.
        run.relays["a"].proc.alive = False
        self.assertEqual(versions(), {"a-b": "down"})
        # Restarted, the dialer counts only the sessions its new process opened.
        run.relays["a"].proc.alive, run.relays["a"].started = True, 6.0
        self.assertEqual(versions(), {"a-b": "down"})
        events.append({"event": "connect", "id": "dd", "peer": "a", "t": 7.0})
        with log.open("a") as f:
            f.write(line("dd", "moq-lite-07-wip"))
        self.assertEqual(versions(), {"a-b": "moq-lite-07-wip"})
        negotiated.restart()
        self.assertEqual(versions(), {"a-b": "down"})
        # On the old build, the dialer dials its own link too.
        run.relays["c"].build = old
        self.assertEqual(versions(), {"a-b": "down", "c-b": "down"})
        self.assertTrue(replay.is_lite07("moq-lite-07-wip") and replay.is_lite07("moq-lite-07"))
        self.assertFalse(replay.is_lite07("moq-lite-06") or replay.is_lite07("down"))

    def test_the_report_lists_each_event_that_moved_a_link(self):
        events = [
            {"name": "start", "versions": {"a-b": "moq-lite-06", "a-c": "moq-lite-06"}},
            {"name": "kill b", "versions": {"a-b": "moq-lite-06", "a-c": "moq-lite-06"}},
            {"name": "roll a to new", "versions": {"a-b": "moq-lite-07-wip"}},
        ]
        self.assertEqual(
            replay.version_rows(events),
            [
                "| start | moq-lite-06 2 | - |",
                "| roll a to new | moq-lite-07-wip 1 | a-b: moq-lite-06 to moq-lite-07-wip; a-c: moq-lite-06 to - |",
            ],
        )
        mesh = {"links": [{"build": None}, {"build": "old"}], "diameter": {"old": 1, "new": 2}}
        self.assertEqual(
            replay.graph(mesh), "2 links on the old build, diameter 1; 1 links on the new build, diameter 2"
        )

    def test_readers_follow_the_first_broadcasts_of_each_relay(self):
        relay = replay.Relay.__new__(replay.Relay)
        relay.name, relay.run = "a", SimpleNamespace(broadcasts=4, follow=2)
        self.assertEqual(relay.followed(), ["replay/a/0", "replay/a/1"])
        self.assertEqual(len(relay.paths()), 4)
        relay.run.follow = 0
        self.assertEqual(relay.followed(), relay.paths())

    def test_a_build_dials_only_its_links(self):
        run, _ = self.fake_run(None, None)
        run.mesh.links = [replay.Link("a", "b", None, False, {}, "old"), replay.Link("a", "c", None, False, {})]
        run.shaper_port = {"a-b": 1, "a-c": 5}
        relay = replay.Relay.__new__(replay.Relay)
        relay.run, relay.name, relay.port, relay.internal = run, "a", 2, 3
        self.assertIn("127.0.0.1:1/", relay.config(replay.Build("old", "relay", [])))
        new = relay.config(replay.Build("new", "relay", []))
        self.assertNotIn("127.0.0.1:1/", new)
        self.assertIn("127.0.0.1:5/", new)

    def test_a_link_is_priced_on_the_build_that_names_it(self):
        run, _ = self.fake_run(None, None)
        run.mesh.links = [
            replay.Link("a", "b", 3, False, {}, None, "new"),
            replay.Link("a", "c", 4, False, {}),
        ]
        run.shaper_port = {"a-b": 1, "a-c": 5}
        relay = replay.Relay.__new__(replay.Relay)
        relay.run, relay.name, relay.port, relay.internal = run, "a", 2, 3
        for build, priced in [("old", None), ("new", 3)]:
            config = tomllib.loads(relay.config(replay.Build(build, "relay", [])))
            cost = {peer["url"]: peer.get("cost") for peer in config["cluster"]["connect"]}
            self.assertEqual(cost, {"https://127.0.0.1:1/": priced, "https://127.0.0.1:5/": 4}, build)

    def test_only_the_new_build_marks_a_link_upstream(self):
        run, _ = self.fake_run(None, None)
        relay = replay.Relay.__new__(replay.Relay)
        relay.run, relay.name, relay.port, relay.internal = run, "a", 2, 3
        new = replay.Build("new", "relay", [])
        old = replay.Build("old", "relay", [])
        self.assertIn("upstream = true", relay.config(new))
        self.assertNotIn("upstream", relay.config(old))


class EventTest(unittest.TestCase):
    def test_a_roll_repeats_its_times(self):
        from types import SimpleNamespace

        rolls = []
        run = SimpleNamespace(roll=lambda to, relays, suffix: rolls.append((to, suffix)))
        replay.Run.run_event(run, {"kind": "roll", "to": "new", "relays": ["a"], "times": 2})
        self.assertEqual(rolls, [("new", " (1/2)"), ("new", " (2/2)")])

    def test_the_start_needs_every_route_unless_an_applied_mark_may_hide_one(self):
        from types import SimpleNamespace

        def run(upstream, build):
            relays = {"a": SimpleNamespace(build=replay.Build(build, "relay", []))}
            return SimpleNamespace(
                mesh=SimpleNamespace(links=[replay.Link("a", "b", None, upstream, {})]), relays=relays
            )

        self.assertTrue(replay.Run.baseline(run(False, "new"), "start"))
        self.assertFalse(replay.Run.baseline(run(False, "new"), "kill"))
        self.assertFalse(replay.Run.baseline(run(True, "new"), "start"))
        # The old build is written no mark, so nothing may hide its routes.
        self.assertTrue(replay.Run.baseline(run(True, "old"), "start"))


if __name__ == "__main__":
    unittest.main()
