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
independent once the spec lands and may run in parallel. All three are
additive and target `main`: the spec is a document, the native knob is a new
field on a `#[non_exhaustive]` struct, and the browser estimator is a new
module plus a new `spread` observation.

Neither `main` nor `dev` has a measured estimator. `js/watch/src/sync.ts:150`
still computes `max(MIN_JITTER, minRtt * 1.25)` from the connection's PROBE
(`sync.ts:36-39`, `:144-150`), and `js/watch/src/audio/latency.ts` still
exists.

The prior art is the branch of PR #3517, `origin/quest/m0/3477-watch-auto-latency`,
two commits ahead of `dev`. The PR is closed and never merged; the watch quest
starts from the branch rather than from `dev`. It already deletes the RTT term
(`latency.ts`, `MIN_JITTER`, `FALLBACK_JITTER`, `#minRtt`, and the `probe`
input are gone from `sync.ts`) and plumbs a per-track arrival `spread` through
`Container.Consumer`, measured at container frame arrival and before the age
budget can skip a group, which is the right observation point. Its estimator,
`js/hang/src/container/jitter.ts`, is a decaying histogram of 5 ms buckets, 200
of them so the percentile saturates at one second, read at the 95th percentile
plus one frame, with the arrival minimum expiring over two 30 s windows,
`reanchor()` on a discontinuity, and the step down bounded to one frame per
second. `jitter.test.ts` and `js/watch/src/audio/replay.test.ts` cover it. What
the branch still gets wrong is recorded in
[Spec](/quest/m2/audio-jitter-target/spec.md): the extra frame is learned from
the first gap between observed timestamps and the rise is immediate and
unclamped, so a tune-in across a stale group sets the target to seconds.

Native has no jitter buffer at all. `rs/moq-audio`'s decode `Config`
(`rs/moq-audio/src/decode/decoder.rs:60-80`) carries `max_age`, how far
playback may drift from the live edge before skipping a stalled group, and
`start`, where to begin on a track that already holds groups. Nothing pads the
buffer against uneven arrivals.

## Quests

- [Spec](/quest/m2/audio-jitter-target/spec.md) - survey what already exists, then write the algorithm down once
- [Watch](/quest/m2/audio-jitter-target/watch.md) - js/watch and js/hang bring the #3517 branch's estimator into conformance
- [Native](/quest/m2/audio-jitter-target/native.md) - rs/moq-audio grows a measured jitter buffer from the same algorithm

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Jitter clock](/quest/m2/jitter-flush-clock.md) - the advertised jitter (#3513 landed the flush span), whose relationship to the measured target the spec settles
- [Audio quality harness](/quest/m2/audio-quality-harness/README.md) - the automated proof, built on its own schedule
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - inaudible convergence, on top of this
- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - the clock this target eventually feeds
