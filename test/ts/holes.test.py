import contextlib
import importlib.util
import io
import pathlib
import subprocess
import sys
import tempfile
import unittest
from fractions import Fraction
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("holes", pathlib.Path(__file__).with_name("holes.py"))
holes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(holes)


class Holes(unittest.TestCase):
    SOURCE = [i * 2160 for i in range(50)]

    def test_continuous(self):
        self.assertEqual(holes.step(self.SOURCE), 2160)
        self.assertEqual(holes.holes(self.SOURCE, 2160), [])

    def test_one_lost_frame(self):
        dts = [i * 2160 for i in range(50) if i != 20]
        self.assertEqual(holes.holes(dts, holes.step(self.SOURCE)), [(19 * 2160, 2 * 2160)])

    def test_every_other_frame_lost(self):
        # The capture's own step is two frames and its last frame is the source's, so only
        # the source's step shows that half the media is gone.
        dts = self.SOURCE[1::2]
        self.assertEqual(len(holes.holes(dts, holes.step(self.SOURCE))), len(dts) - 1)

    def test_jitter_within_half_a_frame(self):
        # 90 kHz rounding of a 1024-sample frame at 44.1 kHz alternates 2089 and 2090.
        dts = [i * 2089 + i // 2 for i in range(50)]
        self.assertEqual(holes.holes(dts, holes.step(dts)), [])

    def test_too_short(self):
        self.assertEqual(holes.step([0]), 0)


# Two B-pyramid groups of 25 frames in decode order (I P B b b ...), in 40 ms frames.
PYRAMID = [0] + [i + d for i in range(1, 25, 4) for d in (3, 1, 0, 2)]


class PerGroupDecodeTimes(unittest.TestCase):
    PTS = [(g * 25 + i) * 3600 for g in range(2) for i in PYRAMID]
    # The export decodes each group's first two frames a tick apart just below its earliest
    # presentation, so its decode timestamps step at every group with nothing lost.
    DTS = [g * 25 * 3600 + (i - 2 if i < 2 else (i - 2) * 3600) for g in range(2) for i in range(25)]

    def test_decode_steps_at_groups_are_not_holes(self):
        self.assertTrue(holes.holes(self.DTS, 3600))
        self.assertEqual(holes.holes(sorted(self.PTS), holes.step(sorted(self.PTS))), [])

    def test_a_lost_b_picture_is_a_hole(self):
        lost = self.PTS[:10] + self.PTS[11:]
        self.assertEqual(len(holes.holes(sorted(lost), holes.step(sorted(self.PTS)))), 1)

    def grade(self, kept: list[int]) -> int:
        def stream(index):
            dts, pts = [self.DTS[i] for i in index], [self.PTS[i] for i in index]
            return holes.Stream(
                {"codec_name": "h264", "time_base": "1/90000"}, dts, pts, [True] * len(pts),
                tuple(Fraction(t, 90_000) for t in (min(pts), max(pts), dts[-1])),
            )
        with patch.object(holes, "probe", side_effect=[{"0x101": stream(kept)}, {"0x101": stream(range(50))}]), \
                patch.object(sys, "argv", ["holes.py", "capture.ts", "source.ts"]), \
                contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit) as exit:
            holes.main()
        return exit.exception.code

    def test_main_grades_presentation_timestamps(self):
        self.assertEqual(self.grade(range(50)), 0)
        self.assertEqual(self.grade([i for i in range(50) if i != 10]), 1)


class Shed(unittest.TestCase):
    # Four groups of 25 frames, each opening on a sync frame, with each P picture decoded ahead
    # of the B picture it presents after.
    PTS = [(i + (0 if i % 25 == 0 else 1 if i % 25 % 2 else -1)) * 3600 for i in range(100)]
    SYNC = [i % 25 == 0 for i in range(100)]

    def grade(self, *lost: range) -> holes.Shed:
        kept = [t for i, t in enumerate(self.PTS) if not any(i in r for r in lost)]
        return holes.shed(self.PTS, self.SYNC, kept)

    def test_whole(self):
        self.assertEqual(self.grade(), holes.Shed(missing=0, mid_group=[], tail=0, unmatched=[]))

    def test_whole_group(self):
        self.assertEqual(self.grade(range(25, 50)), holes.Shed(missing=25, mid_group=[], tail=0, unmatched=[]))

    def test_rest_of_a_group(self):
        # The rest of a group skipped once it went stale, resuming at the next one.
        self.assertEqual(self.grade(range(40, 50)), holes.Shed(missing=10, mid_group=[], tail=0, unmatched=[]))

    def test_late_join(self):
        # Groups before the first kept frame are the join, not shed.
        self.assertEqual(self.grade(range(0, 50)), holes.Shed(missing=0, mid_group=[], tail=0, unmatched=[]))

    def test_reordered(self):
        kept = list(self.PTS)
        kept[30], kept[31] = kept[31], kept[30]
        self.assertTrue(holes.shed(self.PTS, self.SYNC, kept).unmatched)

    def test_repeated(self):
        kept = self.PTS[:31] + self.PTS[30:]
        self.assertTrue(holes.shed(self.PTS, self.SYNC, kept).unmatched)

    def test_not_in_the_source(self):
        self.assertTrue(holes.shed(self.PTS, self.SYNC, self.PTS + [1000 * 3600]).unmatched)

    def test_resumes_mid_group(self):
        graded = self.grade(range(40, 45))
        self.assertEqual(graded.mid_group, self.PTS[45:50])

    def test_joins_mid_group(self):
        # Opening on a frame whose group's sync frame was never captured is as undecodable as resuming there.
        self.assertEqual(self.grade(range(0, 30)).mid_group, self.PTS[30:50])

    def test_lost_tail(self):
        # Shedding the last group means the export fell behind and never caught up.
        self.assertEqual(self.grade(range(90, 100)).tail, 10)
        self.assertEqual(self.grade(range(75, 100)).tail, 25)


class Cuts(unittest.TestCase):
    # First PTS, last PTS, last DTS: the last pictures in decode order are B pictures presented before the P.
    SOURCE = (Fraction(1), Fraction(31), Fraction(30) + Fraction(23, 25))
    FRAME = Fraction(1, 25)

    def test_joined_on_a_later_group(self):
        capture = (Fraction(3), *self.SOURCE[1:])
        self.assertEqual(holes.cuts(self.SOURCE, capture, self.FRAME), [])

    def test_lost_tail(self):
        # One continuous stream that stops early has no hole and nothing to compare against.
        capture = (Fraction(3), Fraction(27), Fraction(26))
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)

    def test_lost_trailing_b_pictures(self):
        # The last P picture still presents last, and nothing after it leaves a step.
        capture = (Fraction(3), Fraction(31), Fraction(30) + Fraction(20, 25))
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)

    def test_lost_prefix(self):
        capture = (Fraction(10), *self.SOURCE[1:])
        self.assertEqual(len(holes.cuts(self.SOURCE, capture, self.FRAME)), 1)


class LegacyTimestamps(unittest.TestCase):
    def grade(self, codec, source, capture):
        def stream(pts):
            return holes.Stream(
                {"codec_name": codec, "time_base": "1/90000"}, pts, pts, [True] * len(pts),
                tuple(Fraction(t, 90_000) for t in (pts[0], pts[-1], pts[-1])),
            )
        with patch.object(holes, "probe", side_effect=[{"0x101": stream(capture)}, {"0x101": stream(source)}]), \
                patch.object(sys, "argv", ["holes.py", "capture.ts", "source.ts", "--shed"]), \
                contextlib.redirect_stdout(io.StringIO()), self.assertRaises(SystemExit) as exit:
            holes.main()
        return exit.exception.code

    def test_legacy_microseconds_keep_the_source_frames(self):
        # Legacy audio keeps its cadence, but its microsecond timestamps return
        # one 90 kHz tick early unless they already land on the microsecond grid.
        for codec, first, step, shift in [
            ("mp2", 132298, 2160, 1), ("ac3", 132720, 2880, 1),
            ("mp2", 132300, 2160, 0), ("ac3", 132723, 2880, 0),
        ]:
            with self.subTest(codec=codec, first=first):
                source = [first + i * step for i in range(5)]
                self.assertEqual(self.grade(codec, source, [t - shift for t in source]), 0)

    def test_a_shift_beyond_legacy_quantization_fails(self):
        source = [132298 + i * 2160 for i in range(5)]
        self.assertEqual(self.grade("mp2", source, [t - 2 for t in source]), 1)

    def test_a_lost_legacy_tail_still_fails(self):
        source = [132298 + i * 2160 for i in range(5)]
        self.assertEqual(self.grade("mp2", source, [t - 1 for t in source[:-1]]), 1)

    def test_raw_video_does_not_use_legacy_quantization(self):
        source = [133200 + i * 3600 for i in range(5)]
        self.assertEqual(self.grade("h264", source, [t - 1 for t in source]), 1)


class Probe(unittest.TestCase):
    def test_audio_starting_past_the_default_probe(self):
        # Intra-only video ahead of an MP2 stream that starts 3 s in, past FFmpeg's default 5 MB
        # probe: probed that far only, its first PES reads as a hole.
        with tempfile.TemporaryDirectory() as tmp:
            path = str(pathlib.Path(tmp, "late.ts"))
            subprocess.run([
                "ffmpeg", "-v", "error",
                "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25:duration=4",
                "-itsoffset", "3", "-f", "lavfi", "-i", "sine=sample_rate=48000:duration=1",
                "-map", "0:v", "-map", "1:a", "-c:v", "mpeg2video", "-g", "1", "-q:v", "1",
                "-c:a", "mp2", "-f", "mpegts", path,
            ], check=True)
            streams = holes.probe(path)
        dts = streams["0x101"].dts
        self.assertEqual(holes.holes(dts, holes.step(dts)), [])

if __name__ == "__main__":
    unittest.main()
