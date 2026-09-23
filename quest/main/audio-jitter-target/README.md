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
stretch](/quest/next/watch-audio-time-stretch.md). No packet loss concealment.
Video keeps its own target; making the audio playhead the clock is [Plan: A/V
clock](/quest/dev/plan-av-clock.md).

## Plan

Two quests: one implementation per language against the written algorithm.
The two implementations are independent and may run in parallel. Both are
additive and target `main`: the native knob is a new field on a
`#[non_exhaustive]` struct, and the browser estimator is a new module plus a
new `spread` observation.

The algorithm is written down at `doc/concept/audio-jitter.md`, with a
conformance corpus beside it that both implementations will read.

The browser implementation has landed on this line: `js/hang/src/container/jitter.ts`
observes each frame in `Container.Consumer` and passes the corpus, `js/watch`
composes each track's target and `Sync` holds the deepest one in `"auto"`. What
remains of the watch quest is proving it in a real browser.

Native has no jitter buffer at all. `rs/moq-audio`'s `decode::Options`
(`rs/moq-audio/src/decode/consumer.rs`) carries `max_age`, how far
playback may drift from the live edge before skipping a stalled group, and
`start`, where to begin on a track that already holds groups. Nothing pads the
buffer against uneven arrivals.

## Quests

- [Watch](/quest/main/audio-jitter-target/watch.md) - the browser's measured target, proven on Chrome and Safari against the public relay
- [Native](/quest/main/audio-jitter-target/native.md) - rs/moq-audio grows a measured jitter buffer from the same algorithm

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

## Related

- [Jitter clock](/quest/next/jitter-flush-clock.md) - the advertised jitter (#3513 landed the flush span), which `doc/concept/audio-jitter.md` settles as a floor on the measured target
- [Audio quality harness](/quest/next/audio-quality-harness/README.md) - the automated proof, built on its own schedule
- [Time stretch](/quest/next/watch-audio-time-stretch.md) - inaudible convergence, on top of this
- [Plan: A/V clock](/quest/dev/plan-av-clock.md) - the clock this target eventually feeds
