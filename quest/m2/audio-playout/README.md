# Audio playout

## Goal

Every player (`moq play`, a live `<moq-watch>`, and a buffered one) plays
audio on the shared clock by one rule driven by audio's lag behind that
clock, measured where the audio is heard. In steady state audio and video
are in sync, and neither waits on the other or on the other's settings. A
discrepancy up to the `stretch` threshold, 200 ms by default, is corrected by
time stretch at no more than 10%, so audio is back on the clock within about
2 s; a larger one snaps the way video does, skipping late audio or padding
silence when audio is ahead. A buffered player keeps what it holds: it snaps
only past the larger of its lookahead and the threshold.

The playout target is a measured estimate of arrival timing, from one written
algorithm both players implement, and it falls back soon after a one-off
spike instead of holding audio and video seconds behind for 10 to 20 s.

Boundaries: zero underruns is not a goal, and no latency is added to reach
it. No packet loss concealment. Video stays on the shared clock and shows a
late frame when it arrives. Audio and video may desync during congestion, not
in steady state.

## Plan

The estimator is on `main` (#4162): `js/hang/src/container/jitter.ts` and
`rs/moq-audio`'s `jitter.rs` follow `doc/concept/audio-jitter.md` and pass its
conformance corpus, `js/watch` `Sync` holds the deepest track's target in
`"auto"`, and `moq play --delay` defaults to `auto`. Time stretch followed in
[moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856),
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058), and the
2026-11-09 rule [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)
and [moq-dev/moq#8532](https://github.com/moq-dev/moq/pull/8532) put in every
player. None of it is on `release` yet, so the cut publishes whatever this
epic leaves.

Decided 2026-11-13 (maintainer, /quest-plan, from an audit of `main`'s
audio): the rule in the Goal replaces the 2026-11-09 one. Measured on `main`
by two simulations of the real ring and stretch code and by the Nightlies of
11-12 and 11-13, that rule:

- Rests off the clock. NetEq's `DecisionLogic` is a buffer-level controller,
  and here the clock decides where audio belongs. With the newest frame J
  late, `level = target + frame - J`, so audio rests up to J behind: browser
  `wide` 32 to 41 ms, AAC `bursty` 37 to 47 ms, natively 70 to 80 ms after
  video's arrivals move the reference earlier.
- Catches up at the talker's pitch, 4 to 20%, and not at all on music or
  noise that fails correlation, which held a 1.4 s offset in simulation.
- Lets audio trail 2 s: congestion rows trail video by 1.1 to 1.4 s at p95 and
  take 17 to 28 s to return, against 2 to 3.3 s before #8507.
- Makes native video wait on audio's `--stretch` for a missing group.
- Ignores output latency in both players, and natively floors the shared
  clock at 50 ms instead.

Decisions, with the prompts as asked:

- Goal: ✅ one lag-driven rule that snaps past a small threshold, no audio
  setting makes video wait, lag measured where audio is heard, and the target
  falls back faster after a one-off spike (rejected: only shrinking the bound
  and speeding the catch-up; the same without the target fall).
- Threshold: ✅ 200 ms both ways, the rate capped at 10% and proportional to
  the lag below that (rejected: 200 ms behind and 100 ms ahead; 100 ms both
  ways; a fraction of the target). Every move of the clock, a target rise or
  fall or a reference move, is lag like any other, so a large fall snaps as
  video skips.
- Stretch: ✅ a continuous WSOLA time stretch for any content (rejected:
  NetEq's pitch-period splices driven by lag; resampling, which shifts pitch).
- Gaps: ✅ audio slows down when the audio received ahead of the playhead is
  about to run out, never trailing past the threshold (rejected: lag only,
  an underrun being silence).
- Target fall: ✅ the implementer measures and picks (rejected: a shorter
  half-life now; one observation per outage now). Picked 2026-11-13 by
  replaying three Nightlies' arrivals: one outage, the intervals its frames
  more than one interval late span and the empty ones before them, folds as
  one observation (`doc/concept/audio-jitter.md`, "One observation per
  stall"). A shorter half-life or a faster-decaying tail cost underruns on
  `wide` and `bursty` for less of the fall.
- Setting: ✅ `stretch` stays a duration and is the threshold, zero turning
  the stretch off; it sizes nothing else (rejected: an on/off switch;
  removing it).
- Buffered: ✅ a buffered player snaps past the larger of its lookahead and
  the threshold (rejected: the threshold alone).
- Milestone: ✅ this epic in m2 and its calibration in m3 (rejected: the whole
  epic with calibration in m2; only the lag rule in m2).

Overturned: from 2026-11-09, the 2 s bound, audio gliding to each new target
by stretching, NetEq's thresholds and rates untuned, and the estimator
staying as it is; from 2026-11-11, the bound alone deciding a lag skip. Kept:
lip sync in steady state with desync allowed under congestion, and a shared
clock that never follows audio (2026-11-05); latency never added to chase
zero underruns (2026-11-08); the target formula and the playout as the
largest `delay + target` among subscribed renditions; one algorithm with
shared vectors for both players.

Order: the 200 ms snap both ways and the
[lag rule](https://github.com/moq-dev/moq/pull/8722), a continuous stretch
that replaced the NetEq controller and splices under it, are done, as is the
shared clock following a lasting path-delay step
([moq-dev/moq#8749](https://github.com/moq-dev/moq/pull/8749)).
[Output latency](/quest/m2/audio-playout/output-latency.md) and the
[join's hold](/quest/m2/audio-playout/join-hold.md) are independent.
The [calibration](/quest/m3/audio-calibrate.md) re-measures every ceiling
after this epic.

Decided 2026-11-04 (maintainer), for push-to-talk's silence suppression
([moq-dev/moq#7942](https://github.com/moq-dev/moq/pull/7942)): a silence the
publisher declares with an end marker is not idle, so the estimator folds no
idle intervals for it and keeps its estimate, where an undeclared gap still
relaxes toward the prior. The document's `silence` corpus case holds both
implementations to it.

What the line still owes: its recorded trace replayed through the native
decode path too, asserting the same target series the browser's
`playout.test.ts` does.

## Closes

- [#2812](https://github.com/moq-dev/moq/issues/2812) - the iOS stutter report the same estimator fixes
