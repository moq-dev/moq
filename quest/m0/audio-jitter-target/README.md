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

Both implementations are on `main` (#4162): `js/hang/src/container/jitter.ts`
and `rs/moq-audio`'s `jitter.rs` follow `doc/concept/audio-jitter.md` and pass
its conformance corpus, `js/watch` `Sync` holds the deepest track's target in
`"auto"`, and `moq play --delay` defaults to `auto`. Both are the default for
every viewer, so `"auto"` shipped ahead of the Chrome and Safari proof the
Watch quest still owes; the maintainer accepted that, and the re-measured
replay budgets, on 2026-10-04.

What the line still owes once Watch lands: its recorded trace replayed through
the native decode path too, asserting the same target series the browser's
`playout.test.ts` does. Grading native playback against the harness budgets is
[Audio quality native](/quest/m1/audio-quality-native.md).

## Required

- [Watch](/quest/m0/audio-jitter-target/watch.md) - the browser's measured target, proven on Chrome and Safari against the public relay

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - inaudible convergence, on top of this
