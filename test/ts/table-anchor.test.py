#!/usr/bin/env python3
"""Exercise the pair grader through complete synthetic transport captures."""

import json
import subprocess
import sys
import tempfile
import unittest
from importlib import import_module
from pathlib import Path

fixtures = import_module("tstd-controls")
PKT, PMT, VIDEO = fixtures.packet, 0x1000, 0x100
CLOCK = 27_000_000


def capture(
    path, *, start=0, phase=0, drift=0, loss=None, interleave=False, bootstrap=False, wrap=False, extra_program=False
):
    pat = fixtures.section(0, 1, bytes.fromhex("0001f0000002f001" if extra_program else "0001f000"))
    pmt = fixtures.section(2, 1, bytes.fromhex("e100f0001be100f000"))
    sdt = fixtures.section(0x42, 1, bytes.fromhex("0001ff"))
    # A packet per millisecond; PCR every 25 packets, independently of tables/PES.
    events = {}
    for at in range(7, 30000, 2000):
        events[at + phase + (at // 2000) * drift] = 0x11
    if bootstrap:
        events[start + 7] = 0x11
    with open(path, "wb") as out:
        for at in range(start, 30001):
            pid, payload, pusi, adaptation = 0x1FFF, b"", False, None
            if at % 25 == 0:
                ticks = (at * CLOCK // 1000 + (((1 << 33) * 300 - 10 * CLOCK) if wrap else 0)) % ((1 << 33) * 300)
                base, ext = divmod(ticks, 300)
                adaptation = b"\x10" + ((base << 15) | (0x3F << 9) | ext).to_bytes(6, "big")
                pid = VIDEO
            elif at % 500 == 2:
                pid, payload, pusi = 0, b"\0" + pat, True
            elif at % 500 == 3:
                pid, payload, pusi = PMT, b"\0" + pmt, True
            elif extra_program and at % 500 == 4:
                pid, payload, pusi = 0x1001, b"\0" + fixtures.section(2, 2, bytes.fromhex("e100f0001be100f000")), True
            elif at in events:
                pid, payload, pusi = events[at], b"\0" + sdt, True
            elif at % 20 == 10:
                # Changing which PES follows a table does not change table placement.
                audio = interleave and at % 1000 != 10
                pid = 0x101 if audio else VIDEO
                pts = (at + (600 if audio else 200)) * 90
                payload = bytes.fromhex("000001") + bytes([0xC0 if audio else 0xE0])
                payload += bytes.fromhex("0000808005") + fixtures.stamp(2, pts) + b"media"
                pusi = True
                adaptation = b"\x40" if not audio and at % 1000 == 10 else None
            if pid == 0x11 and (loss == "all" or loss == "tail" and at > 14000 or loss == "sparse" and at % 4000 != 7):
                pid, payload, pusi = 0x1FFF, b"", False
            out.write(PKT(pid, 0, payload, pusi, adaptation))


class Controls(unittest.TestCase):
    def test_source_declared_program(self):
        with tempfile.TemporaryDirectory(prefix="ts-pair-source-") as directory:
            directory = Path(directory)
            source, output, report = directory / "source.ts", directory / "output.ts", directory / "report.json"
            capture(source, extra_program=True)
            capture(output)
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("table-anchor.py")),
                    str(output),
                    str(output),
                    "--source",
                    str(source),
                    "--report-json",
                    str(report),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            missing = next(row for row in json.loads(report.read_text())["tables"] if row["pid"] == 0x1001)
            self.assertTrue(missing["graded"])
            self.assertFalse(missing["ok"])
            self.assertEqual((missing["a"], missing["b"]), (0, 0))

    def test_controls(self):
        with tempfile.TemporaryDirectory(prefix="ts-pair-controls-") as directory:
            directory = Path(directory)
            a, b, report = directory / "a.ts", directory / "b.ts", directory / "report.json"
            capture(a)
            source = directory / "source.ts"
            source.write_bytes(a.read_bytes())
            cases = [
                ("identical", {}, True),
                ("different PES interleave", dict(interleave=True), True),
                ("late acquisition and bootstrap", dict(start=3500, bootstrap=True, interleave=True), True),
                ("persistent phase", dict(start=3500, phase=100), False),
                ("drift", dict(start=3500, drift=10), False),
                ("table disappears in shared media", dict(loss="tail"), False),
                ("table absent", dict(loss="all"), False),
                ("table absent on both legs", dict(loss="all"), False),
                ("sparse emissions", dict(loss="sparse"), False),
                ("sparse on both legs", dict(loss="sparse"), False),
                ("insufficient shared media", dict(start=15000), False),
            ]
            for label, options, expected in cases:
                with self.subTest(label=label):
                    capture(
                        a,
                        loss=options["loss"] if label in ("table absent on both legs", "sparse on both legs") else None,
                    )
                    capture(b, **options)
                    result = subprocess.run(
                        [
                            sys.executable,
                            str(Path(__file__).with_name("table-anchor.py")),
                            str(a),
                            str(b),
                            "--source",
                            str(source),
                            "--report-json",
                            str(report),
                        ],
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, 0 if expected else 1, result.stdout + result.stderr)
                    parsed = json.loads(report.read_text())
                    if label == "insufficient shared media":
                        self.assertEqual(parsed["error"], "only 13.990s of acquired shared clock; need 20.0s")
                    else:
                        self.assertNotIn("error", parsed)
                        sdt = next(r for r in parsed["tables"] if r["pid"] == 0x11)
                        self.assertTrue(sdt["graded"])
                        self.assertEqual(sdt["ok"], expected)
                        self.assertTrue(all(r["ok"] for r in parsed["tables"] if r["pid"] != 0x11))
                        if label in ("persistent phase", "drift"):
                            self.assertEqual((sdt["a"], sdt["b"]), (12, 12))
                            self.assertLess(sdt["agreement_pct"], 90)
                        elif not expected:
                            counts = {
                                "table disappears in shared media": (14, 6),
                                "table absent": (14, 0),
                                "table absent on both legs": (0, 0),
                                "sparse emissions": (14, 7),
                                "sparse on both legs": (7, 7),
                            }
                            self.assertEqual((sdt["a"], sdt["b"]), counts[label])
                    print(f"PASS control: {label}")
            capture(a, wrap=True)
            capture(b, start=11500, wrap=True)
            # A shorter requested evidence window isolates wrap alignment; normal pair
            # runs retain the default 20-second minimum.
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("table-anchor.py")),
                    str(a),
                    str(b),
                    "--min-window",
                    "15",
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            print("PASS control: captures starting across PCR wrap")


if __name__ == "__main__":
    unittest.main()
