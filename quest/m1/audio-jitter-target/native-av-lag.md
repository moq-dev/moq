# [S] moq play reports how far audio trails its presentation clock

## Goal

`moq play --metrics` reports how far the native audio playhead trails the
presentation clock video plays from, so every native row in
`test/audio-quality` can grade `av_lag_ms` as the replay rows do. Native's
playback decision does not change here.

## Plan

Decided 2026-11-09 (maintainer, /quest-complete on
[moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361)): ✅ keep the
PR's metrics half and revert its playback changes (rejected: land its stretch
keyed to the lag at arrival). That stretch held audio on a clock that goes
stale after a path-delay step, taking AAC `step` from 0 to 614 underruns a
minute, where `main`'s slow-down had hidden the stale clock by settling
behind it. It was built on the rule the
[epic](/quest/m1/audio-jitter-target/README.md) replaced the same day, so the
PR's recorded decisions "accept AAC step underruns" and "p50 within one pitch
period" go with it.

What stays: `sync::Consumer::position` and the `clock` field. The metrics
writer (`rs/moq-cli/src/play/metrics.rs`) writes each sample's `clock`, the
presentation clock's media time at the instant the playhead is read, with the
field and meaning `test/audio-quality/clients/js/src/schema.ts` gives the
browser's, so `analyze.ts` reduces native samples to `av_lag_ms` unchanged.
Leave it out while nothing is mapped, as other unreadable fields are.

Budgeting the lag belongs to
[the grading quest](/quest/m1/audio-jitter-target/grading.md), and native's
decision to [the stretch model](/quest/m1/audio-jitter-target/stretch.md).

Public API: an additive `sync::Consumer::position`. Wire: none.
