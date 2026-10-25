# [L] Catalog HLS field

## Goal

The hang catalog has no root `archive` section: `timelines`, `timescale`,
`replay`, `store`, and `version` are gone from `rs/hang`, `js/hang`,
`drafts/draft-lcurley-moq-hang.md`, `moq-mux`, and `moq-hls`. What remains of
timelines is an opt-in, per-rendition `hls` field that exists only for HLS
compatibility: it names a track whose records give each HLS segment's first
group and pts, and declares the nominal segment duration. A rendition without
it is not HLS-renderable.

## Plan

Decided by the maintainer on 2026-10-17:

- No publisher-owned archive pointer remains; archives are found by the host
  that stores them ([epic](/quest/m1/archive/README.md)).
- **Renamed to `hls`** (catalog field, track naming, types, wording), since it
  exists only for HLS compatibility, not archiving or seeking; seeking uses
  `Time(pts)` ranges. The track naming convention becomes `<track>.hls`;
  consumers still use the advertised name. Decided 2026-10-20 (maintainer):
  no `.z` suffix. This quest publishes it uncompressed, and
  [one compressed track per name](/quest/m1/compress/migrate.md), which
  lands after it, sets `compress: true` on the same name.
- **Opt-in.** `moq-mux` publishes it only when the application enables it,
  and nothing publishes it by default. Rejected: always-on timelines for every
  track, which cost every publisher for a feature most never use.
- **Aligned segments are the opt-in contract.** A publisher that enables `hls`
  aligns group boundaries across the renditions it lists, so segment n starts
  at the same pts in every rendition. This is part of why it is opt-in. An edge
  maps segment n to record n of each rendition directly; the reference
  rendition resolving others against their own timelines, keyframe snapping,
  and `EXT-X-GAP` for a rendition with no nearby boundary all go away
  (`rs/moq-hls/src/export/spans.rs`). An edge that finds renditions misaligned
  refuses that segment loudly rather than repairing it.
- **Per rendition, video and audio only.** The catalog track, data tracks, and
  untimed tracks get no `hls` field. Each declares its nominal segment
  duration, which is the landed per-timeline `duration` (#5715) moved onto the
  rendition; it never changes for the life of the track.
- **Records are (first group, pts).** Segments start at group boundaries, so
  mid-group splits, frame positions, and record `end`s go away. `EXTINF`
  derives from the next record's pts; an ended track's last record carries an
  explicit duration. Rejected: group only, which would make the edge read media
  to time a segment.
- The archive does not use these records, and DVR seeks with `Time(pts)`
  ranges.

Decided with the maintainer on 2026-10-24, from the moq-hls audit: this quest
is the export's segment-finalization redesign, with no separate quest. The
audit traced several bugs to a segment's frames, timing, and muxing inputs
being decided in different places, some again on each request:

- **Each rendition times its segments from its own records.** `EXTINF`, DASH
  `t`/`d`, the fragment's `tfdt`, and the recorder's duration all come from
  that rendition's record n and n+1. Today a keyframe-snapped rendition can
  serve up to a second more or less video than the reference row it
  advertises.
- **No catalog estimate changes media bytes.** A segment is served only once
  complete, so its last frame lasts until the next record's pts, or the ended
  track's explicit duration. Today `Rendition::muxer` is rebuilt per request
  from the live framerate, so the same URL can return a different `trun` after
  an estimate update. Rejected: snapshotting the framerate per instance, which
  is still a guess and wrong for VFR.
- **Slices start at group boundaries.** With mid-group records gone, no
  segment starts on an inter frame, so the fMP4 muxer's first-frame keyframe
  flag stays true. A group that does not open on an IDR is
  [export sync flags](/quest/m2/intra-refresh/export-sync-flags.md).
- **`Spans` goes, and with it its unbounded growth** while a stalled
  reference stops trimming a sibling's records.

Guidance:

- Fold in the old README's fix: `moq-mux`'s consumer refuses a record whose
  `sequence` differs from its window index
  ([review](https://github.com/moq-dev/moq/pull/4280#pullrequestreview-5332725290)).
- `js/hang` updates its schema and consumer; the JS producer and the
  `@moq/publish` opt-in are [JS HLS field](/quest/m2/js-hls.md).
- Update the draft's Timeline section (renamed for HLS) and changelog,
  `doc/concept/hang.md`'s Archive bullet, and `doc/bin/hls.md` (how to opt in,
  and the alignment it requires); run `just drafts check`.

Test that a catalog with the old root `archive` entry still parses with the
entry ignored, that nothing publishes `hls` unless enabled, that aligned
renditions list matching segments with a declared target, that a misaligned
segment is refused, and that a rendition without `hls` gets no playlist.

Public API: `hang::catalog::Archive` is deleted, the rendition types gain an
`hls` field in Rust and JS, and `moq-mux` gains the opt-in. Wire: the hang
catalog format.
