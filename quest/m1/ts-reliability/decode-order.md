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
[#5949](https://github.com/moq-dev/moq/pull/5949)). Today there are two
authors: `DecodeClock::pop` (`rs/moq-mux/src/container/decode.rs`, TS and
MKV) settles a slot by the highest PTS read and a delay that only grows; FLV
export authors `PTS - jitter` bumped past the last DTS, which passes the PTS
on B-frames. #5949's `pop_counted`, which depended on the held frames alone,
left with `export ts --sync`. fMP4 export already anchors its decode timeline at each group's
first frame and needs no change.

Decided 2026-10-22 by the maintainer, after the TS root-cause audit:

- Frames carry PTS only, in decode order. A decoder needs order; an export
  needs *a* DTS that is monotonic, never after its PTS, and fits the T-STD
  buffer, not the source's own. Source DTS spacing (field coding, pulldown)
  is normalized, not preserved.
- Rejected: carrying decode time in the frame (CMAF composition offsets, a
  LOC property, Legacy inference). It adds a wire field to every container
  to preserve spacing no consumer needs.
- One clock serves TS, MKV and FLV, counting frames against the depth. The
  depth is fixed for the rendition's life, so it goes into the constructor
  (`DecodeClock::new(reorder)`) and the release is a plain `pop(flush)`.
  A new rendition gets a new clock. Delete the timestamp-settled `pop`, its
  growing delay, `reach`, FLV's `author_dts`/`dts_reserve`, and the catalog
  `jitter`'s role in DTS.
- The depth is a new optional `reorder` field on the Hang video rendition:
  the most frames that can precede a frame in decode order and follow it in
  presentation, the unit of H.264 `max_num_reorder_frames` and HEVC
  `sps_max_num_reorder_pics`.
- DTS shifts by slots, not by a time: the nth frame of a group decodes at the
  group's (n - `reorder`)th presentation slot, which no frame still to come
  can precede. The group's first `reorder` frames step one 90 kHz tick apart
  up to the keyframe's slot: the clock cannot know the group's spacing until
  frames it may not hold have arrived (`references_far_apart_keep_their_slots` in #5949),
  and one tick needs no spacing, keeps DTS increasing within a group and
  across groups without history, holds no frame past `reorder`, and only
  relaxes those frames' delivery deadlines (found in review). Rejected: a delay of
  `reorder` times a frame period, since the catalog `framerate` may change in
  place and two exporters would build different clocks (found in review), and
  freezing the rate or requiring an SPS fixed rate, which both add a rule
  the slot shift does not need.
- Those first frames can decode before the timeline's zero, so the clock
  works in signed ticks and each exporter maps them to its wire: TS wraps
  modulo 2^33 as its timestamps already do, MKV writes no DTS and only
  schedules by it, and FLV, whose tag time is unsigned, rebases its whole
  audio and video timeline at its first DTS. Never saturate or push a DTS
  past its PTS.
- Absent means 0: no reordering and no decode delay, so a stream without
  B-frames pays no latency. Rejected: absent meaning the level's
  `MaxDpbFrames` (hundreds of ms on High profile streams that never reorder),
  and inferring 0 from Baseline profile only (still charges High profile).
- Every producer that can reorder sets it before publishing the rendition:
  encoders from their config; importers (TS, FLV, fMP4, MKV) from the SPS's
  declared depth, else from the source's own decode offsets on its first
  keyframe (TS PTS - DTS, FLV composition time, fMP4 `ctts`/`trun`): none
  means 0, any means the level's `MaxDpbFrames`. MKV carries no decode
  offsets, so it takes `MaxDpbFrames` when the SPS declares nothing. An
  importer knows a frame reorders when its PTS is below an earlier frame's in
  decode order; one that reorders deeper than declared publishes a new
  rendition under a fresh name, as a PMT revision does. Rejected (from
  review): sampling the whole first group before publishing, which delays
  every start by a group to catch a source the new rendition already covers.
  `js/publish` refuses encoder output that reorders, since WebCodecs exposes
  no B-frame setting to declare.
- An exporter that reads a frame reordered deeper than its rendition's
  `reorder` refuses the export, never authors an invalid DTS. So does a
  `reorder` above the codec level's `MaxDpbFrames` (at most 16), which no
  conformant stream can reach, and a nonzero `reorder` on a codec whose
  presentation order is its decode order. This bounds the
  clock's held frames by the declared depth whatever the timestamps do, which
  replaces the release audit's export-reorder quest: flat or backward PTS can
  no longer hold frames without bound or drain quadratically.

Rust: `rs/hang` catalog; `rs/moq-mux` decode clock, TS/MKV/FLV export, and
the TS, FLV, fMP4 and MKV importers; `rs/moq-video` encoders. JS: `js/hang`
catalog, `js/publish`. Draft: `drafts/draft-lcurley-moq-hang.md` (the field
and its absent meaning), `drafts/draft-lcurley-moq-mpegts.md` (how export
authors DTS). Public API and wire: an optional catalog field whose absence
now means "never reorders", a breaking change for any producer that reorders
without it; add a `MIGRATE.md` entry for `hang`, `@moq/hang` and `moq-mux`, whose
direct users must now set it.

Tests: two exporters joining one B-frame source at different groups, and one
losing a group, author identical DTS for every group both read, in TS, MKV
and FLV; a stream without `reorder` exports with DTS equal to PTS; a frame
reordered past the declared depth refuses the export, and so does a `reorder`
above the level's `MaxDpbFrames`; a framerate change mid-rendition leaves every
DTS unchanged; a group starting at timestamp zero with a nonzero `reorder`
exports valid TS, MKV and FLV; a keyframe without a decode offset followed by
a reordered frame publishes a new rendition; a source deepening its
reordering publishes a new rendition; flat and backward PTS through MKV and
TS export retain at most `reorder` frames, swept over item count and payload
size; each importer sets `reorder` from the SPS and from source offsets.
Keep the T-STD controls passing.

## Related

- [Access units across PES](/quest/m1/ts-reliability/pes-access-units.md) - TS import's other video timing gap
