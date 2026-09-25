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
- Take this up when an application needs synchronized data playback; until
  then, a raw consumer reads payloads as they arrive.

## Required

- [Jitter clock](/quest/m1/jitter-flush-clock.md) - the `delay` field and `Sync` sizing this registers into
- [Data jitter](/quest/m1/data-jitter.md) - data tracks advertise the `delay` and `jitter` this reads

## Related

- [Cross-track correlation](/quest/m2/teleop/correlation.md) - joins recordings on the broadcast clock rather than live playout
