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
stretch](/quest/m1/watch-audio-time-stretch.md). No packet loss concealment.
Video keeps its own target; making the audio playhead the clock is [A/V
clock](/quest/m1/av-clock.md).

## Plan

Two quests: one implementation per language against the written algorithm.
The two implementations are independent and may run in parallel. Both are
additive and target `main`: the native knob is a new field on a
`#[non_exhaustive]` struct, and the browser estimator is a new module plus a
new `spread` observation.

Decided for landing: the line merges to `main` with a changelog
note for two behavior changes treated as fixes. `@moq/watch` `Sync` takes a
numeric delay literally instead of adding the rendition delay on top
([#3954](https://github.com/moq-dev/moq/pull/3954)), and `moq play --delay`
defaults to `auto` instead of `100ms`
([#3967](https://github.com/moq-dev/moq/pull/3967)). The old additive delay
was wrong, and both compile unchanged for existing callers. The line branch is
about 200 commits behind `main` with conflicts in `js/watch/src/sync.ts` and
`rs/moq-cli`; merge `main` in (never rebase the shared branch) before
finishing the watch quest. The raw #3477 traces are gone, so record fresh
traces with the audio quality harness in `test/audio-quality/` instead of
asking the reporter; they replace the #3477 traces wherever the quests name them.

The algorithm is written down at `doc/concept/audio-jitter.md`, with a
conformance corpus beside it that both implementations will read.

Neither `main` nor `release` has a measured estimator yet. `js/watch/src/sync.ts:159`
still computes `max(MIN_JITTER, minRtt * 1.25)` from the connection's PROBE,
and `js/watch/src/audio/latency.ts` still exists. `sync.ts` also adds the
advertised jitter to that term, where the document settles on a maximum.

The prior art from PR #3517 (closed 2026-09-10, branch deleted 2026-09-30)
landed on this line's branch through
[#3954](https://github.com/moq-dev/moq/pull/3954), so the watch quest works
there. It deletes the RTT term
(`MIN_JITTER`, `FALLBACK_JITTER`, `#minRtt`, and the `probe` input are gone
from `sync.ts`; `latency.ts` survives, minus `reanchorFloor`) and plumbs a
per-track arrival `spread` through `Container.Consumer`, measured at container
frame arrival and before the age budget can skip a group, which is the right
observation point. Its estimator, `js/hang/src/container/jitter.ts`, is a
decaying histogram of 5 ms buckets, 200 of them so the percentile saturates at
one second, read at the 95th percentile plus one frame, with the arrival
minimum expiring over two 30 s windows, `reanchor()` on a discontinuity, and
the step down bounded to one frame per second. `jitter.test.ts` and
`js/watch/src/audio/replay.test.ts` cover it. What it gets wrong is the extra
frame, learned from the first gap between observed timestamps, and a rise that
is immediate and unclamped, so a tune-in across a stale group sets the target
to seconds.

Note that `sync.ts` has since been refactored on `main` to a `register(jitter)`
list, which is one of the conflicts merging `main` in resolves.

Native has no jitter buffer at all. `rs/moq-audio`'s `decode::Options`
(`rs/moq-audio/src/decode/consumer.rs`) carries `max_age`, how far
playback may drift from the live edge before skipping a stalled group, and
`start`, where to begin on a track that already holds groups. Nothing pads the
buffer against uneven arrivals.

## Required

- [Watch](/quest/m0/audio-jitter-target/watch.md) - js/watch and js/hang bring the #3954 estimator into conformance
- [Native](/quest/m0/audio-jitter-target/native.md) - rs/moq-audio grows a measured jitter buffer from the same algorithm

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - inaudible convergence, on top of this
