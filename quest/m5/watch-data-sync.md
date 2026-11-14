# [M] js/watch: play data tracks in sync with media

## Goal

A browser viewer receives JSON and binary track payloads on the same playhead
as the video and audio it plays, so telemetry shown beside a frame describes
that frame. A data track whose advertised `delay` and `jitter` exceed the
media's holds the media back by that much, and dropping the data track
releases it.

## Plan

- A `js/watch` data-track reader subscribes a catalog JSON or binary entry
  (built-in section or an application section embedding the config), releases
  each payload when the playhead reaches its frame timestamp, and registers its
  `delay` and `jitter` with `Sync` like a media rendition.
- Snapshot tracks release the newest state at or before the playhead, picked
  from the in-order states the consumer yields; stream tracks release every
  record in order.
- An untimed track's payloads add no timestamp wait; they apply in order as
  they arrive. Since 2026-10-05 a track is all timed or all untimed (the untimed
  model, [#4822](https://github.com/moq-dev/moq/pull/4822)), so no stream mixes the two.
- This replaces the sync code a web frontend writes today to hold KLV or
  MAVLink telemetry back to the video playhead.
- Register with `Sync` where it runs when this starts. It no longer waits on
  the [watch worker](/quest/m4/watch-worker.md), which moves `Sync` but
  changes nothing this reader needs; whichever lands second adapts.

Moved to m5 2026-11-12 (maintainer, /quest-audit): no named consumer, since that workaround exists.
