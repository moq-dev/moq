# [S] Replays play a speech-like signal

## Goal

The audio-quality replays drive the players with a signal whose pitch
period and level vary like speech, so a replay's lag p95 measures how fast
the player catches up, not how slowly the time stretch removes periods from
a constant tone.

## Plan

Decided 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)): keep.

On the replays' constant tone, the pitch search removes one 2.5 ms period a
30 ms window, so every replay p95 lag after a stall is a slow catch-up that
speech would finish faster (#8507's relay-mic-stall p95: 501.8 ms).

Recommendations:

- Synthesize the signal in the harness (a varying fundamental with
  syllable-rate level changes and short pauses above the -60 dBFS silence
  classifier), seeded, so replays stay deterministic and no clip needs a
  license.
- Re-measure the replay ceilings in `test/audio-quality/budgets.json` in the
  same PR, noting the signal change.

## Related

- [ViSQOL](/quest/m4/audio-visqol.md) - scores a recorded clip on the live rows; this changes only the replays' synthetic input
