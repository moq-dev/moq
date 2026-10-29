# [S] A broadcast stamps everything from one clock

## Goal

Every group a moq-mux broadcast writes, its media and also its hang and MSF
catalog groups and HLS records, is stamped from the broadcast's own clock,
captured once when it is created. None reads the ambient `Timestamp::now()`,
so a call made from another runtime or clock context cannot move a track's
timestamps backwards.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7161](https://github.com/moq-dev/moq/pull/7161): one clock for
the whole broadcast is the fix, not stamping with `max(now, last)`, which
would hide the second clock.

Found 2026-10-29: media is stamped on the broadcast timeline
(`moq_mux::Clock`, held by the catalog producer), while the catalog's hang and
MSF groups (`rs/moq-mux/src/catalog/producer.rs`) and the HLS record track
(`rs/moq-mux/src/hls.rs`) stamp with ambient `Timestamp::now()`, which reads
whichever runtime or `Manual` clock is current and anchors on its origin. The
`Clock` itself also reads its instant from the ambient clock. In moq-ffi's
`containers_of_one_source_keep_their_relative_media_time`, the broadcast is
built under a paused runtime while some calls enter the FFI worker's
runtime, so one catalog track got groups from two anchors and was refused
with `TimestampMismatch` (moq-net checks group order per track).

- Find every stamp a broadcast writes that reads the ambient clock, and route
  it through the broadcast's clock.
- The catalog's timestamps change meaning if they move onto the media
  timeline. Check that against
  [Address groups by timestamp](/quest/m1/time-addressing/README.md) and
  record the choice.
- Fix the moq-ffi test's mixed clock contexts, or make it prove the new
  guarantee.

Tests: a broadcast written from two clock contexts keeps every track's
timestamps non-decreasing.

Public API: none expected; record any change. Wire: none.

## Related

- [Address groups by timestamp](/quest/m1/time-addressing/README.md) - defines what a group's timestamp means
