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
was wrong, and both compile unchanged for existing callers. The raw #3477
traces are gone, so record fresh traces with the audio quality harness in
`test/audio-quality/` instead of asking the reporter.

The algorithm is written down at `doc/concept/audio-jitter.md`, with a
conformance corpus beside it that both implementations will read.

The browser implementation has landed on this line: `js/hang/src/container/jitter.ts`
observes each frame in `Container.Consumer` and passes the corpus, `js/watch`
composes each track's target and `Sync` holds the deepest one in `"auto"`. What
remains of the watch quest is proving it in a real browser.

Native is done: `rs/moq-audio` estimates the target behind
`decode::Options::delay` and `decode::Consumer::delay`, passes the corpus both
directly and through the decode path, and `moq play --delay auto` holds it.
What the line still owes once the watch quest lands: its recorded trace
replayed through the native decode path too, asserting the same target series
the browser's `playout.test.ts` does. Grading native playback against the
harness budgets is [Audio quality native](/quest/m1/audio-quality-native.md).

## Required

- [Opus DTX](/quest/m0/opus-dtx.md) - the acceptance run measures network jitter, not DTX drift
- [Watch](/quest/m0/audio-jitter-target/watch.md) - the browser's measured target, proven on Chrome and Safari against the public relay

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - inaudible convergence, on top of this
