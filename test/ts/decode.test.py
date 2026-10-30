import json
import pathlib
import subprocess
import tempfile
import unittest

import decode


class Decode(unittest.TestCase):
    def test_audio_starting_past_the_default_probe(self):
        # The first MP2 PES follows more than 5 MB of intra-only video.
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            path = root / "late.ts"
            subprocess.run(
                [
                    "ffmpeg",
                    "-nostdin",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=1280x720:rate=25:duration=4",
                    "-itsoffset",
                    "3",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=1",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "mpeg2video",
                    "-g",
                    "1",
                    "-q:v",
                    "1",
                    "-threads",
                    "1",
                    "-c:a",
                    "mp2",
                    "-f",
                    "mpegts",
                    str(path),
                ],
                check=True,
            )
            # This profile stays unsupported, but its recorded probe must describe
            # the real codec rather than guessing MP3 from an empty prefix.
            with self.assertRaises(decode.Failure) as refused:
                decode.decode(path, root / "decoded")
            self.assertEqual(refused.exception.check, "profile")
            streams = json.loads((root / "decoded/probe.json").read_bytes())["streams"]
            audio = next(s for s in streams if s["codec_type"] == "audio")
            self.assertEqual(audio["codec_name"], "mp2")
            self.assertEqual(int(audio["sample_rate"]), 48000)


if __name__ == "__main__":
    unittest.main()
