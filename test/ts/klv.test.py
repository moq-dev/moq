#!/usr/bin/env python3
"""Controls for the KLV fixture and its grader: each miss it exists to catch fails it."""

import unittest

import compliance
import klv

VIDEO = 0x100
PMT = 0x1000
# The picture presents this long after the PCR its PES starts by, in 90 kHz ticks.
LEAD = 63_000


def section(table_id, extension, body):
    out = bytearray([table_id, 0xB0, 0, extension >> 8, extension & 0xFF, 0xC1, 0, 0]) + body
    length = len(out) + 4 - 3
    out[1] |= length >> 8
    out[2] = length & 0xFF
    return bytes(out) + compliance.crc32_mpeg(bytes(out)).to_bytes(4, "big")


def psi(route, table):
    return bytes([0x47, 0x40 | route >> 8, route & 0xFF, 0x10, 0]) + table + b"\xff" * (183 - len(table))


def picture(counter, pcr):
    """A video packet carrying `pcr` and an unbounded PES presenting LEAD after it."""
    base, ext = pcr // 300, pcr % 300
    field = bytes([7, 0x10]) + (base >> 1).to_bytes(4, "big") + bytes([(base & 1) << 7 | 0x7E | ext >> 8, ext & 0xFF])
    pes = bytes([0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5]) + klv.clock_field(0x2, (base + LEAD) % klv.PTS_WRAP)
    body = pes + b"\x00" * (184 - len(field) - len(pes))
    return bytes([0x47, 0x40 | VIDEO >> 8, VIDEO & 0xFF, 0x30 | counter & 15]) + field + body


def null():
    return bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * 184


def media(pictures=50):
    """A clip of `pictures` 40 ms apart, each followed by enough null slots for its KLV."""
    out = [
        psi(0, section(0x00, 1, bytes([0, 1, 0xE0 | PMT >> 8, PMT & 0xFF]))),
        psi(
            PMT,
            section(
                0x02,
                1,
                bytes([0xE0 | VIDEO >> 8, VIDEO & 0xFF, 0xF0, 0, 0x1B, 0xE0 | VIDEO >> 8, VIDEO & 0xFF, 0xF0, 0]),
            ),
        ),
    ]
    for k in range(pictures):
        out.append(picture(k, 27_000_000 + k * 40 * 27_000))
        out.extend(null() for _ in range(6))
    return b"".join(out)


def edit(data, offset, change):
    out = bytearray(data)
    change(out, offset)
    return bytes(out)


def restamp(data, delta):
    """`data` with every PCR moved by `delta` 27 MHz ticks."""
    out = bytearray(data)
    for at in range(0, len(out), klv.SIZE):
        packet = out[at : at + klv.SIZE]
        value = klv.pcr(packet)
        if value is None:
            continue
        value += delta
        base, ext = value // 300, value % 300
        out[at + 6 : at + 10] = (base >> 1).to_bytes(4, "big")
        out[at + 10] = (base & 1) << 7 | 0x7E | ext >> 8
        out[at + 11] = ext & 0xFF
    return bytes(out)


class Klv(unittest.TestCase):
    def setUp(self):
        self.source, report = klv.inject(media())
        self.assertEqual(report, {"0x110": 20, "0x111": 50})
        self.pes = klv.private_pes(self.source, [klv.ASYNC_PID, klv.SYNC_PID])

    def assertFails(self, check, capture):
        with self.assertRaises(klv.Failure) as caught:
            klv.grade(self.source, capture)
        self.assertEqual(caught.exception.check, check, str(caught.exception))

    def test_the_source_grades_itself(self):
        report = klv.grade(self.source, self.source)
        self.assertEqual(report["shift"], 0)
        self.assertEqual(report["0x111"]["pes"], 50)

    def test_only_null_slots_change(self):
        before, after = klv.packets(media()), klv.packets(self.source)
        self.assertEqual(len(before), len(after))
        for old, new in zip(before, after):
            if klv.pid(old) not in (klv.NULL, PMT):
                self.assertEqual(old, new)

    def test_the_streams_and_their_pes(self):
        streams = klv.program(self.source).streams
        self.assertEqual(streams[klv.ASYNC_PID], (0x06, klv.KLVA))
        self.assertEqual(streams[klv.SYNC_PID], (0x15, klv.METADATA))
        sync = self.pes[klv.SYNC_PID][3].data
        self.assertEqual(sync[3], 0xFC)
        self.assertEqual(klv.clock_value(sync[9:14]), 27_000_000 // 300 + 3 * 3_600 + LEAD)
        untimed = self.pes[klv.ASYNC_PID][0].data
        self.assertEqual((untimed[3], untimed[7] >> 6), (0xBD, 0))

    def test_a_shift_moves_every_clock_alike(self):
        delta = 90_000
        capture = self.source
        for pes in self.pes[klv.SYNC_PID]:
            at = pes.at + 9
            value = (klv.clock_value(capture[at : at + 5]) + delta) % klv.PTS_WRAP
            capture = edit(
                capture,
                at,
                lambda out, at, value=value: out.__setitem__(slice(at, at + 5), klv.clock_field(0x2, value)),
            )
        self.assertEqual(klv.grade(self.source, restamp(capture, delta * 300))["shift"], delta)
        # One clock moved apart from the rest.
        at = self.pes[klv.SYNC_PID][7].at + 9
        self.assertFails(
            "shift", edit(capture, at, lambda out, at: out.__setitem__(slice(at, at + 5), klv.clock_field(0x2, 0)))
        )

    def test_a_dropped_pes_fails(self):
        at = self.pes[klv.SYNC_PID][10].at // klv.SIZE * klv.SIZE
        self.assertFails(
            "missing", edit(self.source, at, lambda out, at: out.__setitem__(slice(at, at + klv.SIZE), null()))
        )

    def test_a_changed_byte_fails(self):
        at = self.pes[klv.ASYNC_PID][1].at + 30
        self.assertFails("bytes", edit(self.source, at, lambda out, at: out.__setitem__(at, out[at] ^ 1)))

    def test_a_changed_descriptor_fails(self):
        self.assertFails("pmt", self.source.replace(b"KLVA", b"KLVB", 1))

    def test_a_pes_out_early_fails(self):
        # Past one packet, which at this sparse clip's rate is about 6 ms.
        self.assertFails("early", restamp(self.source, -20 * 27_000))

    def test_a_pes_out_after_its_pts_fails(self):
        self.assertFails("late", restamp(self.source, LEAD * 300 + 27_000))


if __name__ == "__main__":
    unittest.main()
