# Voice gate validation

The harness behind [moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998) (SILK's detector) and [moq-dev/moq#9031](https://github.com/moq-dev/moq/pull/9031) (libopus's tonality analysis), committed once for a permalink.
Run everything from this directory inside the repository's `nix develop` shell.

## Inputs

```sh
mkdir -p dl
curl -L -o dl/speech.mp3 https://archive.org/download/count_monte_cristo_0711_librivox/count_of_monte_cristo_001_dumas.mp3
./make-signal.sh   # signal.wav: speech, room tone, a fan switched on at 20 s, a quiet talker; speech.wav alone
./make-fan.sh      # fan.wav: the same with the fan running from the start
./build.sh         # ref_analysis and dtx, against libopus 1.6.1
```

## The port against libopus

`./sweep.sh` runs each check at 16, 24, and 48 kHz and 2.5 to 60 ms frames:

- `prob.ts` against `ref_analysis`: the analysis's activity probability per frame (`cmp.py`: the largest difference, and decisions at 0.1 that differ).
- `decide.ts` against `dtx`: whether the encoder is in DTX per frame, from the activity and its 25 dB peak rule (`cmpdtx.py`).
- `gatevs.ts` against `dtx`: the frames the `Gate` sends against the frames the encoder sends, allowing the gate's pre-roll.

`bun fixture.ts <rate> <out.s16>` writes `analysis.test.ts`'s signal for regenerating its vectors with `ref_analysis` and `dtx`.
`bun cpu.ts signal.s16` times the analysis and the gate.

## Browsers

`drive.ts` serves `page/` and plays an input as the microphone: Chromium's fake capture, or under `test/lib/pulse.py` a private PulseAudio for Firefox.
The page captures it twice, as a voice source (gated) and a kindless one (ungated), and records both encoders' frames and the gated capture's PCM.

```sh
./runs.sh chromium signal fan                    # processing on and off; writes <run>.txt
python3 ../../test/lib/pulse.py --dir "$PWD/pulse" -- ./firefox.sh signal on
```

`analyze.py` grades a run: bytes per segment against the ungated encoder, speech windows (above -45 dBFS in both the capture and `speech.wav`) that no sent frame covers, and the comfort-noise level a player would fill each pause with against the captured background.
For a baseline, run the same in a checkout of the base branch.
`levels.py` shows what the browser's processing did to the level.
