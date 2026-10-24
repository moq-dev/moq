#!/usr/bin/env python3
"""Controls for the PMT revision fixture and its grader: each miss it exists to catch fails it."""

import unittest

import compliance
import klv
import revision

VIDEO = 0x100
PMT = 0x1000


def section(table_id, extension, body):
    out = bytearray([table_id, 0xB0, 0, extension >> 8, extension & 0xFF, 0xC1, 0, 0]) + body
    length = len(out) + 4 - 3
    out[1] |= length >> 8
    out[2] = length & 0xFF
    return bytes(out) + compliance.crc32_mpeg(bytes(out)).to_bytes(4, "big")


def psi(route, table):
    return bytes([0x47, 0x40 | route >> 8, route & 0xFF, 0x10, 0]) + table + b"\xff" * (183 - len(table))


def picture(counter, pcr):
    """A video packet carrying `pcr` and the start of an unbounded PES."""
    base, ext = pcr // 300, pcr % 300
    field = bytes([7, 0x10]) + (base >> 1).to_bytes(4, "big") + bytes([(base & 1) << 7 | 0x7E | ext >> 8, ext & 0xFF])
    pes = bytes([0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5]) + klv.clock_field(0x2, base % klv.PTS_WRAP)
    body = pes + b"\x00" * (184 - len(field) - len(pes))
    return bytes([0x47, 0x40 | VIDEO >> 8, VIDEO & 0xFF, 0x30 | counter & 15]) + field + body


NULL = bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * 184
PAT = psi(0, section(0x00, 1, bytes([0, 1, 0xE0 | PMT >> 8, PMT & 0xFF])))
TABLE = psi(PMT, section(0x02, 1, bytes([0xE0 | VIDEO >> 8, VIDEO & 0xFF, 0xF0, 0, 0x1B, 0xE0 | VIDEO >> 8, VIDEO & 0xFF, 0xF0, 0])))


def media(pictures=150):
    """A clip of `pictures` 40 ms apart, its tables every ten, each picture followed by null slots."""
    out = []
    for k in range(pictures):
        if k % 10 == 0:
            out += [PAT, TABLE]
        out.append(picture(k, 27_000_000 + k * 40 * 27_000))
        out.extend([NULL] * 6)
    return b"".join(out)


def replace(data, index, packet):
    return data[: index * klv.SIZE] + packet + data[(index + 1) * klv.SIZE :]


class Revision(unittest.TestCase):
    def setUp(self):
        self.source, self.report = revision.inject(media())

    def assertFails(self, check, capture):
        with self.assertRaises(revision.Failure) as caught:
            revision.grade(self.source, capture)
        self.assertEqual(caught.exception.check, check, str(caught.exception))

    def test_the_source_revises_twice_and_grades_itself(self):
        tables = revision.layouts(self.source)
        self.assertEqual([version for _, version, _ in tables], [0, 1, 2])
        self.assertEqual([revision.PID in streams for _, _, streams in tables], [False, True, False])
        self.assertEqual(revision.grade(self.source, self.source)["versions"], [0, 1, 2])
        self.assertGreater(self.report["pes"], 10)

    def test_only_null_slots_and_tables_change(self):
        for old, new in zip(klv.packets(media()), klv.packets(self.source)):
            if klv.pid(old) not in (klv.NULL, PMT):
                self.assertEqual(old, new)

    def test_the_stream_runs_between_its_revisions(self):
        opened, closed = self.report["revised_at"]
        on_pid = [k for k, packet in enumerate(klv.packets(self.source)) if klv.pid(packet) == revision.PID]
        self.assertLess(opened, on_pid[0])
        self.assertLess(on_pid[-1], closed)

    def test_a_skipped_version_fails(self):
        _, closed = self.report["revised_at"]
        capture = self.source
        for k, packet in enumerate(klv.packets(self.source)):
            if k >= closed and klv.pid(packet) == PMT:
                capture = replace(capture, k, revision.revise(TABLE, 3, []))
        self.assertFails("version", capture)

    def test_a_stale_table_after_the_revision_fails(self):
        packets = klv.packets(self.source)
        _, closed = self.report["revised_at"]
        listing = packets[self.report["revised_at"][0]]
        after = next(k for k in range(closed + 1, len(packets)) if klv.pid(packets[k]) == klv.NULL)
        self.assertFails("layouts", replace(self.source, after, listing))

    def test_a_packet_after_the_stream_was_dropped_fails(self):
        packets = klv.packets(self.source)
        _, closed = self.report["revised_at"]
        stream = next(packet for packet in packets if klv.pid(packet) == revision.PID)
        after = next(k for k in range(closed + 1, len(packets)) if klv.pid(packets[k]) == klv.NULL)
        self.assertFails("window", replace(self.source, after, stream))

    def test_a_dropped_pes_fails(self):
        packets = klv.packets(self.source)
        index = [k for k, packet in enumerate(packets) if klv.pid(packet) == revision.PID][3]
        self.assertFails("pes", replace(self.source, index, NULL))

    def test_a_capture_that_never_revised_fails(self):
        self.assertFails("layouts", media())


if __name__ == "__main__":
    unittest.main()
