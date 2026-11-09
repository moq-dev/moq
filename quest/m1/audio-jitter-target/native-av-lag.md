# [M] Native audio never settles behind its presentation clock

## Goal

`moq play --metrics` reports how far the native audio playhead trails the
presentation clock video plays from, so every native row in
`test/audio-quality` grades `av_lag_ms` (p50, p95) as the replay rows do, and
native's time stretch no longer settles audio behind that clock: at steady
state its p50 lag is at or below zero on every native row, and its ceilings
guard that.

Boundaries: the jitter estimator and its target stay as
`doc/concept/audio-jitter.md` specifies them. Underruns during congestion are
accepted, never bought down with latency. Native keeps compensating clock
drift between publisher and device, which the browser's ring cannot.

## Plan

Decided 2026-11-08 (maintainer, on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): never add
latency to chase zero underruns; audio and video keep separate clocks that
align at steady state once the delay is right.

[moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361) writes each
sample's presentation clock (`clock`, from `sync::Consumer::position`) and
moves native playback onto the browser's decision: the depth right after each
arrival, less what the stretch removed since, with an expand only once every
arrival over the last one to two seconds fell short, which still follows a
slower publisher clock. One local run of the 12 native rows each way (seed 7,
60 s, a loaded developer host) measured:

- `main`'s decision on `buffered`: A/V lag p50 11 to 59 ms on Opus and 67 to
  88 ms on AAC, which accelerated about 2 s a minute even at `fixed-250`
  while expanding into the gaps between PES bursts.
- The arrival decision: p50 1.3 to 6.6 ms on the adaptive rows, -13 and -16 ms
  at `fixed-250`, accelerating 10 to 340 ms a minute. Underruns rose on
  `step` (AAC 0 to 613.6 a minute, Opus 9.8 to 25) and AAC `wide` (0 to 17.3).

The AAC `step` underruns start at 47 s, when the estimator relaxes its target
from 223 to 186 ms. `moq play` sets the sink's target per frame to
`due(end) - now` on the shared clock, whose reference is the earliest arrival
ever and never moves later, while the estimator measures against the fastest
frame of the last 2 s. After the shaper's delay steps from 5 to 60 ms, the
clock-aligned depth stays 55 ms short of the estimator's target, below AAC's
~160 ms PES burst spacing. `main`'s decision hid that by settling a burst
behind the clock.

The remaining positive p50 is the accelerate's resting band: an ordinary
accelerate removes one pitch period only when it fits in the excess, so the
ring rests up to one period above its target, as the browser's does.

Once the decisions below settle, the native `av_lag_ms` ceilings come from
[Native stretch](/quest/m1/audio-jitter-target/native-stretch.md)'s
seven-Nightly calibration of this decision.

## Waiting

- Maintainer: AAC `step` underruns after the shared clock stops matching a
  persistent delay increase. Options: (1, recommended) accept them here and
  plan a quest so the shared clock's reference (`moq_mux::sync` and
  `js/watch` `Sync`) follows a delay increase every arrival in the
  estimator's window shows, which also re-aligns video; (2) floor the native
  sink target at the estimator's target, settling audio behind the clock by
  the step; (3) keep `main`'s decision. Answer on
  [moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361).
- Maintainer: the adaptive rows' p50 of 1.3 to 6.6 ms, the accelerate's
  one-pitch-period resting band, against the Goal's "at or below zero".
  Options: (1, recommended) accept a p50 within one pitch period as aligned
  and budget it as measured; (2) let an ordinary accelerate overshoot the
  target by up to one period, trading underruns for it in both players.
  Answer on [moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361).

## Related

- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native ceilings this lag joins, calibrated from this decision
- [Browser ceilings](/quest/m1/audio-jitter-target/browser-ceilings.md) - budgets `av_lag_ms` on the chromium rows the same way
