# [L] Audio outlives video under bufferbloat

## Goal

A nightly test sends audio and video together through a bufferbloated link
and proves audio holds up better than video: audio stays within its underrun
and skip budgets while video may freeze, skip, or lag audio. Today no test
publishes both through the shaper, and the audio-quality profiles leave rate
limits off.

## Plan

Decided in planning (2026-10-20):

- Behavior asserted is audio continuity. Audio is sent first (players subscribe to audio at a higher priority than video) and its
  ring plays through a video stall. In `"auto"` delay the shared target is
  the deepest track's, so late video raises audio latency too; that is expected, not a failure. Making
  audio latency independent of video would be a player change, not this
  quest.
- Extend `test/audio-quality` rather than routing media QA through the
  shaper: add rows whose publisher sends the media QA pattern (frame counter
  plus stepped tone), and reuse the tap worklet, budgets, captures, and
  replays. Add media QA's sync probe to report each rendered frame's gap to
  audio. Video freezes, skips, and lag are reported, not gated: the point is
  that audio does not pay for them. Since 2026-11-05 the shared clock never
  follows audio, so congested video lags it by design, and steady-state lip
  sync belongs to the audio ring's time stretch. Since 2026-11-13 audio
  trails the clock by at most its 200 ms `stretch` threshold and snaps onto
  it past that ([audio playout](/quest/m2/audio-playout/README.md)), so these
  rows gate how soon audio is back on the clock and its underruns, and budget
  the audio it skips rather than forbid it.
- Folded in: the native `pulse` rows become enforced. They report only
  because an intermittent two-second device stall voids about one AAC native
  row in seven (see `test/audio-quality/README.md`). Reproduce and fix that
  stall at its cause; do not loosen the void rule to get green.
  Start by re-measuring: [moq-dev/moq#7138](https://github.com/moq-dev/moq/pull/7138)
  found that the private PulseAudio null sink processed audio in 2-second
  blocks and capped them at 50 ms (`norewinds=1` in `test/lib/pulse.py`), which
  is likely this stall. If the rows stop voiding, enforce them.

Recommendations:

- Impair per the repo rule: `moq-shaper --rate` below the combined bitrate but
  above audio's, behind a deep but finite `--queue` whose overflow drops.
  Random loss is not the gate.
- Prefer the Rust ffmpeg pattern publisher media QA uses
  (`test/interop/clients/js/src/native.ts`, run by `run_native_media` in
  `test/interop/interop.sh`); the browser fixture publisher works otherwise.
- Budgets come from nightly runs by the rule in `budgets.json`'s note, not a
  developer machine. The native `pulse` ceilings there are provisional
  developer-host numbers; re-measure them from nightlies before enforcing.
- Priorities are signed since
  [moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142); assert on
  behavior, not on the 80 and 60 constants.
- Update the harness README's "Not covered" list, which names video.

Public API: none. Wire: none.

## Related

- [Audio playout](/quest/m2/audio-playout/README.md) - the playout rule and estimator whose budgets these rows extend
- [ViSQOL grading](/quest/m4/audio-visqol.md) - adds a quality score to the same harness
