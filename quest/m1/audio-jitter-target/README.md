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

Every player follows one rule around that target, with NetEq's time stretch.
In steady state audio plays on the shared clock at the playout delay, which is
the expected resting point for audio and video alike. When the audio received
ahead of the playhead runs low, playback slows down and audio falls behind the
clock; once the buffer refills past the target, playback speeds up until audio
is back on it. Congestion then costs a gap and a catch-up instead of the audio
that arrived late.

Boundaries: zero underruns is not a goal, and neither the target nor a fixed
delay is raised to reach it. No packet loss concealment. Video stays on the
shared clock and shows a late frame when it arrives, so it recovers on its
own. Video measures no network term: the shared delay is the largest
requirement among the subscribed renditions, so video waits for audio's.

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

Decided 2026-11-09 (maintainer, /quest-plan): the rule in the Goal. It
replaces the 2026-11-08 decision on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) that the browser
only plays faster and audio never settles later. Video gets the effect for
free, since a late frame shows at once and the picture is live again; audio
cannot, so without a slow-down and a catch-up a stall drops whatever audio
arrives late. What carries over from 2026-11-08: latency is never added to
reach zero underruns, and some underruns during congestion are expected.

- Players: ✅ `moq play`, a live `<moq-watch>`, and a buffered one, on one rule
  (rejected: live only, with buffered mode keeping its own).
- Target: ✅ unchanged. The shared delay is
  `max over subscribed renditions of (delay + target)`, where audio's target
  is `max(measured, jitter) + frame` and video's is its catalog span
  (rejected: a network term on every rendition; the plain sum
  `delay + jitter + frame + measured`, which buffers a batched publisher's
  batch twice; a catalog-only target).
- Video: ✅ on the same clock as audio, and audio alone speeds up and slows
  down with the network (rejected: video at its catalog target, with audio
  trailing it by the measured lateness).
- Target moves: ✅ the estimator stays as it is, and audio glides to each new
  target by stretching (rejected: a slew limit on the measured term, which
  the spec already declined as a second knob; deleting the estimator and
  letting the lag be the only network term, which underruns a 20 ms Opus
  target until the lag builds).
- Bound: ✅ configurable, as
  [the `stretch` setting](/quest/m1/audio-jitter-target/stretch-setting.md)
  records.
- Tests: no gate may reward moving away from the rule, so
  [the grading quest](/quest/m1/audio-jitter-target/grading.md) lands before
  [the model](/quest/m1/audio-jitter-target/stretch.md).
- Milestone: ✅ every new child in m1, here (rejected: the model and grading
  in m0; the setting in m2).

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

- [Grading](/quest/m1/audio-jitter-target/grading.md) - the audio harness grades steady-state lag, dropped audio, and recovery from a stall, with no gate fixed at zero underruns or zero lag
- [Stretch model](/quest/m1/audio-jitter-target/stretch.md) - every player slows down when its buffer runs low and speeds up once it refills, by one rule and one set of vectors, buffered mode included
- [The `stretch` setting](/quest/m1/audio-jitter-target/stretch-setting.md) - a player sets how far audio may trail before it skips, and zero turns the stretch off
- [Calibrate](/quest/m1/audio-jitter-target/calibrate.md) - every browser and native ceiling re-measured from seven Nightlies with the model
- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - the AAC browser budgets are re-measured from nightlies after the TS importer groups each PES
- [Native A/V lag](/quest/m1/audio-jitter-target/native-av-lag.md) - `moq play --metrics` reports how far audio trails its presentation clock, so native rows grade `av_lag_ms`
- [Native rendition delay](/quest/m1/audio-jitter-target/native-rendition-delay.md) - `moq play` holds each subscribed rendition's catalog `delay` plus its target, as the browser does
- [Chromium Opus stalled share](/quest/m1/audio-jitter-target/opus-stalled-share.md) - the Chromium Opus browser rows' stalled-share ceiling is measured with the stretch, and passes

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes

