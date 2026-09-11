# Audio jitter target

## Goal

The audio playout target is a measured estimate of arrival timing, ported from
a known-good implementation, in the browser and natively alike. The round-trip
formula is gone. `max(20ms, 1.25 x minRtt)` sized the buffer for a single
retransmit, which describes how long the network takes to recover a loss and
not how unevenly a publisher emits frames, so a sender flushing 100 ms of media
at once got a target far too shallow to play through the next flush. One
written algorithm, cited by both implementations, produces the same target from
the same arrival trace.

Boundaries: convergence still uses skip-ahead and silence, so playing slightly
faster or slower to converge stays [Time
stretch](/quest/m2/watch-audio-time-stretch.md). No packet loss concealment.
Video keeps its own target; making the audio playhead the clock is [Plan: A/V
clock](/quest/m1/plan-av-clock.md).

## Plan

Three quests: the survey and the written algorithm first, then one
implementation per language against it. The two implementations are
independent once the spec lands and may run in parallel.

PR #3517 is closed unmerged. It is a historical prototype and source of a
runaway-target reproducer, not an implementation prerequisite. Revalidate any
reused plumbing against the current API. Native playback already has a
--delay-buffered sink; extend that owner rather than adding a second buffer.

This feature follows the dev release. Current freeze, restart-stall, capture
failure, and silent-audio fixes remain in M0 independently of this estimator.

## Quests

- [Spec](/quest/m2/audio-jitter-target/spec.md) - survey what already exists, then write the algorithm down once
- [Watch](/quest/m2/audio-jitter-target/watch.md) - js/watch and js/hang implement it, revalidating the closed prototype
- [Native](/quest/m2/audio-jitter-target/native.md) - native playout extends the existing sink using the same algorithm

## Closes

- [#3477](https://github.com/moq-dev/moq/issues/3477) - close this issue when the questline finishes
- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - the automated proof, built on its own schedule
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - inaudible convergence, on top of this
- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - the clock this target eventually feeds
