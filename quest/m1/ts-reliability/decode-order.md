# [XL] Decode time comes from decode order and a declared reorder depth

## Goal

Every exporter that authors a decode time (TS, MKV, FLV) derives it only from
the frames of the current group, in the decode order they arrive, and from the
rendition's declared reorder depth. Two exporters that read the same group
author the same DTS for it, whatever they joined at or lost before, and an
exporter never holds more video frames than that depth. A rendition that
declares no reordering adds no decode delay. Media frames stay
presentation-time only: no decode time on the wire.

## Plan

Why: a DTS that depends on what the exporter read before has been patched
six times ([moq-dev/moq#1843](https://github.com/moq-dev/moq/pull/1843),
[#2967](https://github.com/moq-dev/moq/pull/2967),
[#4500](https://github.com/moq-dev/moq/pull/4500),
[#4574](https://github.com/moq-dev/moq/pull/4574),
[#4645](https://github.com/moq-dev/moq/pull/4645),
[#5949](https://github.com/moq-dev/moq/pull/5949)). Today there are three
authors: `DecodeClock::pop` (`rs/moq-mux/src/container/decode.rs`, TS without
`--sync` and MKV) settles a slot by the highest PTS read and a delay that only
grows; `pop_counted` (TS `--sync`) depends on the held frames alone; FLV
export authors `PTS - jitter` bumped past the last DTS, which passes the PTS
on B-frames. fMP4 export already anchors its decode timeline at each group's
first frame and needs no change.

Decided 2026-10-22 by the maintainer, after the TS root-cause audit:

- Frames carry PTS only, in decode order. A decoder needs order; an export
  needs *a* DTS that is monotonic, never after its PTS, and fits the T-STD
  buffer, not the source's own. Source DTS spacing (field coding, pulldown)
  is normalized, not preserved.
- Rejected: carrying decode time in the frame (CMAF composition offsets, a
  LOC property, Legacy inference). It adds a wire field to every container
  to preserve spacing no consumer needs.
- One clock serves TS, MKV and FLV. The counted path becomes the only one,
  and it is not called `pop_counted`: the depth and delay are fixed for the
  rendition's life, so they move into the constructor
  (`DecodeClock::new(depth, delay)`) and the release is a plain `pop(flush)`.
  A new rendition gets a new clock. Delete the timestamp-settled `pop`, its
  growing delay, `reach`, FLV's `author_dts`/`dts_reserve`, and the catalog
  `jitter`'s role in DTS.
- The depth is a new optional `reorder` field on the Hang video rendition:
  the most frames that can precede a frame in decode order and follow it in
  presentation, the unit of H.264 `max_num_reorder_frames` and HEVC
  `sps_max_num_reorder_pics`. The delay is `reorder` times the frame period
  (the SPS's fixed rate, else the catalog `framerate`).
- Absent means 0: no reordering and no decode delay, so a stream without
  B-frames pays no latency. Rejected: absent meaning the level's
  `MaxDpbFrames` (hundreds of ms on High profile streams that never reorder),
  and inferring 0 from Baseline profile only (still charges High profile).
- Every producer that can reorder sets it before publishing the rendition:
  encoders from their config; importers from the SPS's declared depth, else
  from the source's own decode offsets (TS PTS - DTS, FLV composition time,
  fMP4 `ctts`/`trun`) on its first keyframe: none means 0, any means the
  level's `MaxDpbFrames`. A source that later reorders deeper than declared
  publishes a new rendition under a fresh name, as a PMT revision does.
  `js/publish` refuses encoder output that reorders, since WebCodecs exposes
  no B-frame setting to declare.
- An exporter that reads a frame reordered deeper than its rendition's
  `reorder` refuses the export, never authors an invalid DTS. This bounds the
  clock's held frames by the declared depth whatever the timestamps do, which
  replaces the release audit's export-reorder quest: flat or backward PTS can
  no longer hold frames without bound or drain quadratically.

Rust: `rs/hang` catalog; `rs/moq-mux` decode clock, TS/MKV/FLV export, and
the TS, FLV and fMP4 importers; `rs/moq-video` encoders. JS: `js/hang`
catalog, `js/publish`. Draft: `drafts/draft-lcurley-moq-hang.md` (the field
and its absent meaning), `drafts/draft-lcurley-moq-mpegts.md` (how export
authors DTS). Public API and wire: an optional catalog field whose absence
now means "never reorders", a breaking change for any producer that reorders
without it; add a `MIGRATE.md` entry for `hang` and `@moq/hang`.

Tests: two exporters joining one B-frame source at different groups, and one
losing a group, author identical DTS for every group both read, in TS, MKV
and FLV; a stream without `reorder` exports with DTS equal to PTS; a frame
reordered past the declared depth refuses the export; a source deepening its
reordering publishes a new rendition; flat and backward PTS through MKV and
TS export retain at most `reorder` frames, swept over item count and payload
size; each importer sets `reorder` from the SPS and from source offsets.
Keep the T-STD controls passing.

## Related

- [Deterministic TS legs for H.265](/quest/m2/ts-hitless-h265.md) - the `--sync` group anchor this path shares
- [Access units across PES](/quest/m1/ts-reliability/pes-access-units.md) - TS import's other video timing gap
