# [S] js/watch: the measured auto latency is proven clean in a browser against a real relay

## Goal

The "Real-time" / auto preset plays clean audio on a LAN and against the
public relay: zero underruns after convergence and no skip-aheads in steady
state, proven by the browser harness and by a manual run on Chrome and
Safari, not only by the unit replay. The publisher-side audio encoder lag the
same report observed is measured in the same manual run and either confirmed
fixed or turned into a root-cause quest.

Boundaries: the estimator and the ring re-buffer themselves are PR #3517 on
`dev`; this quest only verifies them where the PR could not. Convergence
still uses skip-ahead and silence; time-stretching is
[Time stretch](/quest/m2/watch-audio-time-stretch.md).

## Plan

Branch from dev. #3517 lands the measured arrival `spread` per track, the
`max(advertised, measured)` target, one chunk of ring slack, re-stall on
underrun, and an underrun counter in the stats panel, verified with synthetic
replay traces. It leaves three things undone, which are this quest:

- Browser assertion: the `test/smoke` JS driver plays `bbb` from a local
  `moq import ts` with ffmpeg's default PES packing at auto and asserts the
  underrun counter stays at zero after convergence, on the isolated and the
  postMessage ring paths.
- Recorded traces: the arrival traces from #3477 live on the reporter's fork.
  Attach a trimmed copy under `js/watch` and replay them through both rings
  in `replay.test.ts` in place of the synthetic traces of the same shape.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured, with a real microphone and 40 ms or more of added RTT.
  Measure the audio encoder's input-to-output lag in the same run using the
  reporter's instrumented harness (fork branch `debug/rt-audio`); #3518 fixed
  the known cause (the encoder was rebuilt on every subscriber churn), so the
  7.35 s lag and the 88 to 275 ms/s drift the issue reported stand
  unconfirmed. If drift survives, the suspects are `writeFrame` opening a
  group per audio frame under WebTransport stream credit and the main-thread
  task queue that delivers encoder output; turn that into its own quest
  rather than fixing it here.

## Required

- PR #3517 has merged to `dev`

## Closes

- [#3477](https://github.com/moq-dev/moq/issues/3477) - close this issue when the quest finishes
- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report this reproduces on desktop; close it with this

## Related

- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - the audio-master clock that follows the estimator
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - convergence without skips or silence, on top of this
