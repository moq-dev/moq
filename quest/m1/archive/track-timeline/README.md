# Per-track timelines

## Goal

Every track carries its own timeline, so tracks segment, commit, and expire
independently. A DVR keeps each track's newest group, such as a catalog that
never changes, and an append-only group stays addressable as frames arrive. An
edge like `moq-hls` derives HLS and DASH from group timestamps, so a publisher
never needs to know about HLS.

Nothing on `main` has users yet: break the timeline, catalog `archive` entry,
and recording format in place, with no compatibility path, even though hang
0.21 released the current shape. The children merge into this line's branch,
so Rust and JS reach `main` together.

## Plan

The Rust side has landed: a `hang::timeline::Record` is one span of its own
track (`sequence`, `pts`, `duration`, `start`/`end` group and frame positions),
`moq_mux::timeline::Timelines` publishes one timeline per enrolled track,
`moq-archive` writes recording version 2 (`<track>/segments/<n>` beside its
timeline's `segments/<n>`), and `moq-hls` derives segments from a reference
rendition's records (`rs/moq-hls/src/export/spans.rs`). The draft is updated
in moq-hang-04.

Decisions:

- One timeline track per track, live and recorded. `moq-mux` publishes them for
  every broadcast; an unsubscribed track costs nothing.
- The catalog's root `archive` entry maps each track to its timeline, including
  the catalog track itself. `replay`, `store`, and `version` stay beside it.
- Each track cuts on its own: automatically at a group boundary between a
  minimum and maximum duration (roughly 1s and 10s), splitting a long-lived
  group by frame at the maximum. Manual cuts stay as an optimization, such as a
  video keyframe cutting audio so derived segments need fewer objects.
- A stored object may hold a frame range of a group, not only whole groups.
- HLS and DASH segments are derived at the edge from group timestamps, not
  from storage objects. Fetching extra objects is fine when they land in the
  reader's cache for the next request.

## Quests

- [JS per-track timelines](/quest/m1/archive/track-timeline/js.md) - `@moq/hang` publishes and reads the same per-track timelines as Rust

## Related

- [Catalog track identity](/quest/m2/catalog-tracks.md) - the catalog's own timeline gives it timestamps, but which catalog applies to a group stays there
