"""The tone check passes a clean decode and fails each way a decode can garble the tone."""
import importlib.util
import math
from pathlib import Path
import struct
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('tone', HERE / 'tone.py')
tone = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tone)


class ToneTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.prefix = Path(temp.name) / 'audio'

    def write(self, tones, *, rate=48000, generated_at=None, frame=960, frames=40, gap_after=None, start=123_456, late=0):
        """Decode output as a subscriber writes it: `frames` frames of `frame` samples per channel,
        each tone `sin(2πft)` at its timestamp, or `late` samples behind it."""
        generated_at = generated_at or rate
        log = [f'{rate} {len(tones)}']
        pcm = bytearray()
        ts = start
        for index in range(frames):
            log.append(f'{ts} {frame}')
            for n in range(index * frame, (index + 1) * frame):
                pcm += struct.pack(f'<{len(tones)}f', *(0.5 * math.sin(2 * math.pi * hz * (start / 1_000_000 + (n - late) / generated_at)) for hz in tones))
            ts += frame * 1_000_000 // rate
            if index == gap_after:
                ts += 20_000
        Path(f'{self.prefix}.txt').write_text('\n'.join(log) + '\n')
        Path(f'{self.prefix}.pcm').write_bytes(bytes(pcm))

    def assertFails(self, tones, reason):
        with self.assertRaises(tone.Failure) as caught:
            tone.check(self.prefix, tones, phase=True)
        self.assertIn(reason, str(caught.exception))

    def test_a_clean_stereo_decode_passes(self):
        self.write([440, 660])
        self.assertIn('2 ch @ 48000 Hz', tone.check(self.prefix, [440, 660], phase=True))

    def test_a_clean_mono_decode_passes(self):
        self.write([440], frame=120, frames=320)
        self.assertIn('1 ch', tone.check(self.prefix, [440], phase=True))

    def test_swapped_channels_fail(self):
        self.write([660, 440])
        self.assertFails([440, 660], 'wrong tone: channel 0 carries ~660 Hz, not 440 Hz')

    def test_the_wrong_frequency_fails(self):
        self.write([880])
        self.assertFails([440], 'not 440 Hz')

    def test_a_wrong_sample_rate_fails(self):
        self.write([440], generated_at=44100)
        self.assertFails([440], 'not 440 Hz')

    def test_silence_fails(self):
        self.write([0])
        self.assertFails([440], 'silent')

    def test_a_downmix_fails(self):
        self.write([440])
        self.assertFails([440, 660], 'decoded 1 channels, the publisher sent 2')

    def test_a_timestamp_gap_fails(self):
        self.write([440], gap_after=10)
        self.assertFails([440], 'timestamp gap')

    # Negative controls for --phase: a decoder that keeps Opus's usual 312 samples of pre-skip, or
    # trims them twice, decodes a clean tone that only its timestamps can show is misplaced.
    def test_a_kept_pre_skip_fails(self):
        self.write([440, 660], late=312)
        self.assertFails([440, 660], "tone off its timestamps: channel 0's 440 Hz is +318us")

    def test_a_pre_skip_trimmed_twice_fails(self):
        self.write([440], frame=120, frames=320, late=-312)
        self.assertFails([440], 'is -318us')

    def test_a_kept_pre_skip_passes_without_phase(self):
        self.write([440, 660], late=312)
        self.assertIn('2 ch', tone.check(self.prefix, [440, 660]))

    def test_too_little_audio_fails(self):
        self.write([440], frames=10)
        self.assertFails([440], 'only 200ms decoded')


if __name__ == '__main__':
    unittest.main()
