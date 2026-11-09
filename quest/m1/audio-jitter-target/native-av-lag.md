# [M] Native audio never settles behind its presentation clock

## Goal

`moq play --metrics` reports how far the native audio playhead trails the
presentation clock video plays from, so every native row in
`test/audio-quality` grades `av_lag_ms` (p50, p95) as the replay rows do. If
native's time stretch settles audio behind that clock, as the browser's did
before [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058), native
stops spending latency on it: at steady state its p50 lag is at or below zero
on every native row, and its ceilings guard that.

Boundaries: the jitter estimator and its target stay as
`doc/concept/audio-jitter.md` specifies them. Underruns during congestion are
accepted, never bought down with latency. Native keeps compensating clock
drift between publisher and device, which the browser's ring cannot.

## Plan

Decided 2026-11-08 (maintainer, on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): never add
latency to chase zero underruns; audio and video keep separate clocks that
align at steady state once the delay is right. The browser's ring now decides
on its depth right after each arrival and never expands, because expanding on
the depth between arrivals settled audio a burst behind the shared clock
(`bursty-aac` lag p50 1 to 289 ms). Native's `Stretch::decide`
(`rs/moq-audio/src/stretch.rs`) still decides on `buffered` at each callback,
so it may do the same.

1. Report the lag. The metrics writer (`rs/moq-cli/src/play/metrics.rs`)
   already holds the presentation `moq_mux::sync::Consumer` and the
   `Playhead`. Write each sample's `clock`, the presentation clock's media
   time at the instant the playhead is read, with the field and meaning
   `test/audio-quality/clients/js/src/schema.ts` gives the browser's, so
   `analyze.ts` reduces native samples to `av_lag_ms` unchanged. Leave it out
   while nothing is mapped, as other unreadable fields are.
2. Measure. Run the native rows locally against `main`'s decision, then read
   the scheduled Nightlies. Lag p50 near or below zero means native already
   aligns: budget `av_lag_ms` by the native rule and finish.
3. If p50 sits above zero on any row, decide on the depth right after each
   arrival, less what the stretch removed since, as the browser's
   `AudioRingBuffer` does, and expand only to follow a publisher clock
   slower than the device's, not to fill the gap between arrivals. Hold the
   listened drift case (media per render second about 0.998 against a 0.998
   publisher) and the shared stretch corpus, and update the spec's native
   decision in the same PR.

[Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) is
calibrating the native ceilings from seven Nightlies of today's decision.
Step 3 moves those ceilings, so a decision change re-measures them by the
same rule rather than stacking on the old calibration.

## Related

- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native ceilings this lag joins, calibrated from the current decision
- [Browser ceilings](/quest/m1/audio-jitter-target/browser-ceilings.md) - budgets `av_lag_ms` on the chromium rows the same way
