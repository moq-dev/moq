#!/usr/bin/env python3
"""Controls for the generated fragmented-header fixture."""

import unittest

from fragment_pes import fragment, packet, payload, pid


class FragmentedHeaders(unittest.TestCase):
    def test_payload_pcr_packet_count_and_continuity_survive(self):
        source = []
        expected = {}
        for counter in range(2):
            for route, stream in [(0x50, 0xE0), (0x51, 0xC0)]:
                header = bytes([0, 0, 1, stream, 0, 0, 0x80, 0x80, 5])
                body = header + b"\x21\0\x01\0\x01" + bytes(range(40))
                base = bytes([0x47, 0x40, route, 0x10]) + b"\xff" * 184
                original = bytearray(packet(base, body, True, counter))
                if route == 0x50:
                    original[5] = 0x10
                    original[6:12] = b"\x01\x02\x03\x04\x7e\0"
                source.extend([bytes(original), b"\x47\x1f\xff\x10" + b"\xff" * 184])
                expected[(route, counter)] = body
        result, report = fragment(b"".join(source))
        self.assertEqual(len(result), len(source) * 188)
        actual = [result[at : at + 188] for at in range(0, len(result), 188)]
        for at in range(0, len(actual), 2):
            first, second = actual[at : at + 2]
            route = pid(first)
            self.assertEqual(pid(second), route)
            original_counter = source[at][3] & 15
            self.assertEqual(payload(first) + payload(second), expected[(route, original_counter)])
            self.assertTrue(first[1] & 0x40)
            self.assertFalse(second[1] & 0x40)
            self.assertEqual(first[3] & 15, original_counter * 2)
            self.assertEqual(second[3] & 15, original_counter * 2 + 1)
            if route == 0x50:
                self.assertEqual(first[5:12], source[at][5:12])
                self.assertEqual(second[5], 0)
        self.assertEqual(report["counts"], {0x50: 2, 0x51: 2})
        self.assertEqual(report["splits"], {0x50: [1, 2], 0x51: [1, 2]})

    def test_a_full_pes_burst_shifts_into_the_following_null_slot(self):
        body = bytes([0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5]) + b"\x21\0\x01\0\x01" + b"v" * 170
        first = bytes([0x47, 0x40, 0x50, 0x10]) + body
        continuation = bytearray(packet(bytes([0x47, 0, 0x50, 0x10]) + b"\xff" * 184, b"c" * 170, False, 1))
        continuation[5] = 0x10
        continuation[6:12] = b"\x01\x02\x03\x04\x7e\0"
        audio_body = bytes([0, 0, 1, 0xC0, 0, 0, 0x80, 0x80, 5]) + b"\x21\0\x01\0\x01"
        audio = packet(bytes([0x47, 0x40, 0x51, 0x10]) + b"\xff" * 184, audio_body, True, 0)
        null = b"\x47\x1f\xff\x10" + b"\xff" * 184
        result, _ = fragment(first + continuation + null + audio + null)
        packets = [result[at : at + 188] for at in range(0, len(result), 188)]
        self.assertEqual(b"".join(payload(p) for p in packets if pid(p) == 0x50), body + b"c" * 170)
        self.assertEqual(packets[1][5:12], continuation[5:12])
        self.assertEqual([p[3] & 15 for p in packets[:3]], [0, 1, 2])

    def test_missing_media_null_slots_and_misalignment_are_refused(self):
        for data in [b"", b"\x47" * 187, b"\x47\x1f\xff\x10" + b"\xff" * 184]:
            with self.assertRaises(ValueError):
                fragment(data)


if __name__ == "__main__":
    unittest.main()
