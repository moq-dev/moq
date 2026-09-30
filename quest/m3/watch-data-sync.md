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
- Snapshot tracks release the newest value at or before the playhead; stream
  tracks release every record in order.
- In m3 until an application needs synchronized data playback, such as
  teleop telemetry beside video; until then, a raw consumer reads payloads as
  they arrive.

## Required

- [Data track clock](/quest/m1/data-track-clock.md) - data timestamps share the media clock mapping, so the playhead can release them
