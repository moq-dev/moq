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
  they arrive. Since 2026-10-05 a track is all timed or all untimed ([Typed
  timedness](/quest/m1/typed-timedness.md)), so no stream mixes the two.
- OneTooMany is the application this waited for (2026-10-01): their web
  frontend holds KLV and MAVLink telemetry back to the video playhead with
  its own sync code, which this replaces. In m2 rather than m1 because
  they aren't blocked.

## Required

- [JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md) - the reader releases each value by the timestamp its consumer returns
