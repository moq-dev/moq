# Voice silence validation

The harness behind [moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998), committed once for a permalink.
Run everything from this directory inside the repository's `nix develop` shell.

## Inputs

```sh
mkdir -p dl
curl -L -o dl/speech.mp3 https://archive.org/download/count_monte_cristo_0711_librivox/count_of_monte_cristo_001_dumas.mp3
./make-signal.sh   # signal.wav: speech, room tone, a fan switched on at 20 s, a quiet talker
./make-fan.sh      # fan.wav: the same with the fan running from the start
./build.sh         # harness and encoder, against libopus 1.6.1
```

## The port against libopus

- `./rates.sh`: the port's Q8 activity against SILK's C at 8 to 48 kHz, 10 and 20 ms blocks (expect 0 differ).
- `bun compare.ts signal.s16 48000 20 > js.txt`, `./encoder 9 < signal.s16 > enc.txt`, `python3 agree.py js.txt enc.txt`: per-segment agreement with the real encoder's SILK VAD flag and DTX (try complexity 5 and 9).
- `bun fixture.ts <rate> <out.s16>` writes `vad.test.ts`'s signal for regenerating its conformance vectors with `harness`.
- `bun steady.ts`: which blocks of steady white noise score as speech, by level.

## The gate

- `bun gate.ts signal.s16`: frames the gate passes per segment.
- `bun trace.ts signal.s16 <from s> <to s>`: per frame RMS, Q8, and gate state.

## Browsers

`drive.ts` serves `page/` and plays an input as the microphone: Chromium's fake capture, or under `test/lib/pulse.py` a private PulseAudio for Firefox.
The page captures it twice, as a voice source (gated) and a kindless one (ungated), and records both encoders' frames and the gated capture's PCM.

```sh
./runs.sh chromium signal fan                    # processing on and off; writes <run>.txt
python3 ../../test/lib/pulse.py --dir "$PWD/pulse" -- ./firefox.sh signal on
```

`analyze.py` grades a run: bytes per segment against the ungated encoder, captured speech windows above -45 dBFS that no sent frame covers, and the comfort-noise level a player would fill each pause with against the captured background.
`levels.py` shows what the browser's processing did to the level.
