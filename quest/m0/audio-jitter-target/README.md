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

PR #3517 stays open and is fixed in place by the watch quest rather than
restarted. It already deleted the RTT term and plumbed a per-track arrival
`spread` through `Container.Consumer`, measured at container frame arrival and
before the age budget can skip a group, which is the right observation point.
What it got wrong is the estimator on top, which tested broken in a real
browser. The likely reason is in [Spec](/quest/m0/audio-jitter-target/spec.md):
it decays its histogram with NetEq's *reorder* forget factor rather than the
underrun one, and adds an observation per arrival instead of one per resampling
interval, so its target barely moves.

Native has no jitter buffer at all. `rs/moq-audio`'s decode `Config` carries
only `latency_max`, an upper bound before skipping a stalled group, and its own
doc comment promises "a companion `latency_min` for jitter-buffer padding will
land in a follow-up". This questline is that follow-up.

## Quests

- [Spec](/quest/m0/audio-jitter-target/spec.md) - survey what already exists, then write the algorithm down once
- [Watch](/quest/m0/audio-jitter-target/watch.md) - js/watch and js/hang implement it, fixing #3517 in place
- [Native](/quest/m0/audio-jitter-target/native.md) - rs/moq-audio grows `latency_min` from the same algorithm

## Closes

- [#3477](https://github.com/moq-dev/moq/issues/3477) - close this issue when the questline finishes
- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Jitter estimator](/quest/m0/3479-mux-jitter-flush-span.md) - the advertised flush span the measured target is held above
- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - the automated proof, built on its own schedule
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - inaudible convergence, on top of this
- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - the clock this target eventually feeds
