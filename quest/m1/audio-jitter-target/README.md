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

Boundaries: both players converge on the target by playing slightly faster
or slower, NetEq's time stretch. No packet loss concealment. Video keeps its
own target; the shared clock stays on local time, and the audio ring's stretch
converges on it.

## Plan

Both implementations are on `main` (#4162): `js/hang/src/container/jitter.ts`
and `rs/moq-audio`'s `jitter.rs` follow `doc/concept/audio-jitter.md` and pass
its conformance corpus, `js/watch` `Sync` holds the deepest track's target in
`"auto"`, and `moq play --delay` defaults to `auto`. Both are the default for
every viewer, so `"auto"` shipped ahead of its browser proof; the maintainer
accepted that, and the re-measured
replay budgets, on 2026-10-04. Decided 2026-10-13: retain the remaining proof in m1 because the A/V
clock and worker release contracts depend on it. Native quality expansion
remains separately scoped in m2. The 2026-11-03 proof run found the 0.95
quantile leaves about six re-stalls a minute on real paths by design; decided
2026-11-03 (maintainer), time stretch closes that rather than a deeper
target, in both players from one algorithm and its vectors:
[moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856) natively and
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) in the browser's ring.

Decided 2026-11-04 (maintainer), for push-to-talk's silence suppression
([moq-dev/moq#7942](https://github.com/moq-dev/moq/pull/7942)): a silence the publisher declares
with an end marker is not idle, so the estimator folds no idle intervals for
it and keeps its estimate, where an undeclared gap still relaxes toward the
prior. The document's `silence` corpus case holds both implementations to it.

What the line still owes: its recorded trace replayed through
the native decode path too, asserting the same target series the browser's
`playout.test.ts` does. The audio quality harness's `pulse` rows already grade
native playback against its budgets.

## Required

- [Browser ceilings](/quest/m1/audio-jitter-target/browser-ceilings.md) - the chromium rows' ceilings re-measured from seven Nightlies with the browser's time stretch, budgeting `player_skips_per_min`
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - `moq play` copies NetEq's accelerate and preemptive expand, sharing the browser's algorithm and vectors, so its native rows reach zero steady-state underruns
- [Buffered converge](/quest/m1/audio-jitter-target/buffered-converge.md) - a buffered `<moq-watch>` time-stretches its audio back onto the shared clock after a stall or drift, so lip sync returns and no audio group is dropped
- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - the AAC browser budgets are re-measured from nightlies after the TS importer groups each PES

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

