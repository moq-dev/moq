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

Boundaries: converging by playing slightly faster or slower is the browser's
[Watch](/quest/m1/audio-jitter-target/watch.md) quest. No packet loss concealment.
Video keeps its own target; making the audio playhead the clock is [A/V
clock](/quest/m1/av-clock.md).

## Plan

Both implementations are on `main` (#4162): `js/hang/src/container/jitter.ts`
and `rs/moq-audio`'s `jitter.rs` follow `doc/concept/audio-jitter.md` and pass
its conformance corpus, `js/watch` `Sync` holds the deepest track's target in
`"auto"`, and `moq play --delay` defaults to `auto`. Both are the default for
every viewer, so `"auto"` shipped ahead of the Chrome and Safari proof the
Watch quest still owes; the maintainer accepted that, and the re-measured
replay budgets, on 2026-10-04. Decided 2026-10-13: retain the remaining proof in m1 because the A/V
clock and worker release contracts depend on it. Native quality expansion
remains separately scoped in m2. The 2026-11-03 proof run found the 0.95
quantile leaves about six re-stalls a minute on real paths by design; decided
2026-11-03 (maintainer), Watch closes that by time stretch rather than a deeper
target, and `moq play` copies NetEq's stretch in m1 as
[Native stretch](/quest/m1/audio-jitter-target/native-stretch.md), sharing
one algorithm and its vectors with the browser's.

What the line still owes once Watch lands: its recorded trace replayed through
the native decode path too, asserting the same target series the browser's
`playout.test.ts` does. The audio quality harness's `pulse` rows already grade
native playback against its budgets.

## Required

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the browser plays the measured real-microphone rows with zero steady-state underruns by time-stretching the ring
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - `moq play` copies NetEq's accelerate and preemptive expand, sharing the browser's algorithm and vectors, so its native rows reach zero steady-state underruns
- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - the AAC browser budgets are re-measured from nightlies after the TS importer groups each PES

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

