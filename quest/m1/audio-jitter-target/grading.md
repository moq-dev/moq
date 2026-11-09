# [M] The audio harness grades the stretch model

## Goal

No audio test, budget, or benchmark rewards a player for moving away from the
[epic's model](/quest/m1/audio-jitter-target/README.md). On today's player,
before the model lands, `test/audio-quality` grades:

- Steady-state lag: every row, native and browser included, carries
  `av_lag_ms` p50, so latency can no longer buy fewer underruns unnoticed.
- Temporary lag passes: p95 lag is a ceiling with measured headroom on the
  browser and native rows, and no note calls audio trailing through a stall a
  regression.
- Dropped audio is the player's own count (`trimmed_ms`, `player_skips`).
  Speeding up is not a skip.
- No gate is fixed at zero underruns or zero stalls.
- A congestion row, where the path stalls for longer than the target and
  then delivers the backlog, reports the audio dropped and how long audio
  takes to return to the clock.

Boundaries: no player behavior changes here. Ceilings are measured, never
chosen.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan: "make sure there are no tests or
benchmarks that cause us to optimize in the wrong direction"). ✅ m1, in this
epic, and before [the model](/quest/m1/audio-jitter-target/stretch.md), so
its implementer is never graded against the old rule.

The gates the audit of `main` found pushing the wrong way (2026-11-09,
`10772522a5`):

- `test/audio-quality/budgets.json` budgets the replay rows' `av_lag_ms` p50
  and p95 exactly, at -19 to -22 ms, and its note says "any change that
  settles audio later to buy fewer underruns fails them". That forbids any
  temporary lag and treats audio leading the clock as the resting point.
- `skip_aheads_per_min` and `discarded_ms_per_min`, on all 29 rows, infer a
  skip from a step in the playhead's lag, so a catch-up reads as dropped
  audio. Decided 2026-11-09 (maintainer, on
  [moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308#issuecomment-6730322286)):
  ✅ budget the player's own trims (rejected: keep budgeting accelerates as
  `discarded_ms`).
- The 24 browser and native rows cap underruns with no ceiling on lag. Native
  settled 60 to 88 ms behind its clock without failing one.
- `test/audio-quality/clients/js/ptt-analyze.ts` fails on any underrun or
  stall. [Push-to-talk latency](/quest/m1/push-to-talk-latency.md) owns that
  change in
  [moq-dev/moq#8364](https://github.com/moq-dev/moq/pull/8364); leave it there.
- The unit tests that pin the old rule belong to the model quest, which
  rewrites them with the behavior.

Neutral, and left alone: `just rs bench-playback` and
`js/watch/bench/ring.ts` measure CPU per callback, and the estimator's
conformance corpus measures the target.

Recommendations:

- Replay rows stay exact. Rewrite the note to say which direction is a
  regression: a steady-state p50 lag that rises, or audio the player
  trimmed. A p95 lag that rises through a stall, or an underrun count that
  moves, is not one by itself.
- Add the congestion case twice: a shaper profile that queues until packets
  drop, per the repository's impairment rule, and a replay trace where media
  trickles in late after a stall, which the audit of #8360 asked for.
- New ceilings on the browser and native rows are provisional until
  [the re-measure](/quest/m1/audio-jitter-target/calibrate.md), as the native
  rows already are.
- The `step` rows' lag is budgeted as measured, with a sentence in the note:
  the shared clock's reference only moves earlier, so a lasting path-delay
  step leaves it stale. The follow-up is deferred until the model's numbers
  exist.
- Update `test/audio-quality/README.md` for every metric added or dropped.

Public API: none. Wire: none.

## Required

- [Native A/V lag](/quest/m1/audio-jitter-target/native-av-lag.md) - `moq play --metrics` reports the presentation clock, so native rows can grade `av_lag_ms`

## Related

- [Audio outlives video under bufferbloat](/quest/m4/audio-bufferbloat-av.md) - adds audio and video rows through a rate-limited link to the same harness
