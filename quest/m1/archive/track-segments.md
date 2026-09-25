# [M] Per-track archive segments

## Goal

Decide how a recording's tracks segment and expire independently, so a DVR
keeps the newest group of every track, such as a catalog that never changes,
while HLS still gets aligned audio and video segments. Produce the draft change
and rewritten implementation quests; this is a planning quest, not production
code.

## Plan

Today one timeline numbers segments for every track. A non-pacing track's group
is listed only in the segment open when it arrived, so a DVR expires a static
catalog with the first video segment and deletes the only copy. A long-lived
group, such as a catalog carrying deltas, is listed once in the segment it
opened in, so frames appended later are not addressable per segment.

Aligned segments exist mainly for HLS, and they break down for tracks that are
not audio or video. Open questions:

- A segment counter per track, so catalog or audio segment 0 need not align
  with video segment 0, versus a timeline per track.
- How HLS keeps aligned renditions under either shape, and whether every
  track's segments still flush at the same timeline commit.
- Whether a long-lived group's frames split across segments, trading an extra
  GET for per-segment addressability, while audio and video groups stay one
  object each.
- How expiry keeps each track's newest group, and what a reader or importer
  does when a track has nothing in the window.

Weigh the format and wire cost against the landed writer, reader, and HLS
exporter, and against [Catalog track identity](/quest/m2/catalog-tracks.md).
Present the recommendation for maintainer agreement before changing the draft.

## Related

- [Catalog track identity](/quest/m2/catalog-tracks.md) - which catalog applies to a media group, which per-track expiry must not settle by accident
