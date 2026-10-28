import unittest

import clockless
import klv


class ClocklessTests(unittest.TestCase):
    def test_data_only_profiles_grade_their_source(self):
        for kind in ["sections", "pes"]:
            with self.subTest(kind=kind):
                source = clockless.generate(kind, 2, 1_000_000)
                clockless.grade(source, source, kind)

    def test_a_missing_tail_fails(self):
        source = clockless.generate("sections", 2, 1_000_000)
        packets = klv.packets(source)
        last = max(i for i, packet in enumerate(packets) if klv.pid(packet) == clockless.DATA)
        packets[last] = bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * 184
        with self.assertRaisesRegex(ValueError, "source suffix"):
            clockless.grade(source, b"".join(packets), "sections")

    def test_a_dedicated_clock_cannot_carry_payload(self):
        source = clockless.generate("pes", 2, 1_000_000)
        packets = klv.packets(source)
        index = next(i for i, packet in enumerate(packets) if klv.pid(packet) == clockless.CLOCK)
        packet = bytearray(packets[index])
        packet[3] |= 0x10
        packets[index] = bytes(packet)
        with self.assertRaisesRegex(ValueError, "carried a payload"):
            clockless.grade(source, b"".join(packets), "pes")

    def test_a_unit_before_its_placement_fails(self):
        source = clockless.generate("pes", 2, 1_000_000)
        packets = klv.packets(source)
        for index, packet in enumerate(packets):
            if (ticks := klv.pcr(packet)) is not None:
                packets[index] = clockless.clock_packet(klv.pid(packet), ticks - clockless.HZ, packet[3] & 15)
        with self.assertRaisesRegex(ValueError, "before its source placement"):
            clockless.grade(source, b"".join(packets), "pes")


if __name__ == "__main__":
    unittest.main()
