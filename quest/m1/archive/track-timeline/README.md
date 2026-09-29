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
- Each track cuts on its own by one rule: at a group boundary between a
  minimum and maximum duration (a 4s minimum and a multiple of the declared
  duration, a zero minimum only for sparse data such as the catalog),
  splitting a long-lived group by frame at the maximum. Manual cuts stay as an optimization, such as a
  video keyframe cutting audio so derived segments need fewer objects.
- A stored object may hold a frame range of a group, not only whole groups.
- HLS and DASH segments are derived at the edge from group timestamps, not
  from storage objects. Fetching extra objects is fine when they land in the
  reader's cache for the next request.
- HLS `EXT-X-TARGETDURATION` is fixed for the run, not the observed maximum
  #4280 shipped. This reverses that PR's decision 4 (09-28 merged-PR audit).
  The target comes from each timeline's declared duration, which replaces the
  broadcast-wide `durationMax` (09-29 planning).

For triage, not blocking: two Codex P2s arrived after #4280 merged and are
unanswered. A timeline record whose `sequence` differs from its window index
is passed through unvalidated
([review](https://github.com/moq-dev/moq/pull/4280#pullrequestreview-5332725290)),
and `Timelines::track` re-enrolling a name while its old `Recorder` is alive
leaves two handles on one segmenter
([r4117516035](https://github.com/moq-dev/moq/pull/4280#discussion_r4117516035)).

## Required

- [Timelines declare their segment duration](/quest/m1/archive/track-timeline/declared-duration.md) - each timeline entry declares its segment duration (reported or estimated by the publisher), replacing the root `durationMax`
- [JS per-track timelines](/quest/m1/archive/track-timeline/js.md) - `@moq/hang` publishes and reads the same per-track timelines as Rust
- [Fixed HLS target duration](/quest/m1/archive/track-timeline/hls-target.md) - one `EXT-X-TARGETDURATION` for the run, from the reference timeline's declared duration; an overrun is listed with a warning
- [HLS timeline resubscribe](/quest/m1/archive/track-timeline/hls-resubscribe.md) - a transient timeline error re-subscribes instead of freezing the playlists or turning a rendition's segments into gaps

## Related

- [Catalog track identity](/quest/m2/catalog-tracks.md) - the catalog's own timeline gives it timestamps, but which catalog applies to a group stays there
