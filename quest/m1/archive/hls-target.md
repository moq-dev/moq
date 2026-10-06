# [S] Fixed HLS target duration

## Goal

A `moq-hls` media playlist advertises one `EXT-X-TARGETDURATION` for the
whole run, taken from the reference timeline's declared duration, so every
edge and every reload agree on it. Segments stay one-to-one with timeline
records: the edge never skips, splits, or renumbers one.

## Plan

[#4280](https://github.com/moq-dev/moq/pull/4280) derives the target from the
current window's longest segment (`snapshot_as` in
`rs/moq-hls/src/export/rendition.rs`), so it rises when a long record arrives
and falls when that record is evicted. Codex flagged it
([r4113921580](https://github.com/moq-dev/moq/pull/4280#discussion_r4113921580)).
The 09-28 audit reversed #4280's decision 4 (the observed maximum), and the
09-29 planning settled the rest:

- The target is `ceil` of the reference timeline's declared duration from
  [Timelines declare their segment duration](/quest/m1/archive/declared-duration.md).
  Until that timeline's entry is declared, the rendition has no playlist yet.
- A segment whose rounded `EXTINF` exceeds the target (a GOP that overran the
  publisher's declared value) is listed anyway, with a rate-limited warning.
  This deliberately deviates from RFC 8216 4.3.3.1's MUST, because refusing
  the segment loses content, and splitting or skipping it at the edge breaks
  the stable media sequence numbers of RFC 8216 6.2.2. Document the
  deviation. hls.js tolerates it; Apple's mediastreamvalidator flags it.
- A record the publisher split mid-group at its safety ceiling is listed as
  its own segment. Stop merging split halves back into one range
  (`a_frame_split_record_is_not_a_video_boundary` in `spans.rs`). This is legal
  because moq-hls never emits `EXT-X-INDEPENDENT-SEGMENTS`, so a segment may
  start without a keyframe.
- A non-reference rendition keeps the reference record's `EXTINF`, even though
  its keyframe-snapped content can run longer. The user decided this needs no
  change.

Replace `target_duration_follows_the_observed_segments` with tests that pin
the declared target across a window whose segment durations vary, and that
cover an overrun at the rounding boundary (listed, warned). Fix its stale
comment ("default 1s minimum"; the minimum is 2s). Hold the target per
playlist URI, not per `Rendition`: `renditions::Producer::sync` replaces the
`Rendition` on a catalog reconfigure, so test that a reconfigure mid-run
keeps the original target.

## Required

- [Timelines declare their segment duration](/quest/m1/archive/declared-duration.md) - the declared value the target is taken from

## Related

- [Per-track timelines](/quest/m1/archive/track-timeline.md) - the quest this blocks
