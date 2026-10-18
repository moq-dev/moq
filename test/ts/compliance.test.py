import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import compliance  # noqa: E402


def eac3(stream: int) -> bytes:
    """A six-block 48 kHz E-AC-3 sync frame of 512 bytes; `stream` is byte 2's top bits."""
    return bytes([0x0B, 0x77, stream, 0xFF, 0x3F, 0x80]) + b"\xaa" * 506


class Eac3Frames(unittest.TestCase):
    def test_only_independent_substream_zero_advances_time(self):
        # Independent substream 0, a dependent substream, then independent substream 1.
        for stream, duration in ((0x00, 0.032), (0x40, 0.0), (0x08, 0.0)):
            with self.subTest(stream=stream):
                self.assertEqual(compliance.ac3_frame(eac3(stream), 0), (512, duration))

    def test_unsupported_bsid_is_refused(self):
        for bsid in (9, 10, 17):
            with self.subTest(bsid=bsid):
                frame = bytearray(eac3(0x00))
                frame[5] = bsid << 3
                with self.assertRaises(compliance.Refused):
                    compliance.ac3_frame(bytes(frame), 0)

    def test_reserved_stream_type_is_refused(self):
        with self.assertRaises(compliance.Refused):
            compliance.ac3_frame(eac3(0xC0), 0)

    def test_reserved_reduced_rate_is_refused(self):
        frame = bytearray(eac3(0x00))
        frame[4] = 0xF0  # fscod 3 with fscod2 3
        with self.assertRaises(compliance.Refused):
            compliance.ac3_frame(bytes(frame), 0)


if __name__ == "__main__":
    unittest.main()
