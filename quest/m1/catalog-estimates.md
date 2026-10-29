# [M] Catalog estimates settle before they churn

## Goal

An unhinted moq-mux publisher's catalogs stop changing for small estimate
creep. Hang republishes `jitter` and `delay` only when they cross a coarse
step. The MSF catalog publishes once its estimates are made and then changes
only when the track list or a track's identity does. MSF-01 §5.3 fixes a
declared track's attributes.

## Plan

From [#7067](https://github.com/moq-dev/moq/issues/7067): an SDI encoder
through moq-c published 68 catalog versions in 32 s, and 16 even with every
hint from [#7068](https://github.com/moq-dev/moq/pull/7068). Of the ten that
weren't structural, eight were `delay` rising in ~8 ms steps.
[#6595](https://github.com/moq-dev/moq/pull/6595) already measures from
timestamps. `delay` still follows the cross-track write order, which tracks
runtime encode latency. It only rises, at most once per `ESTIMATE_WINDOW`
(`rs/moq-mux/src/catalog/tracks.rs:655-700`).

Decided 2026-10-28 (maintainer):

- **`jitter` and `delay` stay in hang.** @moq/watch, moq-audio and the TS
  export use them as latency floors, and a receiver can't measure cross-track
  skew itself. Hang keeps updating them in place; only a decoder identity
  change publishes a new track
  ([#7110](https://github.com/moq-dev/moq/pull/7110)).
- **Hang quantizes the rise.** A new `jitter` or `delay` republishes only when
  it crosses the next multiple of the track's frame duration. Measurement is
  unchanged.
- **MSF deduplicates.** `emit` (`rs/moq-msf` producer, `catalog/producer.rs:221-243`)
  skips a group whose serialized catalog matches the previous one.
- **MSF waits for estimates, then freezes them.** MSF publishes once every
  track with an unhinted estimate has closed its first group and carried
  `RATE_WINDOW` (1 s, `catalog/estimate.rs:7`) of media, whichever is later.
  The minimum duration matters because bitrate is measured over that window:
  20 ms audio groups would otherwise freeze it absent. Its estimated fields
  then stay as first declared, and MSF republishes only for track-list or
  identity changes. A track whose every estimated field is hinted publishes
  at once. moq-c can hint bitrate and framerate but not jitter or delay
  ([#7068](https://github.com/moq-dev/moq/pull/7068)), so its tracks wait.
- **No opt-out.** Quantizing and freezing remove the churn without new config
  surface on fields players rely on.
- The HLS pointer goes away with
  [time-addressed consumers](/quest/m1/time-addressing/consumers.md), so it is
  out of scope here.

Tests: a measured `delay` creeping in 1 ms steps republishes hang once per
frame-duration step. Identical MSF content publishes once. An unhinted MSF
catalog appears once the first groups close and a second of media has
passed, carries a bitrate for 20 ms audio groups, and keeps its estimates.

Public API: none. Wire: fewer catalog groups; MSF estimate fields frozen at
first declaration.

## Closes

- [#7067](https://github.com/moq-dev/moq/issues/7067) - a catalog describes what a track is: keep channel measurements from churning it
