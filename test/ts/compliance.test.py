import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import compliance  # noqa: E402

PMT_PID = 0x1000


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


def section(table_id: int, extension: int, version: int, body: bytes) -> bytes:
    out = bytearray([table_id, 0xB0, 0, extension >> 8, extension & 0xFF, 0xC1 | version << 1, 0, 0]) + body
    length = len(out) + 4 - 3
    out[1] |= length >> 8
    out[2] = length & 0xFF
    return bytes(out) + compliance.crc32_mpeg(bytes(out)).to_bytes(4, "big")


def psi(pid: int, counter: int, table: bytes) -> bytes:
    return bytes([0x47, 0x40 | pid >> 8, pid & 0xFF, 0x10 | counter & 15, 0]) + table + b"\xff" * (183 - len(table))


def tables(*pmts: tuple[int, list[int]]) -> bytes:
    """A PAT, then each PMT as (version, the PIDs of its AAC streams), on PMT_PID."""
    pat = section(0x00, 1, 0, bytes([0, 1, 0xE0 | PMT_PID >> 8, PMT_PID & 0xFF]))
    out = [psi(0, 0, pat)]
    for counter, (version, pids) in enumerate(pmts):
        streams = b"".join(bytes([0x0F, 0xE0 | pid >> 8, pid & 0xFF, 0xF0, 0]) for pid in pids)
        out.append(psi(PMT_PID, counter, section(0x02, 1, version, bytes([0xE1, 0x01, 0xF0, 0]) + streams)))
    return b"".join(out)


class PmtVersions(unittest.TestCase):
    def check(self, *pmts: tuple[int, list[int]]) -> compliance.Check:
        with tempfile.NamedTemporaryFile(suffix=".ts") as handle:
            handle.write(tables(*pmts))
            handle.flush()
            return compliance.check_pmt_versions(handle.name, 188)

    def test_repeats_and_revisions_pass(self):
        check = self.check((0, [0x101]), (0, [0x101]), (1, [0x101, 0x102]), (1, [0x101, 0x102]), (2, [0x101]))
        self.assertEqual((check.status, check.metrics), (compliance.Status.PASS, {"revisions": 2, "skipped": 0}))

    def test_the_version_wraps(self):
        check = self.check((31, [0x101]), (0, [0x101, 0x102]))
        self.assertEqual((check.status, check.metrics["revisions"]), (compliance.Status.PASS, 1))

    def test_a_skipped_version_is_reported(self):
        check = self.check((0, [0x101]), (2, [0x101, 0x102]))
        self.assertEqual((check.status, check.metrics["skipped"]), (compliance.Status.PASS, 1))

    def test_a_change_under_the_same_version_fails(self):
        self.assertEqual(self.check((0, [0x101]), (0, [0x101, 0x102])).status, compliance.Status.FAIL)

    def test_an_older_version_after_a_newer_fails(self):
        check = self.check((0, [0x101]), (1, [0x101, 0x102]), (0, [0x101]))
        self.assertEqual(check.status, compliance.Status.FAIL)
        self.assertIn("after version 1", check.detail)

    def test_a_copy_past_half_the_versions_back_fails(self):
        revisions = [(version, [0x101, 0x200 + version]) for version in range(18)]
        check = self.check(*revisions, (0, [0x101, 0x200]))
        self.assertEqual(check.status, compliance.Status.FAIL)
        self.assertIn("after version 17", check.detail)

    def test_a_table_back_after_every_version_passes(self):
        cycle = [(version % 32, [0x101, 0x200 + version % 32]) for version in range(33)]
        check = self.check(*cycle)
        self.assertEqual((check.status, check.metrics["revisions"]), (compliance.Status.PASS, 32))


if __name__ == "__main__":
    unittest.main()
